//! Isolated native UI preview. No keychain, microphone, service, or real user data.
//! Run with: cargo run --example ui_preview -- [home|insights|dictionary|sync|settings] [light|dark] [en|es] [section] [empty]
//! Add `--png PATH` to render the window offscreen to a PNG and quit
//! (README screenshots; works without a visible desktop).
use gtk4::prelude::*;
use mywhisper::app::{AppState, apply_theme, build_app_window};

#[path = "ui_preview/smoke.rs"]
mod smoke;

fn main() -> gtk4::glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let app = gtk4::Application::builder()
        .application_id("com.mywhisper.UIPreview")
        .flags(gtk4::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(move |app| {
        let png = args
            .windows(2)
            .find(|w| w[0] == "--png")
            .map(|w| w[1].clone());
        let reset_fixture = args.iter().any(|arg| arg == "--reset-smoke").then(|| {
            assert_eq!(
                std::env::var("MYWHISPER_DB_KEY").as_deref(),
                Ok(smoke::RESET_TEST_KEY),
                "reset smoke requires the disposable database key"
            );
            tempfile::tempdir().unwrap()
        });
        let state = if let Some(root) = &reset_fixture {
            let db = root.path().join("state.db");
            let store = std::rc::Rc::new(mywhisper::store::Store::open(&db).unwrap());
            let identity = mywhisper::device::DeviceIdentity::ephemeral();
            AppState::load(&store, "preview", &db, identity.creds(), Default::default())
        } else {
            let mut state = AppState::fresh();
            std::rc::Rc::get_mut(&mut state).unwrap().db_path =
                std::env::temp_dir().join("mywhisper-ui-preview/state.db");
            state
        };
        state.settings.borrow_mut().sync.enabled = false;
        if !args.iter().any(|a| a == "empty") {
            for (raw, text, secs) in [
                (
                    "um so for the standup i finished the export feature and uh tomorrow i'm on the login bug",
                    "For the standup: I finished the export feature, and tomorrow I'm on the login bug.",
                    7,
                ),
                (
                    "meeting link",
                    "https://meet.example.com/weekly-sync",
                    2,
                ),
                (
                    "can you send me the slides from yesterday's design review when you get a chance",
                    "Can you send me the slides from yesterday's design review when you get a chance?",
                    6,
                ),
            ] {
                state
                    .store
                    .add_history(
                        raw,
                        text,
                        text.split_whitespace().count() as u32,
                        secs,
                        "preview",
                    )
                    .unwrap();
            }
            state.store.add_discarded(3, "preview").unwrap();
            state.store.upsert_word("MyWhisper", "preview").unwrap();
            state.store.upsert_word("PostgreSQL", "preview").unwrap();
            state
                .store
                .upsert_snippet(
                    "meeting link",
                    "https://meet.example.com/weekly-sync",
                    "preview",
                )
                .unwrap();
        }
        mywhisper::i18n::set_lang(mywhisper::i18n::UiLang::parse(
            args.get(3).map(String::as_str).unwrap_or("en"),
        ));
        let theme = if args.get(2).map(String::as_str) == Some("dark") {
            mywhisper::settings::Theme::Dark
        } else {
            mywhisper::settings::Theme::Light
        };
        state.settings.borrow_mut().preferences.theme = theme;
        state.settings.borrow_mut().preferences.ui_language =
            mywhisper::i18n::lang().code().to_string();
        let run_smoke = args.iter().any(|arg| arg == "--smoke");
        if run_smoke {
            smoke::seed_transcription_language(&state);
        }
        state.store.save_settings(&state.settings.borrow()).unwrap();
        apply_theme(theme);
        let window = build_app_window(
            app,
            &state,
            args.get(1).map(String::as_str).unwrap_or("home"),
            args.get(4).map(String::as_str).unwrap_or("Preferences"),
        );
        window.present();
        if args.iter().any(|a| a == "--expanded") {
            smoke::expand(&window, args.get(4).map(String::as_str) == Some("Snippets"));
        }
        if run_smoke {
            smoke::run(app, &state, &window);
        }
        if let Some(root) = reset_fixture {
            smoke::run_reset(app, &state, &window, root);
        }
        if let Some(path) = png {
            capture_after(app, 1500, path);
        }
    });
    app.run_with_args::<&str>(&[])
}

/// After `delay_ms`, render the first mapped toplevel at 2x to `path`,
/// then quit.
fn capture_after(app: &gtk4::Application, delay_ms: u64, path: String) {
    let app = app.clone();
    gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(delay_ms), move || {
        let window = app
            .windows()
            .into_iter()
            .find(|w| w.is_mapped())
            .expect("a mapped window to capture");
        let target: gtk4::Widget = window.clone().upcast();
        let (width, height) = (target.width() as f32, target.height() as f32);
        let snapshot = gtk4::Snapshot::new();
        snapshot.scale(2.0, 2.0);
        gtk4::WidgetPaintable::new(Some(&target)).snapshot(&snapshot, width as f64, height as f64);
        let node = snapshot.to_node().expect("window drew something");
        let renderer = window.renderer().expect("realized window has a renderer");
        let bounds = gtk4::graphene::Rect::new(0.0, 0.0, width * 2.0, height * 2.0);
        renderer
            .render_texture(&node, Some(&bounds))
            .save_to_png(&path)
            .expect("write png");
        eprintln!("saved {path} ({width}x{height} @2x)");
        app.quit();
    });
}
