//! Replace mode: sends Ctrl+V through the XDG RemoteDesktop portal.
//!
//! The portal's restore token lets GNOME skip the consent dialog after the first
//! grant. It is kept in `$XDG_STATE_HOME/grammalocal/portal_token` (mode 0600).

use std::fs;
use std::future::Future;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::process::Command;

use ashpd::desktop::remote_desktop::{
    DeviceType, KeyState, NotifyKeyboardKeysymOptions, RemoteDesktop, SelectDevicesOptions,
    StartOptions,
};
use ashpd::desktop::{CreateSessionOptions, PersistMode, Session};
use ashpd::enumflags2::BitFlags;

/// X11 keysyms, so the paste works whatever the keyboard layout (AZERTY included).
const KEYSYM_CONTROL_L: i32 = 0xffe3;
const KEYSYM_V: i32 = 0x0076;

/// Where the restore token is stored.
pub fn token_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".local/state"),
    };
    Some(base.join("grammalocal").join("portal_token"))
}

fn load_token() -> Option<String> {
    let text = fs::read_to_string(token_path()?).ok()?;
    let token = text.trim();
    (!token.is_empty()).then(|| token.to_string())
}

fn store_token(token: &str) -> Result<(), String> {
    let path = token_path().ok_or("HOME is not set")?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    file.write_all(token.as_bytes())
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    // `mode` only applies on creation; enforce 0600 on pre-existing files too.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("chmod {}: {e}", path.display()))
}

fn block_on<T>(fut: impl Future<Output = Result<T, String>>) -> Result<T, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    rt.block_on(fut)
}

fn portal_err(what: &str, e: ashpd::Error) -> String {
    format!("{what}: {e}")
}

/// Opens a keyboard session and waits for the consent answer. Returns the session and
/// the restore token the portal issued, if any.
async fn open_keyboard(
    rd: &RemoteDesktop,
    restore: Option<&str>,
) -> Result<(Session<RemoteDesktop>, Option<String>), String> {
    let session = rd
        .create_session(CreateSessionOptions::default())
        .await
        .map_err(|e| portal_err("create session", e))?;
    match select_and_start(rd, &session, restore).await {
        Ok(token) => Ok((session, token)),
        Err(e) => {
            let _ = session.close().await;
            Err(e)
        }
    }
}

async fn select_and_start(
    rd: &RemoteDesktop,
    session: &Session<RemoteDesktop>,
    restore: Option<&str>,
) -> Result<Option<String>, String> {
    let options = SelectDevicesOptions::default()
        .set_devices(BitFlags::from(DeviceType::Keyboard))
        .set_persist_mode(PersistMode::ExplicitlyRevoked)
        .set_restore_token(restore);
    rd.select_devices(session, options)
        .await
        .map_err(|e| portal_err("select devices", e))?
        .response()
        .map_err(|e| portal_err("select devices", e))?;
    let devices = rd
        .start(session, None, StartOptions::default())
        .await
        .map_err(|e| portal_err("start", e))?
        .response()
        .map_err(|e| portal_err("start", e))?;
    Ok(devices.restore_token().map(str::to_string))
}

async fn send_keys(rd: &RemoteDesktop, session: &Session<RemoteDesktop>) -> Result<(), String> {
    let steps = [
        (KEYSYM_CONTROL_L, KeyState::Pressed),
        (KEYSYM_V, KeyState::Pressed),
        (KEYSYM_V, KeyState::Released),
        (KEYSYM_CONTROL_L, KeyState::Released),
    ];
    for (keysym, state) in steps {
        rd.notify_keyboard_keysym(
            session,
            keysym,
            state,
            NotifyKeyboardKeysymOptions::default(),
        )
        .await
        .map_err(|e| portal_err("send key", e))?;
    }
    Ok(())
}

/// Sends Ctrl+V to the focused window through the portal. Blocking; run it off the GTK thread.
pub fn paste_ctrl_v() -> Result<(), String> {
    block_on(async {
        let rd = RemoteDesktop::new()
            .await
            .map_err(|e| portal_err("portal unavailable", e))?;
        let restore = load_token();
        let (session, token) = open_keyboard(&rd, restore.as_deref()).await?;
        match &token {
            Some(t) => {
                if let Err(e) = store_token(t) {
                    eprintln!("grammalocal-popup: could not save restore token: {e}");
                }
            }
            None if restore.is_some() => eprintln!(
                "grammalocal-popup: the portal did not return a restore token; \
                 the consent dialog will show again"
            ),
            None => {}
        }
        let result = send_keys(&rd, &session).await;
        let _ = session.close().await;
        result
    })
}

/// Runs the consent flow without pasting, so GNOME shows its dialog once and the
/// restore token is saved for later replace-mode pastes. Blocking.
pub fn authorize() -> Result<(), String> {
    block_on(async {
        let rd = RemoteDesktop::new()
            .await
            .map_err(|e| portal_err("portal unavailable", e))?;
        let restore = load_token();
        let (session, token) = open_keyboard(&rd, restore.as_deref()).await?;
        let _ = session.close().await;
        let token = token.ok_or(
            "the portal returned no restore token, so the consent dialog would reappear on every paste",
        )?;
        store_token(&token)
    })
}

/// Fallback paste through ydotool. Returns false when ydotool is missing or fails.
pub fn ydotool_paste() -> bool {
    // 29 is KEY_LEFTCTRL and 47 is KEY_V in the Linux input event codes.
    Command::new("ydotool")
        .args(["key", "29:1", "47:1", "47:0", "29:0"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
