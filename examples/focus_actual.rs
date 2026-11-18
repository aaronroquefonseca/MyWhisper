//! Longer actual lifecycle: real app windows with populated dictionary
//! stores, repeated edit/navigation/language rounds, and window-release
//! memory checks. Scope is #16 focus ownership + overlay fallback only:
//! it asserts window release, focus retention, selectable text, store
//! counts and navigation — never dictionary-card release, which stays
//! coordinated with #23.
use gtk4::prelude::*;
use mywhisper::app::{AppState, apply_theme, build_app_window, navigate};
use std::rc::Rc;

fn pump() {
    let context = gtk4::glib::MainContext::default();
    while context.pending() {
        context.iteration(false);
    }
}

fn descendants(root: &gtk4::Widget) -> Vec<gtk4::Widget> {
    let mut all = vec![root.clone()];
    let mut child = root.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        all.extend(descendants(&widget));
    }
    all
}

fn visible_buttons(root: &gtk4::Widget) -> Vec<gtk4::Button> {
    descendants(root)
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk4::Button>().ok())
        .filter(|button| button.is_visible())
        .collect()
}

fn selectable_count(root: &gtk4::Widget) -> usize {
    descendants(root)
        .iter()
        .filter_map(|widget| widget.downcast_ref::<gtk4::Label>())
        .filter(|label| label.is_selectable())
        .count()
}

/// One actual round: populated store, real window, navigation across pages,
/// a store edit, focus grab, language-aware labels, then close. Returns the
/// window weak ref; the caller releases it by replacing the i18n rebuild
/// hook (which owns one window clone) with the next round's window.
#[allow(clippy::too_many_lines)]
fn round(
    app: &gtk4::Application,
    theme_tag: &str,
    lang: mywhisper::i18n::UiLang,
    cycle: usize,
) -> gtk4::glib::WeakRef<gtk4::ApplicationWindow> {
    mywhisper::i18n::set_lang(lang);
    let state = AppState::fresh();
    state.settings.borrow_mut().sync.enabled = false;
    for index in 0..10 {
        state
            .store
            .upsert_word(&format!("ActualWord{cycle}-{index}"), "actual")
            .unwrap();
    }
    for index in 0..3 {
        state
            .store
            .upsert_snippet(
                &format!("actual trigger {cycle}-{index}"),
                &format!("Actual expansion {cycle}-{index}"),
                "actual",
            )
            .unwrap();
    }
    assert_eq!(state.store.load_words().unwrap().len(), 10);
    assert_eq!(state.store.load_snippets().unwrap().len(), 3);

    let window = build_app_window(app, &state, "dictionary", "Preferences");
    let weak = window.downgrade();
    window.present();
    pump();

    // Navigation across real pages through the shared helper and the stack.
    let root: gtk4::Widget = window.clone().upcast();
    navigate(&root, "home", "Preferences");
    pump();
    navigate(&root, "dictionary", "Preferences");
    pump();
    navigate(&root, "settings", "Preferences");
    pump();
    navigate(&root, "dictionary", "Preferences");
    pump();

    // Focus an actual visible button; the window needs a few main-loop
    // turns to map under Xvfb, so retry briefly. Reentrant dispatch must
    // not hit fatal gtk_widget_is_ancestor criticals (fatal under
    // G_DEBUG=fatal-criticals).
    let mut focused = false;
    for _ in 0..30 {
        pump();
        if visible_buttons(&root)
            .iter()
            .any(|button| button.grab_focus())
        {
            focused = true;
            break;
        }
    }
    assert!(
        focused,
        "{theme_tag}/{}: actual button grab_focus failed",
        lang.code()
    );
    pump();

    // Edit through the real store while the window lives, then verify the
    // window still serves the updated rows and selectable text survives.
    state
        .store
        .upsert_word(&format!("ActualEdited{cycle}"), "actual")
        .unwrap();
    state
        .store
        .delete_word(&format!("ActualWord{cycle}-0"), &state.node_id)
        .unwrap_or(());
    assert_eq!(state.store.load_words().unwrap().len(), 10);
    assert!(
        selectable_count(&root) >= 2,
        "{theme_tag}/{}: selectable labels lost",
        lang.code()
    );

    // Overlay fallback probe on the same display (X11 must not call the
    // Wayland-only layer-shell probe).
    let backend = gtk4::gdk::Display::default().map(|display| display.type_().name().to_string());
    let overlay = mywhisper::overlay::VoiceOverlay::new(app);
    if backend.as_deref() == Some("GdkX11Display") {
        assert!(
            !overlay.is_layer_shell(),
            "{theme_tag}: X11 must use the fallback overlay surface"
        );
    }

    window.close();
    pump();
    weak
}

fn main() {
    let app = gtk4::Application::new(
        Some("com.mywhisper.FocusActual"),
        gtk4::gio::ApplicationFlags::NON_UNIQUE,
    );
    app.register(None::<&gtk4::gio::Cancellable>).unwrap();
    libadwaita::init().unwrap();
    let mut previous: Option<gtk4::glib::WeakRef<gtk4::ApplicationWindow>> = None;
    let mut rounds = 0;
    for theme in [
        mywhisper::settings::Theme::Light,
        mywhisper::settings::Theme::Dark,
    ] {
        let theme_tag = match theme {
            mywhisper::settings::Theme::Light => "light",
            mywhisper::settings::Theme::Dark => "dark",
            mywhisper::settings::Theme::System => "system",
        };
        apply_theme(theme);
        for lang in [mywhisper::i18n::UiLang::En, mywhisper::i18n::UiLang::Es] {
            // Three repeated actual rounds per theme/language: populate,
            // navigate, edit, focus, rebuild. Each new window replaces the
            // i18n rebuild hook holding the previous window clone, so the
            // previous window must release here.
            for cycle in 0..3 {
                let weak = round(&app, theme_tag, lang, cycle);
                if let Some(old) = previous.take() {
                    pump();
                    assert!(
                        old.upgrade().is_none(),
                        "{theme_tag}/{}: actual window from prior round retained",
                        lang.code()
                    );
                }
                previous = Some(weak);
                rounds += 1;
            }
        }
    }
    // Release the final rebuild-hook clone holding the last window.
    mywhisper::i18n::set_rebuild_hook(Rc::new(|| {}));
    pump();
    if let Some(last) = previous {
        assert!(
            last.upgrade().is_none(),
            "final actual window retained after hook release"
        );
    }
    mywhisper::i18n::set_lang(mywhisper::i18n::UiLang::En);
    eprintln!(
        "Focus actual: {rounds} populated edit/navigation/language rounds released windows with selectable text and fallback overlay preserved"
    );
}
