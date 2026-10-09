//! Desktop app shell: left sidebar + content stack.
//!
//! Pages live in `app_home`, `app_insights`, `app_dictionary` and
//! `app_settings`. All widgets initialize from [`AppState`] but nothing
//! is wired yet: changing a control does not persist or apply.

use crate::i18n::tr;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, Box as GtkBox, Button, Label, Orientation, Stack};
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Once;

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
        let words_spoken: u64 = history.iter().map(|e| e.word_count as u64).sum();
        let secs: u64 = history.iter().map(|e| e.duration_secs.max(0) as u64).sum();
        let stats = UsageStats {
            words_spoken,
            words_per_minute: if secs > 0 {
                words_spoken as f64 * 60.0 / secs as f64
            } else {
                0.0
            },
            dictations: history.len() as u64,
            streak_days: streak(&store.active_days().unwrap_or_default()),
        };
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

/// Consecutive active days ending today (or yesterday when today is
/// still quiet), from descending epoch-day numbers.
fn streak(days_desc: &[i64]) -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let today = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64 / 86_400)
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
const CSS: &str = r#"
@define-color app_bg #fafafa;
@define-color app_fg #1e1e1e;
@define-color app_card #ffffff;
@define-color app_borders #e0e0e0;
@define-color app_sidebar #f1f1f1;
@define-color app_accent #3584e4;
@define-color app_accent_fg #ffffff;
@define-color app_entry_bg #ffffff;
@define-color app_entry_border #d0d0d0;
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
    background-clip: padding-box;
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
.cap-ok {
    color: #2fa866;
    font-weight: 500;
}
.cap-no {
    color: #e05252;
    font-weight: 500;
}
.warn {
    color: #e05252;
    font-weight: 500;
}
entry, textview {
    background: @app_entry_bg;
    color: @app_fg;
    border: 1px solid @app_entry_border;
    border-radius: 8px;
    padding: 8px;
}
button {
    color: @app_fg;
}
button:hover {
    background: @app_borders;
}
/* The modal backdrop is a full-window button: it must never show a
   hover state, or the page behind the modal lights up. */
.backdrop:hover {
    background: rgba(0, 0, 0, 0.45);
}
/* Dropdown/combobox popup rows are list rows, not buttons. (GTK4
   DropDown popups are ListViews.) */
popover listview row:hover,
popover listbox row:hover {
    background: @app_borders;
    border-radius: 6px;
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
    background-clip: padding-box;
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
@define-color app_entry_bg #222222;
@define-color app_entry_border #4a4a4a;
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
/// windows (title included). An `AdwClamp` caps the width but shrinks
/// on narrow windows instead of forcing a phantom horizontal
/// scrollbar. Returns the inner column to fill.
pub fn centered_column(outer: &GtkBox, width: i32) -> GtkBox {
    let clamp = libadwaita::Clamp::new();
    clamp.set_maximum_size(width);
    clamp.set_hexpand(true);
    outer.append(&clamp);
    let col = GtkBox::new(Orientation::Vertical, 16);
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

    let title = Label::new(Some(tr("app.name")));
    title.add_css_class("app-title");
    title.set_xalign(0.0);
    sidebar.append(&title);

    let home_btn = nav_button(tr("nav.home"));
    let insights_btn = nav_button(tr("nav.insights"));
    let dict_btn = nav_button(tr("nav.dictionary"));
    let sync_btn = nav_button(tr("nav.sync"));
    sidebar.append(&home_btn);
    sidebar.append(&insights_btn);
    sidebar.append(&dict_btn);

    let spacer = GtkBox::new(Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    sidebar.append(&spacer);

    sidebar.append(&sync_btn);

    let settings_btn = nav_button(tr("nav.settings"));
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
    let listening_port = state.sync_port.get();

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

    // Floating panels: header (title + close) over each modal's
    // content. Settings and Sync share one backdrop; showing one
    // modal always hides the other.
    fn modal_panel(overlay: &gtk4::Overlay, title: &str) -> (GtkBox, Button) {
        let panel = GtkBox::new(Orientation::Vertical, 0);
        panel.add_css_class("modal");
        panel.set_halign(gtk4::Align::Center);
        panel.set_valign(gtk4::Align::Center);
        panel.set_size_request(900, 620);
        panel.set_visible(false);
        let header = GtkBox::new(Orientation::Horizontal, 8);
        header.add_css_class("modal-header");
        let header_title = Label::new(Some(title));
        header_title.add_css_class("modal-title");
        header_title.set_xalign(0.0);
        header_title.set_hexpand(true);
        let close = Button::with_label(crate::i18n::tr("action.close"));
        header.append(&header_title);
        header.append(&close);
        panel.append(&header);
        overlay.add_overlay(&panel);
        (panel, close)
    }

    let (settings_panel, settings_close) = modal_panel(&overlay, crate::i18n::tr("nav.settings"));
    let settings_content = crate::app_settings::page(state, initial_section);
    settings_content.set_vexpand(true);
    // Inset from the panel edges: full-bleed content would paint over
    // the panel's rounded corners.
    settings_content.set_margin_start(12);
    settings_content.set_margin_end(12);
    settings_content.set_margin_bottom(12);
    settings_panel.append(&settings_content);

    let (sync_panel, sync_close) = modal_panel(&overlay, crate::i18n::tr("nav.sync"));
    let sync_content = crate::app_sync::page(state, listening_port);
    sync_content.set_vexpand(true);
    sync_content.set_margin_start(12);
    sync_content.set_margin_end(12);
    sync_content.set_margin_bottom(12);
    sync_panel.append(&sync_content);

    /// Show one modal (hiding the other), or hide both with `None`.
    /// Backdrop follows any visible panel.
    fn show_modal(
        which: Option<&str>,
        backdrop: &Button,
        settings_panel: &GtkBox,
        sync_panel: &GtkBox,
    ) {
        settings_panel.set_visible(which == Some("settings"));
        sync_panel.set_visible(which == Some("sync"));
        backdrop.set_visible(which.is_some());
    }

    {
        let backdrop = backdrop.clone();
        let settings_panel = settings_panel.clone();
        let sync_panel = sync_panel.clone();
        let backdrop_conn = backdrop.clone();
        backdrop_conn
            .connect_clicked(move |_| show_modal(None, &backdrop, &settings_panel, &sync_panel));
    }
    {
        let backdrop = backdrop.clone();
        let settings_panel = settings_panel.clone();
        let sync_panel = sync_panel.clone();
        let settings_close_conn = settings_close.clone();
        settings_close_conn
            .connect_clicked(move |_| show_modal(None, &backdrop, &settings_panel, &sync_panel));
    }
    {
        let backdrop = backdrop.clone();
        let settings_panel = settings_panel.clone();
        let sync_panel = sync_panel.clone();
        let sync_close_conn = sync_close.clone();
        sync_close_conn
            .connect_clicked(move |_| show_modal(None, &backdrop, &settings_panel, &sync_panel));
    }

    let buttons = [
        home_btn.clone(),
        insights_btn.clone(),
        dict_btn.clone(),
        sync_btn.clone(),
        settings_btn.clone(),
    ];
    let names = ["home", "insights", "dictionary", "sync", "settings"];
    let set_active = Rc::new(move |active: &str| {
        for (b, n) in buttons.iter().zip(names.iter()) {
            if *n == active {
                b.add_css_class("active");
            } else {
                b.remove_css_class("active");
            }
        }
    });

    // Main nav switches pages and dismisses any modal.
    for (btn, name) in
        [home_btn, insights_btn, dict_btn]
            .into_iter()
            .zip(["home", "insights", "dictionary"])
    {
        let stack = stack.clone();
        let set_active = set_active.clone();
        let backdrop = backdrop.clone();
        let settings_panel = settings_panel.clone();
        let sync_panel = sync_panel.clone();
        let name = name.to_string();
        btn.connect_clicked(move |_| {
            show_modal(None, &backdrop, &settings_panel, &sync_panel);
            stack.set_visible_child_name(&name);
            set_active(&name);
        });
    }
    // Bottom nav toggles its modal instead of switching pages. The
    // sidebar highlight always stays on the page underneath (modals
    // are overlays, not places).
    for (btn, name, panel) in [
        (sync_btn, "sync", sync_panel.clone()),
        (settings_btn, "settings", settings_panel.clone()),
    ] {
        let backdrop = backdrop.clone();
        let settings_panel = settings_panel.clone();
        let sync_panel = sync_panel.clone();
        let name = name.to_string();
        btn.connect_clicked(move |_| {
            show_modal(
                if panel.is_visible() {
                    None
                } else {
                    Some(&name)
                },
                &backdrop,
                &settings_panel,
                &sync_panel,
            );
        });
    }
    let initial = match initial_page {
        "insights" | "dictionary" => initial_page,
        _ => "home",
    };
    stack.set_visible_child_name(initial);
    if initial_page == "settings" {
        show_modal(Some("settings"), &backdrop, &settings_panel, &sync_panel);
    } else if initial_page == "sync" {
        show_modal(Some("sync"), &backdrop, &settings_panel, &sync_panel);
    }
    set_active(initial);

    // Escape dismisses any modal. The highlight already sits on
    // the page underneath, so nothing else changes.
    {
        let backdrop = backdrop.clone();
        let settings_panel = settings_panel.clone();
        let sync_panel = sync_panel.clone();
        let escape = gtk4::ShortcutController::new();
        let trigger = gtk4::ShortcutTrigger::parse_string("Escape")
            .expect("Escape parses as a shortcut trigger");
        let dismiss = gtk4::CallbackAction::new(move |_, _| {
            show_modal(None, &backdrop, &settings_panel, &sync_panel);
            gtk4::glib::Propagation::Stop
        });
        escape.add_shortcut(gtk4::Shortcut::new(Some(trigger), Some(dismiss)));
        window.add_controller(escape);
    }

    // Language changes rebuild the whole window (labels are baked at
    // build time); modal state is not preserved across the rebuild.
    {
        let app = app.clone();
        let window = window.clone();
        let state = state.clone();
        crate::i18n::set_rebuild_hook(Rc::new(move || {
            let fresh = build_app_window(&app, &state, "home", "Preferences");
            fresh.present();
            window.close();
        }));
    }

    window
}
