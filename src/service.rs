//! Background engine ownership: sync listener, hotkey loop, pill.
//!
//! Exactly one process owns the engine at a time, decided by an
//! `flock`'d lockfile (`engine.lock`). The `--service` process (or a
//! standalone GUI when no service runs) holds the lock and runs
//! everything; a GUI started while the lock is held runs frontend
//! only -- it reads the shared `state.db` (WAL, concurrent reads are
//! fine) and never binds ports or grabs hotkeys.
//!
//! Cross-process change propagation is poll-based, no IPC (yet):
//! every engine/GUI polls `PRAGMA data_version` and reloads when it
//! moves. Settings writes from the GUI are picked up by the
//! service's settings watcher the same way.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::device::DeviceIdentity;
use crate::hotkey::DictationHook;
use crate::net::Discovered;
use crate::paths::AppPaths;
use crate::store::Store;

/// Try to become the engine owner. `Some(file)` = lock held (keep it
/// alive for the whole process); `None` = another process owns it.
pub fn acquire_engine_lock(paths: &AppPaths) -> Option<std::fs::File> {
    try_acquire(paths, true)
}

fn try_acquire(paths: &AppPaths, loud: bool) -> Option<std::fs::File> {
    use fs2::FileExt;
    let path = paths.engine_lock_file();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&path)
        .ok()?;
    match file.try_lock_exclusive() {
        Ok(()) => {
            if loud {
                eprintln!("[service] engine lock acquired");
            }
            Some(file)
        }
        Err(_) => {
            if loud {
                eprintln!("[service] engine owned elsewhere, frontend mode");
            }
            None
        }
    }
}

/// One-shot check: is another process holding the engine lock right
/// now? Used by the GUI (pure frontend) to report service status.
/// The probe lock is released immediately.
pub fn engine_running(paths: &AppPaths) -> bool {
    try_acquire(paths, false).is_none()
}

/// Same probe for a raw lock path (the GUI derives it from the db dir).
pub fn engine_running_paths(lock_path: &std::path::Path) -> bool {
    use fs2::FileExt;
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(lock_path)
    else {
        return false;
    };
    file.try_lock_exclusive().is_err()
}

/// Start/stop the user service (installs the unit first if needed).
pub fn set_service_running(running: bool) -> Result<String, String> {
    if running {
        ensure_unit_installed()?;
        systemctl(&["start", UNIT_NAME])?;
        Ok("background service started".to_string())
    } else {
        systemctl(&["stop", UNIT_NAME])?;
        Ok("background service stopped".to_string())
    }
}

/// Start the sync stack: TCP listener, mDNS advertise+browse,
/// periodic auto-sync, startup catch-up, appear-watcher. Returns the
/// bound port (0 = listener off). Safe to call only as engine owner.
pub fn start_sync_engine(
    paths: &AppPaths,
    store: &Store,
    identity: &DeviceIdentity,
    discovered: Arc<Mutex<HashMap<String, Discovered>>>,
) -> u16 {
    let saved = store.load_settings();
    let device_name = crate::device::display_name(&saved.sync.device_name, &identity.node_id);
    match crate::net::listen(saved.sync.listen_port) {
        Ok(Some(listener)) => {
            let port = listener
                .local_addr()
                .map(|a| a.port())
                .unwrap_or(saved.sync.listen_port);
            eprintln!("[sync] listening on port {port}");
            let db_path = paths.db_file();
            let creds = identity.creds();
            let mut hello_addrs = Vec::new();
            if let Some(ip) = crate::net::lan_ip() {
                hello_addrs.push(format!("{ip}:{port}"));
            }
            std::thread::spawn(move || {
                crate::net::serve(listener, db_path, creds, device_name, hello_addrs)
            });
            match crate::net::start_mdns(
                &identity.node_id,
                &saved.sync.device_name.clone(),
                port,
                discovered.clone(),
            ) {
                Ok(_handles) => {
                    std::mem::forget(_handles);
                    eprintln!("[sync] mdns advertising on :{port}");
                }
                Err(e) => eprintln!("[sync] mdns off: {e}"),
            }
            {
                let db_path = paths.db_file();
                let creds = identity.creds();
                std::thread::spawn(move || crate::net::auto_sync_loop(db_path, creds));
            }
            {
                let db_path = paths.db_file();
                crate::net::sync_on_startup(db_path, identity.creds());
            }
            {
                let db_path = paths.db_file();
                crate::net::watch_discovered(db_path, identity.creds(), discovered.clone());
            }
            port
        }
        Ok(None) => 0,
        Err(e) => {
            eprintln!("[sync] listener off: {e}");
            0
        }
    }
}

/// Watch the database for hotkey changes (written by any frontend)
/// and restart the listener with pill hooks when they move.
pub fn spawn_hotkey_watcher(
    db_path: PathBuf,
    down: DictationHook,
    up: DictationHook,
    cancel: DictationHook,
) {
    std::thread::spawn(move || {
        let mut last: (Vec<(String, crate::settings::ActivationMode)>, u64) = (Vec::new(), 0);
        let mut first = true;
        // Stale GUI commands from before this boot die unrun.
        if let Ok(store) = Store::open(&db_path) {
            let _ = store.take_server_cmd();
        }
        loop {
            std::thread::sleep(Duration::from_secs(2));
            let Ok(store) = Store::open(&db_path) else {
                continue;
            };
            // One-shot GUI commands: Stop / Cold-start the STT server.
            if let Some(cmd) = store.take_server_cmd() {
                let data_dir = db_path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_default();
                match cmd.as_str() {
                    "stop" => {
                        eprintln!("[service] GUI stop-servers command");
                        crate::transcribe::drop_servers(&data_dir);
                    }
                    "start" => {
                        eprintln!("[service] GUI cold-start command");
                        let settings = store.load_settings();
                        match crate::transcribe::plan_for(&settings.stt, &data_dir) {
                            Ok(plan) => {
                                let net = crate::transcribe::server_net(&settings.stt);
                                let _ = crate::transcribe::ensure_for_plan(
                                    &plan,
                                    &data_dir,
                                    settings.stt.gpu_backend,
                                    settings.stt.sherpa_backend,
                                    &net,
                                );
                            }
                            Err(e) => {
                                eprintln!("[service] cold start: no local plan: {e}");
                            }
                        }
                    }
                    _ => {}
                }
            }
            let settings = store.load_settings();
            let hotkeys = settings.hotkeys;
            crate::transcribe::set_hotkeys_listening(hotkeys.hotkeys_enabled);
            crate::sound::set_enabled(settings.preferences.dictation_sounds);
            crate::media::set_enabled(settings.preferences.pause_media_on_dictation);
            let current = (
                hotkeys
                    .effective_hotkeys()
                    .iter()
                    .map(|b| (b.combo.clone(), b.mode))
                    .collect::<Vec<_>>(),
                hotkeys.smart_tap_ms,
            );
            if current != last {
                last = current.clone();
                if !first {
                    eprintln!("[service] hotkeys changed, restarting listener");
                }
                first = false;
                crate::hotkey::start_dictation(
                    hotkeys.effective_hotkeys(),
                    hotkeys.smart_tap_ms,
                    down.clone(),
                    up.clone(),
                    cancel.clone(),
                );
            }
        }
    });
}

/// Commands from background threads to the service's GTK thread.
/// `VoiceOverlay` is `!Send`, so everything pill-related crosses as
/// a message and applies on the main loop.
#[derive(Debug, Clone)]
pub enum PillCmd {
    Show,
    Hide,
    /// Escape midway: hide at once, discard worker lands the row.
    Cancel,
    /// Worker finished (success/empty/fail): hide if generation matches.
    Done(u64),
    Apply(crate::settings::PillPrefs),
}

/// Watch the database for pill preference changes (written by any
/// frontend) and forward snapshots to the GTK thread.
pub fn spawn_pill_watcher(db_path: PathBuf, tx: std::sync::mpsc::Sender<PillCmd>) {
    std::thread::spawn(move || {
        let mut last: Option<crate::settings::PillPrefs> = None;
        loop {
            std::thread::sleep(Duration::from_secs(2));
            let Ok(store) = Store::open(&db_path) else {
                continue;
            };
            let current = store.load_settings().preferences.pill;
            if last.as_ref() != Some(&current) {
                last = Some(current.clone());
                eprintln!("[service] pill prefs changed: {current:?}");
                if tx.send(PillCmd::Apply(current)).is_err() {
                    return;
                }
            }
        }
    });
}

/// Service heartbeat file for the home banner (`<state>/service.json`,
/// 5 s cadence): pid, version, boot time, last beat, consecutive
/// engine failures, hotkey listening. Stale/missing = not running.
pub fn spawn_heartbeat(state_dir: PathBuf) {
    let path = state_dir.join("service.json");
    let boot = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let pid = std::process::id();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(5));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let body = serde_json::json!({
            "pid": pid,
            "version": crate::APP_VERSION,
            "boot": boot,
            "beat": now,
            "engine_failures": crate::transcribe::engine_failures(),
            "hotkeys_listening": crate::transcribe::hotkeys_listening(),
        });
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, body.to_string());
    });
}

/// Read someone else's heartbeat (GUI side). Ok only when fresh
/// (< 15 s): stale or missing means not running.
pub fn read_heartbeat(state_dir: &std::path::Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(state_dir.join("service.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let beat = v.get("beat")?.as_i64()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if now - beat > 15 {
        return None;
    }
    Some(v)
}

// ---------------------------------------------------------------- systemd

const UNIT_NAME: &str = "mywhisper.service";

fn unit_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| {
        PathBuf::from(h)
            .join(".config/systemd/user")
            .join(UNIT_NAME)
    })
}

fn unit_body(exe: &std::path::Path) -> String {
    format!(
        "[Unit]\n\
         Description=MyWhisper background service (hotkeys, sync, pill)\n\
         After=graphical-session.target\n\
         \n\
         [Service]\n\
         ExecStart={} --service\n\
         Restart=on-failure\n\
         RestartSec=3\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe.display()
    )
}

fn systemctl(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| format!("systemctl missing: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Install/remove the systemd user unit and enable/disable it.
/// Uses the current executable path, so run this from the binary you
/// want started at login (release install, not a dev target dir).
pub fn ensure_unit_installed() -> Result<String, String> {
    let path = unit_path().ok_or_else(|| "no HOME".to_string())?;
    if path.exists() {
        return Ok("unit already installed".to_string());
    }
    let exe = std::env::current_exe().map_err(|e| format!("own path: {e}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    std::fs::write(&path, unit_body(&exe)).map_err(|e| format!("write unit: {e}"))?;
    systemctl(&["daemon-reload"])?;
    Ok(format!("unit installed ({})", exe.display()))
}
