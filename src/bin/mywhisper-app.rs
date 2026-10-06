use gtk4::prelude::*;
use mywhisper::app::{AppState, apply_theme, build_app_window, log_theme_state};

const APP_ID: &str = "com.mywhisper.App";
/// Desktop app host. Deep-link flags mirror the reference app:
/// `--page home|insights|dictionary|settings`
/// `--section Preferences|Hotkeys|Speech to Text|Language Models|Privacy & Data|System`
/// `--theme system|light|dark` (overrides the stored preference)
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

    // Sync listener (background thread, own DB handle). Port 0 disables.
    let saved = store.load_settings();
    let device_name = mywhisper::device::display_name(&saved.sync.device_name, &identity.node_id);
    // Discovery sink shared with the UI (built per activation).
    let discovered: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<String, mywhisper::net::Discovered>>,
    > = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    let sync_port = match mywhisper::net::listen(saved.sync.listen_port) {
        Ok(Some(listener)) => {
            let port = listener
                .local_addr()
                .map(|a| a.port())
                .unwrap_or(saved.sync.listen_port);
            eprintln!("[sync] listening on port {port}");
            let db_path = paths.db_file();
            let creds = identity.creds();
            let mut hello_addrs = Vec::new();
            if let Some(ip) = mywhisper::net::lan_ip() {
                hello_addrs.push(format!("{ip}:{port}"));
            }
            std::thread::spawn(move || {
                mywhisper::net::serve(listener, db_path, creds, device_name, hello_addrs)
            });
            // mDNS advertisement + background browsing.
            match mywhisper::net::start_mdns(
                &identity.node_id,
                &saved.sync.device_name.clone(),
                port,
                discovered.clone(),
            ) {
                Ok(_handles) => {
                    // Handles must live for the process lifetime; leak
                    // deliberately (app-lifetime service).
                    std::mem::forget(_handles);
                    eprintln!("[sync] mdns advertising on :{port}");
                }
                Err(e) => eprintln!("[sync] mdns off: {e}"),
            }
            // Periodic auto-sync with trusted reachable peers.
            {
                let db_path = paths.db_file();
                let creds = identity.creds();
                std::thread::spawn(move || mywhisper::net::auto_sync_loop(db_path, creds));
            }
            // Catch-up shortly after launch + sync when trusted peers
            // appear on the LAN.
            {
                let db_path = paths.db_file();
                mywhisper::net::sync_on_startup(db_path, identity.creds());
            }
            {
                let db_path = paths.db_file();
                mywhisper::net::watch_discovered(db_path, identity.creds(), discovered.clone());
            }
            port
        }
        Ok(None) => 0,
        Err(e) => {
            eprintln!("[sync] listener off: {e}");
            0
        }
    };

    let app = gtk4::Application::builder().application_id(APP_ID).build();

    let discovered_ui = discovered.clone();
    app.connect_activate(move |app| {
        eprintln!("[app] activate");
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
        state.sync_port.set(sync_port);
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
        let window = build_app_window(app, &state, &page, &section);
        window.present();
    });

    app.run_with_args::<&str>(&[])
}

fn value_of(args: &[String], key: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == key).map(|w| w[1].clone())
}
