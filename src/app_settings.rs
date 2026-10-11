//! Settings page: sub-nav + sections.
//!
//! Controls persist immediately; advanced details expand on demand.

use gtk4::prelude::*;
use gtk4::{
    Box as GtkBox, Button, DropDown, Entry, EventControllerKey, Label, Orientation, SpinButton,
    Stack, StringList, Switch, TextView,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::app::{AppState, apply_theme, log_theme_state};
use crate::clipboard::{BackendStatus, copy_backend_deps, paste_backend_deps, probe};
use crate::i18n::{tr, trf};
use crate::settings::{CopyBackend, PasteBackend, PasteCombo, Theme};

// Signal handlers keep weak widget references. A form is upgraded only for
// one callback, so removing its page releases fields and their sensitive text.
macro_rules! weak_form {
    ($strong:ident, $weak:ident, { $($field:ident: $kind:ty),* $(,)? }, { $($state:ident: $state_kind:ty),* $(,)? }) => {
        #[derive(Clone)]
        struct $weak {
            $($field: gtk4::glib::WeakRef<$kind>,)*
            $($state: $state_kind,)*
        }
        impl $strong {
            fn downgrade(&self) -> $weak {
                $weak {
                    $($field: self.$field.downgrade(),)*
                    $($state: self.$state.clone(),)*
                }
            }
        }
        impl $weak {
            fn upgrade(&self) -> Option<$strong> {
                Some($strong {
                    $($field: self.$field.upgrade()?,)*
                    $($state: self.$state.clone(),)*
                })
            }
        }
    };
}

// ---------------------------------------------------------------- helpers

/// Poll a bounded worker result without retaining a removed widget or blocking GTK.
fn file_action(
    button: &Button,
    work: impl FnOnce() -> std::io::Result<()> + Send + 'static,
    success: &'static str,
) {
    button.set_sensitive(false);
    let (tx, rx) = std::sync::mpsc::channel();
    if let Err(error) = crate::runtime::submit("file-action", move || {
        let result = work().map_err(|error| error.to_string());
        let _ = tx.send(result);
    }) {
        button.set_sensitive(true);
        eprintln!("[app] file action admission: {error}");
        super::app::notify(tr("feedback.deletefailed"));
        return;
    }
    let target = button.downgrade();
    gtk4::glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        use std::sync::mpsc::TryRecvError;
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return gtk4::glib::ControlFlow::Continue,
            Err(TryRecvError::Disconnected) => Err("file action failed unexpectedly".into()),
        };
        if let Some(button) = target.upgrade() {
            button.set_sensitive(true);
        }
        match result {
            Ok(()) => super::app::notify(tr(success)),
            Err(error) => {
                eprintln!("[app] file action: {error}");
                super::app::notify(tr("feedback.deletefailed"));
            }
        }
        gtk4::glib::ControlFlow::Break
    });
}

fn connect_download_delete(
    button: &Button,
    remove: impl Fn() -> std::io::Result<()> + Send + Sync + 'static,
) {
    let remove = std::sync::Arc::new(remove);
    super::app::connect_confirmed(
        button,
        tr("confirm.download.title"),
        tr("confirm.download.body"),
        tr("action.delete"),
        move |button| {
            let remove = remove.clone();
            file_action(button, move || remove(), "feedback.deleted");
        },
    );
}

fn section_box(title: &str, subtitle: &str) -> GtkBox {
    let root = GtkBox::new(Orientation::Vertical, 12);
    root.add_css_class("page");
    super::app::page_heading(&root, title, subtitle);
    root
}

fn group(title: &str) -> (GtkBox, GtkBox) {
    let card = GtkBox::new(Orientation::Vertical, 4);
    card.add_css_class("card");
    card.add_css_class("settings-group");
    let t = Label::new(Some(title));
    t.add_css_class("section-title");
    t.set_xalign(0.0);
    card.append(&t);
    let rows = GtkBox::new(Orientation::Vertical, 14);
    card.append(&rows);
    (card, rows)
}

fn append_collapsible(root: &GtkBox, card: &GtkBox, title: &str) -> gtk4::Expander {
    if let Some(heading) = card.first_child() {
        card.remove(&heading);
    }
    card.remove_css_class("card");
    card.remove_css_class("settings-group");
    card.add_css_class("expanded-content");
    let expander = gtk4::Expander::new(Some(title));
    expander.add_css_class("settings-expander");
    expander.set_child(Some(card));
    root.append(&expander);
    expander
}

fn row(label: &str) -> (GtkBox, Label) {
    let b = GtkBox::new(Orientation::Horizontal, 12);
    b.add_css_class("setting-row");
    let l = Label::new(Some(label));
    l.set_wrap(true);
    l.set_max_width_chars(28);
    l.add_css_class("row-label");
    l.set_xalign(0.0);
    l.set_hexpand(true);
    b.append(&l);
    let accessible_label = l.clone();
    b.connect_map(move |row| {
        let mut child = row.first_child();
        while let Some(control) = child {
            if control.is::<Entry>()
                || control.is::<DropDown>()
                || control.is::<Switch>()
                || control.is::<SpinButton>()
            {
                control.update_property(&[gtk4::accessible::Property::Label(
                    &accessible_label.text(),
                )]);
                if let Some(entry) = control.downcast_ref::<Entry>() {
                    entry.set_width_chars(12);
                }
            }
            child = control.next_sibling();
        }
    });
    (b, l)
}

fn spin_row(parent: &GtkBox, label: &str, value: f64, min: f64, max: f64, step: f64) {
    let (b, _) = row(label);
    let s = SpinButton::with_range(min, max, step);
    s.set_value(value);
    s.set_sensitive(false); // Enabled by the section that attaches persistence.
    spin_text_buttons(&s);
    b.append(&s);
    parent.append(&b);
}

/// GtkSpinButton arrows are icon-theme images and can render blank
/// when the active theme lacks them. Replace them with text labels:
/// first button is decrement, second is increment (GTK lays them out
/// − then +). `flat` keeps the native joined spinbutton look.
fn spin_text_buttons(spin: &SpinButton) {
    let mut first = true;
    let mut child = spin.first_child();
    while let Some(c) = child {
        child = c.next_sibling();
        if let Ok(btn) = c.downcast::<Button>() {
            // Single child is the icon image; drop it, keep text.
            btn.set_child(None::<&gtk4::Widget>);
            btn.set_label(if first { "−" } else { "+" });
            btn.add_css_class("flat");
            first = false;
        }
    }
}

/// Index of `value` in `options`, clamped to 0.
fn idx_of(options: &[&str], value: &str) -> usize {
    options.iter().position(|o| *o == value).unwrap_or(0)
}

fn connection_id(prefix: &str) -> std::io::Result<String> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(std::io::Error::other)?;
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!("{prefix}-{suffix}"))
}

fn valid_server_url(value: &str, websocket: bool) -> bool {
    reqwest::Url::parse(value.trim()).is_ok_and(|url| {
        url.host_str().is_some()
            && (matches!(url.scheme(), "http" | "https")
                || (websocket && matches!(url.scheme(), "ws" | "wss")))
    })
}

fn same_shortcut(left: &str, right: &str) -> bool {
    crate::hotkey::parse_combo(left)
        .zip(crate::hotkey::parse_combo(right))
        .is_some_and(|(a, b)| {
            (a.ctrl, a.alt, a.shift, a.super_, a.target)
                == (b.ctrl, b.alt, b.shift, b.super_, b.target)
        })
}

// ---------------------------------------------------------------- sections

/// Save settings without touching the hotkey listener (pill and
/// preference changes propagate to the service through the DB).
pub(crate) fn persist_prefs(state: Rc<AppState>) -> Rc<dyn Fn()> {
    Rc::new(move || {
        let settings = state.settings.borrow();
        if let Err(e) = state.store.save_settings(&settings) {
            eprintln!("[app] prefs save failed: {e}");
            super::app::notify(tr("feedback.savefailed"));
        }
    })
}

fn preferences_page(state: &Rc<AppState>, on_theme: &Rc<dyn Fn(Theme)>) -> GtkBox {
    let root = section_box(tr("settings.preferences"), tr("settings.preferences.sub"));
    let p = state.settings.borrow().preferences.clone();

    let (card, rows) = group(tr("group.appearance"));
    // The theme picker is wired: it writes the model and applies the
    // variant immediately. Other preferences persist through the same store.
    let (b, _) = row(tr("row.theme"));
    let theme_drop = DropDown::new(
        Some(StringList::new(&[
            tr("opt.system"),
            tr("opt.light"),
            tr("opt.dark"),
        ])),
        None::<gtk4::Expression>,
    );
    theme_drop.set_widget_name("theme-picker");
    theme_drop.set_selected(match p.theme {
        Theme::System => 0,
        Theme::Light => 1,
        Theme::Dark => 2,
    });
    {
        let on_theme = on_theme.clone();
        theme_drop.connect_selected_notify(move |d| {
            let theme = match d.selected() {
                1 => Theme::Light,
                2 => Theme::Dark,
                _ => Theme::System,
            };
            on_theme(theme);
        });
    }
    b.append(&theme_drop);
    rows.append(&b);
    root.append(&card);

    let (card, rows) = group(tr("group.sound"));
    {
        let (b, _) = row(tr("row.dictsounds"));
        let sw = Switch::new();
        sw.set_active(p.dictation_sounds);
        sw.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            sw.connect_state_set(move |_, active| {
                state.settings.borrow_mut().preferences.dictation_sounds = active;
                persist_prefs(state.clone())();
                gtk4::glib::Propagation::Proceed
            });
        }
        b.append(&sw);
        rows.append(&b);
    }
    // Wired: the service watcher picks the preference up live.
    {
        let (b, _) = row(tr("row.pausemedia"));
        let sw = Switch::new();
        sw.set_active(p.pause_media_on_dictation);
        sw.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            sw.connect_state_set(move |_, active| {
                state
                    .settings
                    .borrow_mut()
                    .preferences
                    .pause_media_on_dictation = active;
                persist_prefs(state.clone())();
                gtk4::glib::Propagation::Proceed
            });
        }
        b.append(&sw);
        rows.append(&b);
    }
    root.append(&card);

    let (card, rows) = group(tr("group.language"));
    {
        let (b, _) = row(tr("row.uilang"));
        let lang_drop = DropDown::new(
            Some(StringList::new(&[tr("opt.system"), "English", "Español"])),
            None::<gtk4::Expression>,
        );
        lang_drop.set_widget_name("language-picker");
        lang_drop.set_selected(match p.ui_language.as_str() {
            "en" => 1,
            "es" => 2,
            _ => 0,
        });
        {
            let state = state.clone();
            lang_drop.connect_selected_notify(move |d| {
                let code = match d.selected() {
                    1 => "en",
                    2 => "es",
                    _ => "system",
                };
                state.settings.borrow_mut().preferences.ui_language = code.to_string();
                persist_prefs(state.clone())();
                crate::i18n::set_lang(crate::i18n::UiLang::parse(code));
                crate::i18n::rebuild_ui();
            });
        }
        b.append(&lang_drop);
        rows.append(&b);
    }
    root.append(&card);

    let (card, rows) = group(tr("group.microphone"));
    // One picker: System default / Built-in / each live source by
    // its server description (Bluetooth included; ALSA plugins and
    // monitors excluded). Picking a device stores Specific + its
    // server name. A stored-but-unlisted device (unplugged) still
    // shows as the current choice.
    {
        let sources = crate::mic::input_sources();
        let mut options: Vec<String> = vec![
            tr("opt.sysdefault").to_string(),
            tr("opt.builtin").to_string(),
        ];
        options.extend(sources.iter().map(|d| d.description.clone()));
        if !matches!(
            p.mic_selection,
            crate::settings::MicSelection::System | crate::settings::MicSelection::BuiltIn
        ) && !sources.iter().any(|d| d.name == p.mic_device_id)
        {
            options.push(tr("row.micunlisted").to_string() + &p.mic_device_id);
        }
        let (b, _) = row(tr("row.micinput"));
        let input_drop = DropDown::new(
            Some(StringList::new(
                &options.iter().map(String::as_str).collect::<Vec<_>>(),
            )),
            None::<gtk4::Expression>,
        );
        input_drop.set_selected(match p.mic_selection {
            crate::settings::MicSelection::System => 0,
            crate::settings::MicSelection::BuiltIn => 1,
            crate::settings::MicSelection::Specific => sources
                .iter()
                .position(|d| d.name == p.mic_device_id)
                .map(|i| (i + 2) as u32)
                .unwrap_or(options.len().saturating_sub(1) as u32),
        });
        {
            let state = state.clone();
            input_drop.connect_selected_notify(move |d| {
                {
                    let mut prefs = state.settings.borrow_mut();
                    match d.selected() {
                        0 => {
                            prefs.preferences.mic_selection = crate::settings::MicSelection::System;
                        }
                        1 => {
                            prefs.preferences.mic_selection =
                                crate::settings::MicSelection::BuiltIn;
                        }
                        n => {
                            prefs.preferences.mic_selection =
                                crate::settings::MicSelection::Specific;
                            if let Some(src) = sources.get((n as usize).saturating_sub(2)) {
                                prefs.preferences.mic_device_id = src.name.clone();
                            }
                        }
                    }
                }
                persist_prefs(state.clone())();
            });
        }
        b.append(&input_drop);
        rows.append(&b);
    }
    // Warm hold is already wired (persisted; the mic honors it on
    // every edge). Display-only combo becomes a live picker here.
    {
        let hold_opts = [
            tr("opt.off"),
            tr("opt.hold10s"),
            tr("opt.hold60s"),
            tr("opt.hold15m"),
        ];
        let (b, _) = row(tr("row.micwarm"));
        let warm_drop = DropDown::new(Some(StringList::new(&hold_opts)), None::<gtk4::Expression>);
        warm_drop.set_selected(match p.mic_warm_hold_secs {
            10 => 1,
            60 => 2,
            900 => 3,
            _ => 0,
        });
        {
            let state = state.clone();
            warm_drop.connect_selected_notify(move |d| {
                state.settings.borrow_mut().preferences.mic_warm_hold_secs = match d.selected() {
                    1 => 10,
                    2 => 60,
                    3 => 900,
                    _ => 0,
                };
                persist_prefs(state.clone())();
            });
        }
        b.append(&warm_drop);
        rows.append(&b);
        let delay_opts = [
            tr("opt.off"),
            tr("opt.ms100"),
            tr("opt.ms250"),
            tr("opt.ms500"),
            tr("opt.ms1000"),
        ];
        let (b, _) = row(tr("row.micextra"));
        let delay_drop =
            DropDown::new(Some(StringList::new(&delay_opts)), None::<gtk4::Expression>);
        delay_drop.set_selected(match p.mic_release_delay_ms {
            100 => 1,
            250 => 2,
            500 => 3,
            1000 => 4,
            _ => 0,
        });
        {
            let state = state.clone();
            delay_drop.connect_selected_notify(move |d| {
                state.settings.borrow_mut().preferences.mic_release_delay_ms = match d.selected() {
                    1 => 100,
                    2 => 250,
                    3 => 500,
                    4 => 1000,
                    _ => 0,
                };
                persist_prefs(state.clone())();
            });
        }
        b.append(&delay_drop);
        rows.append(&b);
    }
    root.append(&card);

    let (card, rows) = group(tr("group.floating"));
    {
        let (b, _) = row(tr("row.startpos"));
        let pos = DropDown::new(
            Some(StringList::new(&[
                tr("opt.bottomcenter"),
                tr("opt.bottomleft"),
                tr("opt.bottomright"),
            ])),
            None::<gtk4::Expression>,
        );
        pos.set_selected(match p.pill.position {
            crate::settings::FloatingPosition::BottomCenter => 0,
            crate::settings::FloatingPosition::BottomLeft => 1,
            crate::settings::FloatingPosition::BottomRight => 2,
        });
        {
            let state = state.clone();
            let persist = persist_prefs(state.clone());
            pos.connect_selected_notify(move |d| {
                state.settings.borrow_mut().preferences.pill.position = match d.selected() {
                    1 => crate::settings::FloatingPosition::BottomLeft,
                    2 => crate::settings::FloatingPosition::BottomRight,
                    _ => crate::settings::FloatingPosition::BottomCenter,
                };
                persist();
            });
        }
        b.append(&pos);
        rows.append(&b);
    }

    {
        let (b, _) = row(tr("row.animation"));
        let mode = DropDown::new(
            Some(StringList::new(&[tr("opt.tape"), tr("opt.history")])),
            None::<gtk4::Expression>,
        );
        mode.set_selected(match p.pill.viz_mode {
            crate::viz::VizMode::Tape => 0,
            crate::viz::VizMode::History => 1,
        });
        {
            let state = state.clone();
            let persist = persist_prefs(state.clone());
            mode.connect_selected_notify(move |d| {
                state.settings.borrow_mut().preferences.pill.viz_mode = match d.selected() {
                    1 => crate::viz::VizMode::History,
                    _ => crate::viz::VizMode::Tape,
                };
                persist();
            });
        }
        b.append(&mode);
        rows.append(&b);
    }
    {
        let (b, _) = row(tr("row.bars"));
        let bars = DropDown::new(
            Some(StringList::new(&["7", "11", "15", "21"])),
            None::<gtk4::Expression>,
        );
        bars.set_selected(match p.pill.bars {
            7 => 0,
            15 => 2,
            21 => 3,
            _ => 1,
        });
        {
            let state = state.clone();
            let persist = persist_prefs(state.clone());
            bars.connect_selected_notify(move |d| {
                state.settings.borrow_mut().preferences.pill.bars = match d.selected() {
                    0 => 7,
                    2 => 15,
                    3 => 21,
                    _ => 11,
                };
                persist();
            });
        }
        b.append(&bars);
        rows.append(&b);
    }
    {
        let (b, _) = row(tr("row.height"));
        let heights = ["12", "14", "16", "18", "20", "24", "28", "32"];
        let height = DropDown::new(Some(StringList::new(&heights)), None::<gtk4::Expression>);
        let current = p.pill.lane_max as i32;
        height.set_selected(
            heights
                .iter()
                .position(|h| h.parse::<i32>().ok() == Some(current))
                .unwrap_or(3) as u32,
        );
        {
            let state = state.clone();
            let persist = persist_prefs(state.clone());
            height.connect_selected_notify(move |d| {
                let v: f64 = ["12", "14", "16", "18", "20", "24", "28", "32"]
                    [d.selected().min(7) as usize]
                    .parse()
                    .unwrap_or(18.0);
                state.settings.borrow_mut().preferences.pill.lane_max = v;
                persist();
            });
        }
        b.append(&height);
        rows.append(&b);
    }
    {
        let (b, _) = row(tr("row.theme"));
        let theme = DropDown::new(
            Some(StringList::new(&[
                tr("opt.white"),
                tr("opt.grey"),
                tr("opt.red"),
            ])),
            None::<gtk4::Expression>,
        );
        theme.set_selected(match p.pill.theme {
            crate::viz::VizTheme::White => 0,
            crate::viz::VizTheme::Grey => 1,
            crate::viz::VizTheme::Red => 2,
        });
        {
            let state = state.clone();
            let persist = persist_prefs(state.clone());
            theme.connect_selected_notify(move |d| {
                state.settings.borrow_mut().preferences.pill.theme = match d.selected() {
                    1 => crate::viz::VizTheme::Grey,
                    2 => crate::viz::VizTheme::Red,
                    _ => crate::viz::VizTheme::White,
                };
                persist();
            });
        }
        b.append(&theme);
        rows.append(&b);
    }
    {
        let (b, _) = row(tr("row.ornament"));
        let orn = DropDown::new(
            Some(StringList::new(&[
                tr("opt.none"),
                tr("opt.mark"),
                tr("opt.mic"),
                tr("opt.dot"),
            ])),
            None::<gtk4::Expression>,
        );
        orn.set_selected(match p.pill.ornament {
            crate::viz::Ornament::None => 0,
            crate::viz::Ornament::Mark => 1,
            crate::viz::Ornament::Mic => 2,
            crate::viz::Ornament::Dot => 3,
        });
        {
            let state = state.clone();
            let persist = persist_prefs(state.clone());
            orn.connect_selected_notify(move |d| {
                state.settings.borrow_mut().preferences.pill.ornament = match d.selected() {
                    1 => crate::viz::Ornament::Mark,
                    2 => crate::viz::Ornament::Mic,
                    3 => crate::viz::Ornament::Dot,
                    _ => crate::viz::Ornament::None,
                };
                persist();
            });
        }
        b.append(&orn);
        rows.append(&b);
    }
    {
        let (b, _) = row(tr("row.idle"));
        let idle = DropDown::new(
            Some(StringList::new(&[
                tr("opt.still"),
                tr("opt.silhouette"),
                tr("opt.calm"),
            ])),
            None::<gtk4::Expression>,
        );
        idle.set_selected(match p.pill.idle {
            crate::viz::IdleStyle::Still => 0,
            crate::viz::IdleStyle::Silhouette => 1,
            crate::viz::IdleStyle::Calm => 2,
        });
        {
            let state = state.clone();
            let persist = persist_prefs(state.clone());
            idle.connect_selected_notify(move |d| {
                state.settings.borrow_mut().preferences.pill.idle = match d.selected() {
                    1 => crate::viz::IdleStyle::Silhouette,
                    2 => crate::viz::IdleStyle::Calm,
                    _ => crate::viz::IdleStyle::Still,
                };
                persist();
            });
        }
        b.append(&idle);
        rows.append(&b);
    }
    append_collapsible(&root, &card, tr("group.floating"));

    root
}

/// Clipboard section (moved out of Preferences): auto-paste
/// master switch + whether the transcription stays in the clipboard.
/// Backend selection (which paste method) comes next; the toggles
/// below are already model + persistence wired via persist_prefs.
fn clipboard_page(state: &Rc<AppState>) -> GtkBox {
    let root = section_box(tr("settings.clipboard"), tr("settings.clipboard.sub"));
    let p = state.settings.borrow().preferences.clone();
    let status: BackendStatus = probe();

    // Visible dependency + availability note under a backend row.
    fn dep_note(text: &str) -> Label {
        let l = Label::new(Some(text));
        l.add_css_class("page-subtitle");
        l.set_xalign(0.0);
        l.set_wrap(true);
        l.set_max_width_chars(80);
        l
    }
    fn avail(ok: bool) -> &'static str {
        if ok {
            tr("status.available")
        } else {
            tr("status.missing")
        }
    }
    fn paste_note_text(backend: PasteBackend, status: &BackendStatus) -> String {
        match backend {
            PasteBackend::Wtype => {
                format!("{} — {}", paste_backend_deps(backend), avail(status.wtype))
            }
            PasteBackend::Ydotool => format!(
                "{} — {}, ydotoold: {}",
                paste_backend_deps(backend),
                avail(status.ydotool),
                avail(status.ydotoold_socket)
            ),
        }
    }

    let (card, rows) = group(tr("group.clipboard"));
    {
        let (b, _) = row(tr("row.autopaste"));
        let sw = Switch::new();
        sw.set_active(p.auto_paste);
        sw.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            sw.connect_state_set(move |_, active| {
                state.settings.borrow_mut().preferences.auto_paste = active;
                persist_prefs(state.clone())();
                gtk4::glib::Propagation::Proceed
            });
        }
        b.append(&sw);
        rows.append(&b);
    }
    {
        let (b, _) = row(tr("row.keepclip"));
        let sw = Switch::new();
        sw.set_active(p.keep_transcription_in_clipboard);
        sw.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            sw.connect_state_set(move |_, active| {
                state
                    .settings
                    .borrow_mut()
                    .preferences
                    .keep_transcription_in_clipboard = active;
                persist_prefs(state.clone())();
                gtk4::glib::Propagation::Proceed
            });
        }
        b.append(&sw);
        rows.append(&b);
    }
    {
        let (b, _) = row(tr("row.copybackend"));
        let dd = DropDown::new(
            Some(StringList::new(&["Wayland"])),
            None::<gtk4::Expression>,
        );
        dd.set_selected(match p.copy_backend {
            CopyBackend::WlCopy => 0,
        });
        dd.set_valign(gtk4::Align::Center);
        dd.set_tooltip_text(Some(copy_backend_deps()));
        {
            let state = state.clone();
            dd.connect_selected_notify(move |dd| {
                let _ = dd.selected();
                state.settings.borrow_mut().preferences.copy_backend = CopyBackend::WlCopy;
                persist_prefs(state.clone())();
            });
        }
        b.append(&dd);
        rows.append(&b);
        rows.append(&dep_note(&format!(
            "{} — {}",
            copy_backend_deps(),
            avail(status.wayland)
        )));
    }
    {
        let (b, _) = row(tr("row.hidehistory"));
        let sw = Switch::new();
        sw.set_active(p.hide_from_history);
        sw.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            sw.connect_state_set(move |_, active| {
                state.settings.borrow_mut().preferences.hide_from_history = active;
                persist_prefs(state.clone())();
                gtk4::glib::Propagation::Proceed
            });
        }
        b.append(&sw);
        rows.append(&b);
    }
    root.append(&card);

    let (card, rows) = group(tr("group.paste"));
    {
        let (b, _) = row(tr("row.pastebackend"));
        let dd = DropDown::new(
            Some(StringList::new(&["wtype", "ydotool"])),
            None::<gtk4::Expression>,
        );
        dd.set_selected(match p.paste_backend {
            PasteBackend::Wtype => 0,
            PasteBackend::Ydotool => 1,
        });
        dd.set_valign(gtk4::Align::Center);
        dd.set_tooltip_text(Some(paste_backend_deps(p.paste_backend)));
        let paste_note = dep_note(&paste_note_text(p.paste_backend, &status));
        rows.append(&paste_note);
        {
            let state = state.clone();
            dd.connect_selected_notify(move |dd| {
                let backend = match dd.selected() {
                    1 => PasteBackend::Ydotool,
                    _ => PasteBackend::Wtype,
                };
                state.settings.borrow_mut().preferences.paste_backend = backend;
                dd.set_tooltip_text(Some(paste_backend_deps(backend)));
                paste_note.set_text(&paste_note_text(backend, &status));
                persist_prefs(state.clone())();
            });
        }
        b.append(&dd);
        rows.append(&b);
    }
    {
        let (b, _) = row(tr("row.pastecombo"));
        let dd = DropDown::new(
            Some(StringList::new(&["Ctrl+Shift+V", "Ctrl+V", "Shift+Insert"])),
            None::<gtk4::Expression>,
        );
        dd.set_selected(match p.paste_combo {
            PasteCombo::CtrlShiftV => 0,
            PasteCombo::CtrlV => 1,
            PasteCombo::ShiftInsert => 2,
        });
        dd.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            dd.connect_selected_notify(move |dd| {
                state.settings.borrow_mut().preferences.paste_combo = match dd.selected() {
                    1 => PasteCombo::CtrlV,
                    2 => PasteCombo::ShiftInsert,
                    _ => PasteCombo::CtrlShiftV,
                };
                persist_prefs(state.clone())();
            });
        }
        b.append(&dd);
        rows.append(&b);
    }
    root.append(&card);

    root
}

/// Hotkeys section.
fn hotkeys_page(state: &Rc<AppState>) -> GtkBox {
    let root = section_box(tr("settings.hotkeys"), tr("settings.hotkeys.sub"));
    root.set_widget_name("hotkey-capture-page");
    let persist: Rc<dyn Fn()> = {
        let state = state.clone();
        Rc::new(move || {
            let settings = state.settings.borrow();
            if let Err(e) = state.store.save_settings(&settings) {
                eprintln!("[app] hotkey save failed: {e}");
            }
            crate::hotkey::apply_from_settings(&state);
        })
    };

    let (card, rows) = group(tr("group.dictation"));
    // One row per binding, each with its own Remove. Refill is a
    // plain fn (list + state passed in) so Remove closures recurse
    // without self-referential Rc tricks.
    let list = GtkBox::new(Orientation::Vertical, 4);
    rows.append(&list);
    refill_hotkey_list(&list, state, &persist);

    // Add row: text field + per-binding mode + Add/Record. Validated
    // by the same parser the listener uses; invalid combos are
    // refused with a hint.
    let (add_heading, _) = row(tr("row.addcombo"));
    let add_row = GtkBox::new(Orientation::Horizontal, 8);
    let add_entry = Entry::new();
    add_entry.set_widget_name("hotkey-capture-entry");
    add_entry.set_hexpand(true);
    add_entry.set_width_chars(12);
    add_entry.update_property(&[gtk4::accessible::Property::Label(tr("row.addcombo"))]);
    add_entry.set_placeholder_text(Some("Ctrl+Alt+D"));
    add_entry.set_tooltip_text(Some(tr("hotkeys.physical")));
    add_entry.connect_changed(|entry| entry.remove_css_class("error"));
    add_row.append(&add_entry);
    let add_mode = DropDown::new(
        Some(StringList::new(&[
            tr("opt.hold"),
            tr("opt.toggle"),
            tr("opt.smart"),
        ])),
        None::<gtk4::Expression>,
    );
    add_mode.set_tooltip_text(Some(tr("hotkeys.behavior")));
    add_row.append(&add_mode);
    let add_btn = Button::with_label(tr("action.add"));
    {
        let state = state.clone();
        let list = list.downgrade();
        let persist = persist.clone();
        let add_entry = add_entry.clone();
        let add_mode = add_mode.clone();
        add_btn.connect_clicked(move |_| {
            let Some(list) = list.upgrade() else {
                return;
            };
            let raw = add_entry.text().trim().to_string();
            if crate::hotkey::parse_combo(&raw).is_none() {
                add_entry.add_css_class("error");
                super::app::toast(&add_entry, tr("hotkeys.invalidcombo"));
                add_entry.grab_focus();
                return;
            }
            if state
                .settings
                .borrow()
                .hotkeys
                .effective_hotkeys()
                .iter()
                .any(|binding| same_shortcut(&binding.combo, &raw))
            {
                add_entry.add_css_class("error");
                super::app::toast(&add_entry, tr("hotkeys.duplicate"));
                add_entry.grab_focus();
                return;
            }
            let mode = match add_mode.selected() {
                1 => crate::settings::ActivationMode::Tap,
                2 => crate::settings::ActivationMode::Smart,
                _ => crate::settings::ActivationMode::Push,
            };
            {
                let mut s = state.settings.borrow_mut();
                s.hotkeys
                    .dictation_hotkeys
                    .push(crate::settings::HotkeyBinding { combo: raw, mode });
            }
            add_entry.set_text("");
            persist();
            refill_hotkey_list(&list, &state, &persist);
        });
    }
    add_row.append(&add_btn);
    let record_btn = Button::with_label(tr("hotkeys.record"));
    record_btn.set_widget_name("hotkey-capture-start");
    record_btn.set_tooltip_text(Some(tr("hotkeys.recordhint")));
    add_heading.append(&record_btn);
    let add_form = GtkBox::new(Orientation::Vertical, 6);
    add_form.append(&add_heading);
    add_form.append(&add_row);
    rows.append(&add_form);

    let (b, _) = row(tr("row.resettodefault"));
    let reset = Button::with_label(tr("action.reset"));
    {
        let state = state.clone();
        let list = list.downgrade();
        let persist = persist.clone();
        reset.connect_clicked(move |_| {
            let Some(list) = list.upgrade() else {
                return;
            };
            state.settings.borrow_mut().hotkeys.dictation_hotkeys =
                vec![crate::settings::HotkeyBinding {
                    combo: "F13".to_string(),
                    mode: crate::settings::ActivationMode::Push,
                }];
            persist();
            refill_hotkey_list(&list, &state, &persist);
        });
    }
    b.append(&reset);
    rows.append(&b);
    root.append(&card);

    let (smart_card, smart_rows) = group(tr("group.smart"));
    spin_row(
        &smart_rows,
        tr("smart.label"),
        state.settings.borrow().hotkeys.smart_tap_ms as f64,
        20.0,
        1000.0,
        5.0,
    );
    // spin_row leaves the control insensitive (display-only helper);
    // enable it: this one is wired.
    if let Some(row_box) = smart_rows.last_child() {
        let mut child = row_box.first_child();
        while let Some(c) = child {
            if let Ok(spin) = c.clone().downcast::<SpinButton>() {
                spin.set_sensitive(true);
                let state = state.clone();
                let persist = persist_prefs(state.clone());
                spin.connect_value_changed(move |s| {
                    state.settings.borrow_mut().hotkeys.smart_tap_ms = s.value() as u64;
                    persist();
                });
            }
            child = c.next_sibling();
        }
    }
    append_collapsible(&root, &smart_card, tr("group.smart"));

    attach_hotkey_capture(
        &root,
        state,
        &list,
        &persist,
        &record_btn,
        &add_entry,
        &add_mode,
    );

    root
}

/// Repaint the binding rows from the model. Each row shows the
/// combo, its own Hold/Toggle picker, and Remove.
fn refill_hotkey_list(list: &GtkBox, state: &Rc<AppState>, persist: &Rc<dyn Fn()>) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let bindings = state.settings.borrow().hotkeys.effective_hotkeys();
    let can_remove = bindings.len() > 1;
    for binding in bindings {
        let (b, _) = row(&binding.combo);
        let mode_drop = DropDown::new(
            Some(StringList::new(&[
                tr("opt.hold"),
                tr("opt.toggle"),
                tr("opt.smart"),
            ])),
            None::<gtk4::Expression>,
        );
        mode_drop.set_selected(match binding.mode {
            crate::settings::ActivationMode::Push => 0,
            crate::settings::ActivationMode::Tap => 1,
            crate::settings::ActivationMode::Smart => 2,
        });
        {
            let state = state.clone();
            let persist = persist.clone();
            let combo = binding.combo.clone();
            mode_drop.connect_selected_notify(move |d| {
                let mode = match d.selected() {
                    1 => crate::settings::ActivationMode::Tap,
                    2 => crate::settings::ActivationMode::Smart,
                    _ => crate::settings::ActivationMode::Push,
                };
                if let Some(b) = state
                    .settings
                    .borrow_mut()
                    .hotkeys
                    .dictation_hotkeys
                    .iter_mut()
                    .find(|b| b.combo == combo)
                {
                    b.mode = mode;
                }
                persist();
            });
        }
        b.append(&mode_drop);
        let remove = Button::with_label(tr("action.remove"));
        remove.set_sensitive(can_remove);
        remove.set_tooltip_text(Some(tr("hotkeys.keepone")));
        {
            let state = state.clone();
            let list = list.downgrade();
            let persist = persist.clone();
            let combo = binding.combo.clone();
            remove.connect_clicked(move |_| {
                let Some(list) = list.upgrade() else {
                    return;
                };
                state
                    .settings
                    .borrow_mut()
                    .hotkeys
                    .dictation_hotkeys
                    .retain(|b| b.combo != combo);
                persist();
                refill_hotkey_list(&list, &state, &persist);
            });
        }
        b.append(&remove);
        list.append(&b);
    }
}

// ---------------------------------------------------------------------------
// Press-to-record combo capture
// ---------------------------------------------------------------------------
//
// Click Record, press the desired keys: the next non-modifier press
// finalizes (e.g. F13, Ctrl+Alt+D). Pressing only modifiers and
// releasing finalizes a modifier-only combo. Esc with nothing held
// cancels.

/// Canonical modifier slot: 0 Ctrl, 1 Alt, 2 Shift, 3 Super.
fn capture_modifier(key: gtk4::gdk::Key) -> Option<usize> {
    use gtk4::gdk::Key as K;
    if key == K::Control_L || key == K::Control_R {
        Some(0)
    } else if key == K::Alt_L || key == K::Alt_R {
        Some(1)
    } else if key == K::Shift_L || key == K::Shift_R {
        Some(2)
    } else if key == K::Super_L
        || key == K::Super_R
        || key == K::Meta_L
        || key == K::Meta_R
        || key == K::Hyper_L
    {
        Some(3)
    } else {
        None
    }
}

/// Map a GDK key to our combo token vocabulary (same strings
/// `hotkey::parse_combo` accepts). `None` = unsupported key;
/// modifiers return `None` here (handled as modifiers, not keys).
fn capture_token(key: gtk4::gdk::Key) -> Option<String> {
    if capture_modifier(key).is_some() {
        return None;
    }
    let name = key.name()?.to_string();
    let upper = name.to_ascii_uppercase().replace('_', "");
    // F-keys and well-known names pass through normalized.
    const KNOWN: &[&str] = &[
        "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "F13", "F14",
        "F15", "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24", "TAB", "UP", "DOWN",
        "LEFT", "RIGHT", "HOME", "END", "INSERT", "DELETE", "PAGEUP", "PAGEDOWN",
    ];
    if KNOWN.contains(&upper.as_str()) {
        return Some(upper);
    }
    match upper.as_str() {
        "MINUS" => Some("Minus".into()),
        "EQUAL" => Some("Equal".into()),
        "BRACKETLEFT" => Some("LeftBracket".into()),
        "BRACKETRIGHT" => Some("RightBracket".into()),
        "SEMICOLON" => Some("Semicolon".into()),
        "APOSTROPHE" => Some("Apostrophe".into()),
        "GRAVE" => Some("Grave".into()),
        "BACKSLASH" => Some("Backslash".into()),
        "COMMA" => Some("Comma".into()),
        "PERIOD" => Some("Period".into()),
        "SLASH" => Some("Slash".into()),
        "SPACE" => Some("Space".to_string()),
        "RETURN" => Some("Enter".to_string()),
        "BACKSPACE" => Some("Backspace".to_string()),
        "ESCAPE" => Some("Escape".to_string()),
        _ if name.len() == 1 => {
            let c = name.chars().next().unwrap();
            Some(if c.is_ascii_alphabetic() {
                c.to_ascii_uppercase().to_string()
            } else {
                c.to_string()
            })
        }
        _ => None,
    }
}

/// Canonical combo string from modifier slots + optional key.
fn combo_string(mods: &[bool; 4], key: Option<&str>) -> String {
    let mut parts = Vec::new();
    if mods[0] {
        parts.push("Ctrl");
    }
    if mods[1] {
        parts.push("Alt");
    }
    if mods[2] {
        parts.push("Shift");
    }
    if mods[3] {
        parts.push("Super");
    }
    if let Some(k) = key {
        parts.push(k);
    }
    parts.join("+")
}

fn mods_from_state(state: gtk4::gdk::ModifierType) -> [bool; 4] {
    use gtk4::gdk::ModifierType as M;
    [
        state.contains(M::CONTROL_MASK),
        state.contains(M::ALT_MASK),
        state.contains(M::SHIFT_MASK),
        state.contains(M::SUPER_MASK) || state.contains(M::META_MASK),
    ]
}

struct Capture {
    capturing: Rc<Cell<bool>>,
    /// Modifier slots currently held (press-tracked).
    held: Rc<RefCell<[bool; 4]>>,
    /// Modifier slots pressed at any point this round (for
    /// modifier-only finalize on release).
    pressed: Rc<RefCell<[bool; 4]>>,
    record_btn: gtk4::glib::WeakRef<Button>,
    add_entry: gtk4::glib::WeakRef<Entry>,
    suspend: RefCell<Option<std::fs::File>>,
    nonce: Cell<Option<[u8; 16]>>,
    suspend_path: std::path::PathBuf,
}

impl Capture {
    fn stop(&self) {
        self.capturing.set(false);
        self.held.replace([false; 4]);
        self.pressed.replace([false; 4]);
        if let Some(nonce) = self.nonce.take() {
            let request = self.suspend_path.with_extension("request");
            if crate::private_fs::read_bounded(&request, 16)
                .ok()
                .as_deref()
                == Some(nonce.as_slice())
            {
                let _ = std::fs::remove_file(request);
            }
        }
        self.suspend.borrow_mut().take();
        let (Some(record_btn), Some(add_entry)) =
            (self.record_btn.upgrade(), self.add_entry.upgrade())
        else {
            return;
        };
        record_btn.set_label(tr("hotkeys.record"));
        add_entry.set_sensitive(true);
        add_entry.set_placeholder_text(Some("F13"));
    }

    fn ready(&self) {
        if self.suspend.borrow().is_none() {
            return;
        }
        self.capturing.set(true);
        self.live_label();
        if let Some(entry) = self.add_entry.upgrade() {
            entry.set_placeholder_text(Some(tr("hotkeys.pressnow")));
        }
    }

    fn begin(self: &Rc<Self>) {
        let (Some(record_btn), Some(add_entry)) =
            (self.record_btn.upgrade(), self.add_entry.upgrade())
        else {
            return;
        };
        let result = (|| -> std::io::Result<bool> {
            let guard = crate::private_fs::try_lock(&self.suspend_path)?;
            *self.suspend.borrow_mut() = Some(guard);
            let mut nonce = [0u8; 16];
            getrandom::fill(&mut nonce).map_err(std::io::Error::other)?;
            self.nonce.set(Some(nonce));
            crate::private_fs::replace(&self.suspend_path.with_extension("request"), &nonce)?;
            // No engine owner: the capture lock still prevents a concurrent
            // engine startup from admitting keys before this capture ends.
            match crate::private_fs::try_lock(&self.suspend_path.with_file_name("engine.lock")) {
                Ok(_) => Ok(false),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
                Err(error) => Err(error),
            }
        })();
        let waiting = match result {
            Ok(waiting) => waiting,
            Err(error) => {
                self.stop();
                eprintln!("[hotkey] capture suspend: {error}");
                super::app::toast(&record_btn, tr("hotkeys.suspendfailed"));
                return;
            }
        };
        add_entry.set_sensitive(false);
        if !waiting {
            self.ready();
            return;
        }
        record_btn.set_label(tr("hotkeys.suspending"));
        let capture = Rc::downgrade(self);
        let nonce = self.nonce.get().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
            let Some(capture) = capture.upgrade() else {
                return gtk4::glib::ControlFlow::Break;
            };
            if capture.nonce.get() != Some(nonce) {
                return gtk4::glib::ControlFlow::Break;
            }
            if crate::private_fs::read_bounded(&capture.suspend_path.with_extension("ack"), 16)
                .ok()
                .as_deref()
                == Some(nonce.as_slice())
            {
                capture.ready();
                return gtk4::glib::ControlFlow::Break;
            }
            if std::time::Instant::now() >= deadline {
                capture.stop();
                if let Some(button) = capture.record_btn.upgrade() {
                    super::app::toast(&button, tr("hotkeys.suspendfailed"));
                }
                return gtk4::glib::ControlFlow::Break;
            }
            gtk4::glib::ControlFlow::Continue
        });
    }

    fn live_label(&self) {
        let Some(record_btn) = self.record_btn.upgrade() else {
            return;
        };
        let held = *self.held.borrow();
        let shown = combo_string(&held, None);
        let keys = if shown.is_empty() {
            tr("hotkeys.presskeys").to_string()
        } else {
            shown
        };
        record_btn.set_label(crate::i18n::trf("hotkeys.recording", &[("keys", &keys)]).as_str());
    }

    fn finish(
        &self,
        combo: String,
        mode: crate::settings::ActivationMode,
        state: &Rc<AppState>,
        list: &GtkBox,
        persist: &Rc<dyn Fn()>,
    ) {
        if crate::hotkey::parse_combo(&combo).is_none() {
            let Some(record_btn) = self.record_btn.upgrade() else {
                return;
            };
            record_btn.set_label(tr("hotkeys.invalid"));
            return;
        }
        {
            let mut s = state.settings.borrow_mut();
            if !s
                .hotkeys
                .dictation_hotkeys
                .iter()
                .any(|b| same_shortcut(&b.combo, &combo))
            {
                s.hotkeys
                    .dictation_hotkeys
                    .push(crate::settings::HotkeyBinding { combo, mode });
            }
        }
        persist();
        refill_hotkey_list(list, state, persist);
        self.stop();
        // Recording is over either way: bring the listener back with
        // the fresh bindings.
        crate::hotkey::apply_from_settings(state);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Wire the Record button + a capture-phase key controller onto the
/// hotkeys page. Any keypress while capturing is swallowed (never
/// reaches entries).
fn attach_hotkey_capture(
    root: &GtkBox,
    state: &Rc<AppState>,
    list: &GtkBox,
    persist: &Rc<dyn Fn()>,
    record_btn: &Button,
    add_entry: &Entry,
    add_mode: &DropDown,
) {
    let cap = Rc::new(Capture {
        capturing: Rc::new(Cell::new(false)),
        held: Rc::new(RefCell::new([false; 4])),
        pressed: Rc::new(RefCell::new([false; 4])),
        record_btn: record_btn.downgrade(),
        add_entry: add_entry.downgrade(),
        suspend: RefCell::new(None),
        nonce: Cell::new(None),
        suspend_path: state
            .db_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("hotkey-capture.lock"),
    });
    {
        let cap = cap.clone();
        root.connect_unmap(move |_| cap.stop());
    }
    {
        let cap = cap.clone();
        let state = state.clone();
        record_btn.connect_clicked(move |_| {
            if cap.suspend.borrow().is_some() {
                cap.stop();
                crate::hotkey::apply_from_settings(&state);
            } else {
                cap.begin();
            }
        });
    }

    let ctl = EventControllerKey::new();
    ctl.set_propagation_phase(gtk4::PropagationPhase::Capture);
    {
        let cap = cap.clone();
        let state = state.clone();
        let list = list.downgrade();
        let persist = persist.clone();
        let add_mode = add_mode.clone();
        ctl.connect_key_pressed(move |_, key, keycode, mod_state| {
            let Some(list) = list.upgrade() else {
                return gtk4::glib::Propagation::Proceed;
            };
            if !cap.capturing.get() {
                if cap.nonce.get().is_some() {
                    if key == gtk4::gdk::Key::Escape {
                        cap.stop();
                    }
                    return gtk4::glib::Propagation::Stop;
                }
                return gtk4::glib::Propagation::Proceed;
            }
            // Seed from the event's own modifier state (covers
            // modifiers held before capture started).
            for (slot, on) in mods_from_state(mod_state).iter().enumerate() {
                if *on {
                    cap.held.borrow_mut()[slot] = true;
                    cap.pressed.borrow_mut()[slot] = true;
                }
            }
            if let Some(slot) = capture_modifier(key) {
                cap.held.borrow_mut()[slot] = true;
                cap.pressed.borrow_mut()[slot] = true;
                cap.live_label();
                return gtk4::glib::Propagation::Stop;
            }
            // Bare Escape (nothing held) cancels.
            use gtk4::gdk::Key as K;
            let held = *cap.held.borrow();
            if key == K::Escape && !held.iter().any(|h| *h) {
                cap.stop();
                crate::hotkey::apply_from_settings(&state);
                return gtk4::glib::Propagation::Stop;
            }
            // Hardware keycode first (XKB keycode = evdev + 8): this is
            // the same vocabulary the listener uses, immune to layout
            // keysyms like XF86Tools-on-an-F13-key. Keyval name only
            // as fallback.
            let token = keycode
                .checked_sub(8)
                .and_then(|c| crate::hotkey::token_for_evdev_code(c as u16).map(str::to_string))
                .or_else(|| capture_token(key));
            eprintln!("[hotkey] capture saw keycode={keycode} token={token:?}");
            match token {
                Some(token) => {
                    let combo = combo_string(&held, Some(&token));
                    let mode = match add_mode.selected() {
                        1 => crate::settings::ActivationMode::Tap,
                        2 => crate::settings::ActivationMode::Smart,
                        _ => crate::settings::ActivationMode::Push,
                    };
                    cap.finish(combo, mode, &state, &list, &persist);
                }
                None => {
                    if let Some(button) = cap.record_btn.upgrade() {
                        button.set_label(tr("hotkeys.unsupported"));
                    }
                }
            }
            gtk4::glib::Propagation::Stop
        });
    }
    {
        let cap = cap.clone();
        let state = state.clone();
        let list = list.downgrade();
        let persist = persist.clone();
        let add_mode = add_mode.clone();
        ctl.connect_key_released(move |_, key, _, _| {
            let Some(list) = list.upgrade() else {
                return;
            };
            if !cap.capturing.get() {
                return;
            }
            if let Some(slot) = capture_modifier(key) {
                cap.held.borrow_mut()[slot] = false;
                // All modifiers released with no regular key seen:
                // finalize a modifier-only combo.
                if !cap.held.borrow().iter().any(|h| *h) {
                    let pressed = *cap.pressed.borrow();
                    if pressed.iter().any(|h| *h) {
                        let combo = combo_string(&pressed, None);
                        let mode = match add_mode.selected() {
                            1 => crate::settings::ActivationMode::Tap,
                            2 => crate::settings::ActivationMode::Smart,
                            _ => crate::settings::ActivationMode::Push,
                        };
                        cap.finish(combo, mode, &state, &list, &persist);
                    }
                }
            }
        });
    }
    root.add_controller(ctl);
}

fn append_asset_feedback(container: &GtkBox, status: crate::installed_asset::Status) {
    use crate::installed_asset::Status;
    let text = match status {
        Status::Checking => tr("stt.asset.checking").to_owned(),
        Status::ReadyLocal => tr("stt.asset.local").to_owned(),
        Status::NeedsVerification => tr("stt.asset.legacy").to_owned(),
        Status::Invalid(message) => trf("stt.asset.invalid", &[("err", &message)]),
        Status::Missing | Status::Ready => return,
    };
    let label = Label::new(Some(&text));
    label.add_css_class("dim");
    label.set_wrap(true);
    label.set_max_width_chars(40);
    container.append(&label);
}

fn append_asset_verification(
    container: &GtkBox,
    spec: crate::installed_asset::Spec,
    id: &'static str,
) {
    if !matches!(
        crate::installed_asset::status(spec.clone()),
        crate::installed_asset::Status::NeedsVerification
            | crate::installed_asset::Status::Invalid(_)
    ) || crate::stt::download_state(id).is_some_and(|download| {
        matches!(
            download.phase,
            crate::stt::DlPhase::Downloading | crate::stt::DlPhase::Installing
        )
    }) {
        return;
    }
    // Pinned raw assets verify offline automatically. Legacy archives need
    // an authenticated reference; comparison never replaces the local files.
    if spec.raw.is_none() {
        let verify = Button::with_label(tr("stt.asset.verify"));
        verify.set_tooltip_text(Some(tr("stt.asset.verifyhelp")));
        let data = spec.data.clone();
        verify.connect_clicked(move |_| crate::stt::start_verification(data.clone(), id));
        container.append(&verify);
    }
    let trust = Button::with_label(tr("stt.asset.trust"));
    super::app::connect_confirmed(
        &trust,
        tr("stt.asset.trusttitle"),
        tr("stt.asset.trustbody"),
        tr("stt.asset.trust"),
        move |_| crate::stt::start_local_trust(spec.clone(), id),
    );
    container.append(&trust);
}

/// Registry model list for the active local engine: name, size,
/// install state, download/delete/select actions. Rebuilt when the
/// signature (selection, install set, download phases) moves;
/// progress bars update in place every tick via tooltip-keyed rows.
fn fill_stt_models(container: &GtkBox, state: &Rc<AppState>) {
    use crate::settings::LocalSttProvider;
    use crate::stt;
    let settings = state.settings.borrow();
    // Vendor-filtered list; single-model vendors (Oruk, Cohere) show
    // just their model with no Use choice.
    let vendor_entries: Vec<&stt::ModelEntry> = stt::models_for_vendor(settings.stt.local_provider);
    let Some(data_dir) = state.db_path.parent().map(|p| p.to_path_buf()) else {
        return;
    };
    let selected = match settings.stt.local_provider {
        LocalSttProvider::Nvidia => settings.stt.parakeet_model.clone(),
        LocalSttProvider::OpenAi => settings.stt.whisper_model.clone(),
        LocalSttProvider::Oruk => "orukeet-v0.1.0".to_string(),
        LocalSttProvider::Cohere => "cohere-transcribe-03-2026".to_string(),
        LocalSttProvider::Whistle => "whistle-v1".to_string(),
    };
    drop(settings);
    // Owned copy: the poll closure below borrows nothing across ticks.
    let entries: Vec<stt::ModelEntry> = vendor_entries.into_iter().cloned().collect();

    let mut sig = format!("sel={selected}|");
    for m in &entries {
        let phase = match stt::download_state(m.id) {
            Some(d) => format!("{:?}", d.phase),
            None => "-".to_string(),
        };
        sig.push_str(&format!(
            "{}:{:?}:{};",
            m.id,
            stt::model_status(&data_dir, m),
            phase
        ));
    }
    let current_sig = container
        .first_child()
        .and_then(|c| c.tooltip_text().map(|t| t.to_string()));
    if current_sig.as_deref() == Some(sig.as_str()) {
        update_stt_progress(container);
        return;
    }
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    let marker = GtkBox::new(Orientation::Horizontal, 0);
    marker.set_tooltip_text(Some(&sig));
    marker.set_visible(false);
    container.append(&marker);

    for m in &entries {
        let card = GtkBox::new(Orientation::Horizontal, 12);
        card.add_css_class("card");
        // Tooltip keys the row to its model id for progress updates.
        card.set_tooltip_text(Some(m.id));
        let info = GtkBox::new(Orientation::Vertical, 2);
        info.set_hexpand(true);
        let title = format!("{}{}", m.name, if m.recommended { " ★" } else { "" });
        let name = Label::new(Some(&title));
        name.add_css_class("row-label");
        name.set_xalign(0.0);
        info.append(&name);
        let sub = Label::new(Some(&format!(
            "{} · {}",
            crate::i18n::backend_text(m.description),
            m.size_label
        )));
        sub.add_css_class("dim");
        sub.set_xalign(0.0);
        sub.set_wrap(true);
        info.append(&sub);
        // Capability badge: green when fully supported, red naming
        // exactly what's missing.
        let (streaming, dict) = stt::capabilities(m);
        let (badge, ok) = match (streaming, dict) {
            (true, true) => (tr("stt.models.full"), true),
            (true, false) => (tr("stt.models.nodict"), false),
            (false, true) => (tr("stt.models.nostream"), false),
            (false, false) => (tr("stt.models.neither"), false),
        };
        let cap = Label::new(Some(badge));
        cap.add_css_class(if ok { "cap-ok" } else { "cap-no" });
        cap.set_xalign(0.0);
        info.append(&cap);
        card.append(&info);

        if m.id == "cohere-transcribe-03-2026" {
            let explanation = Label::new(Some(tr("stt.models.coherelanguage")));
            explanation.set_wrap(true);
            explanation.set_xalign(0.0);
            explanation.set_max_width_chars(65);
            info.append(&explanation);
        }

        append_asset_feedback(&card, stt::model_status(&data_dir, m));
        append_asset_verification(&card, stt::model_spec(&data_dir, m), m.id);
        if let Some(dl) = stt::download_state(m.id) {
            let col = GtkBox::new(Orientation::Vertical, 4);
            match &dl.phase {
                stt::DlPhase::Failed(e) => {
                    let l = Label::new(Some(&trf("stt.dl.failed", &[("err", e)])));
                    l.add_css_class("dim");
                    l.set_wrap(true);
                    l.set_max_width_chars(60);
                    col.append(&l);
                    let retry = Button::with_label(tr("action.retry"));
                    {
                        let state = state.clone();
                        let id = m.id;
                        retry.connect_clicked(move |_| {
                            let data_dir = state
                                .db_path
                                .parent()
                                .map(|p| p.to_path_buf())
                                .unwrap_or_default();
                            stt::start_download(data_dir, id);
                        });
                    }
                    col.append(&retry);
                }
                _ => {
                    let bar = gtk4::ProgressBar::new();
                    set_bar_state(&bar, &dl);
                    col.append(&bar);
                    let cancel = Button::with_label(tr("action.cancel"));
                    {
                        let id = m.id;
                        cancel.connect_clicked(move |_| stt::cancel_download(id));
                    }
                    col.append(&cancel);
                }
            }
            card.append(&col);
        } else if stt::is_installed(&data_dir, m) {
            // Singletons (Oruk/Cohere tabs) have nothing to choose:
            // installed means active.
            let singleton = entries.len() == 1;
            if singleton || selected == m.id {
                let using = Button::with_label(tr("action.using"));
                using.set_sensitive(false);
                card.append(&using);
            } else {
                let use_btn = Button::with_label(tr("action.use"));
                {
                    let state = state.clone();
                    let id = m.id.to_string();
                    use_btn.connect_clicked(move |_| {
                        {
                            let mut s = state.settings.borrow_mut();
                            // Provider read at click time: the list
                            // may have been rebuilt under us.
                            if matches!(s.stt.local_provider, LocalSttProvider::Nvidia) {
                                s.stt.parakeet_model = id.clone();
                            } else {
                                s.stt.whisper_model = id.clone();
                            }
                        }
                        persist_prefs(state.clone())();
                    });
                }
                card.append(&use_btn);
            }
            let del = Button::with_label(tr("action.delete"));
            {
                let db_path = state.db_path.clone();
                let id = m.id;
                connect_download_delete(&del, move || {
                    if let Some(data_dir) = db_path.parent()
                        && let Some(entry) = stt::find_model(id)
                    {
                        return stt::delete_model(data_dir, entry);
                    }
                    Ok(())
                });
            }
            card.append(&del);
        } else {
            let dl_btn = Button::with_label(tr("action.download"));
            {
                let state = state.clone();
                let id = m.id;
                dl_btn.connect_clicked(move |_| {
                    let data_dir = state
                        .db_path
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_default();
                    stt::start_download(data_dir, id);
                });
            }
            card.append(&dl_btn);
        }
        container.append(&card);
    }
}

fn set_bar_state(bar: &gtk4::ProgressBar, dl: &crate::stt::DlSnapshot) {
    use crate::stt::DlPhase;
    let frac = dl
        .total
        .filter(|t| *t > 0)
        .map(|t| (dl.received as f64 / t as f64).min(1.0))
        .unwrap_or(0.0);
    bar.set_fraction(frac);
    bar.set_show_text(true);
    let label = match dl.phase {
        DlPhase::Installing => tr("action.installing").to_string(),
        _ => match dl.total {
            Some(t) => format!("{} / {} MB", dl.received / 1_000_000, t / 1_000_000),
            None => format!("{} MB", dl.received / 1_000_000),
        },
    };
    bar.set_text(Some(&label));
}

/// Refresh visible progress bars from live download states.
fn update_stt_progress(container: &GtkBox) {
    use crate::stt;
    let mut child = container.first_child();
    while let Some(c) = child {
        child = c.next_sibling();
        let Ok(card) = c.downcast::<GtkBox>() else {
            continue;
        };
        let Some(id) = card.tooltip_text().map(|t| t.to_string()) else {
            continue;
        };
        let Some(dl) = stt::download_state(&id) else {
            continue;
        };
        let mut inner = card.first_child();
        while let Some(w) = inner {
            inner = w.next_sibling();
            if let Ok(col) = w.clone().downcast::<GtkBox>() {
                let mut leaf = col.first_child();
                while let Some(l) = leaf {
                    leaf = l.next_sibling();
                    if let Ok(bar) = l.downcast::<gtk4::ProgressBar>() {
                        set_bar_state(&bar, &dl);
                    }
                }
            }
        }
    }
}

/// GPU pack list: backend binaries with download/progress/delete.
/// Manual management only: detection merely recommends.
/// Sherpa server pack list (parakeet/orukeet/cohere vendors).
/// No backend choice: offline vs online server is picked by the model
/// runtime automatically. Otherwise same machinery as whisper packs.
fn fill_sherpa_packs(container: &GtkBox, state: &Rc<AppState>) {
    use crate::stt;
    let Some(data_dir) = state.db_path.parent().map(|p| p.to_path_buf()) else {
        return;
    };
    let mut sig = String::new();
    for pack in stt::SHERPA_PACKS {
        let phase = match stt::download_state(pack.id) {
            Some(d) => format!("{:?}", d.phase),
            None => "-".to_string(),
        };
        sig.push_str(&format!(
            "{}:{:?}:{};",
            pack.id,
            stt::sherpa_status(&data_dir, pack),
            phase
        ));
    }
    let current_sig = container
        .first_child()
        .and_then(|c| c.tooltip_text().map(|t| t.to_string()));
    if current_sig.as_deref() == Some(sig.as_str()) {
        update_gpu_progress(container);
        return;
    }
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    let marker = GtkBox::new(Orientation::Horizontal, 0);
    marker.set_tooltip_text(Some(&sig));
    marker.set_visible(false);
    container.append(&marker);

    for pack in stt::SHERPA_PACKS {
        let card = GtkBox::new(Orientation::Horizontal, 12);
        card.add_css_class("card");
        card.set_tooltip_text(Some(pack.id));
        let info = GtkBox::new(Orientation::Vertical, 2);
        info.set_hexpand(true);
        let name = Label::new(Some(crate::i18n::backend_text(pack.name)));
        name.add_css_class("row-label");
        name.set_xalign(0.0);
        info.append(&name);
        let sub = Label::new(Some(&format!(
            "{} · {}",
            crate::i18n::backend_text(pack.description),
            pack.size_label
        )));
        sub.add_css_class("dim");
        sub.set_xalign(0.0);
        sub.set_wrap(true);
        info.append(&sub);
        card.append(&info);

        append_asset_feedback(&card, stt::sherpa_status(&data_dir, pack));
        append_asset_verification(&card, stt::sherpa_spec(&data_dir, pack), pack.id);
        if let Some(dl) = stt::download_state(pack.id) {
            let col = GtkBox::new(Orientation::Vertical, 4);
            match &dl.phase {
                stt::DlPhase::Failed(e) => {
                    let l = Label::new(Some(&trf("stt.dl.failed", &[("err", e)])));
                    l.add_css_class("dim");
                    l.set_wrap(true);
                    l.set_max_width_chars(60);
                    col.append(&l);
                    let retry = Button::with_label(tr("action.retry"));
                    {
                        let state = state.clone();
                        let id = pack.id;
                        retry.connect_clicked(move |_| {
                            let data_dir = state
                                .db_path
                                .parent()
                                .map(|p| p.to_path_buf())
                                .unwrap_or_default();
                            stt::start_sherpa_download(data_dir, id);
                        });
                    }
                    col.append(&retry);
                }
                _ => {
                    let bar = gtk4::ProgressBar::new();
                    set_bar_state(&bar, &dl);
                    col.append(&bar);
                    let cancel = Button::with_label(tr("action.cancel"));
                    {
                        let id = pack.id;
                        cancel.connect_clicked(move |_| stt::cancel_download(id));
                    }
                    col.append(&cancel);
                }
            }
            card.append(&col);
        } else if stt::sherpa_installed(&data_dir, pack) {
            let installed = Label::new(Some(tr("stt.sherpa.installed")));
            installed.add_css_class("dim");
            installed.set_valign(gtk4::Align::Center);
            card.append(&installed);
            let del = Button::with_label(tr("action.delete"));
            {
                let db_path = state.db_path.clone();
                let id = pack.id;
                connect_download_delete(&del, move || {
                    if let Some(data_dir) = db_path.parent()
                        && let Some(pack) = stt::SHERPA_PACKS.iter().find(|p| p.id == id)
                    {
                        return stt::delete_sherpa_pack(data_dir, pack);
                    }
                    Ok(())
                });
            }
            card.append(&del);
        } else {
            let dl_btn = Button::with_label(tr("action.download"));
            {
                let state = state.clone();
                let id = pack.id;
                dl_btn.connect_clicked(move |_| {
                    let data_dir = state
                        .db_path
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_default();
                    stt::start_sherpa_download(data_dir, id);
                });
            }
            card.append(&dl_btn);
        }
        container.append(&card);
    }
}

fn fill_gpu_packs(container: &GtkBox, state: &Rc<AppState>) {
    use crate::stt;
    let Some(data_dir) = state.db_path.parent().map(|p| p.to_path_buf()) else {
        return;
    };
    // Rebuild signature: installed set + download phases.
    let mut sig = String::new();
    for pack in stt::GPU_PACKS {
        let phase = match stt::download_state(pack.id) {
            Some(d) => format!("{:?}", d.phase),
            None => "-".to_string(),
        };
        sig.push_str(&format!(
            "{}:{:?}:{};",
            pack.id,
            stt::gpu_status(&data_dir, pack),
            phase
        ));
    }
    let current_sig = container
        .first_child()
        .and_then(|c| c.tooltip_text().map(|t| t.to_string()));
    if current_sig.as_deref() == Some(sig.as_str()) {
        update_gpu_progress(container);
        return;
    }
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    let marker = GtkBox::new(Orientation::Horizontal, 0);
    marker.set_tooltip_text(Some(&sig));
    marker.set_visible(false);
    container.append(&marker);

    for pack in stt::GPU_PACKS {
        let card = GtkBox::new(Orientation::Horizontal, 12);
        card.add_css_class("card");
        card.set_tooltip_text(Some(pack.id));
        let info = GtkBox::new(Orientation::Vertical, 2);
        info.set_hexpand(true);
        let name = Label::new(Some(crate::i18n::backend_text(pack.name)));
        name.add_css_class("row-label");
        name.set_xalign(0.0);
        info.append(&name);
        // Single selection method: the backend dropdown above. Rows
        // only show install state + Download/Delete.
        let installed = stt::gpu_installed(&data_dir, pack);
        let sub = Label::new(Some(&format!(
            "{} · {}{}",
            crate::i18n::backend_text(pack.description),
            pack.size_label,
            if installed {
                format!(" · {}", tr("stt.sherpa.installed"))
            } else {
                String::new()
            }
        )));
        sub.add_css_class("dim");
        sub.set_xalign(0.0);
        sub.set_wrap(true);
        info.append(&sub);
        card.append(&info);

        append_asset_feedback(&card, stt::gpu_status(&data_dir, pack));
        append_asset_verification(&card, stt::gpu_spec(&data_dir, pack), pack.id);
        if let Some(dl) = stt::download_state(pack.id) {
            let col = GtkBox::new(Orientation::Vertical, 4);
            match &dl.phase {
                stt::DlPhase::Failed(e) => {
                    let l = Label::new(Some(&trf("stt.dl.failed", &[("err", e)])));
                    l.add_css_class("dim");
                    l.set_wrap(true);
                    l.set_max_width_chars(60);
                    col.append(&l);
                    let retry = Button::with_label(tr("action.retry"));
                    {
                        let state = state.clone();
                        let id = pack.id;
                        retry.connect_clicked(move |_| {
                            let data_dir = state
                                .db_path
                                .parent()
                                .map(|p| p.to_path_buf())
                                .unwrap_or_default();
                            stt::start_gpu_download(data_dir, id);
                        });
                    }
                    col.append(&retry);
                }
                _ => {
                    let bar = gtk4::ProgressBar::new();
                    set_bar_state(&bar, &dl);
                    col.append(&bar);
                    let cancel = Button::with_label(tr("action.cancel"));
                    {
                        let id = pack.id;
                        cancel.connect_clicked(move |_| stt::cancel_download(id));
                    }
                    col.append(&cancel);
                }
            }
            card.append(&col);
        } else if installed {
            let del = Button::with_label(tr("action.delete"));
            {
                let db_path = state.db_path.clone();
                let id = pack.id;
                connect_download_delete(&del, move || {
                    if let Some(data_dir) = db_path.parent()
                        && let Some(pack) = stt::GPU_PACKS.iter().find(|p| p.id == id)
                    {
                        return stt::delete_gpu_pack(data_dir, pack);
                    }
                    Ok(())
                });
            }
            card.append(&del);
        } else {
            let dl_btn = Button::with_label(tr("action.download"));
            {
                let state = state.clone();
                let id = pack.id;
                dl_btn.connect_clicked(move |_| {
                    let data_dir = state
                        .db_path
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_default();
                    stt::start_gpu_download(data_dir, id);
                });
            }
            card.append(&dl_btn);
        }
        container.append(&card);
    }
}

/// Needle engine action cell: progress while downloading, Delete
/// when installed, Download otherwise. Rebuilt on the same 500 ms
/// cadence as the pack lists so progress just appears.
fn refresh_needle_action(container: &GtkBox, state: &Rc<AppState>) {
    use crate::stt;
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    let data_dir = state
        .db_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    append_asset_feedback(container, stt::needle_status(&data_dir));
    if let Ok(spec) = stt::needle_spec(&data_dir) {
        append_asset_verification(container, spec, stt::NEEDLE_ENGINE_ID);
    }
    if let Some(dl) = stt::download_state(stt::NEEDLE_ENGINE_ID) {
        let col = GtkBox::new(Orientation::Vertical, 4);
        match &dl.phase {
            stt::DlPhase::Failed(e) => {
                let l = Label::new(Some(&trf("stt.dl.failed", &[("err", e)])));
                l.add_css_class("dim");
                l.set_wrap(true);
                l.set_max_width_chars(60);
                col.append(&l);
                let retry = Button::with_label(tr("action.retry"));
                {
                    let state = state.clone();
                    retry.connect_clicked(move |_| {
                        let data_dir = state
                            .db_path
                            .parent()
                            .map(|p| p.to_path_buf())
                            .unwrap_or_default();
                        stt::start_needle_download(data_dir);
                    });
                }
                col.append(&retry);
            }
            _ => {
                let bar = gtk4::ProgressBar::new();
                set_bar_state(&bar, &dl);
                col.append(&bar);
                let cancel = Button::with_label(tr("action.cancel"));
                cancel.connect_clicked(move |_| stt::cancel_download(stt::NEEDLE_ENGINE_ID));
                col.append(&cancel);
            }
        }
        container.append(&col);
    } else if stt::needle_installed(&data_dir) {
        let del = Button::with_label(tr("action.delete"));
        {
            let db_path = state.db_path.clone();
            connect_download_delete(&del, move || {
                if let Some(data_dir) = db_path.parent() {
                    return stt::delete_needle_pack(data_dir);
                }
                Ok(())
            });
        }
        container.append(&del);
    } else {
        let dl_btn = Button::with_label(tr("action.download"));
        {
            let state = state.clone();
            dl_btn.connect_clicked(move |_| {
                let data_dir = state
                    .db_path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_default();
                stt::start_needle_download(data_dir);
            });
        }
        container.append(&dl_btn);
    }
}

/// Refresh visible GPU progress bars from live download states.
fn update_gpu_progress(container: &GtkBox) {
    use crate::stt;
    let mut child = container.first_child();
    while let Some(c) = child {
        child = c.next_sibling();
        let Ok(card) = c.downcast::<GtkBox>() else {
            continue;
        };
        let Some(id) = card.tooltip_text().map(|t| t.to_string()) else {
            continue;
        };
        let Some(dl) = stt::download_state(&id) else {
            continue;
        };
        let mut inner = card.first_child();
        while let Some(w) = inner {
            inner = w.next_sibling();
            if let Ok(col) = w.clone().downcast::<GtkBox>() {
                let mut leaf = col.first_child();
                while let Some(l) = leaf {
                    leaf = l.next_sibling();
                    if let Ok(bar) = l.downcast::<gtk4::ProgressBar>() {
                        set_bar_state(&bar, &dl);
                    }
                }
            }
        }
    }
}

/// A shared language choice must preserve remote configurations and must
/// never be changed by rendering. Local-only configurations show their
/// supported choices alongside any unsupported stored value; plan creation
/// rejects that value explicitly until the user chooses a compatible model
/// or language. Sherpa multilingual models choose their own language.
struct SttLang {
    opts: Vec<String>,
    locked: bool,
}

fn stt_lang(state: &Rc<AppState>) -> SttLang {
    use crate::settings::LocalSttProvider;
    use crate::stt;
    let s = state.settings.borrow();
    if s.stt
        .chain()
        .iter()
        .any(|entry| entry.kind != crate::settings::SttEntryKind::Local)
    {
        let mut opts = vec!["auto".to_string()];
        opts.extend(stt::WHISPER_LANGS.iter().map(|lang| lang.to_string()));
        if !opts.contains(&s.stt.language) {
            opts.push(s.stt.language.clone());
        }
        return SttLang {
            opts,
            locked: false,
        };
    }
    let model_id: &str = match s.stt.local_provider {
        LocalSttProvider::OpenAi => {
            let mut v = vec!["auto".to_string()];
            v.extend(stt::WHISPER_LANGS.iter().map(|l| l.to_string()));
            return SttLang {
                opts: v,
                locked: false,
            };
        }
        LocalSttProvider::Nvidia => &s.stt.parakeet_model,
        LocalSttProvider::Oruk => "orukeet-v0.1.0",
        LocalSttProvider::Cohere => "cohere-transcribe-03-2026",
        LocalSttProvider::Whistle => {
            let mut v = vec!["auto".to_string()];
            v.extend(stt::WHISTLE_LANGS.iter().map(|l| l.to_string()));
            return SttLang {
                opts: v,
                locked: false,
            };
        }
    };
    let langs: Vec<String> = stt::model_languages(model_id)
        .iter()
        .map(|l| l.to_string())
        .collect();
    if langs.len() == 1 {
        return SttLang {
            opts: langs,
            locked: true,
        };
    }
    let mut v = if matches!(s.stt.local_provider, LocalSttProvider::Cohere) {
        Vec::new()
    } else {
        vec!["auto".to_string()]
    };
    v.extend(langs);
    SttLang {
        opts: v,
        locked: false,
    }
}

/// Shared handles of the STT remote form (mirrors LmForm).
#[derive(Clone)]
struct SttForm {
    expander: gtk4::Expander,
    name_e: Entry,
    url_e: Entry,
    model_e: Entry,
    key_e: Entry,
    stream_sw: Switch,
    stream_intent: Rc<RefCell<bool>>,
    sh_key_e: Entry,
    provider_dd: DropDown,
    kind_dd: DropDown,
    add_btn: Button,
    cancel_btn: Button,
    editing: Rc<RefCell<Option<String>>>,
}
weak_form!(SttForm, WeakSttForm, {
    expander: gtk4::Expander,
    name_e: Entry,
    url_e: Entry,
    model_e: Entry,
    key_e: Entry,
    stream_sw: Switch,
    sh_key_e: Entry,
    provider_dd: DropDown,
    kind_dd: DropDown,
    add_btn: Button,
    cancel_btn: Button,
}, {
    stream_intent: Rc<RefCell<bool>>,
    editing: Rc<RefCell<Option<String>>>,
});

/// STT chain list. Local rows toggle only (configuration lives in
/// the pills/models below); remote rows edit/remove/move.
fn fill_stt_chain(list: &GtkBox, state: &Rc<AppState>, form: &SttForm, add_local: &Button) {
    use crate::settings::SttEntryKind;
    // Single local option: button hides while one exists.
    add_local.set_visible(
        !state
            .settings
            .borrow()
            .stt
            .entries
            .iter()
            .any(|e| matches!(e.kind, SttEntryKind::Local)),
    );
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let entries = state.settings.borrow().stt.entries.clone();
    if entries.is_empty() {
        let l = Label::new(Some(tr("lm.list.empty")));
        l.add_css_class("dim");
        l.set_xalign(0.0);
        l.set_wrap(true);
        list.append(&l);
        return;
    }
    for (pos, entry) in entries.iter().enumerate() {
        let card = GtkBox::new(Orientation::Horizontal, 8);
        card.add_css_class("card");
        let info = GtkBox::new(Orientation::Vertical, 2);
        info.set_hexpand(true);
        let name = Label::new(Some(&entry.name));
        name.add_css_class("row-label");
        name.set_xalign(0.0);
        info.append(&name);
        let summary = if matches!(entry.kind, SttEntryKind::Local) {
            tr("stt.local.current").to_string()
        } else {
            entry.summary()
        };
        let sub = Label::new(Some(&summary));
        sub.add_css_class("dim");
        sub.set_xalign(0.0);
        sub.set_wrap(true);
        info.append(&sub);
        card.append(&info);
        let sw = Switch::new();
        sw.set_active(entry.enabled);
        sw.update_property(&[gtk4::accessible::Property::Label(&entry.name)]);
        sw.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            let id = entry.id.clone();
            sw.connect_state_set(move |_, active| {
                if let Some(e) = state
                    .settings
                    .borrow_mut()
                    .stt
                    .entries
                    .iter_mut()
                    .find(|e| e.id == id)
                {
                    e.enabled = active;
                }
                persist_prefs(state.clone())();
                gtk4::glib::Propagation::Proceed
            });
        }
        card.append(&sw);
        for (label, delta) in [("↑", -1isize), ("↓", 1isize)] {
            let btn = Button::with_label(label);
            btn.set_tooltip_text(Some(tr(if delta < 0 {
                "action.moveup"
            } else {
                "action.movedown"
            })));
            btn.update_property(&[gtk4::accessible::Property::Label(tr(if delta < 0 {
                "action.moveup"
            } else {
                "action.movedown"
            }))]);
            btn.set_sensitive(if delta < 0 {
                pos > 0
            } else {
                pos + 1 < entries.len()
            });
            btn.set_valign(gtk4::Align::Center);
            {
                let state = state.clone();
                let list = list.downgrade();
                let form = form.downgrade();
                let alb = add_local.clone();
                btn.connect_clicked(move |_| {
                    let Some(list) = list.upgrade() else {
                        return;
                    };
                    let Some(form) = form.upgrade() else {
                        return;
                    };
                    let mut st = state.settings.borrow_mut();
                    let entries = &mut st.stt.entries;
                    let other = pos as isize + delta;
                    if other >= 0 && (other as usize) < entries.len() && (other as usize) != pos {
                        entries.swap(pos, other as usize);
                    }
                    drop(st);
                    persist_prefs(state.clone())();
                    fill_stt_chain(&list, &state, &form, &alb);
                });
            }
            card.append(&btn);
        }
        // Edit only for remotes (local config lives below).
        if !matches!(entry.kind, SttEntryKind::Local) {
            let edit = Button::with_label(tr("action.edit"));
            edit.set_valign(gtk4::Align::Center);
            {
                let state = state.clone();
                let form = form.downgrade();
                let id = entry.id.clone();
                edit.connect_clicked(move |_| {
                    let Some(form) = form.upgrade() else {
                        return;
                    };
                    let entry = state
                        .settings
                        .borrow()
                        .stt
                        .entries
                        .iter()
                        .find(|e| e.id == id)
                        .cloned();
                    let Some(e) = entry else {
                        return;
                    };
                    form.expander.set_expanded(true);
                    form.name_e.grab_focus();
                    form.name_e.set_text(&e.name);
                    form.url_e.set_text(&e.url);
                    form.model_e.set_text(&e.model);
                    form.key_e.set_text(&e.api_key);
                    form.sh_key_e.set_text(&e.api_key);
                    *form.stream_intent.borrow_mut() = e.streaming;
                    {
                        let ws = crate::transcribe::is_ws_url(e.url.trim_start());
                        form.stream_sw.set_sensitive(ws);
                        form.stream_sw.set_active(e.streaming && ws);
                    }
                    let providers = ["OpenAI", "Groq", "Mistral", "Gemini", "Deepgram"];
                    form.provider_dd.set_selected(
                        providers.iter().position(|p| *p == e.provider).unwrap_or(0) as u32,
                    );
                    form.kind_dd.set_selected(match e.kind {
                        SttEntryKind::Cloud => 1,
                        _ => 0,
                    });
                    form.add_btn.set_label(tr("action.save"));
                    form.cancel_btn.set_visible(true);
                    *form.editing.borrow_mut() = Some(id.clone());
                });
            }
            card.append(&edit);
        }
        let del = Button::with_label(tr("action.remove"));
        del.set_sensitive(entries.len() > 1);
        del.set_tooltip_text(Some(tr("engine.keepone")));
        del.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            let list = list.downgrade();
            let form = form.downgrade();
            let alb = add_local.clone();
            let id = entry.id.clone();
            super::app::connect_confirmed(
                &del,
                tr("confirm.engine.title"),
                tr("confirm.engine.body"),
                tr("action.remove"),
                move |_| {
                    let Some(list) = list.upgrade() else {
                        return;
                    };
                    let Some(form) = form.upgrade() else {
                        return;
                    };
                    {
                        let mut st = state.settings.borrow_mut();
                        st.stt.entries.retain(|e| e.id != id);
                    }
                    persist_prefs(state.clone())();
                    fill_stt_chain(&list, &state, &form, &alb);
                },
            );
        }
        card.append(&del);
        list.append(&card);
    }
}

struct ServerProbeTask {
    net: crate::transcribe::ServerNet,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    result: std::sync::mpsc::Receiver<Vec<(String, bool)>>,
    _task: crate::runtime::SessionTask,
}
impl Drop for ServerProbeTask {
    fn drop(&mut self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

#[derive(Default)]
struct ServerStatusProbe {
    pending: Option<ServerProbeTask>,
    cached: Option<(crate::transcribe::ServerNet, Vec<(String, bool)>)>,
    last_attempt: Option<std::time::Instant>,
    failed: bool,
}

/// GTK only polls a page-owned result. TCP and procfs checks run on one
/// bounded owned task; unmapping or changing configuration cancels it.
fn refresh_server_widgets(
    state: &Rc<AppState>,
    status: &Label,
    stop: &Button,
    cold: &Button,
    probe: &RefCell<ServerStatusProbe>,
) {
    let s = state.settings.borrow();
    let local_on = s
        .stt
        .entries
        .iter()
        .any(|e| matches!(e.kind, crate::settings::SttEntryKind::Local) && e.enabled);
    let data_dir = state
        .db_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let net = crate::transcribe::server_net(&s.stt);
    drop(s);
    let mut probe = probe.borrow_mut();
    if !status.is_mapped() {
        probe.pending.take();
        probe.last_attempt = None;
        return;
    }
    if probe.cached.as_ref().is_some_and(|(old, _)| *old != net) {
        probe.cached = None;
        probe.last_attempt = None;
    }
    if probe
        .pending
        .as_ref()
        .is_some_and(|pending| pending.net != net)
    {
        probe.pending.take();
        probe.last_attempt = None;
    }
    if let Some(pending) = &probe.pending {
        match pending.result.try_recv() {
            Ok(running) => {
                probe.cached = Some((net.clone(), running));
                probe.pending.take();
                probe.failed = false;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                probe.pending.take();
                probe.failed = true;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }
    if probe.pending.is_none()
        && probe
            .last_attempt
            .is_none_or(|attempt| attempt.elapsed().as_secs() >= 2)
    {
        let (sender, result) = std::sync::mpsc::sync_channel(1);
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let active = cancel.clone();
        let worker_net = net.clone();
        probe.last_attempt = Some(std::time::Instant::now());
        match crate::runtime::spawn_session("server-status", move || {
            let running = crate::transcribe::server_status_while(&data_dir, &worker_net, &|| {
                !active.load(std::sync::atomic::Ordering::Acquire)
            });
            if !active.load(std::sync::atomic::Ordering::Acquire) {
                let _ = sender.try_send(running);
            }
        }) {
            Ok(task) => {
                probe.pending = Some(ServerProbeTask {
                    net,
                    cancel,
                    result,
                    _task: task,
                })
            }
            Err(_) => probe.failed = true,
        }
    }
    let Some((_, running)) = &probe.cached else {
        status.set_text(tr(if probe.failed {
            "server.unavailable"
        } else {
            "server.checking"
        }));
        stop.set_visible(false);
        cold.set_visible(false);
        return;
    };
    if running.is_empty() {
        status.set_text(tr("server.none"));
    } else {
        let names: Vec<String> = running
            .iter()
            .map(|(n, managed)| {
                if *managed {
                    n.clone()
                } else {
                    format!("{n} ({})", tr("server.external"))
                }
            })
            .collect();
        status.set_text(&format!("{}: {}", tr("server.running"), names.join(", ")));
    }
    // Stop only kills what we supervise (never a foreign process);
    // Cold start hides while anything answers; setup refuses a foreign port.
    let any_managed = running.iter().any(|(_, managed)| *managed);
    stop.set_visible(any_managed);
    cold.set_visible(running.is_empty());
    cold.set_sensitive(local_on && state.service_alive.get());
    stop.set_sensitive(state.service_alive.get());
    cold.set_tooltip_text(if local_on {
        None
    } else {
        Some(tr("coldstart.tip.disabled"))
    });
}

type RefreshCallback = Rc<std::cell::RefCell<Option<Rc<dyn Fn()>>>>;

fn stt_page(state: &Rc<AppState>) -> GtkBox {
    let root = section_box(tr("settings.stt"), tr("settings.stt.sub"));

    // ---- Engine: whisper / parakeet / cloud / self-hosted ----
    // Section visibility refreshes on engine change; the holder is
    // filled after all cards exist (handler below runs later).
    let refresh_holder: RefreshCallback = Rc::new(std::cell::RefCell::new(None));
    // ---- Engines chain: one local option at most (toggle) plus
    // ordered remote options (self-hosted / cloud). First enabled
    // entry wins, failures fall through.
    let (card, rows) = group(tr("group.sttchain"));
    let chain_list = GtkBox::new(Orientation::Vertical, 8);
    rows.append(&chain_list);
    // Add row: local button (iff absent) + remote form.
    let add_local_btn = Button::with_label(tr("stt.local.add"));
    rows.append(&add_local_btn);
    let remote_expander = gtk4::Expander::new(Some(tr("stt.remote.add")));
    remote_expander.set_widget_name("remote-engine-form");
    let remote_rows = GtkBox::new(Orientation::Vertical, 12);
    remote_rows.set_margin_top(12);
    remote_expander.set_child(Some(&remote_rows));
    rows.append(&remote_expander);
    // Remote form: kind picker + provider/url/model/key + Add/Save.
    let kind_dd = DropDown::new(
        Some(StringList::new(&[
            tr("opt.selfhostedserver"),
            tr("opt.cloud"),
        ])),
        None::<gtk4::Expression>,
    );
    kind_dd.set_valign(gtk4::Align::Center);
    {
        let (b, _) = row(tr("row.lmtype"));
        b.append(&kind_dd);
        remote_rows.append(&b);
    }
    let name_e = Entry::new();
    let url_e = Entry::new();
    let model_e = Entry::new();
    let key_e = Entry::new();
    let sh_key_e = Entry::new();
    let stream_sw = Switch::new();
    // Streaming intent (user's choice) vs display: the toggle shows
    // intent && ws-url, so http(s) reads off. Guard silences the
    // handler on programmatic flips.
    let stream_intent = Rc::new(RefCell::new(true));
    let stream_guard = Rc::new(RefCell::new(false));
    let provider_dd = DropDown::new(
        Some(StringList::new(&[
            "OpenAI", "Groq", "Mistral", "Gemini", "Deepgram",
        ])),
        None::<gtk4::Expression>,
    );
    for e in [&name_e, &url_e, &model_e, &key_e, &sh_key_e] {
        e.set_hexpand(true);
    }
    key_e.set_visibility(false);
    url_e.set_text("http://localhost:8000");
    url_e.connect_changed(|entry| entry.remove_css_class("error"));
    {
        let (b, _) = row(tr("row.name"));
        b.append(&name_e);
        remote_rows.append(&b);
    }
    let cloud_box = GtkBox::new(Orientation::Vertical, 8);
    {
        let (b, _) = row(tr("row.provider"));
        b.append(&provider_dd);
        cloud_box.append(&b);
        let (b, _) = row(tr("row.model"));
        b.append(&model_e);
        cloud_box.append(&b);
        let (b, _) = row(tr("row.apikey"));
        b.append(&key_e);
        cloud_box.append(&b);
    }
    remote_rows.append(&cloud_box);
    let url_box = GtkBox::new(Orientation::Vertical, 8);
    {
        let (b, _) = row(tr("row.serverurl"));
        b.append(&url_e);
        url_box.append(&b);
        let (b, _) = row(tr("row.apikey"));
        b.append(&sh_key_e);
        sh_key_e.set_visibility(false);
        url_box.append(&b);
        {
            let note = Label::new(Some(tr("stt.selfhost.key.note")));
            note.add_css_class("dim");
            note.set_xalign(0.0);
            note.set_wrap(true);
            url_box.append(&note);
        }
        let (b, _) = row(tr("row.streaming"));
        b.append(&stream_sw);
        stream_sw.set_valign(gtk4::Align::Center);
        stream_sw.set_active(true);
        url_box.append(&b);
        {
            let note = Label::new(Some(tr("stt.selfhost.stream.note")));
            note.add_css_class("dim");
            note.set_xalign(0.0);
            note.set_wrap(true);
            url_box.append(&note);
        }
    }
    remote_rows.append(&url_box);
    let add_btn = Button::with_label(tr("stt.remote.add"));
    add_btn.add_css_class("suggested-action");
    let cancel_btn = Button::with_label(tr("action.cancel"));
    cancel_btn.set_visible(false);
    {
        let brow = GtkBox::new(Orientation::Horizontal, 8);
        brow.set_halign(gtk4::Align::End);
        brow.append(&cancel_btn);
        brow.append(&add_btn);
        remote_rows.append(&brow);
    }
    root.append(&card);

    let form = SttForm {
        expander: remote_expander.clone(),
        name_e: name_e.clone(),
        url_e: url_e.clone(),
        model_e: model_e.clone(),
        key_e: key_e.clone(),
        stream_sw: stream_sw.clone(),
        stream_intent: stream_intent.clone(),
        sh_key_e: sh_key_e.clone(),
        provider_dd: provider_dd.clone(),
        kind_dd: kind_dd.clone(),
        add_btn: add_btn.clone(),
        cancel_btn: cancel_btn.clone(),
        editing: Rc::new(RefCell::new(None)),
    };
    fill_stt_chain(&chain_list, state, &form, &add_local_btn);
    {
        // Local option (at most one): appends + refills with form.
        let state = state.clone();
        let list = chain_list.downgrade();
        let form = form.downgrade();
        let alb0 = add_local_btn.clone();
        alb0.connect_clicked(move |alb0b| {
            let Some(list) = list.upgrade() else {
                return;
            };
            let Some(form) = form.upgrade() else {
                return;
            };
            {
                let mut lm = state.settings.borrow_mut();
                if !lm
                    .stt
                    .entries
                    .iter()
                    .any(|e| matches!(e.kind, crate::settings::SttEntryKind::Local))
                {
                    lm.stt.entries.push(crate::settings::SttChainEntry {
                        id: "stt-local".to_string(),
                        name: "Local".to_string(),
                        kind: crate::settings::SttEntryKind::Local,
                        enabled: true,
                        provider: String::new(),
                        url: String::new(),
                        model: String::new(),
                        api_key: String::new(),
                        streaming: true,
                    });
                }
            }
            persist_prefs(state.clone())();
            fill_stt_chain(&list, &state, &form, alb0b);
        });
    }
    {
        let cbx = cloud_box.clone();
        let ubx = url_box.clone();
        kind_dd.connect_selected_notify(move |d| {
            cbx.set_visible(d.selected() == 1);
            ubx.set_visible(d.selected() == 0);
        });
        cloud_box.set_visible(false);
    }
    {
        // Streaming toggle only applies to ws:// URLs (sherpa
        // protocol). http(s) always uses request/response, so the
        // toggle reads off there while intent is remembered.
        let sw = stream_sw.clone();
        let intent = stream_intent.clone();
        let guard = stream_guard.clone();
        let intent2 = intent.clone();
        let guard2 = guard.clone();
        let sync = move |url: &str| {
            let ws = crate::transcribe::is_ws_url(url.trim_start());
            sw.set_sensitive(ws);
            *guard.borrow_mut() = true;
            sw.set_active(*intent.borrow() && ws);
            *guard.borrow_mut() = false;
        };
        sync(&url_e.text());
        url_e.connect_changed(move |e| sync(&e.text()));
        stream_sw.connect_state_set(move |_, active| {
            if !*guard2.borrow() {
                *intent2.borrow_mut() = active;
            }
            gtk4::glib::Propagation::Proceed
        });
    }
    {
        // Cancel: drop the edit, reset the form.
        let cb = form.cancel_btn.clone();
        let form = form.downgrade();
        cb.connect_clicked(move |_| {
            let Some(form) = form.upgrade() else {
                return;
            };
            *form.editing.borrow_mut() = None;
            form.add_btn.set_label(tr("stt.remote.add"));
            form.cancel_btn.set_visible(false);
            form.name_e.set_text("");
            form.key_e.set_text("");
            form.sh_key_e.set_text("");
            *form.stream_intent.borrow_mut() = true;
            form.stream_sw.set_active(false);
            form.url_e.set_text("http://localhost:8000");
            form.model_e.set_text("");
            form.provider_dd.set_selected(0);
            form.kind_dd.set_selected(0);
        });
    }
    {
        let state = state.clone();
        let list = chain_list.downgrade();
        let ab = form.add_btn.clone();
        let form = form.downgrade();
        let alb1 = add_local_btn.clone();
        ab.connect_clicked(move |_| {
            let Some(list) = list.upgrade() else {
                return;
            };
            let Some(form) = form.upgrade() else {
                return;
            };
            use crate::settings::SttEntryKind;
            let cloud = form.kind_dd.selected() == 1;
            if cloud
                && (form.model_e.text().trim().is_empty() || form.key_e.text().trim().is_empty())
            {
                super::app::toast(&form.add_btn, tr("stt.required"));
                return;
            }
            if !cloud && !valid_server_url(&form.url_e.text(), true) {
                form.url_e.add_css_class("error");
                super::app::toast(&form.add_btn, tr("form.badurl"));
                return;
            }
            let provider = match form.provider_dd.selected() {
                1 => "Groq",
                2 => "Mistral",
                3 => "Gemini",
                4 => "Deepgram",
                _ => "OpenAI",
            };
            let mut entry = crate::settings::SttChainEntry {
                id: String::new(),
                name: form.name_e.text().to_string(),
                kind: if cloud {
                    SttEntryKind::Cloud
                } else {
                    SttEntryKind::SelfHosted
                },
                enabled: true,
                provider: provider.to_string(),
                url: form.url_e.text().trim().to_string(),
                model: form.model_e.text().trim().to_string(),
                api_key: if cloud {
                    form.key_e.text().to_string()
                } else {
                    form.sh_key_e.text().to_string()
                },
                streaming: *form.stream_intent.borrow(),
            };
            if !cloud {
                entry.provider.clear();
            }
            if entry.name.trim().is_empty() {
                entry.name = if cloud {
                    format!("{} ({})", entry.provider, entry.model)
                } else {
                    format!("Self-hosted ({})", entry.model)
                };
            }
            {
                let mut st = state.settings.borrow_mut();
                if let Some(id) = form.editing.borrow().as_ref() {
                    if let Some(e) = st.stt.entries.iter_mut().find(|e| &e.id == id) {
                        entry.id = e.id.clone();
                        entry.enabled = e.enabled;
                        *e = entry;
                    }
                } else {
                    entry.id = match connection_id("stt") {
                        Ok(id) => id,
                        Err(_) => {
                            drop(st);
                            super::app::toast(&form.add_btn, tr("feedback.savefailed"));
                            return;
                        }
                    };
                    st.stt.entries.push(entry);
                }
            }
            *form.editing.borrow_mut() = None;
            form.add_btn.set_label(tr("stt.remote.add"));
            form.cancel_btn.set_visible(false);
            form.name_e.set_text("");
            form.key_e.set_text("");
            form.sh_key_e.set_text("");
            *form.stream_intent.borrow_mut() = true;
            form.stream_sw.set_active(false);
            persist_prefs(state.clone())();
            fill_stt_chain(&list, &state, &form, &alb1);
            super::app::toast(&form.add_btn, tr("feedback.saved"));
        });
    }

    // ---- Registry model list with download/delete/select ----
    // Vendor pill tabs on top (Oruk / OpenAI / NVIDIA / Cohere),
    // mirroring OpenWhispr's local provider picker.
    let (card, rows) = group(tr("stt.models.title"));
    {
        let pill_row = gtk4::FlowBox::new();
        pill_row.set_max_children_per_line(5);
        pill_row.set_selection_mode(gtk4::SelectionMode::None);
        pill_row.set_halign(gtk4::Align::Start);
        pill_row.set_column_spacing(8);
        pill_row.set_row_spacing(8);
        rows.append(&pill_row);
        let vendors = ["Oruk", "OpenAI", "NVIDIA", "Cohere", "Whistle"];
        {
            use crate::settings::LocalSttProvider;
            let current = state.settings.borrow().stt.local_provider;
            let current_idx = match current {
                LocalSttProvider::Oruk => 0,
                LocalSttProvider::OpenAi => 1,
                LocalSttProvider::Nvidia => 2,
                LocalSttProvider::Cohere => 3,
                LocalSttProvider::Whistle => 4,
            };
            let mut first: Option<gtk4::ToggleButton> = None;
            for (i, name) in vendors.iter().enumerate() {
                let pill = gtk4::ToggleButton::with_label(name);
                if let Some(ref f) = first {
                    pill.set_group(Some(f));
                } else {
                    first = Some(pill.clone());
                }
                pill.set_active(i == current_idx);
                {
                    let state = state.clone();
                    let persist = persist_prefs(state.clone());
                    let refresh_holder = refresh_holder.clone();
                    pill.connect_toggled(move |p| {
                        if !p.is_active() {
                            return;
                        }
                        {
                            let mut s = state.settings.borrow_mut();
                            s.stt.local_provider = match i {
                                0 => LocalSttProvider::Oruk,
                                2 => LocalSttProvider::Nvidia,
                                3 => LocalSttProvider::Cohere,
                                4 => LocalSttProvider::Whistle,
                                _ => LocalSttProvider::OpenAi,
                            };
                        }
                        persist();
                        // Same full refresh as an engine change:
                        // runtime packs visibility + language list
                        // must follow the newly selected provider.
                        if let Some(r) = refresh_holder.borrow().as_ref() {
                            r();
                        }
                    });
                }
                pill_row.insert(&pill, -1);
            }
        }
    }
    let list = GtkBox::new(Orientation::Vertical, 8);
    // Capability overview; per-model badges below name the gaps.
    let dict_note = Label::new(Some(tr("stt.models.compatnote")));
    dict_note.add_css_class("dim");
    dict_note.set_xalign(0.0);
    dict_note.set_wrap(true);
    rows.append(&dict_note);
    rows.append(&list);
    root.append(&card);
    fill_stt_models(&list, state);
    {
        let state = state.clone();
        let list = list.downgrade();
        let weak_root = root.downgrade();
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
            let Some(list) = list.upgrade() else {
                return gtk4::glib::ControlFlow::Break;
            };
            if weak_root.upgrade().is_none() {
                return gtk4::glib::ControlFlow::Break;
            }
            fill_stt_models(&list, &state);
            gtk4::glib::ControlFlow::Continue
        });
    }

    // ---- Runtimes: whisper packs (+ backend picker) for the OpenAI
    // vendor, sherpa packs for Oruk / NVIDIA / Cohere. Toggled per
    // vendor in the refresh closure below.
    let (card, _rows) = group(tr("stt.gpu.title"));
    let whisper_box = GtkBox::new(Orientation::Vertical, 8);
    _rows.append(&whisper_box);
    let sherpa_box = GtkBox::new(Orientation::Vertical, 8);
    sherpa_box.set_widget_name("sherpa-runtime-controls");
    _rows.append(&sherpa_box);
    // Whistle brings its own engine: one binary, downloaded like a
    // pack (sha-verified), not a server runtime.
    let needle_box = GtkBox::new(Orientation::Vertical, 8);
    _rows.append(&needle_box);
    let needle_action = GtkBox::new(Orientation::Horizontal, 12);
    {
        let (b, _) = row(tr("stt.needle.engine"));
        b.append(&needle_action);
        needle_box.append(&b);
    }
    refresh_needle_action(&needle_action, state);
    let rows = whisper_box.clone();
    {
        let (b, _) = row(tr("stt.gpu.backend"));
        let backend = DropDown::new(
            Some(StringList::new(&[
                tr("stt.gpu.cpu"),
                tr("stt.gpu.cuda"),
                tr("stt.gpu.vulkan"),
            ])),
            None::<gtk4::Expression>,
        );
        {
            use crate::settings::GpuBackendSetting;
            let sel = match state.settings.borrow().stt.gpu_backend {
                GpuBackendSetting::Cpu => 0,
                GpuBackendSetting::Cuda => 1,
                GpuBackendSetting::Vulkan => 2,
            };
            backend.set_selected(sel);
        }
        {
            let state = state.clone();
            backend.connect_selected_notify(move |d| {
                use crate::settings::GpuBackendSetting;
                {
                    let mut s = state.settings.borrow_mut();
                    s.stt.gpu_backend = match d.selected() {
                        1 => GpuBackendSetting::Cuda,
                        2 => GpuBackendSetting::Vulkan,
                        _ => GpuBackendSetting::Cpu,
                    };
                }
                persist_prefs(state.clone())();
            });
        }
        b.append(&backend);
        rows.append(&b);
    }
    {
        let (b, _) = row(tr("stt.gpu.recommend"));
        let det = crate::stt::detect_gpu();
        let detail = match &det.nvidia_gpu {
            Some(gpu) => format!("NVIDIA: {gpu}"),
            None => "NVIDIA: none".to_string(),
        };
        let detail = format!(
            "{detail} · Vulkan tools: {}",
            if det.vulkan_tools { "yes" } else { "no" }
        );
        let l = Label::new(Some(
            format!("{}\n{detail}", crate::stt::gpu_recommendation(&det)).as_str(),
        ));
        l.add_css_class("dim");
        l.set_xalign(0.0);
        l.set_wrap(true);
        b.append(&l);
        rows.append(&b);
    }
    let gpu_list = GtkBox::new(Orientation::Vertical, 8);
    rows.append(&gpu_list);
    {
        // Sherpa runtime backend: CPU everywhere, CUDA where the box
        // has it. Manual choice, never auto-switched.
        let (b, _) = row(tr("stt.sherpa.backend"));
        let backend = DropDown::new(
            Some(StringList::new(&[
                tr("stt.sherpa.cpu"),
                tr("stt.sherpa.cuda"),
            ])),
            None::<gtk4::Expression>,
        );
        {
            use crate::settings::SherpaBackendSetting;
            backend.set_selected(match state.settings.borrow().stt.sherpa_backend {
                SherpaBackendSetting::Cpu => 0,
                SherpaBackendSetting::Cuda => 1,
            });
        }
        {
            let state = state.clone();
            backend.connect_selected_notify(move |d| {
                use crate::settings::SherpaBackendSetting;
                state.settings.borrow_mut().stt.sherpa_backend = match d.selected() {
                    1 => SherpaBackendSetting::Cuda,
                    _ => SherpaBackendSetting::Cpu,
                };
                persist_prefs(state.clone())();
            });
        }
        b.append(&backend);
        sherpa_box.append(&b);
    }
    let sherpa_list = GtkBox::new(Orientation::Vertical, 8);
    sherpa_box.append(&sherpa_list);
    root.append(&card);
    let gpu_card = card.clone();
    fill_gpu_packs(&gpu_list, state);
    {
        let state = state.clone();
        let gpu_list = gpu_list.clone();
        let weak_root = root.downgrade();
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
            if weak_root.upgrade().is_none() {
                return gtk4::glib::ControlFlow::Break;
            }
            fill_gpu_packs(&gpu_list, &state);
            gtk4::glib::ControlFlow::Continue
        });
    }
    fill_sherpa_packs(&sherpa_list, state);
    {
        let state = state.clone();
        let sherpa_list = sherpa_list.clone();
        let weak_root = root.downgrade();
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
            if weak_root.upgrade().is_none() {
                return gtk4::glib::ControlFlow::Break;
            }
            fill_sherpa_packs(&sherpa_list, &state);
            gtk4::glib::ControlFlow::Continue
        });
    }
    {
        let state = state.clone();
        let needle_action = needle_action.clone();
        let weak_root = root.downgrade();
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
            if weak_root.upgrade().is_none() {
                return gtk4::glib::ControlFlow::Break;
            }
            refresh_needle_action(&needle_action, &state);
            gtk4::glib::ControlFlow::Continue
        });
    }
    // ---- Voice activity (wired except live preview) ----
    let (card, rows) = group(tr("group.vad"));
    // Language options follow the selected model: whisper reads the
    // full upstream list, sherpa models their own list, Cohere has
    // no auto-detect so it offers its 14 codes only. Rebuilt on every
    // visibility refresh (selection changes swap the list).
    let lang_slot = GtkBox::new(Orientation::Horizontal, 8);
    rows.append(&lang_slot);
    let refresh_lang: Rc<dyn Fn()> = Rc::new({
        let state = state.clone();
        let lang_slot = lang_slot.clone();
        move || {
            while let Some(child) = lang_slot.first_child() {
                lang_slot.remove(&child);
            }
            let lang = stt_lang(&state);
            let mut opts = lang.opts;
            let stored = state.settings.borrow().stt.language.clone();
            let unsupported = !opts.contains(&stored);
            if unsupported {
                opts.push(stored.clone());
            }
            let (b, _) = row(tr("row.language"));
            let lang_drop = DropDown::new(
                Some(StringList::new(
                    &opts.iter().map(String::as_str).collect::<Vec<_>>(),
                )),
                None::<gtk4::Expression>,
            );
            lang_drop.set_widget_name("transcription-language-picker");
            lang_drop.set_selected(idx_of(
                &opts.iter().map(String::as_str).collect::<Vec<_>>(),
                &stored,
            ) as u32);
            lang_drop.set_sensitive(!lang.locked || unsupported);
            if lang.locked || unsupported {
                lang_drop.set_tooltip_text(Some(tr("stt.lang.locked.tip")));
            }
            {
                let state = state.clone();
                lang_drop.connect_selected_notify(move |d| {
                    let Some(code) = opts.get(d.selected() as usize).cloned() else {
                        return;
                    };
                    state.settings.borrow_mut().stt.language = code;
                    persist_prefs(state.clone())();
                });
            }
            b.append(&lang_drop);
            lang_slot.append(&b);
        }
    });
    refresh_lang();
    {
        let (b, _) = row(tr("row.vad"));
        let sw = Switch::new();
        sw.set_active(state.settings.borrow().stt.vad_enabled);
        sw.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            sw.connect_state_set(move |_, active| {
                state.settings.borrow_mut().stt.vad_enabled = active;
                persist_prefs(state.clone())();
                gtk4::glib::Propagation::Proceed
            });
        }
        b.append(&sw);
        rows.append(&b);
    }
    spin_row(
        &rows,
        tr("row.vadthreshold"),
        state.settings.borrow().stt.vad_threshold as f64,
        0.0,
        1.0,
        0.05,
    );
    // spin_row leaves the control insensitive (display-only helper);
    // enable it: this one is wired.
    if let Some(row_box) = rows.last_child() {
        let mut child = row_box.first_child();
        while let Some(c) = child {
            if let Ok(spin) = c.clone().downcast::<SpinButton>() {
                spin.set_sensitive(true);
                let state = state.clone();
                spin.connect_value_changed(move |s| {
                    state.settings.borrow_mut().stt.vad_threshold = s.value() as f32;
                    persist_prefs(state.clone())();
                });
            }
            child = c.next_sibling();
        }
    }
    {
        let (b, _) = row(tr("row.livepreview"));
        let sw = Switch::new();
        sw.set_active(state.settings.borrow().stt.live_preview);
        sw.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            sw.connect_state_set(move |_, active| {
                state.settings.borrow_mut().stt.live_preview = active;
                persist_prefs(state.clone())();
                gtk4::glib::Propagation::Proceed
            });
        }
        b.append(&sw);
        rows.append(&b);
    }
    // Streaming-only: file-based engines have nothing to preview.
    {
        let note = Label::new(Some(tr("stt.live.note")));
        note.add_css_class("dim");
        note.set_xalign(0.0);
        note.set_wrap(true);
        rows.append(&note);
    }
    root.append(&card);

    // ---- Servers: autostart, keep-warm, status, stop/cold-start ----
    // Only one local server runs at a time (one local
    // configuration): ensuring one stops any other. Autostart warms
    // it at service boot (only when a local option is enabled);
    // otherwise the first press records instantly while the server
    // spins up and the buffer catches up. Keep-warm stops idle
    // servers ("" = forever). Buttons reach the service through
    // one-shot DB commands (the GUI never owns the servers).
    let (card, rows) = group(tr("group.sttservers"));
    {
        let note = Label::new(Some(tr("servers.note")));
        note.add_css_class("dim");
        note.set_xalign(0.0);
        note.set_wrap(true);
        rows.append(&note);
    }
    {
        let (b, _) = row(tr("row.autostartserver"));
        let sw = Switch::new();
        sw.set_active(state.settings.borrow().stt.auto_start_server);
        sw.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            sw.connect_state_set(move |_, active| {
                state.settings.borrow_mut().stt.auto_start_server = active;
                persist_prefs(state.clone())();
                gtk4::glib::Propagation::Proceed
            });
        }
        b.append(&sw);
        rows.append(&b);
    }
    {
        let warm_opts = [tr("opt.forever"), "1m", "5m", "15m", "30m", "1h"];
        const WARM_VALUES: &[&str] = &["", "1m", "5m", "15m", "30m", "1h"];
        let (b, _) = row(tr("row.keepwarm"));
        let dd = DropDown::new(Some(StringList::new(&warm_opts)), None::<gtk4::Expression>);
        dd.set_selected(
            WARM_VALUES
                .iter()
                .position(|v| *v == state.settings.borrow().stt.keep_warm.as_str())
                .unwrap_or(0) as u32,
        );
        dd.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            dd.connect_selected_notify(move |d| {
                state.settings.borrow_mut().stt.keep_warm = WARM_VALUES
                    .get(d.selected() as usize)
                    .unwrap_or(&"")
                    .to_string();
                persist_prefs(state.clone())();
            });
        }
        b.append(&dd);
        rows.append(&b);
    }
    let server_probe = Rc::new(RefCell::new(ServerStatusProbe::default()));
    let (srv_status, srv_stop, srv_cold) = {
        let (b, _) = row(tr("row.serverstatus"));
        let status = Label::new(Some(tr("server.checking")));
        status.set_widget_name("local-server-status");
        status.set_xalign(0.0);
        status.set_hexpand(true);
        b.append(&status);
        let stop = Button::with_label(tr("action.stop"));
        stop.set_widget_name("local-server-stop");
        stop.set_visible(false);
        {
            let state = state.clone();
            stop.connect_clicked(move |_| {
                let data_dir = state
                    .db_path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_default();
                // Best effort locally (no-op in the GUI process) +
                // the command the service actually obeys.
                crate::transcribe::drop_servers(&data_dir);
                super::app::notify(tr(if state.store.set_server_cmd("stop").is_ok() {
                    "server.requested"
                } else {
                    "feedback.savefailed"
                }));
            });
        }
        b.append(&stop);
        let cold = Button::with_label(tr("action.coldstart"));
        cold.set_widget_name("local-server-start");
        cold.set_visible(false);
        {
            let state = state.clone();
            cold.connect_clicked(move |_| {
                super::app::notify(tr(if state.store.set_server_cmd("start").is_ok() {
                    "server.requested"
                } else {
                    "feedback.savefailed"
                }));
            });
        }
        b.append(&cold);
        rows.append(&b);
        (status, stop, cold)
    };
    // ---- Network subsection: one port + LAN exposure ----
    // One server runs at a time, so one port suffices: empty =
    // each engine's default, set = that port for whichever runs.
    {
        let t = Label::new(Some(tr("section.network")));
        t.add_css_class("section-title");
        t.set_xalign(0.0);
        t.set_margin_top(12);
        rows.append(&t);
    }
    {
        let note = Label::new(Some(tr("servers.net.note")));
        note.add_css_class("dim");
        note.set_xalign(0.0);
        note.set_wrap(true);
        rows.append(&note);
    }
    {
        let (b, _) = row(tr("row.netport"));
        let e = Entry::new();
        e.set_text(&state.settings.borrow().stt.server_port);
        e.set_placeholder_text(Some(tr("net.port.empty")));
        e.set_tooltip_text(Some(tr("net.port.tip")));
        e.set_hexpand(true);
        e.set_max_width_chars(8);
        {
            let state = state.clone();
            e.connect_changed(move |e| {
                // Digits only; anything else falls back to
                // default at resolve time anyway.
                let clean: String = e.text().chars().filter(|c| c.is_ascii_digit()).collect();
                if clean != e.text() {
                    e.set_text(&clean);
                    return;
                }
                state.settings.borrow_mut().stt.server_port = clean;
                persist_prefs(state.clone())();
            });
        }
        b.append(&e);
        rows.append(&b);
    }
    {
        let (b, _) = row(tr("row.lanexpose"));
        let sw = Switch::new();
        sw.set_active(state.settings.borrow().stt.lan_expose);
        sw.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            sw.connect_state_set(move |_, active| {
                state.settings.borrow_mut().stt.lan_expose = active;
                persist_prefs(state.clone())();
                gtk4::glib::Propagation::Proceed
            });
        }
        b.append(&sw);
        rows.append(&b);
        let note = Label::new(Some(tr("row.lanexpose.note")));
        note.add_css_class("dim");
        note.set_xalign(0.0);
        note.set_wrap(true);
        rows.append(&note);
    }
    append_collapsible(&root, &card, tr("group.sttservers"));
    {
        // Poll completed results without retaining removed controls. One
        // query per page runs at a time and is refreshed every two seconds.
        let state = state.clone();
        let srv_status = srv_status.downgrade();
        let srv_stop = srv_stop.downgrade();
        let srv_cold = srv_cold.downgrade();
        let server_probe = server_probe.clone();
        let weak_root = root.downgrade();
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let (Some(_root), Some(status), Some(stop), Some(cold)) = (
                weak_root.upgrade(),
                srv_status.upgrade(),
                srv_stop.upgrade(),
                srv_cold.upgrade(),
            ) else {
                return gtk4::glib::ControlFlow::Break;
            };
            refresh_server_widgets(&state, &status, &stop, &cold, &server_probe);
            gtk4::glib::ControlFlow::Continue
        });
    }

    // ---- Speculative streaming (whisper only, opt-in) ----
    // Background passes + local agreement while held, tail decode on
    // release. Only the whisper engine consumes this (sherpa-online
    // streams natively, everything else is after-release).
    let (card, rows) = group(tr("group.spec"));
    {
        let note = Label::new(Some(tr("spec.note")));
        note.add_css_class("warn");
        note.set_xalign(0.0);
        note.set_wrap(true);
        rows.append(&note);
    }
    {
        let (b, _) = row(tr("row.specenable"));
        let sw = Switch::new();
        sw.set_active(state.settings.borrow().stt.spec_enabled);
        sw.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            sw.connect_state_set(move |_, active| {
                state.settings.borrow_mut().stt.spec_enabled = active;
                persist_prefs(state.clone())();
                gtk4::glib::Propagation::Proceed
            });
        }
        b.append(&sw);
        rows.append(&b);
    }
    spin_row(
        &rows,
        tr("row.specupdate"),
        state.settings.borrow().stt.spec_update_secs as f64,
        0.5,
        5.0,
        0.5,
    );
    spin_row(
        &rows,
        tr("row.specoverlap"),
        state.settings.borrow().stt.spec_overlap_secs as f64,
        0.0,
        2.0,
        0.25,
    );
    // spin_row controls are display-only; wire both to settings.
    // The VAD spin above is already sensitive+wired: skip it.
    {
        let mut wired = 0;
        let mut child = rows.first_child();
        while let Some(row_box) = child {
            let mut c = row_box.first_child();
            while let Some(w) = c {
                if let Ok(spin) = w.clone().downcast::<SpinButton>()
                    && !spin.is_sensitive()
                {
                    spin.set_sensitive(true);
                    let state = state.clone();
                    let is_update = wired == 0;
                    spin.connect_value_changed(move |s| {
                        if is_update {
                            state.settings.borrow_mut().stt.spec_update_secs = s.value() as f32;
                        } else {
                            state.settings.borrow_mut().stt.spec_overlap_secs = s.value() as f32;
                        }
                        persist_prefs(state.clone())();
                    });
                    wired += 1;
                }
                c = w.next_sibling();
            }
            child = row_box.next_sibling();
        }
    }
    append_collapsible(&root, &card, tr("group.spec"));

    // ---- Local-config visibility ----
    // Runtimes/models/language always show: they configure the local
    // option; the chain above decides what actually runs.
    let refresh: Rc<dyn Fn()> = Rc::new({
        let state = state.clone();
        let gpu_card = gpu_card.clone();
        let whisper_box = whisper_box.clone();
        let sherpa_box = sherpa_box.clone();
        let needle_box = needle_box.clone();
        let srv_status = srv_status.downgrade();
        let srv_stop = srv_stop.downgrade();
        let srv_cold = srv_cold.downgrade();
        let server_probe = server_probe.clone();
        move || {
            {
                use crate::settings::LocalSttProvider;
                let s = state.settings.borrow();
                let whisper = matches!(s.stt.local_provider, LocalSttProvider::OpenAi);
                let sherpa = matches!(
                    s.stt.local_provider,
                    LocalSttProvider::Nvidia | LocalSttProvider::Oruk | LocalSttProvider::Cohere
                );
                let whistle = matches!(s.stt.local_provider, LocalSttProvider::Whistle);
                whisper_box.set_visible(whisper);
                sherpa_box.set_visible(sherpa);
                needle_box.set_visible(whistle);
                // No empty cards: runtimes show only when one of the
                // engine boxes applies.
                gpu_card.set_visible(whisper || sherpa || whistle);
                drop(s);
                // Servers: status + Stop iff one runs, else Cold start
                // (greyed out without an enabled local option).
                if let (Some(status), Some(stop), Some(cold)) =
                    (srv_status.upgrade(), srv_stop.upgrade(), srv_cold.upgrade())
                {
                    refresh_server_widgets(&state, &status, &stop, &cold, &server_probe);
                }
            }
            refresh_lang();
        }
    });
    refresh();
    refresh_holder.borrow_mut().replace(refresh);

    root
}

/// Ollama keep-alive options, parallel to KEEP_ALIVE_VALUES.
fn keep_alive_labels() -> [&'static str; 9] {
    [
        tr("lm.residency.default"),
        tr("lm.residency.forever"),
        tr("lm.residency.unload"),
        "5m",
        "15m",
        "30m",
        "2h",
        "5h",
        "24h",
    ]
}
const KEEP_ALIVE_VALUES: &[&str] = &["", "-1", "0", "5m", "15m", "30m", "2h", "5h", "24h"];
/// Ollama context window options, parallel to NUM_CTX_VALUES.
/// Small prompts need little context; lower reserves less memory.
fn num_ctx_labels() -> [&'static str; 6] {
    [
        tr("opt.thinkingdefault"),
        "512",
        "1024",
        "2048",
        "4096",
        "8192",
    ]
}
const NUM_CTX_VALUES: &[Option<u32>] = &[
    None,
    Some(512),
    Some(1024),
    Some(2048),
    Some(4096),
    Some(8192),
];

/// Shared handles of the add/edit form, so list rows can refill it.
#[derive(Clone)]
struct LmForm {
    expander: gtk4::Expander,
    name_e: Entry,
    url_e: Entry,
    ollama_model_e: Entry,
    cloud_model_e: Entry,
    key_e: Entry,
    cloud_key_e: Entry,
    provider_dd: DropDown,
    kind_dd: DropDown,
    keepalive_dd: DropDown,
    thinking_dd: DropDown,
    cloud_thinking_dd: DropDown,
    numctx_dd: DropDown,
    add_btn: Button,
    cancel_btn: Button,
    editing: Rc<RefCell<Option<String>>>,
}
weak_form!(LmForm, WeakLmForm, {
    expander: gtk4::Expander,
    name_e: Entry,
    url_e: Entry,
    ollama_model_e: Entry,
    cloud_model_e: Entry,
    key_e: Entry,
    cloud_key_e: Entry,
    provider_dd: DropDown,
    kind_dd: DropDown,
    keepalive_dd: DropDown,
    thinking_dd: DropDown,
    cloud_thinking_dd: DropDown,
    numctx_dd: DropDown,
    add_btn: Button,
    cancel_btn: Button,
}, {
    editing: Rc<RefCell<Option<String>>>,
});

fn lm_model_row(entry: &crate::settings::LlmEntry) -> GtkBox {
    let card = GtkBox::new(Orientation::Horizontal, 8);
    card.add_css_class("card");
    let info = GtkBox::new(Orientation::Vertical, 2);
    info.set_hexpand(true);
    let name = Label::new(Some(&entry.name));
    name.add_css_class("row-label");
    name.set_xalign(0.0);
    info.append(&name);
    let sub = Label::new(Some(&entry.summary()));
    sub.add_css_class("dim");
    sub.set_xalign(0.0);
    sub.set_wrap(true);
    info.append(&sub);
    card.append(&info);
    card
}

/// Cleanup selection and fallback order use the existing persisted entries.
fn fill_cleanup_chain(list: &GtkBox, state: &Rc<AppState>) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let entries = state.settings.borrow().language_models.entries.clone();
    if entries.is_empty() {
        let note = Label::new(Some(tr("cleanup.models.empty")));
        note.add_css_class("dim");
        note.set_xalign(0.0);
        note.set_wrap(true);
        list.append(&note);
    }
    for (pos, entry) in entries.iter().enumerate() {
        let card = lm_model_row(entry);
        // Enabled toggle.
        let sw = Switch::new();
        sw.set_active(entry.enabled);
        sw.update_property(&[gtk4::accessible::Property::Label(&format!(
            "{}: {}",
            tr("row.cleanup"),
            entry.name
        ))]);
        sw.set_tooltip_text(Some(tr("lm.usecleanup")));
        sw.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            let id = entry.id.clone();
            sw.connect_state_set(move |_, active| {
                if let Some(e) = state
                    .settings
                    .borrow_mut()
                    .language_models
                    .entries
                    .iter_mut()
                    .find(|e| e.id == id)
                {
                    e.enabled = active;
                }
                persist_prefs(state.clone())();
                gtk4::glib::Propagation::Proceed
            });
        }
        card.append(&sw);
        // Move up / down (priority order).
        for (label, delta) in [("↑", -1isize), ("↓", 1isize)] {
            let btn = Button::with_label(label);
            btn.set_tooltip_text(Some(tr(if delta < 0 {
                "action.moveup"
            } else {
                "action.movedown"
            })));
            btn.update_property(&[gtk4::accessible::Property::Label(tr(if delta < 0 {
                "action.moveup"
            } else {
                "action.movedown"
            }))]);
            btn.set_sensitive(if delta < 0 {
                pos > 0
            } else {
                pos + 1 < entries.len()
            });
            btn.set_valign(gtk4::Align::Center);
            {
                let state = state.clone();
                let list = list.downgrade();
                btn.connect_clicked(move |_| {
                    let Some(list) = list.upgrade() else {
                        return;
                    };
                    let mut lm = state.settings.borrow_mut();
                    let entries = &mut lm.language_models.entries;
                    let other = pos as isize + delta;
                    if other >= 0 && (other as usize) < entries.len() && (other as usize) != pos {
                        entries.swap(pos, other as usize);
                    }
                    drop(lm);
                    persist_prefs(state.clone())();
                    fill_cleanup_chain(&list, &state);
                });
            }
            card.append(&btn);
        }
        list.append(&card);
    }
}

/// Model connections are managed independently of their cleanup selection.
fn fill_lm_models(list: &GtkBox, state: &Rc<AppState>, form: &LmForm) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let entries = state.settings.borrow().language_models.entries.clone();
    if entries.is_empty() {
        let l = Label::new(Some(tr("lm.list.empty")));
        l.add_css_class("dim");
        l.set_xalign(0.0);
        l.set_wrap(true);
        list.append(&l);
        return;
    }
    for entry in &entries {
        let card = lm_model_row(entry);
        // (refills below use the outer form handle)
        let edit = Button::with_label(tr("action.edit"));
        edit.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            let form = form.downgrade();
            let id = entry.id.clone();
            edit.connect_clicked(move |_| {
                let Some(form) = form.upgrade() else {
                    return;
                };
                let entry = state
                    .settings
                    .borrow()
                    .language_models
                    .entries
                    .iter()
                    .find(|e| e.id == id)
                    .cloned();
                let Some(e) = entry else {
                    return;
                };
                form.expander.set_expanded(true);
                form.name_e.grab_focus();
                form.name_e.set_text(&e.name);
                form.url_e.set_text(&e.url);
                form.ollama_model_e.set_text(&e.model);
                form.cloud_model_e.set_text(&e.model);
                form.key_e.set_text(&e.api_key);
                form.cloud_key_e.set_text(&e.api_key);
                let tsel = match e.thinking {
                    Some(true) => 1,
                    Some(false) => 2,
                    None => 0,
                };
                form.thinking_dd.set_selected(tsel);
                form.cloud_thinking_dd.set_selected(tsel);
                form.numctx_dd.set_selected(
                    NUM_CTX_VALUES
                        .iter()
                        .position(|v| *v == e.num_ctx)
                        .unwrap_or(0) as u32,
                );
                form.keepalive_dd.set_selected(
                    KEEP_ALIVE_VALUES
                        .iter()
                        .position(|v| *v == e.keep_alive.trim())
                        .unwrap_or(0) as u32,
                );
                form.provider_dd.set_selected(match e.provider {
                    crate::settings::CloudLlm::Groq => 0,
                    crate::settings::CloudLlm::OpenRouter => 1,
                    crate::settings::CloudLlm::Claude => 3,
                    crate::settings::CloudLlm::Google => 4,
                    _ => 2,
                });
                form.kind_dd.set_selected(match e.kind {
                    crate::settings::LlmKind::Local => 1,
                    crate::settings::LlmKind::Cloud => 2,
                    _ => 0,
                });
                form.add_btn.set_label(tr("action.save"));
                form.cancel_btn.set_visible(true);
                *form.editing.borrow_mut() = Some(id.clone());
            });
        }
        card.append(&edit);
        // Remove.
        let del = Button::with_label(tr("action.remove"));
        del.set_sensitive(entries.len() > 1);
        del.set_tooltip_text(Some(tr("engine.keepone")));
        del.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            let list = list.downgrade();
            let form = form.downgrade();
            let id = entry.id.clone();
            super::app::connect_confirmed(
                &del,
                tr("confirm.engine.title"),
                tr("confirm.engine.body"),
                tr("action.remove"),
                move |_| {
                    let Some(list) = list.upgrade() else {
                        return;
                    };
                    let Some(form) = form.upgrade() else {
                        return;
                    };
                    {
                        let mut lm = state.settings.borrow_mut();
                        lm.language_models.entries.retain(|e| e.id != id);
                    }
                    persist_prefs(state.clone())();
                    fill_lm_models(&list, &state, &form);
                },
            );
        }
        card.append(&del);
        list.append(&card);
    }
}

fn language_models_page(state: &Rc<AppState>) -> GtkBox {
    use crate::settings::{CloudLlm, LlmEntry, LlmKind};
    let root = section_box(tr("settings.lm"), tr("settings.lm.sub"));

    // Add card: pick a type to configure, fill the form, Add.
    let (card, rows) = group(tr("group.lmadd"));
    let kind_dd = DropDown::new(
        Some(StringList::new(&[
            tr("opt.ollama"),
            tr("opt.localmodel.unavailable"),
            tr("opt.cloud"),
        ])),
        None::<gtk4::Expression>,
    );
    kind_dd.set_valign(gtk4::Align::Center);
    {
        let (b, _) = row(tr("row.lmtype"));
        b.append(&kind_dd);
        rows.append(&b);
    }
    // Shared form handles (single-parent rule: name/model rows live
    // in `rows`, kind extras in their boxes).
    let name_e = Entry::new();
    name_e.set_widget_name("lm-name");
    let ollama_model_e = Entry::new();
    ollama_model_e.set_widget_name("lm-ollama-model");
    let cloud_model_e = Entry::new();
    let url_e = Entry::new();
    let key_e = Entry::new();
    let cloud_key_e = Entry::new();
    let provider_dd = DropDown::new(
        Some(StringList::new(&[
            "Groq",
            "OpenRouter",
            "OpenAI",
            "Claude",
            "Google",
        ])),
        None::<gtk4::Expression>,
    );
    for e in [
        &name_e,
        &ollama_model_e,
        &cloud_model_e,
        &url_e,
        &key_e,
        &cloud_key_e,
    ] {
        e.set_hexpand(true);
    }
    key_e.set_visibility(false);
    cloud_key_e.set_visibility(false);
    url_e.set_text("http://127.0.0.1:11434");
    ollama_model_e.set_text("gemma4:e4b");
    cloud_model_e.set_text(CloudLlm::OpenAI.default_model());
    {
        let (b, _) = row(tr("row.name"));
        b.append(&name_e);
        rows.append(&b);
    }
    // Form boxes per kind, only one visible.
    let ollama_box = GtkBox::new(Orientation::Vertical, 8);
    let cloud_box = GtkBox::new(Orientation::Vertical, 8);
    let local_box = GtkBox::new(Orientation::Vertical, 8);
    let keepalive_dd = DropDown::new(
        Some(StringList::new(&keep_alive_labels())),
        None::<gtk4::Expression>,
    );
    keepalive_dd.set_valign(gtk4::Align::Center);
    // Tri-state: Default (omit, model decides) / On / Off.
    let thinking_opts = || {
        let dd = DropDown::new(
            Some(StringList::new(&[
                tr("opt.thinkingdefault"),
                tr("opt.thinkingon"),
                tr("opt.thinkingoff"),
            ])),
            None::<gtk4::Expression>,
        );
        dd.set_valign(gtk4::Align::Center);
        dd
    };
    let thinking_dd = thinking_opts();
    thinking_dd.set_widget_name("lm-ollama-thinking");
    let cloud_thinking_dd = thinking_opts();
    let numctx_dd = DropDown::new(
        Some(StringList::new(&num_ctx_labels())),
        None::<gtk4::Expression>,
    );
    numctx_dd.set_valign(gtk4::Align::Center);
    {
        let (b, _) = row(tr("row.ollamaurl"));
        b.append(&url_e);
        ollama_box.append(&b);
        let (b, _) = row(tr("row.ollamamodel"));
        b.append(&ollama_model_e);
        ollama_box.append(&b);
        // Optional bearer token (reverse-proxy setups); reuses the
        // entry's api_key field, empty = none.
        let (b, _) = row(tr("row.apikey"));
        b.append(&key_e);
        ollama_box.append(&b);
        let note = Label::new(Some(tr("lm.apikey.note")));
        note.add_css_class("dim");
        note.set_xalign(0.0);
        note.set_wrap(true);
        ollama_box.append(&note);
    }
    {
        let (b, _) = row(tr("row.apikey"));
        b.append(&cloud_key_e);
        cloud_box.append(&b);
        // Thinking (Claude extended thinking, Gemini dynamic
        // budget, Ollama think; other providers ignore it).
        {
            let (b, _) = row(tr("row.thinking"));
            b.append(&cloud_thinking_dd);
            cloud_box.append(&b);
        }
    }
    {
        let (b, _) = row(tr("row.keepalive"));
        b.append(&keepalive_dd);
        let update_btn = Button::with_label(tr("action.updatenow"));
        update_btn.set_valign(gtk4::Align::Center);
        b.append(&update_btn);
        ollama_box.append(&b);
        {
            let (b, _) = row(tr("row.thinking"));
            b.append(&thinking_dd);
            ollama_box.append(&b);
        }
        {
            let (b, _) = row(tr("row.numctx"));
            b.append(&numctx_dd);
            ollama_box.append(&b);
        }
        {
            let url_e = url_e.clone();
            let ollama_model_e = ollama_model_e.clone();
            let keepalive_dd = keepalive_dd.clone();
            let key_e = key_e.clone();
            update_btn.connect_clicked(move |button| {
                let url = url_e.text().trim().to_string();
                let model = ollama_model_e.text().trim().to_string();
                let token = key_e.text().trim().to_string();
                if !valid_server_url(&url, false) || model.is_empty() {
                    super::app::toast(button, tr("lm.required"));
                    return;
                }
                let keep = KEEP_ALIVE_VALUES
                    .get(keepalive_dd.selected() as usize)
                    .copied()
                    .unwrap_or("");
                let result = std::sync::Arc::new(std::sync::Mutex::new(None));
                let sender = result.clone();
                button.set_sensitive(false);
                button.set_label(tr("lm.applying"));
                // Empty messages load/unload the model without generating a reply.
                std::thread::spawn(move || {
                    let mut body =
                        serde_json::json!({"model": model, "stream": false, "messages": []});
                    if !keep.is_empty() {
                        body["keep_alive"] = match keep {
                            "-1" => serde_json::Value::from(-1),
                            "0" => serde_json::Value::from(0),
                            _ => serde_json::Value::String(keep.to_string()),
                        };
                    }
                    let success = reqwest::blocking::Client::builder()
                        .timeout(std::time::Duration::from_secs(60))
                        .build()
                        .and_then(|client| {
                            let mut request = client
                                .post(format!("{}/api/chat", url.trim_end_matches('/')))
                                .header("Content-Type", "application/json")
                                .body(body.to_string());
                            if !token.is_empty() {
                                request = request.bearer_auth(token);
                            }
                            request
                                .send()
                                .and_then(|response| response.error_for_status())
                        })
                        .is_ok();
                    *sender.lock().unwrap() = Some(success);
                });
                let button = button.clone();
                gtk4::glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
                    if let Some(success) = result.lock().unwrap().take() {
                        button.set_sensitive(true);
                        button.set_label(tr("action.updatenow"));
                        super::app::toast(
                            &button,
                            tr(if success {
                                "lm.applied"
                            } else {
                                "lm.applyfailed"
                            }),
                        );
                        gtk4::glib::ControlFlow::Break
                    } else {
                        gtk4::glib::ControlFlow::Continue
                    }
                });
            });
        }
    }
    {
        let (b, _) = row(tr("row.provider"));
        b.append(&provider_dd);
        cloud_box.append(&b);
    }
    {
        // Local has no fields: self-managed engines aren't
        // implemented yet. Text only, Add stays greyed.
        let note = Label::new(Some(tr("lm.local.note")));
        note.add_css_class("dim");
        note.set_xalign(0.0);
        note.set_wrap(true);
        local_box.append(&note);
    }
    {
        let (b, _) = row(tr("row.model"));
        b.append(&cloud_model_e);
        cloud_box.append(&b);
    }
    rows.append(&ollama_box);
    rows.append(&cloud_box);
    rows.append(&local_box);
    cloud_box.set_visible(false);
    local_box.set_visible(false);
    // Show the form for the picked kind.
    let add_btn = Button::with_label(tr("action.add"));
    add_btn.add_css_class("suggested-action");
    let cancel_btn = Button::with_label(tr("action.cancel"));
    cancel_btn.set_visible(false); // only while editing
    {
        let brow = GtkBox::new(Orientation::Horizontal, 8);
        brow.set_halign(gtk4::Align::End);
        brow.append(&cancel_btn);
        brow.append(&add_btn);
        rows.append(&brow);
    }
    let form_expander = append_collapsible(&root, &card, tr("group.lmadd"));
    {
        let ollama_box = ollama_box.clone();
        let cloud_box = cloud_box.clone();
        let local_box = local_box.clone();
        let add_btn = add_btn.clone();
        kind_dd.connect_selected_notify(move |d| {
            ollama_box.set_visible(d.selected() == 0);
            cloud_box.set_visible(d.selected() == 2);
            local_box.set_visible(d.selected() == 1);
            // Local can't be added yet: text only.
            add_btn.set_sensitive(d.selected() != 1);
        });
    }
    {
        let me = cloud_model_e.clone();
        provider_dd.connect_selected_notify(move |d| {
            let provider = match d.selected() {
                0 => CloudLlm::Groq,
                1 => CloudLlm::OpenRouter,
                3 => CloudLlm::Claude,
                4 => CloudLlm::Google,
                _ => CloudLlm::OpenAI,
            };
            me.set_text(provider.default_model());
        });
        provider_dd.set_selected(2); // OpenAI default
        cloud_model_e.set_text(CloudLlm::OpenAI.default_model());
    }
    let (card, model_rows) = group(tr("group.lmmodels"));
    let list = GtkBox::new(Orientation::Vertical, 8);
    model_rows.append(&list);
    let cleanup_link = Button::with_label(tr("action.cleanupsettings"));
    cleanup_link.add_css_class("flat");
    cleanup_link.set_halign(gtk4::Align::Start);
    cleanup_link.connect_clicked(|button| {
        super::app::navigate(button, "settings", "Text Cleanup");
    });
    model_rows.append(&cleanup_link);
    root.insert_child_after(&card, root.first_child().as_ref());

    // Form bundle shared with the list rows (Edit refills it).
    let form = LmForm {
        expander: form_expander.clone(),
        name_e: name_e.clone(),
        url_e: url_e.clone(),
        ollama_model_e: ollama_model_e.clone(),
        cloud_model_e: cloud_model_e.clone(),
        key_e: key_e.clone(),
        cloud_key_e: cloud_key_e.clone(),
        provider_dd: provider_dd.clone(),
        kind_dd: kind_dd.clone(),
        keepalive_dd: keepalive_dd.clone(),
        thinking_dd: thinking_dd.clone(),
        cloud_thinking_dd: cloud_thinking_dd.clone(),
        numctx_dd: numctx_dd.clone(),
        add_btn: add_btn.clone(),
        cancel_btn: cancel_btn.clone(),
        editing: Rc::new(RefCell::new(None)),
    };
    fill_lm_models(&list, state, &form);
    // Re-entering this page reflects cleanup priority changes without losing form drafts.
    {
        let state = state.clone();
        let form = form.downgrade();
        list.connect_map(move |list| {
            if let Some(form) = form.upgrade() {
                fill_lm_models(list, &state, &form);
            }
        });
    }
    // Cancel: drop the edit, reset the form to add-mode defaults.
    {
        let cb = form.cancel_btn.clone();
        let form = form.downgrade();
        cb.connect_clicked(move |_| {
            let Some(form) = form.upgrade() else {
                return;
            };
            *form.editing.borrow_mut() = None;
            form.add_btn.set_label(tr("action.add"));
            form.cancel_btn.set_visible(false);
            form.name_e.set_text("");
            form.key_e.set_text("");
            form.cloud_key_e.set_text("");
            form.thinking_dd.set_selected(0);
            form.cloud_thinking_dd.set_selected(0);
            form.numctx_dd.set_selected(0);
            form.url_e.set_text("http://127.0.0.1:11434");
            form.ollama_model_e.set_text("gemma4:e4b");
            form.keepalive_dd.set_selected(0);
            form.provider_dd.set_selected(2);
            form.cloud_model_e
                .set_text(CloudLlm::OpenAI.default_model());
            form.kind_dd.set_selected(0);
        });
    }
    {
        let state = state.clone();
        let list = list.downgrade();
        let add_btn = form.add_btn.clone();
        let form = form.downgrade();
        add_btn.connect_clicked(move |_| {
            let Some(list) = list.upgrade() else {
                return;
            };
            let Some(form) = form.upgrade() else {
                return;
            };
            let kind_dd = form.kind_dd.clone();
            let provider_dd = form.provider_dd.clone();
            let ollama_model_e = form.ollama_model_e.clone();
            let cloud_model_e = form.cloud_model_e.clone();
            let name_e = form.name_e.clone();
            let url_e = form.url_e.clone();
            let key_e = form.key_e.clone();
            let cloud_key_e = form.cloud_key_e.clone();
            let thinking_dd = form.thinking_dd.clone();
            let cloud_thinking_dd = form.cloud_thinking_dd.clone();
            let numctx_dd = form.numctx_dd.clone();
            let editing = form.editing.clone();
            let ab = form.add_btn.clone();
            let cb = form.cancel_btn.clone();
            let kind = match kind_dd.selected() {
                1 => LlmKind::Local,
                2 => LlmKind::Cloud,
                _ => LlmKind::Ollama,
            };
            let provider = match provider_dd.selected() {
                0 => CloudLlm::Groq,
                1 => CloudLlm::OpenRouter,
                3 => CloudLlm::Claude,
                4 => CloudLlm::Google,
                _ => CloudLlm::OpenAI,
            };
            let model = if kind == LlmKind::Local {
                // No fields for local yet: nothing to configure.
                String::new()
            } else if kind == LlmKind::Cloud {
                cloud_model_e.text().to_string()
            } else {
                ollama_model_e.text().to_string()
            };
            if kind == LlmKind::Local
                || model.trim().is_empty()
                || (kind == LlmKind::Cloud && cloud_key_e.text().trim().is_empty())
                || (kind == LlmKind::Ollama && !valid_server_url(&url_e.text(), false))
            {
                super::app::toast(&ab, tr("lm.required"));
                return;
            }
            let keep_alive = KEEP_ALIVE_VALUES
                .get(form.keepalive_dd.selected() as usize)
                .copied()
                .unwrap_or("")
                .to_string();
            let mut entry = LlmEntry {
                id: String::new(),
                name: name_e.text().to_string(),
                kind,
                enabled: true,
                url: url_e.text().to_string(),
                model,
                provider,
                api_key: if kind == LlmKind::Cloud {
                    cloud_key_e.text().to_string()
                } else {
                    // Ollama bearer token (optional, empty = none).
                    key_e.text().to_string()
                },
                keep_alive,
                num_ctx: NUM_CTX_VALUES
                    .get(numctx_dd.selected() as usize)
                    .copied()
                    .flatten(),
                thinking: match if kind == LlmKind::Cloud {
                    cloud_thinking_dd.selected()
                } else {
                    thinking_dd.selected()
                } {
                    1 => Some(true),
                    2 => Some(false),
                    _ => None,
                },
            };
            if entry.name.trim().is_empty() {
                entry.name = match kind {
                    LlmKind::Ollama => format!("Ollama ({})", entry.model),
                    LlmKind::Local => "Local engine".to_string(),
                    LlmKind::Cloud => {
                        format!("{} ({})", entry.provider.name(), entry.model)
                    }
                };
            }
            {
                let mut lm = state.settings.borrow_mut();
                if let Some(id) = editing.borrow().as_ref() {
                    // Save: keep id/enabled/position.
                    if let Some(e) = lm.language_models.entries.iter_mut().find(|e| &e.id == id) {
                        entry.id = e.id.clone();
                        entry.enabled = e.enabled;
                        *e = entry;
                    }
                } else {
                    entry.id = match connection_id("lm") {
                        Ok(id) => id,
                        Err(_) => {
                            drop(lm);
                            super::app::toast(&ab, tr("feedback.savefailed"));
                            return;
                        }
                    };
                    lm.language_models.entries.push(entry);
                }
            }
            *editing.borrow_mut() = None;
            ab.set_label(tr("action.add"));
            cb.set_visible(false);
            name_e.set_text("");
            key_e.set_text("");
            persist_prefs(state.clone())();
            fill_lm_models(&list, &state, &form);
            super::app::toast(&ab, tr("feedback.saved"));
        });
    }

    root
}

fn update_prompt_summary(state: &Rc<AppState>, status: &Label, preview: &Label) {
    let settings = state.settings.borrow();
    let stored = &settings.language_models.cleanup_prompt;
    let is_default = stored.trim().is_empty()
        || stored.trim() == crate::transcribe::DEFAULT_CLEANUP_PROMPT.trim();
    status.set_text(tr(if is_default {
        "prompt.default"
    } else {
        "prompt.custom"
    }));
    preview.set_text(&if is_default {
        tr("prompt.default.summary").to_string()
    } else {
        stored
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(240)
            .collect()
    });
}

/// Editing uses a private draft; only Save updates the persisted prompt.
fn open_prompt_editor(from: &Button, state: &Rc<AppState>, status: &Label, preview: &Label) {
    use libadwaita::prelude::*;
    let Some(parent) = from
        .root()
        .and_then(|root| root.downcast::<gtk4::Window>().ok())
    else {
        return;
    };
    let editor = libadwaita::Window::new();
    editor.set_widget_name("cleanup-prompt-editor");
    editor.add_css_class("mywhisper-app");
    editor.set_title(Some(tr("prompt.edit.title")));
    editor.set_application(parent.application().as_ref());
    editor.set_transient_for(Some(&parent));
    editor.set_destroy_with_parent(true);
    editor.set_modal(true);
    editor.set_default_size(
        (parent.width() - 48).clamp(480, 880),
        (parent.height() - 48).clamp(360, 800),
    );
    let layout = GtkBox::new(Orientation::Vertical, 0);
    let header = libadwaita::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    header.set_title_widget(Some(&Label::new(Some(tr("prompt.edit.title")))));
    let cancel = Button::with_label(tr("action.cancel"));
    cancel.set_widget_name("prompt-editor-cancel");
    let weak_editor = editor.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(editor) = weak_editor.upgrade() {
            editor.close();
        }
    });
    header.pack_start(&cancel);
    let save = Button::with_label(tr("action.save"));
    save.set_widget_name("prompt-editor-save");
    save.add_css_class("suggested-action");
    save.set_sensitive(false);
    header.pack_end(&save);
    layout.append(&header);

    let content = GtkBox::new(Orientation::Vertical, 16);
    content.set_margin_top(20);
    content.set_margin_bottom(20);
    content.set_margin_start(20);
    content.set_margin_end(20);
    content.set_vexpand(true);
    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroll.set_vexpand(true);
    let text = TextView::new();
    text.set_widget_name("expanded-cleanup-prompt");
    text.update_property(&[gtk4::accessible::Property::Label(tr("row.customprompt"))]);
    text.set_wrap_mode(gtk4::WrapMode::WordChar);
    text.set_pixels_above_lines(3);
    text.set_pixels_below_lines(3);
    text.set_pixels_inside_wrap(2);
    let stored = state
        .settings
        .borrow()
        .language_models
        .cleanup_prompt
        .clone();
    let initial = crate::transcribe::effective_prompt(&stored).to_string();
    let buffer = text.buffer();
    buffer.set_text(&initial);
    buffer.set_enable_undo(true);
    scroll.set_child(Some(&text));
    content.append(&scroll);
    let error = Label::new(None);
    error.add_css_class("warn");
    error.set_wrap(true);
    error.set_xalign(0.0);
    error.set_visible(false);
    content.append(&error);
    let actions = GtkBox::new(Orientation::Horizontal, 12);
    let use_default = Button::with_label(tr("action.usedefault"));
    use_default.set_widget_name("prompt-editor-default");
    use_default.set_visible(initial != crate::transcribe::DEFAULT_CLEANUP_PROMPT);
    let draft = buffer.clone();
    use_default.connect_clicked(move |_| draft.set_text(crate::transcribe::DEFAULT_CLEANUP_PROMPT));
    actions.append(&use_default);
    let full_screen = Button::with_label(tr("action.fullscreen"));
    full_screen.set_widget_name("prompt-editor-fullscreen");
    full_screen.set_hexpand(true);
    full_screen.set_halign(gtk4::Align::End);
    let weak_editor = editor.downgrade();
    full_screen.connect_clicked(move |_| {
        if let Some(editor) = weak_editor.upgrade() {
            if editor.is_fullscreen() {
                editor.unfullscreen();
            } else {
                editor.fullscreen();
            }
        }
    });
    let weak_button = full_screen.downgrade();
    editor.connect_fullscreened_notify(move |editor| {
        if let Some(button) = weak_button.upgrade() {
            button.set_label(tr(if editor.is_fullscreen() {
                "action.exitfullscreen"
            } else {
                "action.fullscreen"
            }));
        }
    });
    actions.append(&full_screen);
    content.append(&actions);
    let clamp = libadwaita::Clamp::new();
    clamp.set_maximum_size(760);
    clamp.set_tightening_threshold(760);
    clamp.set_vexpand(true);
    clamp.set_child(Some(&content));
    layout.append(&clamp);
    editor.set_content(Some(&layout));
    let weak_save = save.downgrade();
    let weak_default = use_default.downgrade();
    buffer.connect_changed(move |buffer| {
        let draft = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
        if let Some(save) = weak_save.upgrade() {
            save.set_sensitive(draft != initial);
        }
        if let Some(default) = weak_default.upgrade() {
            default.set_visible(draft != crate::transcribe::DEFAULT_CLEANUP_PROMPT);
        }
    });
    {
        let state = state.clone();
        let status = status.clone();
        let preview = preview.clone();
        let weak_editor = editor.downgrade();
        save.connect_clicked(move |_| {
            let mut settings = state.settings.borrow().clone();
            settings.language_models.cleanup_prompt = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();
            match state.store.save_settings(&settings) {
                Ok(()) => {
                    *state.settings.borrow_mut() = settings;
                    update_prompt_summary(&state, &status, &preview);
                    if let Some(editor) = weak_editor.upgrade() {
                        editor.close();
                    }
                }
                Err(failure) => {
                    eprintln!("[app] prompt save failed: {failure}");
                    error.set_text(tr("feedback.savefailed"));
                    error.set_visible(true);
                }
            }
        });
    }
    let keys = EventControllerKey::new();
    keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let weak_editor = editor.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk4::gdk::Key::Escape {
            if let Some(editor) = weak_editor.upgrade() {
                editor.close();
            }
            gtk4::glib::Propagation::Stop
        } else {
            gtk4::glib::Propagation::Proceed
        }
    });
    editor.add_controller(keys);
    editor.present();
    text.grab_focus();
}

fn text_cleanup_page(state: &Rc<AppState>) -> GtkBox {
    let root = section_box(tr("settings.cleanup"), tr("settings.cleanup.sub"));
    let (card, rows) = group(tr("group.cleanup"));
    {
        // Master switch for post-processing (wired): on = chain runs.
        let (b, _) = row(tr("row.cleanup"));
        let sw = Switch::new();
        sw.set_widget_name("cleanup-enabled");
        sw.update_property(&[gtk4::accessible::Property::Label(tr("row.cleanup"))]);
        sw.set_active(state.settings.borrow().language_models.cleanup_enabled);
        sw.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            sw.connect_state_set(move |_, active| {
                state.settings.borrow_mut().language_models.cleanup_enabled = active;
                persist_prefs(state.clone())();
                gtk4::glib::Propagation::Proceed
            });
        }
        b.append(&sw);
        rows.append(&b);
        let instructions = GtkBox::new(Orientation::Horizontal, 16);
        let info = GtkBox::new(Orientation::Vertical, 4);
        info.set_hexpand(true);
        let status = Label::new(None);
        status.set_widget_name("cleanup-prompt-status");
        status.set_xalign(0.0);
        status.add_css_class("row-label");
        let preview = Label::new(None);
        preview.set_widget_name("cleanup-prompt-preview");
        preview.set_xalign(0.0);
        preview.set_wrap(true);
        preview.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
        preview.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        preview.set_lines(2);
        preview.set_max_width_chars(48);
        preview.add_css_class("dim");
        update_prompt_summary(state, &status, &preview);
        info.append(&status);
        info.append(&preview);
        instructions.append(&info);
        let edit = Button::with_label(tr("action.editprompt"));
        edit.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            edit.connect_clicked(move |button| {
                open_prompt_editor(button, &state, &status, &preview)
            });
        }
        instructions.append(&edit);
        rows.append(&instructions);
    }
    root.append(&card);

    let (card, rows) = group(tr("group.lmchain"));
    let note = Label::new(Some(tr("cleanup.priority.note")));
    note.add_css_class("dim");
    note.set_xalign(0.0);
    note.set_wrap(true);
    rows.append(&note);
    let list = GtkBox::new(Orientation::Vertical, 8);
    list.set_widget_name("cleanup-models");
    rows.append(&list);
    fill_cleanup_chain(&list, state);
    {
        let state = state.clone();
        // Models added, edited or removed on the other page are immediately available here.
        list.connect_map(move |list| fill_cleanup_chain(list, &state));
    }
    let manage = Button::with_label(tr("action.managelm"));
    manage.add_css_class("flat");
    manage.set_halign(gtk4::Align::Start);
    manage.connect_clicked(|button| {
        super::app::navigate(button, "settings", "Language Models");
    });
    rows.append(&manage);
    root.append(&card);

    root
}

fn retention_choices(current: u32, audio: bool) -> (Vec<u32>, Vec<String>, u32) {
    let mut days = vec![1, 7, 14, 30, 60, 90];
    let mut labels: Vec<String> = [
        "opt.older1",
        "opt.older7",
        "opt.older14",
        "opt.older30",
        "opt.older60",
        "opt.older90",
    ]
    .iter()
    .map(|key| tr(key).to_string())
    .collect();
    if !audio {
        days.insert(0, 0);
        labels.insert(0, tr("opt.never").to_string());
    }
    let selected = match days.iter().position(|days| *days == current) {
        Some(selected) => selected,
        None => {
            days.push(current);
            // Zero audio retention has never meant forever; match pruning's
            // one-day policy without silently rewriting the stored value.
            labels.push(if audio && current == 0 {
                tr("opt.older1").to_string()
            } else {
                tr("opt.oldercustom").replace("{n}", &current.to_string())
            });
            days.len() - 1
        }
    };
    (days, labels, selected as u32)
}

fn privacy_page(state: &Rc<AppState>) -> GtkBox {
    let root = section_box(tr("settings.privacy"), tr("settings.privacy.sub"));

    let (card, rows) = group(tr("group.audioret"));
    // No Never here by design (clips grow fast). Default 30 days.
    {
        let (b, _) = row(tr("row.keepaudio"));
        let sw = Switch::new();
        sw.set_active(state.settings.borrow().privacy.keep_audio_recordings);
        sw.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            sw.connect_state_set(move |_, active| {
                state.settings.borrow_mut().privacy.keep_audio_recordings = active;
                persist_prefs(state.clone())();
                gtk4::glib::Propagation::Proceed
            });
        }
        b.append(&sw);
        rows.append(&b);
    }
    {
        let (b, _) = row(tr("row.autodeleteaudio"));
        let (days, labels, selected) =
            retention_choices(state.settings.borrow().privacy.audio_retention_days, true);
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let dd = DropDown::new(Some(StringList::new(&labels)), None::<gtk4::Expression>);
        dd.set_widget_name("audio-retention-picker");
        dd.set_selected(selected);
        dd.set_valign(gtk4::Align::Center);
        // Retention continues to govern old clips when new recording is off.
        {
            let state = state.clone();
            dd.connect_selected_notify(move |d| {
                if let Some(days) = days.get(d.selected() as usize) {
                    state.settings.borrow_mut().privacy.audio_retention_days = *days;
                    persist_prefs(state.clone())();
                }
            });
        }
        b.append(&dd);
        rows.append(&b);
    }
    {
        let (b, _) = row(tr("row.storedaudio"));
        let open = Button::with_label(tr("action.openfolder"));
        {
            let state = state.clone();
            open.connect_clicked(move |_| {
                let data_dir = state
                    .db_path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_default();
                let dir = crate::audio::audio_vault_dir(&data_dir);
                let _ = std::fs::create_dir_all(&dir);
                if let Err(e) = std::process::Command::new("xdg-open")
                    .arg(&dir)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    eprintln!("[privacy] open folder: {e}");
                }
            });
        }
        b.append(&open);
        let clear = Button::with_label(tr("action.clearallaudio"));
        {
            let state = state.clone();
            super::app::connect_confirmed(
                &clear,
                tr("confirm.audio.title"),
                tr("confirm.audio.body"),
                tr("action.clearallaudio"),
                move |button| {
                    let db_path = state.db_path.clone();
                    file_action(
                        button,
                        move || {
                            crate::store::Store::open(&db_path)
                                .and_then(|store| store.clear_audio())
                                .map(|_| ())
                                .map_err(std::io::Error::other)
                        },
                        "feedback.cleared",
                    );
                },
            );
        }
        b.append(&clear);
        rows.append(&b);
    }
    root.append(&card);

    let (card, rows) = group(tr("group.transcripts"));
    // Keep dictation history (wired): when off, dictations still
    // paste, they just aren't stored.
    {
        let (b, _) = row(tr("row.keephistory"));
        let sw = Switch::new();
        sw.set_active(state.settings.borrow().privacy.history_enabled);
        sw.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            sw.connect_state_set(move |_, active| {
                state.settings.borrow_mut().privacy.history_enabled = active;
                persist_prefs(state.clone())();
                gtk4::glib::Propagation::Proceed
            });
        }
        b.append(&sw);
        rows.append(&b);
    }
    // Auto-delete transcripts after N days, 0 = keep forever (wired).
    {
        let (b, _) = row(tr("row.autodelete"));
        let (days, labels, selected) = retention_choices(
            state.settings.borrow().privacy.transcript_auto_delete_days,
            false,
        );
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let dd = DropDown::new(Some(StringList::new(&labels)), None::<gtk4::Expression>);
        dd.set_widget_name("transcript-retention-picker");
        dd.set_selected(selected);
        dd.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            dd.connect_selected_notify(move |d| {
                if let Some(days) = days.get(d.selected() as usize) {
                    state
                        .settings
                        .borrow_mut()
                        .privacy
                        .transcript_auto_delete_days = *days;
                    persist_prefs(state.clone())();
                }
            });
        }
        // Existing history still expires while storing new history is off.
        b.append(&dd);
        rows.append(&b);
    }
    // Clear-all (tombstones survive for sync safety).
    {
        let (b, _) = row(tr("row.storedtranscripts"));
        let clear = Button::with_label(tr("action.clearalltranscripts"));
        {
            let state = state.clone();
            super::app::connect_confirmed(
                &clear,
                tr("confirm.history.title"),
                tr("confirm.history.body"),
                tr("action.clearalltranscripts"),
                move |_| match state.store.clear_history() {
                    Ok(n) => {
                        eprintln!("[privacy] cleared {n} transcripts");
                        super::app::notify(tr("feedback.cleared"));
                        crate::net::sync_after_change_at_epoch(
                            state.db_path.clone(),
                            state.creds.clone(),
                            state.store.sync_epoch(),
                        );
                    }
                    Err(e) => {
                        eprintln!("[privacy] clear transcripts: {e}");
                        super::app::notify(tr("feedback.deletefailed"));
                    }
                },
            );
        }
        b.append(&clear);
        rows.append(&b);
    }
    // Keep midway-cancelled dictations as discarded rows (wired).
    {
        let (b, _) = row(tr("row.savediscarded"));
        let sw = Switch::new();
        sw.set_active(
            state
                .settings
                .borrow()
                .privacy
                .save_discarded_transcriptions,
        );
        sw.set_valign(gtk4::Align::Center);
        {
            let state = state.clone();
            sw.connect_state_set(move |_, active| {
                state
                    .settings
                    .borrow_mut()
                    .privacy
                    .save_discarded_transcriptions = active;
                persist_prefs(state.clone())();
                gtk4::glib::Propagation::Proceed
            });
        }
        b.append(&sw);
        rows.append(&b);
    }
    root.append(&card);

    root
}

fn system_page(state: &Rc<AppState>) -> GtkBox {
    let root = section_box(tr("settings.system"), tr("settings.system.sub"));

    let (card, rows) = group(tr("group.version"));
    let (b, _) = row(tr("row.appversion"));
    let v = Label::new(Some(crate::APP_VERSION));
    v.add_css_class("dim");
    b.append(&v);
    rows.append(&b);
    // Update status, consistent with the Home notice: state from the
    // startup `gh` check (tri-state), manual re-check on demand.
    {
        use crate::update::UpdateState;
        fn update_text() -> String {
            match crate::update::update_state() {
                UpdateState::Newer(v) => trf("sys.update.new", &[("version", &v)]),
                UpdateState::Current => {
                    trf("sys.update.current", &[("version", crate::APP_VERSION)])
                }
                UpdateState::Unknown => tr("sys.update.unknown").to_string(),
            }
        }
        let (b, _) = row(tr("row.appupdate"));
        let status = Label::new(Some(update_text().as_str()));
        status.add_css_class("dim");
        status.set_xalign(0.0);
        status.set_hexpand(true);
        b.append(&status);
        let check = Button::with_label(tr("action.check"));
        {
            let status = status.clone();
            // Poll the background check; give up as failed after ~12 s.
            check.connect_clicked(move |_| {
                crate::update::check_for_updates();
                status.set_text(tr("sys.update.checking"));
                let status = status.clone();
                let tries = std::rc::Rc::new(std::cell::Cell::new(0));
                gtk4::glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
                    tries.set(tries.get() + 1);
                    match crate::update::update_state() {
                        UpdateState::Unknown if tries.get() < 12 => {
                            gtk4::glib::ControlFlow::Continue
                        }
                        UpdateState::Unknown => {
                            status.set_text(tr("sys.update.failed"));
                            gtk4::glib::ControlFlow::Break
                        }
                        _ => {
                            status.set_text(update_text().as_str());
                            gtk4::glib::ControlFlow::Break
                        }
                    }
                });
            });
        }
        b.append(&check);
        rows.append(&b);
    }
    root.append(&card);

    // The cache lives here, not configured: models + runtimes under
    // the data dir. Display + actions only.
    let data_dir = state
        .db_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let (card, rows) = group(tr("group.modelcache"));
    {
        let (b, _) = row(tr("row.cachepath"));
        let v = Label::new(Some(data_dir.to_string_lossy().as_ref()));
        v.add_css_class("dim");
        v.set_xalign(0.0);
        v.set_wrap(true);
        v.set_max_width_chars(48);
        v.set_hexpand(true);
        b.append(&v);
        rows.append(&b);
    }
    let (b, _) = row(tr("row.cacheactions"));
    let open = Button::with_label(tr("action.openfolder"));
    {
        let data_dir = data_dir.clone();
        open.connect_clicked(move |_| {
            let _ = std::fs::create_dir_all(&data_dir);
            if let Err(e) = std::process::Command::new("xdg-open")
                .arg(&data_dir)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                eprintln!("[system] open cache: {e}");
            }
        });
    }
    b.append(&open);
    let clear = Button::with_label(tr("action.clearcache"));
    {
        let data_dir = data_dir.clone();
        super::app::connect_confirmed(
            &clear,
            tr("confirm.cache.title"),
            tr("confirm.cache.body"),
            tr("action.clearcache"),
            move |button| {
                let data_dir = data_dir.clone();
                file_action(
                    button,
                    move || crate::stt::clear_download_cache(&data_dir),
                    "feedback.cleared",
                );
            },
        );
    }
    b.append(&clear);
    rows.append(&b);
    root.append(&card);

    let (card, rows) = group(tr("group.danger"));
    let (b, _) = row(tr("row.resetall"));
    let retry = state.store.settings_warning() == Some("settings.reset.incomplete");
    let reset = Button::with_label(tr(if state.reset_busy.get() {
        "settings.reset.pending"
    } else if retry {
        "settings.reset.retry"
    } else {
        "action.resetdots"
    }));
    reset.set_widget_name("reset-all");
    reset.set_sensitive(!state.reset_busy.get());
    {
        let state = state.clone();
        super::app::connect_confirmed(
            &reset,
            tr(if retry {
                "settings.reset.retry"
            } else {
                "confirm.reset.title"
            }),
            tr(if retry {
                "settings.reset.retry.body"
            } else {
                "confirm.reset.body"
            }),
            tr(if retry {
                "settings.reset.retry"
            } else {
                "action.resetdots"
            }),
            move |button| {
                begin_reset(state.clone(), retry);
                if state.reset_busy.get() {
                    button.set_label(tr("settings.reset.pending"));
                    button.set_sensitive(false);
                    if let Some(page) = button.ancestor(Stack::static_type()) {
                        page.set_sensitive(false);
                        if let Some(content) = page.ancestor(Stack::static_type()) {
                            content.set_sensitive(false);
                        }
                    }
                }
            },
        );
    }
    b.append(&reset);
    rows.append(&b);
    root.append(&card);

    root
}

/// Recover only a previously authorized reset; opening a window cannot
/// initiate a new reset or wipe edits after a completed reset transaction.
pub fn resume_pending_reset(state: &Rc<AppState>) {
    if state.store.settings_warning() == Some("settings.reset.incomplete") {
        begin_reset(state.clone(), true);
    }
}

fn begin_reset(state: Rc<AppState>, resume_only: bool) {
    if state.reset_busy.replace(true) {
        return;
    }
    let job = match crate::reset::start(state.db_path.clone(), resume_only) {
        Ok(job) => job,
        Err(error) => {
            state.reset_busy.set(false);
            eprintln!("[reset] admission: {error}");
            super::app::notify(tr("settings.reset.failed"));
            return;
        }
    };
    crate::transcribe::invalidate_all_capture();
    gtk4::glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
        let mut rebuild = false;
        let mut finished = None;
        loop {
            match job.events.try_recv() {
                Ok(crate::reset::Event::Snapshot(settings, warning)) => {
                    if let Err(error) = state.store.adopt_settings_baseline(&settings, warning) {
                        eprintln!("[reset] snapshot: {error}");
                    }
                    crate::i18n::set_lang(crate::i18n::UiLang::parse(
                        &settings.preferences.ui_language,
                    ));
                    apply_theme(settings.preferences.theme);
                    *state.settings.borrow_mut() = *settings;
                    rebuild = true;
                }
                Ok(crate::reset::Event::Finished(result)) => {
                    finished = Some(result);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    finished = Some(Err(
                        "reset worker disconnected; recovery remains available".into()
                    ));
                    break;
                }
            }
        }
        if finished.is_some() {
            state.reset_busy.set(false);
            rebuild = true;
        }
        if rebuild {
            crate::i18n::rebuild_ui();
        }
        match finished {
            Some(Ok(())) => {
                super::app::notify(tr("feedback.cleared"));
                gtk4::glib::ControlFlow::Break
            }
            Some(Err(error)) => {
                eprintln!("[reset] {error}");
                super::app::notify(tr(
                    if state.store.settings_warning() == Some("settings.reset.incomplete") {
                        "settings.reset.incomplete"
                    } else {
                        "settings.reset.failed"
                    },
                ));
                gtk4::glib::ControlFlow::Break
            }
            None => gtk4::glib::ControlFlow::Continue,
        }
    });
}

// ---------------------------------------------------------------- page

pub fn page(state: &Rc<AppState>, initial_section: &str) -> GtkBox {
    let root = GtkBox::new(Orientation::Horizontal, 0);
    root.set_sensitive(!state.reset_busy.get());

    let nav = GtkBox::new(Orientation::Vertical, 4);
    nav.add_css_class("sidebar");
    nav.add_css_class("settings-nav");
    nav.set_size_request(156, -1);
    let title = Label::new(Some(tr("settings.title")));
    title.add_css_class("section-title");
    title.set_xalign(0.0);
    title.set_margin_start(10);
    title.set_margin_bottom(14);
    nav.append(&title);
    root.append(&nav);

    let stack = Stack::new();
    stack.set_widget_name("settings-pages");
    stack.set_hhomogeneous(false);
    stack.set_vhomogeneous(false);
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    root.append(&stack);

    // Theme writes straight into the model and applies live.
    let on_theme: Rc<dyn Fn(Theme)> = Rc::new({
        let state = state.clone();
        move |theme: Theme| {
            state.settings.borrow_mut().preferences.theme = theme;
            persist_prefs(state.clone())();
            apply_theme(theme);
            log_theme_state();
            eprintln!("[app] theme -> {theme:?}");
        }
    });
    // Internal section IDs stay English (stack names, deep-link
    // flags); only the nav button labels translate.
    let sections = [
        ("Preferences", preferences_page(state, &on_theme)),
        ("Hotkeys", hotkeys_page(state)),
        ("Speech to Text", stt_page(state)),
        ("Text Cleanup", text_cleanup_page(state)),
        ("Language Models", language_models_page(state)),
        ("Clipboard", clipboard_page(state)),
        ("Privacy & Data", privacy_page(state)),
        ("System", system_page(state)),
    ];

    fn section_label(id: &str) -> &'static str {
        match id {
            "Preferences" => tr("settings.preferences"),
            "Hotkeys" => tr("settings.hotkeys"),
            "Clipboard" => tr("settings.clipboard"),
            "Speech to Text" => tr("settings.stt"),
            "Text Cleanup" => tr("settings.cleanup"),
            "Language Models" => tr("settings.lm"),
            "Privacy & Data" => tr("settings.privacy"),
            "System" => tr("settings.system"),
            _ => Box::leak(id.to_string().into_boxed_str()),
        }
    }

    let mut buttons: Vec<(gtk4::glib::WeakRef<Button>, String)> = Vec::new();
    let mut pending: Vec<(Button, String)> = Vec::new();
    for (id, widget) in sections {
        let scrolled = gtk4::ScrolledWindow::new();
        scrolled.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        let clamp = libadwaita::Clamp::new();
        clamp.set_maximum_size(840);
        clamp.set_child(Some(&widget));
        scrolled.set_child(Some(&clamp));
        scrolled.set_vexpand(true);
        stack.add_named(&scrolled, Some(id));
        let b = Button::with_label(section_label(id));
        if let Some(label) = b.child().and_then(|child| child.downcast::<Label>().ok()) {
            label.set_wrap(true);
            label.set_max_width_chars(18);
            label.set_justify(gtk4::Justification::Center);
        }
        b.set_halign(gtk4::Align::Fill);
        nav.append(&b);
        buttons.push((b.downgrade(), id.to_string()));
        pending.push((b, id.to_string()));
    }

    let note = Label::new(Some(tr("settings.saved")));
    note.set_wrap(true);
    note.set_max_width_chars(18);
    note.set_xalign(0.5);
    note.set_justify(gtk4::Justification::Center);
    note.add_css_class("settings-note");
    note.set_margin_top(8);
    nav.append(&note);
    let set_active = Rc::new(move |active: &str| {
        for (b, id) in buttons.iter() {
            let Some(b) = b.upgrade() else {
                continue;
            };
            if id == active {
                b.add_css_class("active");
            } else {
                b.remove_css_class("active");
            }
        }
    });

    let highlight = set_active.clone();
    stack.connect_visible_child_name_notify(move |stack| {
        if let Some(name) = stack.visible_child_name() {
            highlight(&name);
        }
    });
    for (b, name) in pending {
        let stack = stack.downgrade();
        let set_active = set_active.clone();
        b.connect_clicked(move |_| {
            let Some(stack) = stack.upgrade() else {
                return;
            };
            stack.set_visible_child_name(&name);
            set_active(&name);
        });
    }
    let initial = match initial_section {
        "Hotkeys" | "Clipboard" | "Speech to Text" | "Text Cleanup" | "Language Models"
        | "Privacy & Data" | "System" => initial_section,
        _ => "Preferences",
    };
    set_active(initial);
    stack.set_visible_child_name(initial);

    root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_conflicts_use_parsed_keys_instead_of_spelling() {
        assert!(same_shortcut("ctrl+alt+d", "Alt+Control+D"));
        assert!(same_shortcut("Super+Ctrl", "Control+Super"));
        assert!(!same_shortcut("F13", "F14"));
        assert!(!same_shortcut("", ""));
        assert!(!same_shortcut("Ctrl+D", "Ctrl+Shift+D"));
    }

    #[test]
    fn provider_urls_require_a_host_and_supported_transport() {
        assert!(valid_server_url("http://localhost:11434", false));
        assert!(valid_server_url("https://speech.example.com/api", false));
        assert!(valid_server_url("wss://speech.example.com", true));
        assert!(!valid_server_url("wss://speech.example.com", false));
        for value in [
            "",
            "localhost:11434",
            "file:///tmp/model",
            "https://",
            "ftp://host",
        ] {
            assert!(!valid_server_url(value, true), "{value}");
        }
    }

    #[test]
    fn capture_maps_keys_to_combo_tokens() {
        use gtk4::gdk::Key as K;
        assert_eq!(capture_modifier(K::Control_L), Some(0));
        assert_eq!(capture_modifier(K::Super_R), Some(3));
        assert_eq!(capture_modifier(K::F13), None);
        assert_eq!(capture_token(K::F13).as_deref(), Some("F13"));
        assert_eq!(capture_token(K::space).as_deref(), Some("Space"));
        assert_eq!(capture_token(K::Return).as_deref(), Some("Enter"));
        // Single chars normalize to uppercase.
        assert_eq!(capture_token(K::d).as_deref(), Some("D"));
        // Modifiers are not keys.
        assert_eq!(capture_token(K::Shift_L), None);
        // Every emitted token must survive the listener parser.
        for token in ["F13", "Space", "Enter", "D", "Escape", "Tab"] {
            let combo = combo_string(&[true, false, true, false], Some(token));
            assert!(
                crate::hotkey::parse_combo(&combo).is_some(),
                "roundtrip failed for {combo}"
            );
        }
        // Modifier-only combo is valid (matches OpenWhispr support).
        assert!(
            crate::hotkey::parse_combo(&combo_string(&[true, false, false, true], None)).is_some()
        );
    }
}
