//! Desktop app shell: left sidebar + content stack.
//!
//! Pages share navigation, appearance, feedback, and confirmation helpers.
//! Controls persist to the store; the background service observes changes.

use crate::i18n::tr;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, Box as GtkBox, Button, Label, Orientation, Stack};
use std::rc::Rc;
use std::sync::Once;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::settings::{AppSettings, HistoryEntry, Theme, UsageStats};

pub struct AppState {
    /// Interior mutability: pages render from here, and the (few)
    /// already-wired controls write back here.
    pub settings: std::cell::RefCell<AppSettings>,
    pub history: Vec<HistoryEntry>,
    pub stats: UsageStats,
    pub store: Rc<crate::store::Store>,
    pub node_id: String,
    pub db_path: std::path::PathBuf,
    pub creds: crate::device::SyncCreds,
    /// Actually-bound sync port (0 = listener off). Set by main after bind.
    pub sync_port: std::cell::Cell<u16>,
    /// True when the background service holds the engine lock. The
    /// GUI is always frontend-only; this only drives status display.
    pub service_alive: std::cell::Cell<bool>,
    /// mDNS-discovered LAN peers, refreshed by the browser thread.
    pub discovered:
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, crate::net::Discovered>>>,
}

impl AppState {
    pub fn fresh() -> Rc<Self> {
        Rc::new(Self {
            settings: std::cell::RefCell::new(AppSettings::default()),
            history: Vec::new(),
            stats: UsageStats::default(),
            store: Rc::new(crate::store::Store::open_memory().expect("in-memory store")),
            node_id: "local".to_string(),
            db_path: std::path::PathBuf::from(":memory:"),
            creds: crate::device::DeviceIdentity::ephemeral().creds(),
            sync_port: std::cell::Cell::new(0),
            service_alive: std::cell::Cell::new(false),
            discovered: Default::default(),
        })
    }

    /// Load persisted state; falls back to defaults table-by-table so
    /// a fresh or partial database still boots. Stats derive from the
    /// history table.
    pub fn load(
        store: &Rc<crate::store::Store>,
        node_id: &str,
        db_path: &std::path::Path,
        creds: crate::device::SyncCreds,
        discovered: std::sync::Arc<
            std::sync::Mutex<std::collections::HashMap<String, crate::net::Discovered>>,
        >,
    ) -> Rc<Self> {
        let mut settings = store.load_settings();
        // One-time hotkey migration (legacy single -> list); persist
        // so the old value can never resurface.
        if settings.hotkeys.migrate_legacy() {
            let _ = store.save_settings(&settings);
        }
        let history = store.load_history(200, false).unwrap_or_default();
        let mut stats = store.usage_stats().unwrap_or_default();
        stats.streak_days = streak(&store.active_days().unwrap_or_default());
        Rc::new(Self {
            settings: std::cell::RefCell::new(settings),
            history,
            stats,
            store: store.clone(),
            node_id: node_id.to_string(),
            db_path: db_path.to_path_buf(),
            creds,
            sync_port: std::cell::Cell::new(0),
            service_alive: std::cell::Cell::new(false),
            discovered,
        })
    }
}

/// Epoch-day index for a local calendar date, including the offset at that date.
pub fn local_day(epoch_secs: i64) -> i64 {
    let offset = gtk4::glib::DateTime::from_unix_utc(epoch_secs)
        .ok()
        .and_then(|date| date.to_local().ok())
        .map(|date| date.utc_offset().as_seconds())
        .unwrap_or(0);
    (epoch_secs + offset).div_euclid(86_400)
}

/// Consecutive active days ending today (or yesterday when today is
/// still quiet), from descending epoch-day numbers.
fn streak(days_desc: &[i64]) -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let today = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| local_day(d.as_secs() as i64))
        .unwrap_or(0);
    let mut expected = today;
    if !days_desc.contains(&today) {
        expected = today - 1;
    }
    let mut run = 0u32;
    for d in days_desc {
        if *d == expected {
            run += 1;
            expected -= 1;
        } else if *d < expected {
            break;
        }
    }
    run
}

/// Streak straight from the store (for live insights refreshes).
pub fn streak_days(store: &crate::store::Store) -> u32 {
    streak(&store.active_days().unwrap_or_default())
}

/// Self-owned palette. The app renders identically on every
/// desktop: light values live here, dark values in DARK_CSS. The
/// system is only ever read as a dark/light signal, never as styling.
const CSS: &str = include_str!("app.css");

/// Dark palette override. Loaded as a separate provider above the
/// base one when dark mode is active; only redefines colors.
const DARK_CSS: &str = r#"
@define-color app_bg #1b1e24;
@define-color app_fg #edf0f5;
@define-color app_muted #aab3c2;
@define-color app_card #242830;
@define-color app_borders #363d49;
@define-color app_sidebar #20242b;
@define-color app_accent #91b4ff;
@define-color app_accent_fg #132343;
@define-color app_selection #2d3c58;
@define-color app_entry_bg #1e2229;
@define-color app_entry_border #485261;
@define-color app_success #83d6a0;
@define-color app_danger #ffa1aa;
"#;

/// Above USER priority on purpose: desktops like Noctalia inject
/// their own palette through the user stylesheet, and this app owns
/// its theme by design (works the same everywhere).
const PALETTE_PRIORITY: u32 = 900;

/// Apply the appearance preference through libadwaita's style
/// manager -- the supported mechanism on GTK 4.20+ (the classic
/// `prefer-dark` Settings property is deprecated and ignored under
/// some desktop theme configurations).
///
/// `System` follows the desktop color-scheme portal; Light and Dark
/// force the variant. Call after the display is open (e.g. in
/// `activate`); re-call when the preference changes.
/// 0 = follow system, 1 = light, 2 = dark.
static THEME_MODE: AtomicU8 = AtomicU8::new(0);
static WATCH_ONCE: Once = Once::new();

std::thread_local! {
    /// Dark palette provider. Thread-local because `CssProvider` is
    /// neither `Send` nor `Sync`; every theme call runs on the GTK
    /// main thread anyway.
    static DARK_PROVIDER: std::cell::RefCell<Option<gtk4::CssProvider>> =
        const { std::cell::RefCell::new(None) };
}

fn with_dark_provider(f: impl FnOnce(&gtk4::CssProvider)) {
    DARK_PROVIDER.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            let provider = gtk4::CssProvider::new();
            provider.load_from_string(DARK_CSS);
            *slot = Some(provider);
        }
        f(slot.as_ref().expect("just initialized"));
    });
}

/// (Re)apply the dark palette overlay according to the current mode.
/// Called on every theme change and whenever the system sensor flips
/// while in System mode.
fn refresh_palette() {
    let mode = THEME_MODE.load(Ordering::SeqCst);
    let dark = match mode {
        1 => false,
        2 => true,
        _ => libadwaita::StyleManager::default().is_dark(),
    };
    if let Some(display) = gtk4::gdk::Display::default() {
        with_dark_provider(|provider| {
            gtk4::style_context_remove_provider_for_display(&display, provider);
            if dark {
                gtk4::style_context_add_provider_for_display(
                    &display,
                    provider,
                    PALETTE_PRIORITY + 10,
                );
            }
        });
    }
    eprintln!("[app] palette -> {}", if dark { "dark" } else { "light" });
}

fn ensure_dark_watcher() {
    WATCH_ONCE.call_once(|| {
        libadwaita::StyleManager::default().connect_dark_notify(|_| {
            if THEME_MODE.load(Ordering::SeqCst) == 0 {
                refresh_palette();
            }
        });
    });
}

pub fn apply_theme(theme: Theme) {
    THEME_MODE.store(
        match theme {
            Theme::System => 0,
            Theme::Light => 1,
            Theme::Dark => 2,
        },
        Ordering::SeqCst,
    );
    ensure_dark_watcher();
    refresh_palette();
    // Keep native widget details in sync; our own palette carries
    // every surface we draw.
    libadwaita::StyleManager::default().set_color_scheme(match theme {
        Theme::System => libadwaita::ColorScheme::Default,
        Theme::Light => libadwaita::ColorScheme::ForceLight,
        Theme::Dark => libadwaita::ColorScheme::ForceDark,
    });
}

/// One-line theme diagnostics for the log: effective dark state,
/// system support flag, requested scheme, and resolved theme name.
#[allow(deprecated)]
pub fn log_theme_state() {
    let sm = libadwaita::StyleManager::default();
    let theme_name = gtk4::Settings::default()
        .and_then(|s| s.gtk_theme_name())
        .unwrap_or_default();
    eprintln!(
        "[app] theme state: dark={} system-supports-color-schemes={} scheme={:?} theme-name={}",
        sm.is_dark(),
        sm.system_supports_color_schemes(),
        sm.color_scheme(),
        theme_name,
    );
}

pub fn load_app_css() {
    let provider = gtk4::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gtk4::gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(&display, &provider, PALETTE_PRIORITY);
    }
}

fn nav_button(label: &str, icon: &str) -> Button {
    let b = Button::new();
    let content = GtkBox::new(Orientation::Horizontal, 10);
    content.append(&gtk4::Image::from_icon_name(icon));
    let text = Label::new(Some(label));
    text.set_xalign(0.0);
    content.append(&text);
    b.set_child(Some(&content));
    b.set_halign(gtk4::Align::Fill);
    b
}

/// Consistent left-aligned page heading and supporting copy.
pub fn page_heading(parent: &GtkBox, title: &str, subtitle: &str) {
    let header = GtkBox::new(Orientation::Vertical, 6);
    let title = Label::new(Some(title));
    title.add_css_class("page-title");
    title.set_xalign(0.0);
    title.set_wrap(true);
    let subtitle = Label::new(Some(subtitle));
    subtitle.add_css_class("page-subtitle");
    subtitle.set_xalign(0.0);
    subtitle.set_wrap(true);
    header.append(&title);
    header.append(&subtitle);
    parent.append(&header);
}

fn find_named(widget: &gtk4::Widget, name: &str) -> Option<gtk4::Widget> {
    if widget.widget_name() == name {
        return Some(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_named(&current, name) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

/// Navigate within this window, including deep links from setup guidance.
pub fn navigate(from: &impl IsA<gtk4::Widget>, page: &str, section: &str) {
    let Some(root) = from.root() else {
        return;
    };
    let root = root.upcast::<gtk4::Widget>();
    if let Some(stack) = find_named(&root, "main-pages").and_then(|w| w.downcast::<Stack>().ok()) {
        stack.set_visible_child_name(page);
    }
    if page == "settings" {
        if let Some(stack) =
            find_named(&root, "settings-pages").and_then(|w| w.downcast::<Stack>().ok())
        {
            stack.set_visible_child_name(section);
        }
    }
}

/// Brief feedback stays attached to the window where the action occurred.
pub fn toast(from: &impl IsA<gtk4::Widget>, message: &str) {
    use libadwaita::prelude::*;
    let mut widget = Some(from.as_ref().clone());
    while let Some(current) = widget {
        if let Ok(overlay) = current.clone().downcast::<libadwaita::ToastOverlay>() {
            overlay.add_toast(libadwaita::Toast::new(message));
            return;
        }
        widget = current.parent();
    }
}

/// Native modal confirmation: Escape and the default response cancel.
/// A callback runs only for an explicit destructive response.
pub fn confirm(
    from: &impl IsA<gtk4::Widget>,
    title: &str,
    body: &str,
    action: &str,
    on_confirm: impl Fn() + 'static,
) {
    use libadwaita::prelude::*;
    let dialog = libadwaita::AlertDialog::builder()
        .heading(title)
        .body(body)
        .build();
    dialog.add_responses(&[("cancel", tr("action.cancel")), ("confirm", action)]);
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("confirm", libadwaita::ResponseAppearance::Destructive);
    dialog.connect_response(Some("confirm"), move |_, _| on_confirm());
    dialog.present(Some(from));
}

pub fn connect_confirmed(
    button: &Button,
    title: &'static str,
    body: &'static str,
    action: &'static str,
    callback: impl Fn(&Button) + 'static,
) {
    button.add_css_class("destructive-action");
    let callback = Rc::new(callback);
    button.connect_clicked(move |button| {
        let callback = callback.clone();
        let target = button.clone();
        confirm(button, title, body, action, move || callback(&target));
    });
}

std::thread_local! {
    static FEEDBACK: std::cell::RefCell<gtk4::glib::WeakRef<libadwaita::ToastOverlay>> = Default::default();
}

pub fn notify(message: &str) {
    FEEDBACK.with(|slot| {
        if let Some(overlay) = slot.borrow().upgrade() {
            overlay.add_toast(libadwaita::Toast::new(message));
        }
    });
}

/// Centered, width-capped content column: the reference look on wide
/// windows (title included). An `AdwClamp` caps the width but shrinks
/// on narrow windows instead of forcing a phantom horizontal
/// scrollbar. Returns the inner column to fill.
pub fn centered_column(outer: &GtkBox, width: i32) -> GtkBox {
    let clamp = libadwaita::Clamp::new();
    clamp.set_maximum_size(width);
    clamp.set_hexpand(true);
    outer.append(&clamp);
    let col = GtkBox::new(Orientation::Vertical, 18);
    col.set_vexpand(true);
    col.set_hexpand(true);
    clamp.set_child(Some(&col));
    col
}

/// Build the main window. Returns the window (not presented).
/// `initial_page` deep-links one of home|insights|dictionary|sync|settings,
/// `initial_section` deep-links a settings section.
pub fn build_app_window(
    app: &Application,
    state: &Rc<AppState>,
    initial_page: &str,
    initial_section: &str,
) -> ApplicationWindow {
    load_app_css();

    let window = libadwaita::ApplicationWindow::new(app);
    window.add_css_class("mywhisper-app");
    window.set_title(Some("MyWhisper"));
    window.set_default_size(1120, 760);

    let root = GtkBox::new(Orientation::Horizontal, 0);
    let sidebar = GtkBox::new(Orientation::Vertical, 6);
    sidebar.add_css_class("sidebar");
    sidebar.set_size_request(190, -1);
    root.append(&sidebar);
    let brand = GtkBox::new(Orientation::Horizontal, 8);
    brand.add_css_class("sidebar-brand");
    // Embed the source asset so development builds also show the real logo.
    let bytes = gtk4::glib::Bytes::from_static(include_bytes!("../assets/icons/app-icon.svg"));
    let logo = gtk4::Image::from_paintable(Some(&gtk4::Svg::from_bytes(&bytes)));
    logo.set_pixel_size(32);
    logo.set_valign(gtk4::Align::Center);
    brand.append(&logo);
    let title = Label::new(Some(tr("app.name")));
    title.add_css_class("app-title");
    title.set_xalign(0.0);
    brand.append(&title);
    sidebar.append(&brand);

    let stack = Stack::new();
    stack.set_widget_name("main-pages");
    stack.set_vexpand(true);
    stack.set_hexpand(true);
    stack.add_named(&crate::app_home::page(state), Some("home"));
    stack.add_named(&crate::app_insights::page(state), Some("insights"));
    stack.add_named(&crate::app_dictionary::page(state), Some("dictionary"));
    stack.add_named(&crate::app_sync::page(state), Some("sync"));
    stack.add_named(
        &crate::app_settings::page(state, initial_section),
        Some("settings"),
    );
    // Hidden pages must not contribute their minimum size to the window.
    stack.set_hhomogeneous(false);
    stack.set_vhomogeneous(false);
    root.append(&stack);

    let mut buttons = Vec::new();
    for (id, label, icon) in [
        ("home", tr("nav.home"), "audio-input-microphone-symbolic"),
        ("insights", tr("nav.insights"), "view-list-symbolic"),
        (
            "dictionary",
            tr("nav.dictionary"),
            "accessories-dictionary-symbolic",
        ),
        ("sync", tr("nav.sync"), "computer-symbolic"),
        (
            "settings",
            tr("nav.settings"),
            "preferences-system-symbolic",
        ),
    ] {
        if id == "sync" {
            let spacer = GtkBox::new(Orientation::Vertical, 0);
            spacer.set_vexpand(true);
            sidebar.append(&spacer);
        }
        let button = nav_button(label, icon);
        let target = stack.clone();
        button.connect_clicked(move |_| target.set_visible_child_name(id));
        sidebar.append(&button);
        buttons.push((button, id));
    }
    let version = Label::new(Some(&format!("MyWhisper {}", crate::APP_VERSION)));
    version.add_css_class("sidebar-footer");
    version.set_xalign(0.0);
    sidebar.append(&version);
    let highlight = Rc::new(move |stack: &Stack| {
        for (button, id) in &buttons {
            if stack.visible_child_name().as_deref() == Some(*id) {
                button.add_css_class("active");
            } else {
                button.remove_css_class("active");
            }
        }
    });
    let on_change = highlight.clone();
    stack.connect_visible_child_name_notify(move |stack| on_change(stack));
    stack.set_visible_child_name(match initial_page {
        "insights" | "dictionary" | "sync" | "settings" => initial_page,
        _ => "home",
    });
    highlight(&stack);

    let feedback = libadwaita::ToastOverlay::new();
    FEEDBACK.with(|slot| *slot.borrow_mut() = feedback.downgrade());
    let layout = GtkBox::new(Orientation::Vertical, 0);
    let header = libadwaita::HeaderBar::new();
    header.set_title_widget(Some(&Label::new(Some(tr("app.name")))));
    layout.append(&header);
    layout.append(&root);
    root.set_vexpand(true);
    feedback.set_child(Some(&layout));
    use libadwaita::prelude::*;
    window.set_content(Some(&feedback));

    // Preserve the current destination and settings section when translating.
    {
        let app = app.clone();
        let window = window.clone();
        let state = state.clone();
        let stack = stack.clone();
        crate::i18n::set_rebuild_hook(Rc::new(move || {
            let page = stack.visible_child_name().unwrap_or_else(|| "home".into());
            let section = find_named(stack.upcast_ref(), "settings-pages")
                .and_then(|w| w.downcast::<Stack>().ok())
                .and_then(|s| s.visible_child_name())
                .unwrap_or_else(|| "Preferences".into());
            let fresh = build_app_window(&app, &state, &page, &section);
            fresh.present();
            window.close();
        }));
    }
    window.upcast()
}
