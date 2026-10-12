//! Device-free native acceptance for the actual application logo and toast.
use gtk4::prelude::*;
use mywhisper::app::{AppState, apply_theme, build_app_window};
use mywhisper::i18n::tr;

fn main() -> gtk4::glib::ExitCode {
    let dark = std::env::args().any(|argument| argument == "dark");
    let app = gtk4::Application::builder()
        .application_id("com.mywhisper.DesktopSmoke")
        .flags(gtk4::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(move |app| {
        mywhisper::i18n::set_lang(if dark { mywhisper::i18n::UiLang::Es } else { mywhisper::i18n::UiLang::En });
        let state = AppState::fresh();
        state.settings.borrow_mut().sync.enabled = false;
        apply_theme(if dark { mywhisper::settings::Theme::Dark } else { mywhisper::settings::Theme::Light });
        let window = build_app_window(app, &state, "home", "Preferences");
        window.present();
        let mapped = window.clone();
        gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || symbolic_logo(&mapped));
        toast_proof(app, &window, dark);
    });
    app.run_with_args::<&str>(&[])
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


fn symbolic_logo(window: &gtk4::ApplicationWindow) {
    let mark = descendants(window.upcast_ref())
        .into_iter()
        .find_map(|widget| {
            widget
                .downcast_ref::<gtk4::Image>()
                .and_then(gtk4::Image::paintable)
                .and_then(|paintable| paintable.downcast::<gtk4::Svg>().ok())
        })
        .expect("embedded brand paintable missing");
    let renderer = window.renderer().expect("native renderer missing");
    for foreground in [gtk4::gdk::RGBA::WHITE, gtk4::gdk::RGBA::BLACK] {
        let snapshot = gtk4::Snapshot::new();
        mark.snapshot_with_weight(&snapshot, 32.0, 32.0, &[foreground; 4], 400.0);
        let node = snapshot.to_node().expect("embedded logo is blank");
        let texture = renderer.render_texture(
            &node,
            Some(&gtk4::graphene::Rect::new(0.0, 0.0, 32.0, 32.0)),
        );
        let mut pixels = [0; 32 * 32 * 4];
        texture.download(&mut pixels, 32 * 4);
        let visible = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[3] != 0)
            .count();
        assert!(
            visible >= 100,
            "embedded logo blank or truncated: {visible}"
        );
        for pixel in pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[3] != 0)
        {
            let expected = if foreground == gtk4::gdk::RGBA::WHITE {
                pixel[3]
            } else {
                0
            };
            assert!(
                pixel[..3].iter().all(|channel| *channel == expected),
                "embedded logo does not follow symbolic foreground"
            );
        }
    }
    eprintln!("[ui-smoke] embedded logo renders in both symbolic foregrounds");
}


pub fn toast_proof(app: &gtk4::Application, window: &gtk4::ApplicationWindow, dark: bool) {
    let app = app.clone();
    let window = window.clone();
    let directory = tempfile::Builder::new()
        .prefix("mywhisper-toast-proof-")
        .tempdir()
        .unwrap()
        .keep();
    gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
        let feedback = descendants(window.upcast_ref())
            .into_iter()
            .find_map(|widget| widget.downcast::<libadwaita::ToastOverlay>().ok())
            .unwrap();
        let toast = libadwaita::Toast::new(tr("feedback.deleted"));
        toast.set_timeout(0);
        feedback.add_toast(toast);
        gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(350), move || {
            let toast = descendants(window.upcast_ref())
                .into_iter()
                .find(|widget| widget.css_name() == "toast")
                .unwrap();
            let close = descendants(&toast)
                .into_iter()
                .find_map(|widget| {
                    widget.downcast::<gtk4::Button>().ok().filter(|button| {
                        button.icon_name().as_deref() == Some("window-close-symbolic")
                    })
                })
                .unwrap();
            let expected =
                gtk4::gdk::RGBA::parse(if dark { "#edf0f5" } else { "#202631" }).unwrap();
            assert_eq!(close.color(), expected);
            let label = descendants(&toast)
                .into_iter()
                .find_map(|widget| widget.downcast::<gtk4::Label>().ok())
                .unwrap();
            assert_eq!(label.color(), expected);
            assert!(
                close.width() <= 36 && close.height() <= 36,
                "toast close must retain compact dimensions"
            );
            let normal = render_widget(&window, &toast);
            normal.save_to_png(directory.join("normal.png")).unwrap();
            close.set_state_flags(gtk4::StateFlags::PRELIGHT, false);
            gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
                let hover = render_widget(&window, &toast);
                hover.save_to_png(directory.join("hover.png")).unwrap();
                let normal_pixel = texture_pixel(&normal, 24, 12);
                let hover_pixel = texture_pixel(&hover, 24, 12);
                assert_eq!(
                    normal_pixel, hover_pixel,
                    "hover must not invert the toast surface"
                );
                let card =
                    gtk4::gdk::RGBA::parse(if dark { "#242830" } else { "#ffffff" }).unwrap();
                let expected = [card.red(), card.green(), card.blue()]
                    .map(|value| (value * 255.0).round() as u8);
                for index in 0..3 {
                    assert!(
                        normal_pixel[index].abs_diff(expected[index]) <= 2,
                        "toast background must match the active card palette"
                    );
                }
                assert_eq!(close.color(), label.color());
                assert_ne!(
                    normal.save_to_png_bytes(),
                    hover.save_to_png_bytes(),
                    "close hover must still provide feedback"
                );
                close.emit_clicked();
                println!(
                    "Native toast palette/close-hover/dismiss regression passed; visual evidence: {}",
                    directory.display()
                );
                app.quit();
            });
        });
    });
}

fn render_widget(window: &gtk4::ApplicationWindow, widget: &gtk4::Widget) -> gtk4::gdk::Texture {
    let snapshot = gtk4::Snapshot::new();
    let paintable = gtk4::WidgetPaintable::new(Some(widget));
    paintable.snapshot(
        &snapshot,
        f64::from(widget.width()),
        f64::from(widget.height()),
    );
    let node = snapshot.to_node().unwrap();
    window.renderer().unwrap().render_texture(
        &node,
        Some(&gtk4::graphene::Rect::new(
            0.0,
            0.0,
            widget.width() as f32,
            widget.height() as f32,
        )),
    )
}

fn texture_pixel(texture: &gtk4::gdk::Texture, x: usize, y: usize) -> [u8; 4] {
    let mut downloader = gtk4::gdk::TextureDownloader::new(texture);
    downloader.set_format(gtk4::gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    bytes[y * stride + x * 4..y * stride + x * 4 + 4]
        .try_into()
        .unwrap()
}
