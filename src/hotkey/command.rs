//! `mywhisper --dictate press|release|toggle|cancel`: compositor
//! keybinds (niri, Sway, Hyprland, ...) drive dictation without any
//! key listener. The CLI sends one datagram to the service's
//! user-only socket; the service feeds it into the same activation
//! state as the key sources, so Push/Smart need the compositor's
//! release binds (Sway `--release`, Hyprland `bindr`) and `toggle`
//! works everywhere.

use super::{COMMAND_SINK, HotkeyAction, HotkeyEvent};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;

/// Binding index for command events: never collides with a key binding.
pub(super) const COMMAND_BINDING: usize = usize::MAX;

const ACTIONS: [&str; 4] = ["press", "release", "toggle", "cancel"];

/// In the per-user runtime dir (0700), so only this user can send.
fn socket_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty())?;
    Some(PathBuf::from(dir).join("mywhisper-dictate.sock"))
}

/// Client side of `--dictate ACTION`.
pub fn send(action: &str) -> Result<(), String> {
    if !ACTIONS.contains(&action) {
        return Err(format!(
            "unknown action {action:?} (use {})",
            ACTIONS.join(", ")
        ));
    }
    let path = socket_path().ok_or("XDG_RUNTIME_DIR is not set")?;
    let socket = UnixDatagram::unbound().map_err(|e| format!("socket: {e}"))?;
    socket
        .send_to(action.as_bytes(), &path)
        .map_err(|e| format!("MyWhisper service not reachable ({e}); is it running?"))?;
    Ok(())
}

/// Service side: bind the socket once and dispatch commands for the
/// life of the process. Only the engine owner calls this (it holds the
/// engine lock), so a leftover socket file is stale and safe to remove.
pub fn serve() {
    let Some(path) = socket_path() else {
        eprintln!("[hotkey] no XDG_RUNTIME_DIR: --dictate unavailable");
        return;
    };
    let _ = std::fs::remove_file(&path);
    let socket = match UnixDatagram::bind(&path) {
        Ok(socket) => socket,
        Err(e) => {
            eprintln!("[hotkey] --dictate socket {path:?}: {e}");
            return;
        }
    };
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    let spawned = std::thread::Builder::new()
        .name("hotkey-command".into())
        .spawn(move || {
            let mut buf = [0u8; 16];
            while let Ok(n) = socket.recv(&mut buf) {
                if let Ok(action) = std::str::from_utf8(&buf[..n]) {
                    dispatch(action);
                }
            }
        });
    if let Err(e) = spawned {
        eprintln!("[hotkey] --dictate listener: {e}");
    }
}

fn dispatch(action: &str) {
    // Same gate as the key sources: muted hotkeys ignore commands too.
    if !crate::transcribe::hotkeys_listening() {
        return;
    }
    let Some((sink, mode)) = COMMAND_SINK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    else {
        return;
    };
    let dictation = HotkeyAction::DictationToggle;
    let (event, binding) = match action {
        "press" => (HotkeyEvent::Down(dictation, mode), Some(COMMAND_BINDING)),
        "release" => (HotkeyEvent::Up(dictation, mode), Some(COMMAND_BINDING)),
        // Tap semantics: one command starts, the next one stops.
        "toggle" => (
            HotkeyEvent::Down(dictation, crate::settings::ActivationMode::Tap),
            Some(COMMAND_BINDING),
        ),
        "cancel" => (HotkeyEvent::Cancel, None),
        _ => return,
    };
    sink(event, binding);
}

#[cfg(test)]
mod tests {
    use super::{dispatch, send};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn unknown_actions_are_rejected_before_sending() {
        assert!(send("explode").unwrap_err().contains("unknown action"));
    }

    #[test]
    fn commands_drive_the_shared_activation() {
        // start_dictation retires running listeners: own the loop slot.
        let _owner = super::super::NATIVE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let downs = Arc::new(AtomicUsize::new(0));
        let ups = Arc::new(AtomicUsize::new(0));
        let hook = |n: &Arc<AtomicUsize>| -> super::super::DictationHook {
            let n = n.clone();
            Arc::new(move || {
                n.fetch_add(1, Ordering::SeqCst);
            })
        };
        super::super::start_dictation(
            vec![crate::settings::HotkeyBinding {
                combo: "F13".into(),
                mode: crate::settings::ActivationMode::Push,
            }],
            150,
            crate::settings::HotkeyBackend::Command,
            hook(&downs),
            hook(&ups),
            Arc::new(|| {}),
        );
        let counts = || (downs.load(Ordering::SeqCst), ups.load(Ordering::SeqCst));
        dispatch("toggle");
        assert_eq!(counts(), (1, 0));
        dispatch("toggle");
        assert_eq!(counts(), (1, 1));
        // Push: press opens, release closes (compositor release binds).
        dispatch("press");
        dispatch("release");
        assert_eq!(counts(), (2, 2));
        super::super::stop();
    }
}
