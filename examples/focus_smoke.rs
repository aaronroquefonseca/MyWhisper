use gtk4::gio;
use gtk4::prelude::*;
fn main() {
    let app = gtk4::Application::new(
        Some("com.mywhisper.FocusProbe"),
        gio::ApplicationFlags::NON_UNIQUE,
    );
    app.register(None::<&gio::Cancellable>).unwrap();
    libadwaita::init().unwrap();
    for _ in 0..16 {
        // The previous focus can be destroyed by focus notification handlers
        // while GtkLabel::grab_focus still holds its borrowed pointer. Keep
        // it alive through that dispatch, then demonstrate actual release.
        let window = gtk4::ApplicationWindow::new(&app);
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
        assert!(previous.grab_focus());
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
        assert!(next.grab_focus());
        assert!(
            weak.upgrade().is_some(),
            "previous focus destroyed during GTK dispatch"
        );
        let context = gtk4::glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }
        assert!(
            weak.upgrade().is_none(),
            "retired focus owner leaked after dispatch"
        );
        assert!(
            next.is_selectable(),
            "selectable labels must remain supported"
        );
        window.destroy();
        drop(window);
        while context.pending() {
            context.iteration(false);
        }
        assert!(
            weak_window.upgrade().is_none(),
            "closed focus owner retained its window"
        );
    }
    eprintln!("Focus ownership: 16 reentrant transitions preserve and release widgets/windows");
}
