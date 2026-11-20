//! Hotkeys on X11 sessions through XInput2 raw key events: every key
//! press/release on the server, no grab, no `input` group. The events
//! become evdev key events (X keycode − 8) so the shared loop matches
//! combos, held keys and Escape-cancel exactly as for `/dev/input`.
//! Under XWayland raw events only arrive while an X client has focus,
//! so this source is for real X11 sessions.

use super::KeyDevice;
use std::path::PathBuf;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xinput::{self, ConnectionExt as _, EventMask, XIEventMask};
use x11rb::rust_connection::RustConnection;

pub(super) struct XInputKeys {
    conn: RustConnection,
}

/// True on an X11 login session (Automatic prefers XInput2 there).
pub(super) fn session_is_x11() -> bool {
    std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t.eq_ignore_ascii_case("x11"))
}

/// Connect to `$DISPLAY` and subscribe to raw key events. The device
/// path is the display's socket, so the shared loop drops the source
/// if the X server goes away.
pub(super) fn open() -> Result<(PathBuf, XInputKeys), String> {
    open_display(None)
}

fn open_display(display: Option<&str>) -> Result<(PathBuf, XInputKeys), String> {
    let (conn, screen) = x11rb::connect(display).map_err(|e| format!("X11 connect: {e}"))?;
    let version = conn
        .xinput_xi_query_version(2, 0)
        .map_err(|e| format!("XInput2: {e}"))?
        .reply()
        .map_err(|e| format!("XInput2: {e}"))?;
    if version.major_version < 2 {
        return Err("XInput2 not supported by the X server".into());
    }
    let root = conn.setup().roots[screen].root;
    conn.xinput_xi_select_events(
        root,
        &[EventMask {
            deviceid: xinput::Device::ALL_MASTER.into(),
            mask: vec![XIEventMask::RAW_KEY_PRESS | XIEventMask::RAW_KEY_RELEASE],
        }],
    )
    .map_err(|e| format!("XInput2 select: {e}"))?
    .check()
    .map_err(|e| format!("XInput2 select: {e}"))?;
    Ok((socket_path(), XInputKeys { conn }))
}

/// `:1` / `:1.0` → `/tmp/.X11-unix/X1`. Remote displays have no local
/// socket; `/` keeps them "present" for the shared loop.
fn socket_path() -> PathBuf {
    std::env::var("DISPLAY")
        .ok()
        .and_then(|d| {
            let number = d.strip_prefix(':')?.split('.').next()?.to_string();
            number.parse::<u32>().ok()?;
            Some(PathBuf::from(format!("/tmp/.X11-unix/X{number}")))
        })
        .unwrap_or_else(|| PathBuf::from("/"))
}

impl KeyDevice for XInputKeys {
    fn events(&mut self) -> std::io::Result<Vec<evdev::InputEvent>> {
        let mut out = Vec::new();
        while let Some(event) = self.conn.poll_for_event().map_err(std::io::Error::other)? {
            let (keycode, value) = match event {
                Event::XinputRawKeyPress(e) => (e.detail, 1),
                Event::XinputRawKeyRelease(e) => (e.detail, 0),
                _ => continue,
            };
            if let Some(code) = keycode.checked_sub(8).and_then(|c| u16::try_from(c).ok()) {
                out.push(evdev::InputEvent::new(evdev::EventType::KEY.0, code, value));
            }
        }
        if out.is_empty() {
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        Ok(out)
    }

    fn name(&self) -> String {
        "X11 (XInput2)".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real X server round trip: XTEST key presses arrive as raw
    /// XInput2 events and come out as evdev key events (keycode − 8).
    #[test]
    fn raw_key_events_become_evdev_keys() {
        let have = |bin: &str| {
            std::process::Command::new("which")
                .arg(bin)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        if !have("Xvfb") || !have("xdotool") {
            eprintln!("SKIP: Xvfb or xdotool missing");
            return;
        }
        let number = 90 + std::process::id() % 900;
        // Never type into a real X server that already owns this number.
        if std::path::Path::new(&format!("/tmp/.X11-unix/X{number}")).exists() {
            eprintln!("SKIP: display :{number} in use");
            return;
        }
        let display = format!(":{number}");
        let mut server = std::process::Command::new("Xvfb")
            .args([display.as_str(), "-nolisten", "tcp"])
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let start = std::time::Instant::now();
        let mut source = loop {
            match open_display(Some(&display)) {
                Ok((_, keys)) => break keys,
                Err(_) if start.elapsed() < std::time::Duration::from_secs(5) => {
                    std::thread::sleep(std::time::Duration::from_millis(50))
                }
                Err(e) => panic!("{e}"),
            }
        };
        let typed = std::process::Command::new("xdotool")
            .args(["key", "a"])
            .env("DISPLAY", &display)
            .status()
            .unwrap();
        assert!(typed.success());
        let mut keys = Vec::new();
        while keys.len() < 2 && start.elapsed() < std::time::Duration::from_secs(5) {
            if let Ok(events) = source.events() {
                keys.extend(events.iter().map(|e| (e.code(), e.value())));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = server.kill();
        let _ = server.wait();
        let a = evdev::KeyCode::KEY_A.code();
        assert_eq!(keys, vec![(a, 1), (a, 0)]);
    }
}
