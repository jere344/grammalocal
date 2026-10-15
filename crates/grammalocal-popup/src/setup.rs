//! `--setup-replace` and `--setup-copy`: one-time consent and switching `apply_mode`.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use grammalocald::config::{self, Config};

/// Handles the setup flags before GTK starts. Returns None for a normal launch,
/// otherwise whether the setup succeeded.
pub fn run_from_args() -> Option<bool> {
    let flag = std::env::args().nth(1)?;
    match flag.as_str() {
        "--setup-replace" => Some(setup_replace()),
        "--setup-copy" => Some(setup_copy()),
        _ => None,
    }
}

fn setup_replace() -> bool {
    if let Err(e) = crate::portal::authorize() {
        eprintln!("grammalocal-popup: replace setup failed: {e}");
        eprintln!("grammalocal-popup: apply_mode was left unchanged");
        return false;
    }
    let token_file = crate::portal::token_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    match set_apply_mode("replace") {
        Ok(()) => {
            println!("Replace mode enabled. Keyboard grant saved to {token_file}.");
            println!("Set apply_mode = \"replace\" in config.toml.");
            true
        }
        Err(e) => {
            eprintln!("grammalocal-popup: grant saved, but config update failed: {e}");
            false
        }
    }
}

fn setup_copy() -> bool {
    match set_apply_mode("copy") {
        Ok(()) => {
            println!("Copy mode enabled. Set apply_mode = \"copy\" in config.toml.");
            true
        }
        Err(e) => {
            eprintln!("grammalocal-popup: config update failed: {e}");
            false
        }
    }
}

fn set_apply_mode(mode: &str) -> Result<(), String> {
    let dir = config::config_dir().map_err(|e| e.to_string())?;
    // Creates config.toml with defaults when it is missing.
    Config::load_or_create(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("config.toml");
    let text = fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    write_private(&path, &rewrite_apply_mode(&text, mode))
}

/// Replaces the `apply_mode` line in a config file, or appends one. Other lines and
/// comments are kept as they are.
pub fn rewrite_apply_mode(text: &str, mode: &str) -> String {
    let line = format!("apply_mode = \"{mode}\"");
    let mut found = false;
    let mut out: Vec<String> = text
        .lines()
        .map(|l| {
            if is_apply_mode_line(l) {
                found = true;
                line.clone()
            } else {
                l.to_string()
            }
        })
        .collect();
    if !found {
        out.push(line);
    }
    let mut result = out.join("\n");
    result.push('\n');
    result
}

fn is_apply_mode_line(line: &str) -> bool {
    line.trim_start()
        .strip_prefix("apply_mode")
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

fn write_private(path: &Path, contents: &str) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("chmod {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::rewrite_apply_mode;

    #[test]
    fn replaces_existing_line_and_keeps_others() {
        let text = "# header\nport = 8091\napply_mode = \"copy\"\ntoken = \"abc\"\n";
        let out = rewrite_apply_mode(text, "replace");
        assert_eq!(
            out,
            "# header\nport = 8091\napply_mode = \"replace\"\ntoken = \"abc\"\n"
        );
    }

    #[test]
    fn appends_when_missing() {
        let out = rewrite_apply_mode("port = 8091\n", "replace");
        assert_eq!(out, "port = 8091\napply_mode = \"replace\"\n");
    }

    #[test]
    fn ignores_commented_out_key() {
        let text = "# apply_mode = \"copy\"\n";
        let out = rewrite_apply_mode(text, "replace");
        assert_eq!(out, "# apply_mode = \"copy\"\napply_mode = \"replace\"\n");
    }

    #[test]
    fn is_idempotent() {
        let once = rewrite_apply_mode("apply_mode = \"copy\"\n", "copy");
        assert_eq!(rewrite_apply_mode(&once, "copy"), once);
    }
}
