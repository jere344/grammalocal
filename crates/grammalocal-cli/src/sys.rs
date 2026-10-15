//! Helpers shared by the lifecycle subcommands: XDG paths, external commands,
//! a plain HTTP probe of the daemon, and yes/no prompts.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};

pub const SERVICE: &str = "grammalocald";
pub const UNIT_FILE: &str = "grammalocald.service";
/// Binaries installed into `~/.local/bin`. `grammalocal` comes last so that
/// uninstall removes itself only after the others.
pub const BINARIES: [&str; 3] = ["grammalocald", "grammalocal-popup", "grammalocal"];

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .context("HOME is not set")
}

fn xdg(var: &str, fallback: &str) -> Result<PathBuf> {
    match std::env::var_os(var) {
        Some(p) if !p.is_empty() => Ok(PathBuf::from(p)),
        _ => Ok(home()?.join(fallback)),
    }
}

/// `$XDG_DATA_HOME/grammalocal`: worker and vendored Grammalecte.
pub fn data_dir() -> Result<PathBuf> {
    Ok(xdg("XDG_DATA_HOME", ".local/share")?.join("grammalocal"))
}

/// `$XDG_STATE_HOME/grammalocal`.
pub fn state_dir() -> Result<PathBuf> {
    Ok(xdg("XDG_STATE_HOME", ".local/state")?.join("grammalocal"))
}

/// `~/.local/bin`, where the binaries are installed.
pub fn bin_dir() -> Result<PathBuf> {
    Ok(home()?.join(".local/bin"))
}

/// `$XDG_CONFIG_HOME/systemd/user/grammalocald.service`.
pub fn unit_path() -> Result<PathBuf> {
    Ok(xdg("XDG_CONFIG_HOME", ".config")?
        .join("systemd/user")
        .join(UNIT_FILE))
}

/// `$XDG_DATA_HOME/icons/hicolor/scalable/apps/grammalocal.svg`.
pub fn icon_path() -> Result<PathBuf> {
    Ok(xdg("XDG_DATA_HOME", ".local/share")?.join("icons/hicolor/scalable/apps/grammalocal.svg"))
}

/// `$XDG_DATA_HOME/applications/grammalocal.desktop`.
pub fn desktop_entry_path() -> Result<PathBuf> {
    Ok(xdg("XDG_DATA_HOME", ".local/share")?.join("applications/grammalocal.desktop"))
}

/// Finds an executable on `PATH`, returning an absolute path only.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .filter(|candidate| candidate.is_absolute())
}

/// Trimmed stdout of a command that exits successfully, `None` otherwise.
pub fn capture(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Trimmed stdout whatever the exit status, `None` only if the command cannot run.
pub fn stdout_of(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Runs `program args` with inherited output and fails when it does not succeed.
pub fn run_ok(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("running {program}"))?;
    if !status.success() {
        bail!("{program} {} failed ({status})", args.join(" "));
    }
    Ok(())
}

/// Runs `systemctl --user` with the given arguments, showing its output.
/// Returns whether it succeeded.
pub fn systemctl(args: &[&str]) -> Result<bool> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("running systemctl --user")?;
    Ok(status.success())
}

/// Runs `systemctl --user` silently and returns whether it succeeded.
pub fn systemctl_quiet(args: &[&str]) -> bool {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Performs `GET path` against `127.0.0.1:port` and returns the status code and body.
pub fn http_get(port: u16, path: &str) -> Result<(u16, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let raw = String::from_utf8_lossy(&raw);
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .context("malformed HTTP response")?;
    let code = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .context("malformed HTTP status line")?;
    Ok((code, body.to_string()))
}

/// Reads one line from stdin, trimmed. Empty at end of input.
pub fn read_line() -> String {
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    line.trim().to_string()
}

/// Asks a y/N question (or Y/n when `default` is true) on stdin.
/// End of input or an empty answer gives `default`.
pub fn confirm(question: &str, default: bool) -> bool {
    let hint = if default { "[Y/n]" } else { "[y/N]" };
    print!("{question} {hint} ");
    let _ = std::io::stdout().flush();
    match read_line().to_ascii_lowercase().as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default,
    }
}

/// Removes a file or symlink if present. Returns whether something was removed.
pub fn remove_file_if_present(path: &Path) -> Result<bool> {
    if fs::symlink_metadata(path).is_err() {
        return Ok(false);
    }
    fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
    Ok(true)
}

/// Removes a directory tree if present. Returns whether something was removed.
pub fn remove_dir_if_present(path: &Path) -> Result<bool> {
    if !path.is_dir() {
        return Ok(false);
    }
    fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))?;
    Ok(true)
}
