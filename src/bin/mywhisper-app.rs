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
    // Fixed task runtime first: every background job below flows
    // through it (bounded pool, no panicking spawns). If even this
    // fails the box cannot run threads at all: fail loud, not limp.
    if let Err(e) = mywhisper::runtime::init() {
        eprintln!("[service] FATAL: {e}");
        return gtk4::glib::ExitCode::FAILURE;
    }
    // Warm the configured STT engine now (model load takes seconds):
    // first press then only arms capture instead of cold-starting.
    {
        let db_path = paths.db_file();
        if let Err(e) = mywhisper::runtime::submit("startup-warmup", move || {
            let data_dir = db_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default();
            match mywhisper::store::Store::open(&db_path) {
                Ok(store) => {
                    let settings = store.load_settings();
                    match mywhisper::transcribe::plan_for(&settings.stt, &data_dir) {
                        Ok(plan) => {
                            eprintln!("[stt] startup warmup: {}", plan.describe());
                            if let Err(e) = mywhisper::transcribe::ensure_for_plan(
                                &plan,
                                &data_dir,
                                settings.stt.gpu_backend,
                            ) {
                                eprintln!("[stt] startup warmup: {e}");
                            }
                        }
                        Err(e) => eprintln!("[stt] startup warmup: {e}"),
                    }
                }
                Err(e) => eprintln!("[stt] startup warmup store: {e}"),
            }
        }) {
            eprintln!("[stt] {e}");
        }
    }

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
        // Utterance generation: a stale worker completion must not
        // hide a newer utterance's pill.
        let ugen: std::sync::Arc<std::sync::atomic::AtomicU64> =
            std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let gen_show = ugen.clone();
        let gen_hide = ugen.clone();
        let tx_done = tx.clone();
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            if let Ok(rx) = rx.lock() {
                while let Ok(cmd) = rx.try_recv() {
                    match cmd {
                        PillCmd::Show => {
                            gen_show.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            mywhisper::sound::play(mywhisper::sound::UiSound::Start);
                            *paused.borrow_mut() = mywhisper::media::pause_playing();
                            overlay.show_listening();
                            // Press: arm the RAM capture tap (+ open a
                            // streaming session when the engine does
                            // realtime). Prefs read fresh so UI edits
                            // apply to the very next utterance. Pooled:
                            // saturation drops the press, never the app.
                            if let Err(e) = mywhisper::runtime::submit("press-arm", || {
                                let paths = match mywhisper::paths::AppPaths::create() {
                                    Ok(p) => p,
                                    Err(e) => {
                                        eprintln!("[stt] paths: {e}");
                                        return;
                                    }
                                };
                                let data_dir = paths.data_dir.clone();
                                match mywhisper::store::Store::open(&paths.db_file()) {
                                    Ok(store) => {
                                        let settings = store.load_settings();
                                        mywhisper::transcribe::begin_utterance(
                                            settings.stt,
                                            &data_dir,
                                        );
                                    }
                                    Err(e) => eprintln!("[stt] store: {e}"),
                                }
                            }) {
                                eprintln!("[stt] press: {e}");
                            }
                        }
                        PillCmd::Hide => {
                            mywhisper::sound::play(mywhisper::sound::UiSound::Stop);
                            mywhisper::media::resume(
                                &paused.borrow_mut().drain(..).collect::<Vec<_>>(),
                            );
                            // Release: contract into the spinner while
                            // the worker transcribes; the worker sends
                            // the real Hide when it may disappear.
                            overlay.show_processing();
                            let tx_done = tx_done.clone();
                            let gen_at_release = gen_hide.load(std::sync::atomic::Ordering::SeqCst);
                            let gen_hide = gen_hide.clone();
                            mywhisper::transcribe::end_utterance(move || {
                                // Stale completion (new press meanwhile):
                                // leave the newer pill alone.
                                if gen_hide.load(std::sync::atomic::Ordering::SeqCst)
                                    != gen_at_release
                                {
                                    return;
                                }
                                let _ = tx_done.send(PillCmd::Done(gen_at_release));
                            });
                        }
                        PillCmd::Done(gen_at_release) => {
                            // Completion race (Show arrived after send):
                            // only hide our own generation.
                            if gen_hide.load(std::sync::atomic::Ordering::SeqCst) != gen_at_release
                            {
                                return gtk4::glib::ControlFlow::Continue;
                            }
                            overlay.hide();
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
