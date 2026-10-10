//! Native reset callbacks under locks owned by a disposable second process.
use gtk4::prelude::*;
use mywhisper::{app::AppState, i18n::tr};
use std::io::{BufRead, Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::smoke::{descendants, named, respond};

pub(super) fn lock_owner(db: &Path) {
    // This helper never touches a host device or keyring. Parent death must
    // release its private locks even if GTK's fatal-critical abort skips Drop.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
    }
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(&format!(
        "PRAGMA key=\"x'{}'\"; BEGIN IMMEDIATE",
        "00".repeat(32)
    ))
    .unwrap();
    let control = db.parent().unwrap().join(".install");
    std::fs::create_dir_all(&control).unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(control.join("reset.lock"))
        .unwrap();
    fs2::FileExt::lock_shared(&file).unwrap();
    println!("READY");
    std::io::stdout().flush().unwrap();
    let mut commands = std::io::stdin().lock().lines();
    assert_eq!(commands.next().unwrap().unwrap(), "database");
    conn.execute_batch("ROLLBACK").unwrap();
    assert_eq!(commands.next().unwrap().unwrap(), "installer");
    drop(file);
}

struct LockOwner {
    child: Child,
    commands: ChildStdin,
}
impl LockOwner {
    fn start(db: &Path) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--reset-lock-owner")
            .arg(db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut output = child.stdout.take().unwrap();
        let commands = child.stdin.take().unwrap();
        let owner = Self { child, commands };
        let fd = output.as_raw_fd();
        // SAFETY: this owned pipe remains live throughout readiness polling.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(
            flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0
        );
        let mut bytes = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            assert!(Instant::now() < deadline, "lock fixture readiness deadline");
            let mut chunk = [0; 64];
            match output.read(&mut chunk) {
                Ok(0) => panic!("lock fixture exited before readiness"),
                Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("lock fixture readiness: {error}"),
            }
            assert!(bytes.len() <= 64);
            if bytes.ends_with(b"\n") {
                assert_eq!(bytes, b"READY\n");
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        owner
    }

    fn release(&mut self, command: &str) {
        writeln!(self.commands, "{command}").unwrap();
    }
}
impl Drop for LockOwner {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(super) fn run(app: &gtk4::Application, state: &Rc<AppState>, window: &gtk4::ApplicationWindow) {
    let data = state.db_path.parent().unwrap().to_owned();
    std::fs::write(data.join("models"), b"synthetic obstruction").unwrap();
    let mut owner = LockOwner::start(&state.db_path);
    let app = app.clone();
    let state = state.clone();
    let window = window.clone();
    let mut phase = 0;
    let mut started = Instant::now();
    let mut last_tick = Instant::now();
    let mut ticks = 0;
    let deadline = Instant::now() + Duration::from_secs(15);
    gtk4::glib::timeout_add_local(Duration::from_millis(20), move || {
        assert!(Instant::now() < deadline, "native reset did not complete");
        let gap = last_tick.elapsed();
        last_tick = Instant::now();
        if phase == 2 {
            assert!(
                gap < Duration::from_millis(250),
                "reset stalled GTK for {gap:?}"
            );
            ticks += 1;
        }
        let current = app
            .active_window()
            .unwrap()
            .downcast::<gtk4::ApplicationWindow>()
            .unwrap();
        let root = current.upcast_ref::<gtk4::Widget>();
        match phase {
            0 => {
                named::<gtk4::Stack>(root, "main-pages").set_visible_child_name("settings");
                named::<gtk4::Stack>(root, "settings-pages").set_visible_child_name("System");
                named::<gtk4::Button>(root, "reset-all").emit_clicked();
                phase = 1;
            }
            1 => {
                let before = Instant::now();
                respond(&window, "confirm");
                assert!(
                    before.elapsed() < Duration::from_millis(100),
                    "confirmation performed blocking reset work"
                );
                assert!(state.reset_busy.get());
                assert!(!named::<gtk4::Button>(root, "reset-all").is_sensitive());
                started = Instant::now();
                phase = 2;
            }
            2 if started.elapsed() >= Duration::from_millis(350) => {
                assert!(
                    ticks >= 10,
                    "GTK heartbeat did not run during held database lock"
                );
                assert!(state.reset_busy.get());
                owner.release("database");
                phase = 3;
            }
            3 if state.store.settings_warning() == Some("settings.reset.incomplete") => {
                assert!(
                    state.reset_busy.get(),
                    "installer owner should hold reset completion"
                );
                assert_eq!(
                    state.settings.borrow().preferences.theme,
                    mywhisper::settings::Theme::System
                );
                assert_eq!(state.settings.borrow().preferences.ui_language, "system");
                assert!(!state.settings.borrow().hotkeys.hotkeys_enabled);
                assert!(!state.settings.borrow().sync.enabled);
                assert!(state.store.load_history(200, false).unwrap().is_empty());
                assert!(!named::<gtk4::Button>(root, "reset-all").is_sensitive());
                owner.release("installer");
                phase = 4;
            }
            4 if !state.reset_busy.get() => {
                assert_eq!(
                    state.store.settings_warning(),
                    Some("settings.reset.incomplete")
                );
                assert!(
                    descendants(root).iter().any(|widget| widget
                        .downcast_ref::<gtk4::Label>()
                        .is_some_and(|label| label.text() == tr("settings.reset.incomplete"))),
                    "incomplete cleanup must be visible"
                );
                assert!(!named::<gtk4::Switch>(root, "hotkeys-master").is_sensitive());
                assert!(!named::<gtk4::Switch>(root, "sync-master").is_sensitive());
                let retry: gtk4::Button = named(root, "reset-all");
                assert_eq!(retry.label().as_deref(), Some(tr("settings.reset.retry")));
                assert!(retry.is_sensitive());
                state
                    .store
                    .upsert_word("preserved synthetic post-reset edit", "synthetic")
                    .unwrap();
                std::fs::remove_file(data.join("models")).unwrap();
                retry.emit_clicked();
                phase = 5;
            }
            5 => {
                respond(&current, "confirm");
                phase = 6;
            }
            6 if !state.reset_busy.get() => {
                assert_eq!(state.store.settings_warning(), None);
                assert!(!data.join(".reset-intent").exists());
                assert_eq!(
                    state.store.load_words().unwrap(),
                    ["preserved synthetic post-reset edit"]
                );
                assert!(!state.settings.borrow().sync.enabled);
                assert!(state.settings.borrow().hotkeys.hotkeys_enabled);
                assert!(named::<gtk4::Button>(root, "reset-all").is_sensitive());
                assert!(named::<gtk4::Switch>(root, "hotkeys-master").is_sensitive());
                assert!(named::<gtk4::Switch>(root, "sync-master").is_sensitive());
                println!(
                    "[ui-smoke] reset passed: independent DB/installer stalls, GTK heartbeat, committed state after file failure, visible retry, preserved later edits"
                );
                app.quit();
                return gtk4::glib::ControlFlow::Break;
            }
            _ => {}
        }
        gtk4::glib::ControlFlow::Continue
    });
}

pub(super) fn prepare_recovery(state: &Rc<AppState>) {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let data = state.db_path.parent().unwrap();
    state.store.reset_all_data().unwrap();
    state
        .store
        .upsert_word("preserved synthetic edit after commit", "synthetic")
        .unwrap();
    let conn = rusqlite::Connection::open(&state.db_path).unwrap();
    conn.execute_batch(&format!("PRAGMA key=\"x'{}'\"", "00".repeat(32)))
        .unwrap();
    let nonce = "33".repeat(16);
    conn.execute(
        "INSERT INTO kv(key,value) VALUES('reset_pending',?1)",
        [&nonce],
    )
    .unwrap();
    let metadata = std::fs::metadata(&state.db_path).unwrap();
    let intent = serde_json::json!({"version":1,"nonce":nonce,"device":metadata.dev(),"inode":metadata.ino()});
    let marker = data.join(".reset-intent");
    std::fs::write(&marker, serde_json::to_vec(&intent).unwrap()).unwrap();
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::create_dir_all(data.join("models")).unwrap();
    std::fs::write(data.join("models/old"), b"synthetic old model").unwrap();
    *state.settings.borrow_mut() = state.store.load_settings();
    assert_eq!(
        state.store.settings_warning(),
        Some("settings.reset.incomplete")
    );
    assert!(!state.settings.borrow().hotkeys.hotkeys_enabled);
    assert!(!state.settings.borrow().sync.enabled);
}

pub(super) fn resume(app: &gtk4::Application, state: &Rc<AppState>) {
    let app = app.clone();
    let state = state.clone();
    mywhisper::app_settings::resume_pending_reset(&state);
    assert!(state.reset_busy.get());
    let until = Instant::now() + Duration::from_secs(3);
    gtk4::glib::timeout_add_local(Duration::from_millis(20), move || {
        assert!(
            Instant::now() < until,
            "frontend did not recover its pending reset"
        );
        if state.reset_busy.get() {
            return gtk4::glib::ControlFlow::Continue;
        }
        let data = state.db_path.parent().unwrap();
        assert_eq!(state.store.settings_warning(), None);
        assert!(!data.join(".reset-intent").exists());
        assert!(!data.join("models/old").exists());
        assert_eq!(
            state.store.load_words().unwrap(),
            ["preserved synthetic edit after commit"]
        );
        assert!(state.settings.borrow().hotkeys.hotkeys_enabled);
        assert!(!state.settings.borrow().sync.enabled);
        assert!(
            state
                .store
                .export_history()
                .unwrap()
                .iter()
                .all(|row| row.deleted && row.text.is_empty() && row.raw_text.is_empty())
        );
        println!(
            "[ui-smoke] startup reset recovery passed: muted pending state, owned recovery, preserved later edit, removed old assets"
        );
        app.quit();
        gtk4::glib::ControlFlow::Break
    });
}
