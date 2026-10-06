//! Media pause/resume over MPRIS D-Bus.
//!
//! On dictation start, every player reporting `Playing` is paused
//! and remembered; on stop, exactly those players resume. Players
//! already paused are never touched, and anything that starts playing
//! mid-dictation is left alone.

use std::sync::atomic::{AtomicBool, Ordering};

/// Master switch for pause-while-dictating, refreshed from the
/// `pause_media_while_dictating` preference by the service watcher.
static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::SeqCst);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

fn run<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>>>>,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(f())
}

async fn players(conn: &zbus::Connection) -> Result<Vec<String>, String> {
    let proxy = zbus::fdo::DBusProxy::new(conn)
        .await
        .map_err(|e| format!("bus proxy: {e}"))?;
    let names = proxy
        .list_names()
        .await
        .map_err(|e| format!("list names: {e}"))?;
    Ok(names
        .into_iter()
        .filter(|n| {
            let s = n.as_str();
            s.starts_with("org.mpris.MediaPlayer2.") && s != "org.mpris.MediaPlayer2.Player"
        })
        .map(|n| n.to_string())
        .collect())
}

async fn status(conn: &zbus::Connection, player: &str) -> Result<String, String> {
    let proxy = zbus::Proxy::new(
        conn,
        player,
        "/org/mpris/MediaPlayer2",
        "org.mpris.MediaPlayer2.Player",
    )
    .await
    .map_err(|e| format!("player proxy: {e}"))?;
    proxy
        .get_property::<String>("PlaybackStatus")
        .await
        .map_err(|e| format!("status: {e}"))
}

async fn call(conn: &zbus::Connection, player: &str, method: &str) -> Result<(), String> {
    let proxy = zbus::Proxy::new(
        conn,
        player,
        "/org/mpris/MediaPlayer2",
        "org.mpris.MediaPlayer2.Player",
    )
    .await
    .map_err(|e| format!("player proxy: {e}"))?;
    proxy
        .call::<_, _, ()>(method, &())
        .await
        .map_err(|e| format!("{method}: {e}"))?;
    Ok(())
}

/// Pause every currently-playing MPRIS player. Returns the bus names
/// actually paused (feed these to `resume`). Never fails dictation:
/// D-Bus errors only log.
pub fn pause_playing() -> Vec<String> {
    if !enabled() {
        return Vec::new();
    }
    match run(|| {
        Box::pin(async {
            let conn = zbus::Connection::session()
                .await
                .map_err(|e| format!("session bus: {e}"))?;
            let mut paused = Vec::new();
            for player in players(&conn).await? {
                match status(&conn, &player).await.as_deref() {
                    Ok("Playing") => match call(&conn, &player, "Pause").await {
                        Ok(()) => {
                            eprintln!("[media] paused {player}");
                            paused.push(player);
                        }
                        Err(e) => eprintln!("[media] pause {player} failed: {e}"),
                    },
                    Ok(_) => {}
                    Err(e) => eprintln!("[media] status {player} failed: {e}"),
                }
            }
            Ok(paused)
        })
    }) {
        Ok(paused) => paused,
        Err(e) => {
            eprintln!("[media] {e}");
            Vec::new()
        }
    }
}

/// Resume exactly the players `pause_playing` paused.
pub fn resume(players: &[String]) {
    if players.is_empty() {
        return;
    }
    let players = players.to_vec();
    if run(|| {
        Box::pin(async move {
            let conn = zbus::Connection::session()
                .await
                .map_err(|e| format!("session bus: {e}"))?;
            for player in &players {
                match call(&conn, player, "Play").await {
                    Ok(()) => eprintln!("[media] resumed {player}"),
                    Err(e) => eprintln!("[media] resume {player} failed: {e}"),
                }
            }
            Ok(())
        })
    })
    .is_err()
    {
        eprintln!("[media] resume pass failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plumbing check against the real session bus: with no MPRIS
    /// players around this returns empty (never errors); resume of
    /// nothing is a silent no-op. Requires a session bus.
    #[test]
    fn no_players_is_empty_success() {
        set_enabled(true);
        let paused = pause_playing();
        assert!(paused.iter().all(|n| n.starts_with("org.mpris.MediaPlayer2.")));
        resume(&paused);
        resume(&[]);
        set_enabled(false);
        assert!(pause_playing().is_empty());
    }
}
