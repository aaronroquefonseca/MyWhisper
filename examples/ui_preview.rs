//! Isolated native UI preview. No keychain, microphone, service, or real user data.
//! Run with: cargo run --example ui_preview -- [home|insights|dictionary|sync|settings] [light|dark] [en|es] [section] [empty]
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
        let mut state = AppState::fresh();
        state.settings.borrow_mut().sync.enabled = false;
        std::rc::Rc::get_mut(&mut state).unwrap().db_path =
            std::env::temp_dir().join("mywhisper-ui-preview/state.db");
        if !args.iter().any(|a| a == "empty") {
            for (raw, text) in [
                (
                    "let's move the review to thursday so everyone has time to read the proposal",
                    "Let’s move the review to Thursday so everyone has time to read the proposal.",
                ),
                (
                    "my email signature",
                    "Aaron\nProduct & engineering\naaron@example.com",
                ),
                (
                    "the new microphone setup sounds much clearer",
                    "The new microphone setup sounds much clearer.",
                ),
            ] {
                state
                    .store
                    .add_history(
                        raw,
                        text,
                        text.split_whitespace().count() as u32,
                        8,
                        "preview",
                    )
                    .unwrap();
            }
            state.store.add_discarded(3, "preview").unwrap();
            state.store.upsert_word("MyWhisper", "preview").unwrap();
            state.store.upsert_word("CachyOS", "preview").unwrap();
            state
                .store
                .upsert_snippet(
                    "my email signature",
                    "Aaron\nProduct & engineering\naaron@example.com",
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
        if args.iter().any(|a| a == "--smoke") {
            smoke::run(app, &state, &window);
        }
    });
    app.run_with_args::<&str>(&[])
}
