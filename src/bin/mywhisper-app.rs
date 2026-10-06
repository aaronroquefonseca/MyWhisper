use gtk4::prelude::*;
use mywhisper::app::{AppState, apply_theme, build_app_window, log_theme_state};

const APP_ID: &str = "com.mywhisper.App";
/// Desktop app host. Deep-link flags mirror the reference app:
/// `--page home|insights|dictionary|settings`
/// `--section Preferences|Hotkeys|Speech to Text|Language Models|Privacy & Data|System`
/// `--theme system|light|dark` (overrides the stored preference)
fn main() -> gtk4::glib::ExitCode {
    let app = gtk4::Application::builder().application_id(APP_ID).build();

    app.connect_activate(|app| {
        eprintln!("[app] activate");
        let args: Vec<String> = std::env::args().collect();
        let page = value_of(&args, "--page").unwrap_or_else(|| "home".to_string());
        let section = value_of(&args, "--section").unwrap_or_else(|| "Preferences".to_string());
        let state = AppState::fresh();
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
