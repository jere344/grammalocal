//! `setup`, `status` and `uninstall`: wire the daemon into systemd, GNOME and
//! the XDG directories, and take it back out again.

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use grammalocald::config::{self, Config};

use crate::sys::{self, BINARIES, SERVICE};

const MEDIA_KEYS: &str = "org.gnome.settings-daemon.plugins.media-keys";
const CUSTOM_KEY_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
const SHORTCUT_DIR: &str =
    "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/grammalocal/";
const SHORTCUT_NAME: &str = "Grammalocal check";
const SHORTCUT_BINDING: &str = "<Super>c";
const HEALTH_TIMEOUT: Duration = Duration::from_secs(15);

/// Asks a question unless `yes` is set, in which case `default` is taken.
fn ask(yes: bool, question: &str, default: bool) -> bool {
    if yes {
        default
    } else {
        sys::confirm(question, default)
    }
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn python_version() -> Option<(u32, u32)> {
    let out = sys::capture(
        "python3",
        &["-c", "import sys; print('%d %d' % sys.version_info[:2])"],
    )?;
    let mut parts = out.split_whitespace().map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next()??))
}

fn gtk4_present() -> bool {
    sys::stdout_of("ldconfig", &["-p"])
        .or_else(|| sys::stdout_of("/sbin/ldconfig", &["-p"]))
        .is_some_and(|out| out.contains("libgtk-4.so.1"))
}

/// Splits a gsettings string-array such as `['a', 'b']` or `@as []` into its items.
pub fn parse_list(value: &str) -> Vec<String> {
    value
        .split('\'')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, item)| item.to_string())
        .collect()
}

fn format_list(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|i| format!("'{i}'")).collect();
    format!("[{}]", quoted.join(", "))
}

/// Returns the new value with `path` appended, or `None` if it is already listed.
pub fn append_keybinding(current: &str, path: &str) -> Option<String> {
    let mut items = parse_list(current);
    if items.iter().any(|p| p == path) {
        return None;
    }
    items.push(path.to_string());
    Some(format_list(&items))
}

/// Returns the new value with `path` removed.
pub fn remove_keybinding(current: &str, path: &str) -> String {
    let items: Vec<String> = parse_list(current)
        .into_iter()
        .filter(|p| p != path)
        .collect();
    format_list(&items)
}

fn gsettings_list() -> Option<String> {
    sys::find_on_path("gsettings")?;
    sys::stdout_of("gsettings", &["get", MEDIA_KEYS, "custom-keybindings"])
}

fn shortcut_registered() -> bool {
    gsettings_list().is_some_and(|v| parse_list(&v).iter().any(|p| p == SHORTCUT_DIR))
}

fn wait_healthy(port: u16) -> Result<()> {
    let deadline = Instant::now() + HEALTH_TIMEOUT;
    loop {
        if let Ok((200, _)) = sys::http_get(port, "/health") {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "grammalocald did not answer /health on port {port} within 15 s; \
                 see: journalctl --user -u {SERVICE}"
            );
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn register_shortcut(popup: &Path) -> Result<()> {
    let popup = path_str(popup)?;
    if popup.contains('\'') {
        bail!("cannot register shortcut: path contains a single quote: {popup}");
    }
    let current = gsettings_list().unwrap_or_else(|| "@as []".to_string());
    if let Some(new) = append_keybinding(&current, SHORTCUT_DIR) {
        sys::run_ok(
            "gsettings",
            &["set", MEDIA_KEYS, "custom-keybindings", &new],
        )?;
    }
    let schema = format!("{CUSTOM_KEY_SCHEMA}:{SHORTCUT_DIR}");
    sys::run_ok(
        "gsettings",
        &["set", &schema, "name", &format!("'{SHORTCUT_NAME}'")],
    )?;
    sys::run_ok(
        "gsettings",
        &["set", &schema, "command", &format!("'{popup}'")],
    )?;
    sys::run_ok(
        "gsettings",
        &["set", &schema, "binding", &format!("'{SHORTCUT_BINDING}'")],
    )?;
    Ok(())
}

fn remove_shortcut() -> Result<()> {
    let Some(current) = gsettings_list() else {
        return Ok(());
    };
    if !parse_list(&current).iter().any(|p| p == SHORTCUT_DIR) {
        return Ok(());
    }
    let new = remove_keybinding(&current, SHORTCUT_DIR);
    sys::run_ok(
        "gsettings",
        &["set", MEDIA_KEYS, "custom-keybindings", &new],
    )?;
    let schema = format!("{CUSTOM_KEY_SCHEMA}:{SHORTCUT_DIR}");
    sys::run_ok("gsettings", &["reset-recursively", &schema])?;
    println!("removed GNOME shortcut {SHORTCUT_BINDING}");
    Ok(())
}

pub fn setup(yes: bool) -> Result<()> {
    println!("Checking prerequisites");
    match python_version() {
        Some((major, minor)) if (major, minor) >= (3, 8) => {
            println!("  python3 {major}.{minor}: ok");
        }
        Some((major, minor)) => bail!("python3 {major}.{minor} found, 3.8 or newer is required"),
        None => bail!("python3 not found: install Python 3.8 or newer"),
    }
    let gtk = gtk4_present();
    println!(
        "  GTK4 (libgtk-4.so.1): {}",
        if gtk { "ok" } else { "missing, popup disabled" }
    );
    if !sys::systemctl_quiet(&["show-environment"]) {
        bail!("systemd user session unavailable (systemctl --user failed)");
    }
    println!("  systemd user session: ok");

    let bin = sys::bin_dir()?;
    let daemon = bin.join("grammalocald");
    let unit = sys::unit_path()?;
    if !daemon.is_file() || !unit.is_file() {
        bail!("grammalocald is not installed; run install.sh first");
    }

    let dir = config::config_dir()?;
    let cfg = Config::load_or_create(&dir)?;
    println!("Config: {}", dir.display());

    println!("Enabling the systemd user service");
    sys::systemctl(&["daemon-reload"])?;
    if !sys::systemctl(&["enable", "--now", SERVICE])? {
        bail!("systemctl --user enable --now {SERVICE} failed");
    }
    wait_healthy(cfg.port)?;
    println!("  daemon healthy on 127.0.0.1:{}", cfg.port);

    let popup = bin.join("grammalocal-popup");
    if sys::find_on_path("gsettings").is_some() {
        if ask(
            yes,
            "Register the GNOME shortcut Super+C to open the checker?",
            true,
        ) {
            register_shortcut(&popup)?;
            println!("  GNOME shortcut {SHORTCUT_BINDING} registered");
        }
    } else {
        println!(
            "  Not on GNOME: bind `{}` to a key in your desktop's keyboard settings.",
            popup.display()
        );
    }

    if gtk && popup.is_file() {
        if cfg.apply_mode == "replace" {
            println!("  Apply mode: replace (corrections are pasted automatically)");
        } else if ask(
            yes,
            "Paste corrections automatically into the app? (asks for a one-time screen-control permission)",
            false,
        ) {
            sys::run_ok(path_str(&popup)?, &["--setup-replace"])?;
        } else {
            println!("  Apply mode: copy (press Ctrl+V after Apply). Change later with `grammalocal-popup --setup-replace`.");
        }
    }

    let url = format!("http://localhost:{}/v2", cfg.port);
    println!();
    println!("Browser setup (Firefox or Chrome)");
    println!("  1. Install the LanguageTool extension:");
    println!("       Firefox: https://addons.mozilla.org/firefox/addon/languagetool/");
    println!(
        "       Chrome:  https://chromewebstore.google.com/detail/oldceeleldhonbafppcapldpdifcinji"
    );
    println!("  2. In its settings: Advanced settings > LanguageTool server > Other server");
    println!("  3. Enter: {url}");
    println!();
    println!("Done. Select text in any app and press Super+C, or just type in your browser.");
    println!("Check the service any time with `grammalocal status`.");
    Ok(())
}

pub fn status() -> Result<()> {
    let dir = config::config_dir()?;
    println!("config:   {}", dir.display());
    let unit = sys::unit_path()?;
    println!(
        "unit:     {} ({})",
        unit.display(),
        if unit.is_file() {
            "installed"
        } else {
            "missing"
        }
    );
    let active = sys::systemctl_quiet(&["is-active", "--quiet", SERVICE]);
    println!("service:  {}", if active { "active" } else { "inactive" });
    if let Some(mem) = sys::stdout_of(
        "systemctl",
        &["--user", "show", "-p", "MemoryCurrent", "--value", SERVICE],
    ) {
        match mem.parse::<u64>() {
            Ok(bytes) => println!("memory:   {} MiB", bytes / (1024 * 1024)),
            Err(_) => println!("memory:   {mem}"),
        }
    }

    match Config::load_existing(&dir)? {
        Some(cfg) => match sys::http_get(cfg.port, "/health") {
            Ok((200, body)) => println!("health:   ok on port {} {body}", cfg.port),
            Ok((code, _)) => println!("health:   HTTP {code} on port {}", cfg.port),
            Err(_) => println!("health:   unreachable on port {}", cfg.port),
        },
        None => println!("health:   no config (run grammalocal setup)"),
    }

    let bin = sys::bin_dir()?;
    let missing: Vec<&str> = BINARIES
        .iter()
        .copied()
        .filter(|b| !bin.join(b).is_file())
        .collect();
    if missing.is_empty() {
        println!("binaries: all present in {}", bin.display());
    } else {
        println!("binaries: missing {}", missing.join(", "));
    }

    if sys::find_on_path("gsettings").is_some() {
        println!(
            "shortcut: {}",
            if shortcut_registered() {
                "registered (Super+C)"
            } else {
                "not registered"
            }
        );
    }
    Ok(())
}

pub fn uninstall(purge: bool, yes: bool) -> Result<()> {
    let bin = sys::bin_dir()?;
    let unit = sys::unit_path()?;
    let icon = sys::icon_path()?;
    let desktop = sys::desktop_entry_path()?;
    let data = sys::data_dir()?;
    let state = sys::state_dir()?;
    let dir = config::config_dir()?;

    println!("This removes:");
    println!("  service unit  {}", unit.display());
    println!("  app icon      {}", icon.display());
    println!("  desktop entry {}", desktop.display());
    println!("  GNOME shortcut (if registered)");
    println!("  data          {}", data.display());
    println!("  state         {}", state.display());
    println!(
        "  binaries      {}/{{{}}}",
        bin.display(),
        BINARIES.join(",")
    );
    if purge {
        println!("  config        {}", dir.display());
    } else {
        println!("  config        kept (pass --purge to remove it)");
    }
    if !ask(yes, "Proceed?", false) {
        println!("Aborted.");
        return Ok(());
    }

    sys::systemctl_quiet(&["disable", "--now", SERVICE]);
    if sys::remove_file_if_present(&unit)? {
        println!("removed {}", unit.display());
    }
    sys::systemctl_quiet(&["daemon-reload"]);

    remove_shortcut()?;

    if sys::remove_file_if_present(&icon)? {
        println!("removed {}", icon.display());
    }
    if sys::remove_file_if_present(&desktop)? {
        println!("removed {}", desktop.display());
    }
    if sys::remove_dir_if_present(&data)? {
        println!("removed {}", data.display());
    }
    if sys::remove_dir_if_present(&state)? {
        println!("removed {}", state.display());
    }
    for name in BINARIES {
        if sys::remove_file_if_present(&bin.join(name))? {
            println!("removed {}", bin.join(name).display());
        }
    }
    if purge && sys::remove_dir_if_present(&dir)? {
        println!("removed {}", dir.display());
    }
    println!("Uninstalled.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str =
        "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/grammalocal/";
    const OTHER: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/custom0/";

    #[test]
    fn parse_handles_empty_typed_array() {
        assert!(parse_list("@as []").is_empty());
        assert!(parse_list("[]").is_empty());
    }

    #[test]
    fn append_to_empty_list() {
        assert_eq!(
            append_keybinding("@as []", DIR).as_deref(),
            Some(format!("['{DIR}']").as_str())
        );
    }

    #[test]
    fn append_keeps_existing_entries() {
        let current = format!("['{OTHER}']");
        let expected = format!("['{OTHER}', '{DIR}']");
        assert_eq!(
            append_keybinding(&current, DIR).as_deref(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn append_is_idempotent() {
        let current = format!("['{OTHER}', '{DIR}']");
        assert_eq!(append_keybinding(&current, DIR), None);
    }

    #[test]
    fn remove_keeps_other_entries() {
        let current = format!("['{OTHER}', '{DIR}']");
        assert_eq!(remove_keybinding(&current, DIR), format!("['{OTHER}']"));
    }

    #[test]
    fn remove_last_entry_gives_empty_list() {
        let current = format!("['{DIR}']");
        assert_eq!(remove_keybinding(&current, DIR), "[]");
    }
}
