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
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
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
        .truncate(false)
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
        .truncate(false)
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

/// Bound sync port, published in the heartbeat so frontends show
/// the real listener (0 = off).
static SYNC_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

pub fn set_sync_port(port: u16) {
    SYNC_PORT.store(port, std::sync::atomic::Ordering::SeqCst);
}

/// Running sync stack. Drop stops everything: the listener loop
/// polls the flag, the periodic threads exit within a second, the
/// mDNS daemon unregisters. In-flight syncs finish on their own.
pub struct SyncEngine {
    stop: Arc<AtomicBool>,
    mdns: Option<crate::net::MdnsHandles>,
}

impl Drop for SyncEngine {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(m) = self.mdns.take() {
            m.shutdown();
        }
        set_sync_port(0);
        eprintln!("[sync] engine stopped");
    }
}

static SYNC_ENGINE: Mutex<Option<SyncEngine>> = Mutex::new(None);

/// Start the sync stack: TCP listener, mDNS advertise+browse,
/// periodic auto-sync, startup catch-up, appear-watcher. Returns the
/// bound port (0 = listener off). Safe to call only as engine owner.
/// Replaces any running engine (single owner: the service).
pub fn start_sync_engine(
    paths: &AppPaths,
    store: &Store,
    identity: &DeviceIdentity,
    discovered: Arc<Mutex<HashMap<String, Discovered>>>,
) -> u16 {
    let saved = store.load_settings();
    let device_name = crate::device::display_name(&saved.sync.device_name, &identity.node_id);
    let stop = Arc::new(AtomicBool::new(false));
    let port: u16 = match crate::net::listen(saved.sync.listen_port) {
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
            {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    crate::net::serve(listener, db_path, creds, device_name, hello_addrs, &stop)
                });
            }
            let mdns = match crate::net::start_mdns(
                &identity.node_id,
                &saved.sync.device_name.clone(),
                port,
                discovered.clone(),
                saved.sync.advertise,
            ) {
                Ok(handles) => {
                    eprintln!(
                        "[sync] mdns browsing on :{port}{}",
                        if saved.sync.advertise {
                            " (advertising)"
                        } else {
                            ""
                        }
                    );
                    Some(handles)
                }
                Err(e) => {
                    eprintln!("[sync] mdns off: {e}");
                    None
                }
            };
            {
                let db_path = paths.db_file();
                let creds = identity.creds();
                let stop = stop.clone();
                std::thread::spawn(move || crate::net::auto_sync_loop(db_path, creds, stop));
            }
            {
                let db_path = paths.db_file();
                crate::net::sync_on_startup(db_path, identity.creds());
            }
            {
                let db_path = paths.db_file();
                let stop = stop.clone();
                crate::net::watch_discovered(db_path, identity.creds(), discovered.clone(), stop);
            }
            *SYNC_ENGINE.lock().unwrap() = Some(SyncEngine { stop, mdns });
            port
        }
        Ok(None) => 0,
        Err(e) => {
            eprintln!("[sync] listener off: {e}");
            0
        }
    };
    set_sync_port(port);
    port
}

/// Stop the sync stack now (toggle off). Dropping the guard stops
/// the listener, the loops and mDNS; the heartbeat flips to 0.
pub fn stop_sync_engine() {
    SYNC_ENGINE.lock().unwrap().take();
}

/// Watch the sync toggle (written by any frontend): applies changes
/// immediately — starts the engine when enabled, stops it when
/// disabled. First check runs at once (covers boot, no restart
/// needed). Engine owner only: call from the service.
pub fn spawn_sync_watcher(
    db_file: PathBuf,
    identity: DeviceIdentity,
    discovered: Arc<Mutex<HashMap<String, Discovered>>>,
) {
    std::thread::spawn(move || {
        let mut first = true;
        // Last applied (enabled, port, name, advertise): any move
        // restarts the engine, so all edits apply immediately.
        let mut applied: Option<(bool, u16, String, bool)> = None;
        loop {
            if !first {
                std::thread::sleep(Duration::from_secs(2));
            }
            first = false;
            let Ok(store) = Store::open(&db_file) else {
                continue;
            };
            let settings = store.load_settings();
            let want = (
                settings.sync.enabled,
                settings.sync.listen_port,
                settings.sync.device_name.clone(),
                settings.sync.advertise,
            );
            if Some(want.clone()) == applied {
                continue;
            }
            if want.0 {
                eprintln!(
                    "[sync] applying engine (port {}, name {:?})",
                    want.1, want.2
                );
                stop_sync_engine();
                let Ok(paths) = AppPaths::create() else {
                    continue;
                };
                let Ok(store) = Store::open(&db_file) else {
                    continue;
                };
                start_sync_engine(&paths, &store, &identity, discovered.clone());
                // If the listener failed, retry on the next change,
                // not in a tight loop.
                applied = Some(want);
            } else {
                if applied.as_ref().map(|(on, _, _, _)| *on).unwrap_or(false) {
                    eprintln!("[sync] toggle off: stopping engine now");
                }
                stop_sync_engine();
                applied = Some(want);
            }
        }
    });
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

/// Watch STT settings for provider switches and reconcile the
/// running local servers (service-owned, 2 s poll).
///
/// - Local-to-local with autostart: warm the new engine now (ensure
///   replaces the old); lazy: stop the old, leave the new cold until
///   the next utterance.
/// - Leaving local (cloud / self-hosted / Whistle / disabled /
///   unresolvable): stop owned locals so the previous engine does not
///   stay warm.
/// - Autostart toggle alone: warm on enable (if local and idle),
///   keep running on disable (no bounce).
/// - Active recording/inference defers: the fingerprint is retried on
///   the next idle tick, never mid-utterance.
/// - Fallback chains key off the primary (first enabled) plan; the
///   release worker still ensures fallbacks per-utterance.
///
/// Engine owner only: call from the service.
pub fn spawn_stt_watcher(db_path: PathBuf) {
    // Boot state already warmed (or deliberately lazy) by
    // run_headless; track it so the first tick is a no-op.
    let (mut last_primary, mut last_auto) = match Store::open(&db_path) {
        Ok(store) => {
            let data_dir = db_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default();
            let stt = store.load_settings().stt;
            (
                crate::transcribe::stt_primary_id(&stt, &data_dir),
                stt.auto_start_server,
            )
        }
        Err(_) => (String::new(), true),
    };
    std::thread::spawn(move || {
        let data_dir = db_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        loop {
            std::thread::sleep(Duration::from_secs(2));
            stt_watcher_tick(&db_path, &data_dir, &mut last_primary, &mut last_auto);
        }
    });
}

/// One STT watcher poll: compare the stored settings against the last
/// applied `(primary, autostart)` pair and reconcile once on change.
/// Deferred/failed ticks leave the pair untouched so the next poll
/// retries. Split from the spawn loop so tests can drive the exact
/// production path deterministically (no sleeps, no threads).
pub fn stt_watcher_tick(
    db_path: &Path,
    data_dir: &Path,
    last_primary: &mut String,
    last_auto: &mut bool,
) {
    let Ok(store) = Store::open(db_path) else {
        return;
    };
    let stt = store.load_settings().stt;
    let curr_primary = crate::transcribe::stt_primary_id(&stt, data_dir);
    let curr_auto = stt.auto_start_server;
    if curr_primary == *last_primary && curr_auto == *last_auto {
        return;
    }
    if crate::transcribe::inference_active() {
        eprintln!("[service] STT switch deferred (active utterance)");
        return;
    }
    if curr_primary != *last_primary {
        // Configuration moved. Transient failures (foreign port,
        // missing runtime, admission contention) retry on the next
        // tick; only success advances the fingerprint. Active
        // captures/inference defer the same way.
        let applied = if curr_auto {
            match crate::transcribe::reconcile_servers(&stt, data_dir) {
                Ok(deferred) => !deferred,
                Err(e) => {
                    eprintln!("[service] STT reconcile failed, retrying: {e}");
                    false
                }
            }
        } else {
            match crate::transcribe::reconcile_lazy_switch(data_dir) {
                Ok(deferred) => !deferred,
                Err(e) => {
                    eprintln!("[service] STT lazy stop failed, retrying: {e}");
                    false
                }
            }
        };
        if applied {
            eprintln!("[service] STT config reconciled");
            *last_primary = curr_primary;
            *last_auto = curr_auto;
        }
    } else {
        // Only the autostart toggle moved.
        if curr_auto {
            // Re-enabling is itself deliberate: expire any Stop
            // suppression of this fingerprint before warming, so
            // the current local configuration starts.
            crate::transcribe::expire_stop_suppression();
            match crate::transcribe::reconcile_servers(&stt, data_dir) {
                Ok(deferred) => {
                    if !deferred {
                        *last_auto = curr_auto;
                    }
                }
                Err(e) => {
                    eprintln!("[service] STT autostart warm failed, retrying: {e}");
                }
            }
        } else {
            // Disable keeps the running server (no bounce).
            *last_auto = curr_auto;
        }
    }
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
    std::thread::spawn(move || {
        loop {
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
                    "sync_port": SYNC_PORT.load(std::sync::atomic::Ordering::SeqCst),
            });
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, body.to_string());
        }
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
