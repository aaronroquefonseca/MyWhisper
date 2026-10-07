use gtk4::prelude::*;
use mywhisper::app::{apply_theme, build_app_window, log_theme_state, AppState};
use std::rc::Rc;

const APP_ID: &str = "com.mywhisper.App";
/// Separate bus name for the headless engine: sharing the GUI's ID
/// would remote-activate GUI launches into the service (which has no
/// window) and the GUI would exit without opening anything.
const SERVICE_APP_ID: &str = "com.mywhisper.Service";
/// Desktop app host. Deep-link flags mirror the reference app:
/// `--page home|insights|dictionary|settings`
/// `--section Preferences|Hotkeys|Speech to Text|Language Models|Privacy & Data|System`
/// `--theme system|light|dark` (overrides the stored preference)
/// `--service` runs the headless background engine (hotkeys, sync,
/// pill) with no window. The engine lives ONLY in the service: the
/// GUI is always a pure frontend reading the shared database. If the
/// service isn't running, the GUI says so and offers to start it.
fn main() -> gtk4::glib::ExitCode {
    let paths = mywhisper::paths::AppPaths::create().expect("create app dirs");
    let _log_guard = mywhisper::paths::init_file_logging(&paths.state_dir);
    let store =
        std::rc::Rc::new(mywhisper::store::Store::open(&paths.db_file()).expect("open state.db"));
    let identity = mywhisper::device::DeviceIdentity::load_or_create(&paths.device_key_file())
        .expect("node identity");
    tracing::info!(
        node = %identity.node_id,
        db = ?paths.db_file(),
        "mywhisper-app starting"
    );

    let args: Vec<String> = std::env::args().collect();
    let service_mode = args.iter().any(|a| a == "--service");

    if service_mode {
        // Single-instance engine: exit if another process owns it.
        let _engine_guard = mywhisper::service::acquire_engine_lock(&paths);
        if _engine_guard.is_none() {
            eprintln!("[service] another engine already runs, exiting");
            return gtk4::glib::ExitCode::FAILURE;
        }
        // Discovery sink shared with the UI (built per activation).
        let discovered: std::sync::Arc<
            std::sync::Mutex<std::collections::HashMap<String, mywhisper::net::Discovered>>,
        > = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let _sync_port =
            mywhisper::service::start_sync_engine(&paths, &store, &identity, discovered);
        let app = gtk4::Application::builder()
            .application_id(SERVICE_APP_ID)
            .build();
        return run_headless(app, &paths);
    }

    // GUI: pure frontend. Engine state is only observed (for the
    // status banner), never taken.
    let service_alive = mywhisper::service::engine_running(&paths);
    eprintln!("[app] background service alive: {service_alive}");
    let discovered: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<String, mywhisper::net::Discovered>>,
    > = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));

    let app = gtk4::Application::builder().application_id(APP_ID).build();

    {
        let discovered_ui = discovered.clone();
        // Single window: repeat launches remote-activate this instance;
        // focus the existing window instead of building another.
        let window: Rc<std::cell::RefCell<Option<gtk4::ApplicationWindow>>> =
            Rc::new(std::cell::RefCell::new(None));
        app.connect_activate(move |app| {
            eprintln!("[app] activate (frontend)");
            if let Some(w) = window.borrow().as_ref() {
                w.present();
                return;
            }
            let args: Vec<String> = std::env::args().collect();
            let page = value_of(&args, "--page").unwrap_or_else(|| "home".to_string());
            let section = value_of(&args, "--section").unwrap_or_else(|| "Preferences".to_string());
            let state = AppState::load(
                &store,
                &identity.node_id,
                &paths.db_file(),
                identity.creds(),
                discovered_ui.clone(),
            );
            state.service_alive.set(service_alive);
            mywhisper::i18n::set_lang(mywhisper::i18n::UiLang::parse(
                &state.settings.borrow().preferences.ui_language,
            ));
            if let Some(theme) = value_of(&args, "--theme") {
                use mywhisper::settings::Theme;
                let theme = match theme.as_str() {
                    "light" => Theme::Light,
                    "dark" => Theme::Dark,
                    _ => Theme::System,
                };
                state.settings.borrow_mut().preferences.theme = theme;
            }
            apply_theme(state.settings.borrow().preferences.theme);
            log_theme_state();
            let win = build_app_window(app, &state, &page, &section);
            win.present();
            {
                let slot = window.clone();
                win.connect_close_request(move |_| {
                    slot.borrow_mut().take();
                    gtk4::glib::Propagation::Proceed
                });
            }
            *window.borrow_mut() = Some(win);
        });
        app.run_with_args::<&str>(&[])
    }
}
/// Headless engine: pill overlay + hotkeys + settings watcher, no
/// window. The overlay and the poll loop live on the GTK thread; the
/// hotkey listener forwards through a channel (its callbacks are
/// `Send` and cannot hold GTK objects).
fn run_headless(
    app: gtk4::Application,
    paths: &mywhisper::paths::AppPaths,
) -> gtk4::glib::ExitCode {
    use mywhisper::service::PillCmd;
    let (tx, rx) = std::sync::mpsc::channel::<PillCmd>();
    let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));
    let down: mywhisper::hotkey::DictationHook = std::sync::Arc::new({
        let tx = tx.clone();
        move || {
            let _ = tx.send(PillCmd::Show);
        }
    });
    let up: mywhisper::hotkey::DictationHook = std::sync::Arc::new({
        let tx = tx.clone();
        move || {
            let _ = tx.send(PillCmd::Hide);
        }
    });

    // Initial bindings now; the watcher restarts the listener with
    // the same hooks whenever the DB changes.
    let db_path = paths.db_file();
    let store = mywhisper::store::Store::open(&db_path).expect("open state.db");
    let initial = store.load_settings().hotkeys;
    mywhisper::hotkey::start_dictation(
        initial.effective_hotkeys(),
        initial.smart_tap_ms,
        down.clone(),
        up.clone(),
    );
    mywhisper::service::spawn_hotkey_watcher(db_path.clone(), down, up);
    mywhisper::service::spawn_pill_watcher(db_path, tx.clone());

    app.connect_activate(move |app| {
        eprintln!("[service] activate, headless engine");
        let overlay = mywhisper::overlay::VoiceOverlay::new(app);
        overlay.hide();
        // Players we paused ourselves; resumed on dictation stop.
        let paused: Rc<std::cell::RefCell<Vec<String>>> =
            Rc::new(std::cell::RefCell::new(Vec::new()));
        // Drain pill commands on the GTK thread.
        let overlay = overlay.clone();
        let rx = rx.clone();
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            if let Ok(rx) = rx.lock() {
                while let Ok(cmd) = rx.try_recv() {
                    match cmd {
                        PillCmd::Show => {
                            mywhisper::sound::play(mywhisper::sound::UiSound::Start);
                            *paused.borrow_mut() = mywhisper::media::pause_playing();
                            overlay.show_listening();
                        }
                        PillCmd::Hide => {
                            mywhisper::sound::play(mywhisper::sound::UiSound::Stop);
                            mywhisper::media::resume(
                                &paused.borrow_mut().drain(..).collect::<Vec<_>>(),
                            );
                            overlay.hide();
                            // TEMP TEST HOOK: full output pipeline with a
                            // fixed word until transcription is wired.
                            std::thread::spawn(|| {
                                let paths = match mywhisper::paths::AppPaths::create() {
                                    Ok(p) => p,
                                    Err(e) => {
                                        eprintln!("[clipboard] test hook paths: {e}");
                                        return;
                                    }
                                };
                                let store = match mywhisper::store::Store::open(&paths.db_file()) {
                                    Ok(s) => s,
                                    Err(e) => {
                                        eprintln!("[clipboard] test hook store: {e}");
                                        return;
                                    }
                                };
                                let prefs = store.load_settings().preferences;
                                mywhisper::clipboard::run_pipeline("test", &prefs);
                            });
                        }
                        PillCmd::Apply(prefs) => overlay.apply_prefs(&prefs),
                    }
                }
            }
            gtk4::glib::ControlFlow::Continue
        });
        // No window: hold the application alive explicitly.
        std::mem::forget(app.hold());
    });
    app.run_with_args::<&str>(&[])
}

fn value_of(args: &[String], key: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == key).map(|w| w[1].clone())
}
