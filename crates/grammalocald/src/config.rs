//! User configuration stored as TOML at `$XDG_CONFIG_HOME/grammalocal/config.toml`
//! (falls back to `~/.config/grammalocal/config.toml`).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 8091;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub port: u16,
    pub default_language: String,
    pub apply_mode: String,
    pub token: String,
    pub personal_dictionary: Vec<String>,
    pub disabled_rules: Vec<String>,
    pub allowed_origins: Vec<String>,
    /// Accept requests from browser extension origins (moz-extension://,
    /// chrome-extension://, safari-web-extension://) without a token.
    pub allow_browser_extensions: bool,
    /// Log one line per HTTP request (field names and lengths only, never text).
    pub log_requests: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            port: DEFAULT_PORT,
            default_language: "fr".into(),
            apply_mode: "copy".into(),
            token: String::new(),
            personal_dictionary: Vec::new(),
            disabled_rules: Vec::new(),
            allowed_origins: Vec::new(),
            allow_browser_extensions: true,
            log_requests: false,
        }
    }
}

/// Directory holding `config.toml`, honouring `XDG_CONFIG_HOME`.
pub fn config_dir() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".config"),
    };
    Ok(base.join("grammalocal"))
}

/// Path of `config.toml` under [`config_dir`].
pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// 16 random bytes rendered as 32 lowercase hex characters.
pub fn generate_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    f.write_all(contents.as_bytes())?;
    // `mode` only applies on creation; enforce 0600 on pre-existing files too.
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

impl Config {
    /// Read `dir/config.toml` without creating or modifying anything.
    /// Returns `None` when the file does not exist.
    pub fn load_existing(dir: &Path) -> Result<Option<Config>> {
        let path = dir.join("config.toml");
        if !path.exists() {
            return Ok(None);
        }
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let cfg = toml::from_str::<Config>(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(Some(cfg))
    }

    /// Load `dir/config.toml`, creating it with defaults and a fresh token when
    /// absent. A missing or empty token is generated and persisted.
    pub fn load_or_create(dir: &Path) -> Result<Config> {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join("config.toml");
        let mut cfg = if path.exists() {
            let text =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            toml::from_str::<Config>(&text)
                .with_context(|| format!("parsing {}", path.display()))?
        } else {
            Config::default()
        };
        if cfg.token.trim().is_empty() {
            cfg.token = generate_token();
            cfg.save(dir)?;
        } else if !path.exists() {
            cfg.save(dir)?;
        }
        Ok(cfg)
    }

    /// Write the configuration with a header comment and 0600 permissions.
    pub fn save(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        let body = toml::to_string_pretty(self).context("serialising config")?;
        let text = format!(
            "# grammalocal daemon configuration.\n# Changes are read at daemon start.\n\n{body}"
        );
        write_private(&dir.join("config.toml"), &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "grammalocal-cfg-{name}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn token_is_32_hex() {
        let t = generate_token();
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn creates_defaults_with_private_token_file() {
        let dir = tmp("create");
        let cfg = Config::load_or_create(&dir).unwrap();
        assert_eq!(cfg.port, 8091);
        assert_eq!(cfg.default_language, "fr");
        assert_eq!(cfg.apply_mode, "copy");
        assert_eq!(cfg.token.len(), 32);
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(dir.join("config.toml"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let again = Config::load_or_create(&dir).unwrap();
        assert_eq!(again.token, cfg.token);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reads_user_lists() {
        let dir = tmp("lists");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("config.toml"),
            "port = 9000\ntoken = \"abc\"\npersonal_dictionary = [\"Grammalocal\"]\ndisabled_rules = [\"TYPO\"]\n",
        )
        .unwrap();
        let cfg = Config::load_or_create(&dir).unwrap();
        assert_eq!(cfg.port, 9000);
        assert_eq!(cfg.token, "abc");
        assert_eq!(cfg.personal_dictionary, vec!["Grammalocal"]);
        assert_eq!(cfg.disabled_rules, vec!["TYPO"]);
        fs::remove_dir_all(&dir).ok();
    }
}
