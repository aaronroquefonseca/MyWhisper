use gtk4::gio;
use gtk4::prelude::*;

fn pump() {
    let context = gtk4::glib::MainContext::default();
    while context.pending() {
        context.iteration(false);
    }
}

fn reentrant_transition(app: &gtk4::Application, tag: &str) {
    // The previous focus can be destroyed by focus notification handlers
    // while GtkLabel::grab_focus still holds its borrowed pointer. Keep
    // it alive through that dispatch, then demonstrate actual release.
    let window = gtk4::ApplicationWindow::new(app);
    let weak_window = window.downgrade();
    mywhisper::app::manage_window_focus(&window);
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    let previous = gtk4::Button::with_label("Synthetic previous focus");
    let weak = previous.downgrade();
    let next = gtk4::Label::new(Some("Synthetic selectable text"));
    next.set_selectable(true);
    content.append(&previous);
    content.append(&next);
    window.set_child(Some(&content));
    window.present();
    assert!(previous.grab_focus(), "{tag}: previous grab_focus failed");
    drop(previous);
    window.connect_focus_widget_notify({
        let content = content.clone();
        let next = next.clone();
        move |window| {
            if gtk4::prelude::GtkWindowExt::focus(window).as_ref() == Some(next.upcast_ref())
                && let Some(previous) = content.first_child()
                && previous.is::<gtk4::Button>()
            {
                content.remove(&previous);
            }
        }
    });
    assert!(next.grab_focus(), "{tag}: selectable grab_focus failed");
    assert!(
        weak.upgrade().is_some(),
        "{tag}: previous focus destroyed during GTK dispatch"
    );
    pump();
    assert!(
        weak.upgrade().is_none(),
        "{tag}: retired focus owner leaked after dispatch"
    );
    assert!(
        next.is_selectable(),
        "{tag}: selectable labels must remain supported"
    );
    window.destroy();
    drop(window);
    pump();
    assert!(
        weak_window.upgrade().is_none(),
        "{tag}: closed focus owner retained its window"
    );
}

fn populated_rows(app: &gtk4::Application, tag: &str) {
    // Twenty populated rows: each row owns a selectable label plus a button.
    // A single reentrant handler retires the previously focused button by
    // removing its row during dispatch, mirroring dismissal handlers. The
    // retired button must survive dispatch (focus ownership) then release;
    // all rows must release after clear and the window after close.
    let window = gtk4::ApplicationWindow::new(app);
    let weak_window = window.downgrade();
    mywhisper::app::manage_window_focus(&window);
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    window.set_child(Some(&content));
    let mut row_weaks = Vec::new();
    let mut buttons = Vec::new();
    for index in 0..20 {
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        let label = gtk4::Label::new(Some(&format!("Synthetic row {index} text")));
        label.set_selectable(true);
        label.set_hexpand(true);
        let button = gtk4::Button::with_label(&format!("Row {index}"));
        row.append(&label);
        row.append(&button);
        content.append(&row);
        row_weaks.push(row.downgrade());
        buttons.push(button);
    }
    // Destination for the reentrant transition: a trailing selectable label.
    let next = gtk4::Label::new(Some("Synthetic populated next"));
    next.set_selectable(true);
    content.append(&next);
    let weak_next_row = next.downgrade();
    window.present();
    let weak_first = buttons[0].downgrade();
    assert!(
        buttons[0].grab_focus(),
        "{tag}: first row grab_focus failed"
    );
    window.connect_focus_widget_notify({
        let content = content.clone();
        let next = next.clone();
        move |window| {
            if gtk4::prelude::GtkWindowExt::focus(window).as_ref() == Some(next.upcast_ref())
                && let Some(first) = content.first_child()
            {
                content.remove(&first);
            }
        }
    });
    assert!(next.grab_focus(), "{tag}: populated next grab_focus failed");
    // Drop our own strong ref to the retired button: only focus ownership
    // may keep it alive through dispatch. The buttons vector keeps the
    // surviving rows' buttons; the retired one must stand alone.
    drop(buttons.remove(0));
    // The retired button survives dispatch through focus ownership even
    // though its parent row is already gone; the row itself needs no
    // retention once its focused child is owned elsewhere.
    assert!(
        weak_first.upgrade().is_some(),
        "{tag}: populated previous focus destroyed during dispatch"
    );
    assert!(
        row_weaks[0].upgrade().is_none(),
        "{tag}: retired row should drop with its container once removed"
    );
    pump();
    assert!(
        weak_first.upgrade().is_none(),
        "{tag}: populated previous focus leaked after dispatch"
    );
    assert!(
        next.is_selectable(),
        "{tag}: populated selectable label lost support"
    );
    // Every surviving row keeps selectable text supported.
    let mut child = content.first_child();
    while let Some(row) = child {
        child = row.next_sibling();
        let mut inner = row.first_child();
        while let Some(widget) = inner {
            inner = widget.next_sibling();
            if let Ok(label) = widget.clone().downcast::<gtk4::Label>() {
                assert!(
                    label.is_selectable(),
                    "{tag}: populated selectable label lost support"
                );
            }
        }
    }
    drop(buttons);
    drop(next);
    assert!(
        weak_next_row.upgrade().is_some(),
        "{tag}: trailing label should stay alive while still parented"
    );
    while let Some(first) = content.first_child() {
        content.remove(&first);
    }
    pump();
    for (index, weak) in row_weaks.iter().enumerate() {
        assert!(
            weak.upgrade().is_none(),
            "{tag}: populated row {index} retained after clear"
        );
    }
    // The trailing label is the current focus owner: focus ownership keeps
    // the live focused widget until the window closes, then releases it.
    assert!(
        weak_next_row.upgrade().is_some(),
        "{tag}: current focus owner should stay alive until window close"
    );
    window.destroy();
    drop(window);
    drop(content);
    pump();
    assert!(
        weak_next_row.upgrade().is_none(),
        "{tag}: trailing populated label retained after close"
    );
    assert!(
        weak_window.upgrade().is_none(),
        "{tag}: populated window retained after close"
    );
}

fn navigation(app: &gtk4::Application, tag: &str) {
    // Stack navigation with focus moving across pages. Retired page focus
    // owners must release, and the window must release on close.
    let window = gtk4::ApplicationWindow::new(app);
    let weak_window = window.downgrade();
    mywhisper::app::manage_window_focus(&window);
    let stack = gtk4::Stack::new();
    stack.set_widget_name("main-pages");
    let mut page_weaks = Vec::new();
    for name in ["home", "dictionary", "settings"] {
        let page = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        let label = gtk4::Label::new(Some(&format!("Synthetic {name} page")));
        label.set_selectable(true);
        let button = gtk4::Button::with_label(&format!("Focus {name}"));
        page.append(&label);
        page.append(&button);
        stack.add_named(&page, Some(name));
        page_weaks.push(page.downgrade());
    }
    window.set_child(Some(&stack));
    window.present();
    for name in ["home", "dictionary", "settings"] {
        stack.set_visible_child_name(name);
        pump();
        assert_eq!(
            stack.visible_child_name().as_deref(),
            Some(name),
            "{tag}: navigation to {name} failed"
        );
        if let Some(page) = stack.visible_child() {
            let mut child = page.first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                if let Ok(button) = widget.clone().downcast::<gtk4::Button>() {
                    assert!(button.grab_focus(), "{tag}: {name} focus failed");
                }
            }
        }
        pump();
    }
    // Exercise the shared navigate helper on the same hierarchy.
    mywhisper::app::navigate(&stack, "dictionary", "Preferences");
    pump();
    assert_eq!(
        stack.visible_child_name().as_deref(),
        Some("dictionary"),
        "{tag}: shared navigate helper failed"
    );
    window.destroy();
    drop(window);
    pump();
    assert!(
        weak_window.upgrade().is_none(),
        "{tag}: navigation window retained after close"
    );
    drop(page_weaks);
}

fn language_rebuild(app: &gtk4::Application, tag: &str) {
    // Language rebuild closes the old window and presents a new one with
    // translated copy. The retired window must release; the fresh window
    // must keep selectable text and reentrant focus ownership.
    for lang in [mywhisper::i18n::UiLang::En, mywhisper::i18n::UiLang::Es] {
        mywhisper::i18n::set_lang(lang);
        let window = gtk4::ApplicationWindow::new(app);
        let weak_window = window.downgrade();
        mywhisper::app::manage_window_focus(&window);
        let content = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        let label = gtk4::Label::new(Some(mywhisper::i18n::tr("nav.home")));
        label.set_selectable(true);
        let button = gtk4::Button::with_label(mywhisper::i18n::tr("nav.dictionary"));
        content.append(&label);
        content.append(&button);
        window.set_child(Some(&content));
        window.present();
        assert!(button.grab_focus(), "{tag}: {} focus failed", lang.code());
        pump();
        assert!(
            label.is_selectable(),
            "{tag}: {} selectable lost",
            lang.code()
        );
        window.destroy();
        drop(window);
        drop(button);
        drop(label);
        drop(content);
        pump();
        assert!(
            weak_window.upgrade().is_none(),
            "{tag}: {} window retained across language rebuild",
            lang.code()
        );
    }
    mywhisper::i18n::set_lang(mywhisper::i18n::UiLang::En);
}

fn overlay_fallback(app: &gtk4::Application, tag: &str) {
    // Under X11 the Wayland-only layer-shell probe must never run. The
    // overlay falls back to an undecorated window and keeps its supported
    // behavior without fatal criticals.
    let backend = gtk4::gdk::Display::default().map(|display| display.type_().name().to_string());
    let overlay = mywhisper::overlay::VoiceOverlay::new(app);
    if backend.as_deref() == Some("GdkX11Display") {
        assert!(
            !overlay.is_layer_shell(),
            "{tag}: X11 must use the fallback overlay surface"
        );
    }
    eprintln!(
        "{tag}: overlay backend={backend:?} layer-shell={}",
        overlay.is_layer_shell()
    );
}

fn main() {
    let app = gtk4::Application::new(
        Some("com.mywhisper.FocusProbe"),
        gio::ApplicationFlags::NON_UNIQUE,
    );
    app.register(None::<&gio::Cancellable>).unwrap();
    libadwaita::init().unwrap();
    for theme in [
        mywhisper::settings::Theme::Light,
        mywhisper::settings::Theme::Dark,
    ] {
        let theme_tag = match theme {
            mywhisper::settings::Theme::Light => "light",
            mywhisper::settings::Theme::Dark => "dark",
            mywhisper::settings::Theme::System => "system",
        };
        mywhisper::app::apply_theme(theme);
        eprintln!("[focus] theme={theme_tag}: 16 reentrant transitions");
        for _ in 0..16 {
            reentrant_transition(&app, theme_tag);
        }
        eprintln!("[focus] theme={theme_tag}: 20 populated rows");
        populated_rows(&app, theme_tag);
        eprintln!("[focus] theme={theme_tag}: navigation");
        navigation(&app, theme_tag);
        eprintln!("[focus] theme={theme_tag}: language rebuild");
        language_rebuild(&app, theme_tag);
        eprintln!("[focus] theme={theme_tag}: overlay backend probe");
        overlay_fallback(&app, theme_tag);
    }
    eprintln!(
        "Focus ownership: 16 reentrant transitions per theme plus 20 populated rows, navigation, and language rebuilds preserve and release widgets/windows; selectable text and fallback overlay behavior preserved"
    );
}
