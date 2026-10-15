//! Background engine ownership: sync listener, hotkey loop, pill.
//!
//! Exactly one process owns the engine at a time, decided by an
//! `flock`'d lockfile (`engine.lock`). The `--service` process holds
//! the lock. The GUI is a frontend: it reads shared local state and
//! never takes capture ownership or binds an incoming sync listener.
//!
//! The engine polls current settings and authorization epochs; pages
//! poll the explicit database generation counter. Capture suspension
//! additionally uses a process lock and nonce-bound request/acknowledgement.

use std::collections::HashMap;
use std::path::PathBuf;
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
/// polls the flag; periodic/discovery tasks and mDNS acknowledge shutdown.
/// Active incoming sockets are closed and their workers joined. Outgoing
/// jobs retain their original database authorization epoch.
pub struct SyncEngine {
    stop: Arc<AtomicBool>,
    mdns: Option<crate::net::MdnsHandles>,
    listener: Option<crate::runtime::OwnedThread>,
    watchers: Vec<crate::runtime::OwnedThread>,
}

impl Drop for SyncEngine {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(m) = self.mdns.take() {
            m.shutdown();
        }
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        for watcher in self.watchers.drain(..) {
            let _ = watcher.join();
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
    // Serialize replacement and release the old listener before rebinding.
    let mut owner = SYNC_ENGINE.lock().unwrap_or_else(|p| p.into_inner());
    drop(owner.take());
    let saved = store.load_settings();
    if !saved.sync.enabled {
        return 0;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let mut engine = SyncEngine {
        stop: stop.clone(),
        mdns: None,
        listener: None,
        watchers: Vec::new(),
    };
    let listener = match crate::net::listen(saved.sync.listen_port) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("[sync] listener unavailable: {error}");
            return 0;
        }
    };
    let port = listener
        .as_ref()
        .and_then(|listener| listener.local_addr().ok())
        .map(|address| address.port())
        .unwrap_or(0);
    if let Some(listener) = listener {
        let db_path = paths.db_file();
        let creds = identity.creds();
        let device_name = crate::device::display_name(&saved.sync.device_name, &identity.node_id);
        let hello_addrs = crate::net::lan_ip()
            .map(|ip| std::net::SocketAddr::new(ip, port).to_string())
            .into_iter()
            .collect();
        let stop = stop.clone();
        match crate::runtime::spawn_dedicated("sync-listener", move || {
            crate::net::serve(listener, db_path, creds, device_name, hello_addrs, &stop)
        }) {
            Ok(listener) => engine.listener = Some(listener),
            Err(error) => {
                eprintln!("[sync] listener worker unavailable: {error}");
                return 0;
            }
        }
    }
    // A zero listen port disables incoming connections/advertising, while
    // browsing and outbound synchronization remain available.
    engine.mdns = match crate::net::start_mdns(
        &identity.node_id,
        &saved.sync.device_name,
        port,
        discovered.clone(),
        saved.sync.advertise && port != 0,
    ) {
        Ok(handles) => Some(handles),
        Err(error) => {
            eprintln!("[sync] discovery unavailable: {error}");
            None
        }
    };
    let db_path = paths.db_file();
    let creds = identity.creds();
    let periodic_stop = stop.clone();
    match crate::runtime::spawn_dedicated("sync-periodic", move || {
        crate::net::auto_sync_loop(db_path, creds, periodic_stop)
    }) {
        Ok(watcher) => engine.watchers.push(watcher),
        Err(error) => {
            eprintln!("[sync] periodic worker unavailable: {error}");
            return 0;
        }
    }
    match crate::net::watch_discovered(paths.db_file(), identity.creds(), discovered, stop.clone())
    {
        Ok(watcher) => engine.watchers.push(watcher),
        Err(error) => {
            eprintln!("[sync] discovery watcher unavailable: {error}");
            return 0;
        }
    }
    crate::net::sync_on_startup_owned(paths.db_file(), identity.creds(), store.sync_epoch(), stop);
    *owner = Some(engine);
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
) -> Result<crate::runtime::BackgroundTask, String> {
    crate::runtime::BackgroundTask::spawn("sync-settings", move |stop| {
        let mut first = true;
        // Last applied (enabled, port, name, advertise): any move
        // restarts the engine, so all edits apply immediately.
        let mut applied: Option<(bool, u16, String, bool)> = None;
        while !stop.stopped() {
            if !first && stop.wait(Duration::from_secs(2)) {
                return;
            }
            first = false;
            let Ok(store) = Store::open_existing(&db_file) else {
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
                let Ok(store) = Store::open_existing(&db_file) else {
                    continue;
                };
                let bound = start_sync_engine(&paths, &store, &identity, discovered.clone());
                if bound == 0
                    && SYNC_ENGINE
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .is_none()
                {
                    continue;
                }
                // Failed startup retries on the next poll, even with the
                // listener disabled; only a running owner is applied.
                applied = Some(want);
            } else {
                if applied.as_ref().map(|(on, _, _, _)| *on).unwrap_or(false) {
                    eprintln!("[sync] toggle off: stopping engine now");
                }
                stop_sync_engine();
                applied = Some(want);
            }
        }
    })
}

/// Apply the persisted input gate and feedback preferences before accepting
/// any listener event. Also used for later settings changes.
pub fn apply_input_preferences(settings: &crate::settings::AppSettings) {
    crate::transcribe::set_hotkeys_listening(settings.hotkeys.hotkeys_enabled);
    crate::sound::set_enabled(settings.preferences.dictation_sounds);
    crate::media::set_enabled(settings.preferences.pause_media_on_dictation);
}

/// Test seam (cfg(test) only): park after the control witness, before the
/// settings read, so a test can land a control transition strictly inside
/// the snapshot window. Production builds contain no hook.
#[cfg(test)]
pub(crate) static RECONCILE_TICK_PAUSE: Mutex<
    Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
> = Mutex::new(None);

/// One STT reconcile poll: compare the stored settings against the last
/// applied `(primary, autostart)` pair and act once on change.
/// Deferred/failed ticks leave the pair untouched so the next poll
/// retries. Split from the spawn loop so tests can drive the exact
/// production path deterministically (no sleeps, no threads).
pub(crate) fn stt_reconcile_tick(db_path: &std::path::Path, last: &mut Option<(String, bool)>) {
    let data_dir = db_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let Ok(store) = Store::open_existing(db_path) else {
        return;
    };
    // Control-generation witness BEFORE any settings reads: a Stop (or
    // any bump) landing inside the snapshot window below must not pair
    // its authority with this observation.
    let gen_before = crate::transcribe::server_generation();
    #[cfg(test)]
    if let Some((entered, proceed)) = RECONCILE_TICK_PAUSE
        .lock()
        .ok()
        .and_then(|mut guard| guard.take())
    {
        let _ = entered.send(());
        let _ = proceed.recv_timeout(Duration::from_secs(10));
    }
    let settings = store.load_settings();
    let primary = crate::transcribe::stt_primary_id(&settings.stt, &data_dir);
    let auto = settings.stt.auto_start_server;
    if crate::transcribe::server_generation() != gen_before {
        // Torn snapshot: a control transition landed mid-read. Apply
        // nothing and retry on the next poll with a fresh snapshot. Never
        // re-sample and continue: that would reintroduce the gap.
        eprintln!("[service] STT snapshot torn by concurrent control; retrying next poll");
        return;
    }
    let Some((last_primary, last_auto)) = last else {
        // First observation records the baseline without acting, exactly
        // like the service spawn path.
        *last = Some((primary, auto));
        return;
    };
    if *last_primary == primary && *last_auto == auto {
        return;
    }
    if crate::transcribe::inference_active() {
        eprintln!("[service] STT switch deferred (active inference)");
        return;
    }
    // The witness is validated authority for this observation: it predates
    // every input above and no transition interleaved. The toggle-on warm
    // below carries it unchanged; the primary-changed branch re-samples
    // after its own deliberate drop (documented there).
    let generation = gen_before;
    if primary != *last_primary {
        // Stale configuration must not serve: retire supervised servers,
        // then warm the current one when autostart is on. Failures retry
        // (pair untouched); only success or intentional lazy state
        // advances it. Active use deferred above, reset pending never
        // reaches here (service barrier). Authority re-samples after our
        // own deliberate drop: the drop is this tick's transition, and any
        // newer concurrent Stop still aborts the warm through its gate.
        crate::transcribe::drop_servers(&data_dir);
        let generation = crate::transcribe::server_generation();
        if auto {
            let warmed = (|| {
                let plan = crate::transcribe::plan_for(&settings.stt, &data_dir)?;
                let net = crate::transcribe::server_net(&settings.stt);
                crate::transcribe::ensure_for_plan_at_generation(
                    &plan,
                    &data_dir,
                    settings.stt.gpu_backend,
                    settings.stt.sherpa_backend,
                    &net,
                    generation,
                )
            })();
            if let Err(error) = warmed {
                eprintln!("[service] STT reconcile failed, retrying: {error}");
                return;
            }
        }
        *last = Some((primary, auto));
    } else if auto {
        // Only the autostart toggle moved on: warming is itself
        // deliberate, so the current local configuration starts (reuse
        // when already current). Failures retry without advancing.
        let warmed = (|| {
            let plan = crate::transcribe::plan_for(&settings.stt, &data_dir)?;
            let net = crate::transcribe::server_net(&settings.stt);
            crate::transcribe::ensure_for_plan_at_generation(
                &plan,
                &data_dir,
                settings.stt.gpu_backend,
                settings.stt.sherpa_backend,
                &net,
                generation,
            )
        })();
        if let Err(error) = warmed {
            eprintln!("[service] STT autostart warm failed, retrying: {error}");
            return;
        }
        *last = Some((primary, auto));
    } else {
        // Disable keeps the running server (no bounce).
        *last = Some((primary, auto));
    }
}

/// Watch for changes after the initial preferences have been applied.
pub fn spawn_hotkey_watcher(
    db_path: PathBuf,
    down: DictationHook,
    up: DictationHook,
    cancel: DictationHook,
) -> Result<crate::runtime::BackgroundTask, String> {
    if let Some(data) = db_path.parent() {
        crate::hotkey::configure_capture_suspend(data);
    }
    crate::runtime::BackgroundTask::spawn("input-settings", move |stop| {
        let mut last: (Vec<(String, crate::settings::ActivationMode)>, u64) = (Vec::new(), 0);
        let mut first = true;
        let mut capture_epoch = Store::open_existing(&db_path)
            .ok()
            .and_then(|store| store.capture_epoch().ok());
        let mut retention = std::time::Instant::now() - Duration::from_secs(60);
        // Stale GUI commands from before this boot die unrun.
        if let Ok(store) = Store::open_existing(&db_path)
            && let Err(e) = store.take_server_cmd()
        {
            eprintln!("[service] stale command cleanup failed: {e}");
        }
        let mut reset_recovery = crate::reset::Recovery::default();
        // Last applied STT (primary, autostart): the reconcile tick below
        // records the baseline on first pass and acts only on change.
        let mut stt_last: Option<(String, bool)> = None;
        let mut next = std::time::Instant::now();
        while !stop.stopped() {
            if stop.wait(next.saturating_duration_since(std::time::Instant::now())) {
                return;
            }
            // Schedule before IO so an unavailable database cannot make
            // any continue path spin. The first pass has no initial delay.
            next = std::time::Instant::now() + Duration::from_secs(2);
            let Ok(store) = Store::open_existing(&db_path) else {
                continue;
            };
            reset_recovery.tick(&db_path, &store);
            match store.capture_epoch() {
                Ok(current) => {
                    if capture_epoch.is_some_and(|old| old != current) {
                        crate::transcribe::invalidate_all_capture();
                        cancel();
                    }
                    capture_epoch = Some(current);
                }
                Err(error) => {
                    crate::transcribe::invalidate_all_capture();
                    eprintln!("[service] capture policy unavailable: {error}");
                }
            }
            if retention.elapsed() >= Duration::from_secs(60) {
                retention = std::time::Instant::now();
                if let Err(error) = store.maintain_retention_at(crate::store::now_ms() / 1000) {
                    eprintln!("[service] retention failed: {error}");
                }
            }
            if store.reset_pending().unwrap_or(true) {
                crate::transcribe::invalidate_all_capture();
                crate::transcribe::set_hotkeys_listening(false);
                crate::transcribe::drop_servers(
                    db_path.parent().unwrap_or(std::path::Path::new(".")),
                );
                continue;
            }
            // One-shot GUI commands: Stop / Cold-start the STT server.
            let command = match store.take_server_cmd() {
                Ok(cmd) => cmd,
                Err(e) => {
                    eprintln!("[service] command read failed: {e}");
                    continue;
                }
            };
            if let Some(cmd) = command {
                let data_dir = db_path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_default();
                match cmd.as_str() {
                    "stop" => {
                        eprintln!("[service] GUI stop-servers command");
                        crate::transcribe::invalidate_all_capture();
                        cancel();
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
                    "reset" => {
                        // Interactive data reset runs on the owned worker;
                        // admission stays withheld while it is pending.
                        eprintln!("[service] GUI reset command");
                        reset_recovery.start_interactive(&db_path);
                    }
                    _ => {}
                }
            }
            // Live STT reconciliation: autostart toggles and primary
            // changes apply within one poll (reset pending already
            // continued above, so the barrier is respected).
            stt_reconcile_tick(&db_path, &mut stt_last);
            let settings = store.load_settings();
            apply_input_preferences(&settings);
            let hotkeys = settings.hotkeys;
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
    })
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
    SuspendCapture([u8; 16]),
    /// Worker finished (success/empty/fail): hide if generation matches.
    Done(u64),
    Error(u64),
    Apply(crate::settings::PillPrefs),
}

/// Watch the database for pill preference changes (written by any
/// frontend) and forward snapshots to the GTK thread.
pub fn spawn_pill_watcher(
    db_path: PathBuf,
    tx: std::sync::mpsc::Sender<PillCmd>,
) -> Result<crate::runtime::BackgroundTask, String> {
    crate::runtime::BackgroundTask::spawn("pill-settings", move |stop| {
        let mut last: Option<crate::settings::PillPrefs> = None;
        while !stop.wait(Duration::from_secs(2)) {
            let Ok(store) = Store::open_existing(&db_path) else {
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
    })
}

/// Service heartbeat file for the home banner (`<state>/service.json`,
/// 5 s cadence): pid, version, boot time, last beat, consecutive
/// engine failures, hotkey listening. Stale/missing = not running.
pub fn spawn_heartbeat(state_dir: PathBuf) -> Result<crate::runtime::BackgroundTask, String> {
    let path = state_dir.join("service.json");
    let boot = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let pid = std::process::id();
    crate::runtime::BackgroundTask::spawn("service-heartbeat", move |stop| {
        while !stop.wait(Duration::from_secs(5)) {
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
    })
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
