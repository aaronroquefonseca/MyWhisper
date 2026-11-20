//! Global shortcuts through the XDG desktop portal
//! (`org.freedesktop.portal.GlobalShortcuts`): KDE Plasma, GNOME 48+,
//! Hyprland. No `input` group: the desktop owns the keys and the user
//! approves (and may rebind) them in its own dialog. `Activated` /
//! `Deactivated` give press and release, so Tap, Push and Smart all work.
//! Escape-to-cancel stays evdev-only: binding Escape globally would
//! steal it from every app.

use super::{BindingCallback, HOTKEY_GEN, HotkeyEvent, HotkeyStatus, Watched};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use zbus::{ObjectPath, OwnedVariant as OwnedValue, Variant as Value};

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
/// Matches `assets/applications/com.mywhisper.App.desktop`; GNOME
/// only grants shortcuts to an app id with a desktop file.
const APP_ID: &str = "com.mywhisper.App";

type StatusCallback = Arc<dyn Fn(HotkeyStatus) + Send + Sync + 'static>;
type Options<'a> = HashMap<&'a str, Value<'a>>;

/// Hold the portal session until `generation` is superseded. `Err`
/// means no session could be set up (no portal, refused, cancelled):
/// the caller may fall back to evdev.
pub(super) fn run(
    generation: u64,
    watched: &[Watched],
    on_event: &BindingCallback,
    on_status: &StatusCallback,
) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(session(generation, watched, on_event, on_status))
}

/// Desktops whose portal backend implements GlobalShortcuts. Others
/// (niri, Sway, ...) may route it to a backend that never answers.
pub(super) fn desktop_supported() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|desktops| {
        desktops.split(':').any(|d| {
            let d = d.to_ascii_lowercase();
            d == "kde" || d == "gnome" || d == "hyprland"
        })
    })
}

/// Covers the user's approval dialog; a backend that never answers
/// must not leave hotkeys dead.
const SETUP_TIMEOUT: Duration = Duration::from_secs(30);

async fn session(
    generation: u64,
    watched: &[Watched],
    on_event: &BindingCallback,
    on_status: &StatusCallback,
) -> Result<(), String> {
    let (conn, portal, handle, mut events) =
        tokio::time::timeout(SETUP_TIMEOUT, setup(generation, watched))
            .await
            .map_err(|_| "no answer from the desktop".to_string())??;
    on_status(HotkeyStatus::Listening);
    listen(generation, watched, on_event, &mut events, &handle).await;
    drop(portal);
    if let Ok(session) =
        zbus::Proxy::new(&conn, DEST, &handle, "org.freedesktop.portal.Session").await
    {
        let _ = session.call::<_, _, ()>("Close", &()).await;
    }
    Ok(())
}

type Events = std::pin::Pin<Box<dyn futures_util::Stream<Item = (bool, zbus::Message)> + Send>>;

async fn setup(
    generation: u64,
    watched: &[Watched],
) -> Result<
    (
        zbus::Connection,
        zbus::Proxy<'static>,
        ObjectPath<'static>,
        Events,
    ),
    String,
> {
    let conn = zbus::Connection::session()
        .await
        .map_err(|e| format!("session bus: {e}"))?;
    // Host (unsandboxed) apps have no portal app id until they register
    // one (xdg-desktop-portal 1.19+). Must precede other portal calls.
    if let Ok(registry) =
        zbus::Proxy::new(&conn, DEST, PATH, "org.freedesktop.host.portal.Registry").await
    {
        let _ = registry
            .call::<_, _, ()>("Register", &(APP_ID, Options::new()))
            .await;
    }
    let portal = zbus::Proxy::new(&conn, DEST, PATH, "org.freedesktop.portal.GlobalShortcuts")
        .await
        .map_err(|e| format!("portal proxy: {e}"))?;
    // Subscribe before binding so no early press is lost.
    let activated = portal
        .receive_signal("Activated")
        .await
        .map_err(|e| format!("subscribe: {e}"))?
        .map(|m| (true, m));
    let deactivated = portal
        .receive_signal("Deactivated")
        .await
        .map_err(|e| format!("subscribe: {e}"))?
        .map(|m| (false, m));
    let events: Events = Box::pin(futures_util::stream::select(activated, deactivated));

    let token = |step: &str| format!("mywhisper_{generation}_{step}");
    let created = request(
        &conn,
        &portal,
        "CreateSession",
        &(Options::from([
            ("handle_token", Value::from(token("create"))),
            ("session_handle_token", Value::from(token("session"))),
        ]),),
        &token("create"),
    )
    .await?;
    let handle = created
        .get("session_handle")
        .and_then(|v| String::try_from(v.clone()).ok())
        .ok_or("portal returned no session handle")?;
    let handle = ObjectPath::try_from(handle).map_err(|e| format!("session handle: {e}"))?;

    let shortcuts: Vec<(String, Options)> = watched
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let mut options = Options::from([("description", Value::from("MyWhisper dictation"))]);
            if let Some(trigger) = xdg_trigger(&w.combo) {
                options.insert("preferred_trigger", Value::from(trigger));
            }
            (format!("dictation-{i}"), options)
        })
        .collect();
    let bound = request(
        &conn,
        &portal,
        "BindShortcuts",
        &(
            &handle,
            shortcuts,
            "",
            Options::from([("handle_token", Value::from(token("bind")))]),
        ),
        &token("bind"),
    )
    .await?;
    eprintln!(
        "[hotkey] portal shortcuts bound: {:?}",
        bound.get("shortcuts")
    );
    Ok((conn, portal, handle, events))
}

async fn listen(
    generation: u64,
    watched: &[Watched],
    on_event: &BindingCallback,
    events: &mut Events,
    handle: &ObjectPath<'_>,
) {
    while HOTKEY_GEN.load(Ordering::SeqCst) == generation {
        // Wake regularly so a settings change retires this session.
        let Ok(next) = tokio::time::timeout(Duration::from_millis(500), events.next()).await else {
            continue;
        };
        let Some((pressed, msg)) = next else {
            eprintln!("[hotkey] portal signal stream closed");
            break;
        };
        let body = msg.body();
        let Ok((session, id, _, _)) =
            body.deserialize::<(ObjectPath, String, u64, HashMap<String, OwnedValue>)>()
        else {
            continue;
        };
        if &session != handle {
            continue;
        }
        let Some((index, w)) = id
            .strip_prefix("dictation-")
            .and_then(|i| i.parse::<usize>().ok())
            .and_then(|i| watched.get(i).map(|w| (i, w)))
        else {
            continue;
        };
        let event = if pressed {
            HotkeyEvent::Down(w.action, w.mode)
        } else {
            HotkeyEvent::Up(w.action, w.mode)
        };
        on_event(event, Some(index));
    }
}

/// One portal request: subscribe to its `Response`, call, await it.
async fn request<B>(
    conn: &zbus::Connection,
    portal: &zbus::Proxy<'_>,
    method: &str,
    body: &B,
    token: &str,
) -> Result<HashMap<String, OwnedValue>, String>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let sender = conn
        .unique_name()
        .ok_or("no bus name")?
        .trim_start_matches(':')
        .replace('.', "_");
    let path = format!("{PATH}/request/{sender}/{token}");
    let req = zbus::Proxy::new(conn, DEST, path, "org.freedesktop.portal.Request")
        .await
        .map_err(|e| format!("request proxy: {e}"))?;
    let mut responses = req
        .receive_signal("Response")
        .await
        .map_err(|e| format!("subscribe: {e}"))?;
    portal
        .call_method(method, body)
        .await
        .map_err(|e| format!("{method}: {e}"))?;
    let msg = responses
        .next()
        .await
        .ok_or_else(|| format!("{method}: no response"))?;
    let (code, results) = msg
        .body()
        .deserialize::<(u32, HashMap<String, OwnedValue>)>()
        .map_err(|e| format!("{method} response: {e}"))?;
    match code {
        0 => Ok(results),
        1 => Err(format!("{method}: cancelled by the user")),
        _ => Err(format!("{method}: refused")),
    }
}

/// A parsed combo as an XDG shortcut trigger ("CTRL+ALT+d", "F13").
/// Only a hint: the desktop may pick another key.
fn xdg_trigger(combo: &super::Combo) -> Option<String> {
    let mut parts: Vec<String> = [
        (combo.ctrl, "CTRL"),
        (combo.alt, "ALT"),
        (combo.shift, "SHIFT"),
        (combo.super_, "LOGO"),
    ]
    .iter()
    .filter(|(on, _)| *on)
    .map(|(_, name)| name.to_string())
    .collect();
    if let Some(key) = combo.target {
        let token = super::token_for_evdev_code(key.code())?;
        parts.push(match token {
            "Space" => "space".into(),
            "Enter" => "Return".into(),
            t if t.len() == 1 => t.to_lowercase(),
            t => t.to_string(),
        });
    }
    (!parts.is_empty()).then(|| parts.join("+"))
}

#[cfg(test)]
mod tests {
    use super::xdg_trigger;

    fn trigger(raw: &str) -> Option<String> {
        xdg_trigger(&crate::hotkey::parse_combo(raw).unwrap())
    }

    #[test]
    fn combos_become_xdg_triggers() {
        assert_eq!(trigger("F13").as_deref(), Some("F13"));
        assert_eq!(trigger("Ctrl+Alt+D").as_deref(), Some("CTRL+ALT+d"));
        assert_eq!(trigger("Super+Space").as_deref(), Some("LOGO+space"));
        assert_eq!(trigger("Control+Super").as_deref(), Some("CTRL+LOGO"));
    }
}
