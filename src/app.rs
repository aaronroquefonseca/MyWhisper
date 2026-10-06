//! Desktop app shell: left sidebar + content stack.
//!
//! Pages live in `app_home`, `app_insights`, `app_dictionary` and
//! `app_settings`. All widgets initialize from [`AppState`] but nothing
//! is wired yet: changing a control does not persist or apply.

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
}

impl AppState {
    pub fn fresh() -> Rc<Self> {
        Rc::new(Self {
            settings: std::cell::RefCell::new(AppSettings::default()),
            history: Vec::new(),
            stats: UsageStats::default(),
        })
    }
}

/// Self-owned palette. The app renders identically on every
/// desktop: light values live here, dark values in DARK_CSS. The
/// system is only ever read as a dark/light signal, never as styling.
const CSS: &str = r#"
@define-color app_bg #fafafa;
@define-color app_fg #1e1e1e;
@define-color app_card #ffffff;
@define-color app_borders #e0e0e0;
@define-color app_sidebar #f1f1f1;
@define-color app_accent #3584e4;
@define-color app_accent_fg #ffffff;
@define-color app_entry_bg #ffffff;
window {
    background: @app_bg;
    color: @app_fg;
}
.sidebar {
    background: @app_sidebar;
    border-right: 1px solid @app_borders;
    padding: 12px 8px;
}
.sidebar button {
    background: @app_card;
    color: @app_fg;
    border: 1px solid @app_borders;
    border-radius: 8px;
    padding: 8px 12px;
    font-weight: 500;
}
.sidebar button:hover {
    border-color: @app_accent;
}
.sidebar button.active {
    background: @app_accent;
    color: @app_accent_fg;
}
.app-title {
    color: @app_fg;
    font-weight: bold;
    font-size: 15px;
    padding: 4px 12px 12px 12px;
}
.page {
    padding: 24px;
}
.page-title {
    color: @app_fg;
    font-size: 22px;
    font-weight: bold;
}
.page-subtitle {
    color: @app_fg;
    opacity: 0.7;
}
.card {
    background: @app_card;
    border: 1px solid @app_borders;
    border-radius: 12px;
    padding: 16px;
}
.stat-value {
    color: @app_fg;
    font-size: 26px;
    font-weight: bold;
}
.stat-label {
    color: @app_fg;
    opacity: 0.7;
    font-size: 13px;
}
.section-title {
    color: @app_fg;
    font-size: 16px;
    font-weight: bold;
}
.row-label {
    color: @app_fg;
    font-weight: 500;
}
.dim {
    color: @app_fg;
    opacity: 0.65;
}
entry, textview {
    background: @app_entry_bg;
    color: @app_fg;
}
button {
    color: @app_fg;
}
/* Everything below exists so no surface falls through to the
   desktop theme (which may be dark while we are light). */
notebook, notebook > stack, scrolledwindow, viewport {
    background: @app_bg;
    color: @app_fg;
}
notebook > header {
    padding: 8px 16px 0 16px;
}
notebook > stack {
    padding: 16px;
}
list, listview, row {
    background: transparent;
    color: @app_fg;
}
tab {
    color: @app_fg;
}
popover, popover > contents {
    background: @app_card;
    color: @app_fg;
    border: 1px solid @app_borders;
    border-radius: 12px;
}
popover button {
    color: @app_fg;
}
.heat {
    background: @app_fg;
    border-radius: 4px;
    opacity: 0.12;
}
.heat-1 {
    background: @app_accent;
    opacity: 0.35;
}
.heat-2 {
    background: @app_accent;
    opacity: 0.6;
}
.heat-3 {
    background: @app_accent;
    opacity: 1.0;
}
.heat:hover {
    opacity: 1.0;
}
.backdrop {
    background: rgba(0, 0, 0, 0.45);
    border: none;
    border-radius: 0;
    box-shadow: none;
}
.modal {
    background: @app_card;
    border: 1px solid @app_borders;
    border-radius: 16px;
    box-shadow: 0 12px 48px rgba(0, 0, 0, 0.45);
}
.modal-header {
    padding: 12px 16px;
    border-bottom: 1px solid @app_borders;
}
.modal-title {
    color: @app_fg;
    font-size: 16px;
    font-weight: bold;
}
"#;

/// Dark palette override. Loaded as a separate provider above the
/// base one when dark mode is active; only redefines colors.
const DARK_CSS: &str = r#"
@define-color app_bg #242424;
@define-color app_fg #eeeeee;
@define-color app_card #2e2e2e;
@define-color app_borders #3d3d3d;
@define-color app_sidebar #2a2a2a;
@define-color app_accent #62a0ea;
@define-color app_accent_fg #ffffff;
@define-color app_entry_bg #2e2e2e;
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

fn nav_button(label: &str) -> Button {
    let b = Button::with_label(label);
    b.set_halign(gtk4::Align::Fill);
    b
}

/// Centered, width-capped content column: the reference look on wide
/// windows (title included). Returns the inner column to fill.
pub fn centered_column(outer: &GtkBox, width: i32) -> GtkBox {
    let center = GtkBox::new(Orientation::Horizontal, 0);
    center.set_halign(gtk4::Align::Center);
    outer.append(&center);
    let col = GtkBox::new(Orientation::Vertical, 16);
    col.set_size_request(width, -1);
    center.append(&col);
    col
}

/// Build the main window. Returns the window (not presented).
/// `initial_page` deep-links one of home|insights|dictionary|settings,
/// `initial_section` deep-links a settings section.
pub fn build_app_window(
    app: &Application,
    state: &Rc<AppState>,
    initial_page: &str,
    initial_section: &str,
) -> ApplicationWindow {
    load_app_css();

    let window = ApplicationWindow::new(app);
    window.set_title(Some("MyWhisper"));
    window.set_default_size(1080, 700);

    let root = GtkBox::new(Orientation::Horizontal, 0);
    // NOTE: `root` goes under the GtkOverlay below (single parent only).

    // ---- Sidebar ----
    let sidebar = GtkBox::new(Orientation::Vertical, 4);
    sidebar.add_css_class("sidebar");
    sidebar.set_size_request(190, -1);
    root.append(&sidebar);

    let title = Label::new(Some("MyWhisper"));
    title.add_css_class("app-title");
    title.set_xalign(0.0);
    sidebar.append(&title);

    let home_btn = nav_button("Home");
    let insights_btn = nav_button("Insights");
    let dict_btn = nav_button("Dictionary");
    sidebar.append(&home_btn);
    sidebar.append(&insights_btn);
    sidebar.append(&dict_btn);

    let spacer = GtkBox::new(Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    sidebar.append(&spacer);

    let settings_btn = nav_button("Settings");
    sidebar.append(&settings_btn);

    // ---- Content (main pages only; settings floats above) ----
    let stack = Stack::new();
    stack.set_vexpand(true);
    stack.set_hexpand(true);
    root.append(&stack);

    stack.add_titled(&crate::app_home::page(state), Some("home"), "Home");
    stack.add_titled(
        &crate::app_insights::page(state),
        Some("insights"),
        "Insights",
    );
    stack.add_titled(
        &crate::app_dictionary::page(state),
        Some("dictionary"),
        "Dictionary",
    );

    // ---- Settings modal (overlay layer, hidden by default) ----
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&root));
    window.set_child(Some(&overlay));

    // Dim backdrop: click anywhere outside the panel to close.
    let backdrop = Button::new();
    backdrop.add_css_class("backdrop");
    backdrop.set_hexpand(true);
    backdrop.set_vexpand(true);
    backdrop.set_visible(false);
    overlay.add_overlay(&backdrop);

    // Floating panel: header (title + close) over the settings content.
    let panel = GtkBox::new(Orientation::Vertical, 0);
    panel.add_css_class("modal");
    panel.set_halign(gtk4::Align::Center);
    panel.set_valign(gtk4::Align::Center);
    panel.set_size_request(900, 620);
    panel.set_visible(false);
    let header = GtkBox::new(Orientation::Horizontal, 8);
    header.add_css_class("modal-header");
    let header_title = Label::new(Some("Settings"));
    header_title.add_css_class("modal-title");
    header_title.set_xalign(0.0);
    header_title.set_hexpand(true);
    let close = Button::with_label("Close");
    header.append(&header_title);
    header.append(&close);
    panel.append(&header);
    let settings_content = crate::app_settings::page(state, initial_section);
    settings_content.set_vexpand(true);
    panel.append(&settings_content);
    overlay.add_overlay(&panel);

    let show_settings = Rc::new({
        let backdrop = backdrop.clone();
        let panel = panel.clone();
        move |show: bool| {
            backdrop.set_visible(show);
            panel.set_visible(show);
        }
    });
    {
        let show_settings = show_settings.clone();
        backdrop.connect_clicked(move |_| show_settings(false));
    }
    {
        let show_settings = show_settings.clone();
        close.connect_clicked(move |_| show_settings(false));
    }

    let buttons = [
        home_btn.clone(),
        insights_btn.clone(),
        dict_btn.clone(),
        settings_btn.clone(),
    ];
    let names = ["home", "insights", "dictionary", "settings"];
    let set_active = Rc::new(move |active: &str| {
        for (b, n) in buttons.iter().zip(names.iter()) {
            if *n == active {
                b.add_css_class("active");
            } else {
                b.remove_css_class("active");
            }
        }
    });

    // Main nav switches pages and dismisses the modal.
    for (btn, name) in
        [home_btn, insights_btn, dict_btn]
            .into_iter()
            .zip(["home", "insights", "dictionary"])
    {
        let stack = stack.clone();
        let set_active = set_active.clone();
        let show_settings = show_settings.clone();
        let name = name.to_string();
        btn.connect_clicked(move |_| {
            show_settings(false);
            stack.set_visible_child_name(&name);
            set_active(&name);
        });
    }
    // Settings nav toggles the modal instead of switching pages.
    {
        let set_active = set_active.clone();
        let show_settings = show_settings.clone();
        let panel = panel.clone();
        let stack = stack.clone();
        settings_btn.connect_clicked(move |_| {
            if panel.is_visible() {
                show_settings(false);
                let current = stack
                    .visible_child_name()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "home".to_string());
                set_active(&current);
            } else {
                show_settings(true);
                set_active("settings");
            }
        });
    }
    let initial = match initial_page {
        "insights" | "dictionary" => initial_page,
        _ => "home",
    };
    stack.set_visible_child_name(initial);
    if initial_page == "settings" {
        show_settings(true);
        set_active("settings");
    } else {
        set_active(initial);
    }

    // Escape dismisses the settings modal, restoring the sidebar
    // highlight to the page underneath.
    {
        let show_settings = show_settings.clone();
        let set_active = set_active.clone();
        let stack = stack.clone();
        let escape = gtk4::ShortcutController::new();
        let trigger = gtk4::ShortcutTrigger::parse_string("Escape")
            .expect("Escape parses as a shortcut trigger");
        let dismiss = gtk4::CallbackAction::new(move |_, _| {
            show_settings(false);
            let current = stack
                .visible_child_name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "home".to_string());
            set_active(&current);
            gtk4::glib::Propagation::Stop
        });
        escape.add_shortcut(gtk4::Shortcut::new(Some(trigger), Some(dismiss)));
        window.add_controller(escape);
    }

    window
}
