//! Settings page: sub-nav + sections.
//!
//! Every control initializes from [`AppSettings`] but nothing is
//! wired yet: flipping a switch changes no model and persists
//! nothing. Wiring later = connect each control to its model field
//! (+ persistence + backend apply).

use gtk4::prelude::*;
use gtk4::{
    Box as GtkBox, Button, DropDown, Entry, EventControllerKey, Label, Orientation, SpinButton,
    Stack, StringList, Switch, TextView,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::app::{apply_theme, log_theme_state, AppState};
use crate::clipboard::{copy_backend_deps, paste_backend_deps, probe, BackendStatus};
use crate::i18n::{tr, trf};
use crate::settings::{AppSettings, CopyBackend, PasteBackend, PasteCombo, Theme};

// ---------------------------------------------------------------- helpers

fn section_box(title: &str, subtitle: &str) -> GtkBox {
    let root = GtkBox::new(Orientation::Vertical, 12);
    root.add_css_class("page");
    let t = Label::new(Some(title));
    t.add_css_class("page-title");
    t.set_xalign(0.0);
    let s = Label::new(Some(subtitle));
    s.add_css_class("page-subtitle");
    s.set_xalign(0.0);
    s.set_wrap(true);
    s.set_max_width_chars(80);
    root.append(&t);
    root.append(&s);
    root
}

fn group(title: &str) -> (GtkBox, GtkBox) {
    let card = GtkBox::new(Orientation::Vertical, 4);
    card.add_css_class("card");
    let t = Label::new(Some(title));
    t.add_css_class("section-title");
    t.set_xalign(0.0);
    card.append(&t);
    let rows = GtkBox::new(Orientation::Vertical, 8);
    card.append(&rows);
    (card, rows)
}

fn row(label: &str) -> (GtkBox, Label) {
    let b = GtkBox::new(Orientation::Horizontal, 12);
    let l = Label::new(Some(label));
    l.add_css_class("row-label");
    l.set_xalign(0.0);
    l.set_hexpand(true);
    b.append(&l);
    (b, l)
}

fn switch_row(parent: &GtkBox, label: &str, active: bool) {
    let (b, _) = row(label);
    let sw = Switch::new();
    sw.set_active(active);
    sw.set_sensitive(false); // not wired yet
    sw.set_valign(gtk4::Align::Center);
    b.append(&sw);
    parent.append(&b);
}

fn combo_row(parent: &GtkBox, label: &str, options: &[&str], active: usize) {
    let (b, _) = row(label);
    let d = DropDown::new(Some(StringList::new(options)), None::<gtk4::Expression>);
    d.set_selected(active as u32);
    d.set_sensitive(false); // not wired yet
    b.append(&d);
    parent.append(&b);
}

fn entry_row(parent: &GtkBox, label: &str, text: &str, secret: bool) {
    let (b, _) = row(label);
    let e = Entry::new();
    e.set_text(text);
    e.set_visibility(!secret);
    e.set_sensitive(false); // not wired yet
    e.set_hexpand(true);
    e.set_max_width_chars(28);
    b.append(&e);
    parent.append(&b);
}

fn spin_row(parent: &GtkBox, label: &str, value: f64, min: f64, max: f64, step: f64) {
    let (b, _) = row(label);
    let s = SpinButton::with_range(min, max, step);
    s.set_value(value);
    s.set_sensitive(false); // not wired yet
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

/// Translated day-count options, parallel to the hardcoded day arrays.
fn retention_opts() -> [&'static str; 7] {
    [
        tr("opt.disabled"),
        tr("opt.day1"),
        tr("opt.day7"),
        tr("opt.day14"),
        tr("opt.day30"),
        tr("opt.day60"),
        tr("opt.day90"),
    ]
}

fn delete_opts() -> [&'static str; 7] {
    [
        tr("opt.forever"),
        tr("opt.day1"),
        tr("opt.day7"),
        tr("opt.day14"),
        tr("opt.day30"),
        tr("opt.day60"),
        tr("opt.day90"),
    ]
}

// ---------------------------------------------------------------- sections

/// Save settings without touching the hotkey listener (pill and
/// preference changes propagate to the service through the DB).
fn persist_prefs(state: Rc<AppState>) -> Rc<dyn Fn()> {
    Rc::new(move || {
        let settings = state.settings.borrow();
        if let Err(e) = state.store.save_settings(&settings) {
            eprintln!("[app] prefs save failed: {e}");
        }
    })
}

fn preferences_page(state: &Rc<AppState>, on_theme: &Rc<dyn Fn(Theme)>) -> GtkBox {
    let root = section_box(tr("settings.preferences"), tr("settings.preferences.sub"));
    let p = state.settings.borrow().preferences.clone();

    let (card, rows) = group(tr("group.appearance"));
    // The theme picker is wired: it writes the model and applies the
    // variant immediately. All other controls below are display-only
    // until their backends land.
    let (b, _) = row(tr("row.theme"));
    let theme_drop = DropDown::new(
        Some(StringList::new(&[
            tr("opt.system"),
            tr("opt.light"),
            tr("opt.dark"),
        ])),
        None::<gtk4::Expression>,
    );
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
    root.append(&card);

    let (card, rows) = group(tr("group.language"));
    {
        let (b, _) = row(tr("row.uilang"));
        let lang_drop = DropDown::new(
            Some(StringList::new(&[tr("opt.system"), "English", "Español"])),
            None::<gtk4::Expression>,
        );
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
    }
    root.append(&card);

    let (card, rows) = group(tr("group.dictionary"));
    switch_row(&rows, tr("row.autolearn"), p.auto_learn_dictionary);
    root.append(&card);

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
            Some(StringList::new(&["wl-copy"])),
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
                state.settings.borrow_mut().preferences.copy_backend = match dd.selected() {
                    _ => CopyBackend::WlCopy,
                };
                persist_prefs(state.clone())();
            });
        }
        b.append(&dd);
        rows.append(&b);
        rows.append(&dep_note(&format!(
            "{} — {}",
            copy_backend_deps(),
            avail(status.wl_copy)
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
    let (add_row, _) = row(tr("row.addcombo"));
    let add_entry = Entry::new();
    add_entry.set_hexpand(true);
    add_entry.set_placeholder_text(Some("F13"));
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
        let list = list.clone();
        let persist = persist.clone();
        let add_entry = add_entry.clone();
        let add_mode = add_mode.clone();
        add_btn.connect_clicked(move |_| {
            let raw = add_entry.text().trim().to_string();
            if raw.is_empty() {
                return;
            }
            if crate::hotkey::parse_combo(&raw).is_none() {
                add_entry.set_text("");
                add_entry.set_placeholder_text(Some(tr("hotkeys.invalidcombo")));
                return;
            }
            let mode = match add_mode.selected() {
                1 => crate::settings::ActivationMode::Tap,
                2 => crate::settings::ActivationMode::Smart,
                _ => crate::settings::ActivationMode::Push,
            };
            {
                let mut s = state.settings.borrow_mut();
                if !s.hotkeys.dictation_hotkeys.iter().any(|b| b.combo == raw) {
                    s.hotkeys
                        .dictation_hotkeys
                        .push(crate::settings::HotkeyBinding { combo: raw, mode });
                }
            }
            add_entry.set_text("");
            persist();
            refill_hotkey_list(&list, &state, &persist);
        });
    }
    add_row.append(&add_btn);
    let record_btn = Button::with_label(tr("hotkeys.record"));
    record_btn.set_tooltip_text(Some(tr("hotkeys.recordhint")));
    add_row.append(&record_btn);
    rows.append(&add_row);

    let (b, _) = row(tr("row.resettodefault"));
    let reset = Button::with_label(tr("action.reset"));
    {
        let state = state.clone();
        let list = list.clone();
        let persist = persist.clone();
        reset.connect_clicked(move |_| {
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
    root.append(&smart_card);

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
    for binding in state.settings.borrow().hotkeys.effective_hotkeys() {
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
        {
            let state = state.clone();
            let list = list.clone();
            let persist = persist.clone();
            let combo = binding.combo.clone();
            remove.connect_clicked(move |_| {
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
    record_btn: Button,
    add_entry: Entry,
}

impl Capture {
    fn stop(&self) {
        self.capturing.set(false);
        self.held.replace([false; 4]);
        self.pressed.replace([false; 4]);
        self.record_btn.set_label(tr("hotkeys.record"));
        self.add_entry.set_sensitive(true);
        self.add_entry.set_placeholder_text(Some("F13"));
    }

    fn begin(&self) {
        // Suspend the global listener while recording so the very
        // keypress being recorded doesn't also fire dictation.
        crate::hotkey::stop();
        self.capturing.set(true);
        self.live_label();
        self.add_entry.set_sensitive(false);
        self.add_entry
            .set_placeholder_text(Some(tr("hotkeys.pressnow")));
    }

    fn live_label(&self) {
        let held = *self.held.borrow();
        let shown = combo_string(&held, None);
        let keys = if shown.is_empty() {
            tr("hotkeys.presskeys").to_string()
        } else {
            shown
        };
        self.record_btn
            .set_label(crate::i18n::trf("hotkeys.recording", &[("keys", &keys)]).as_str());
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
            self.record_btn.set_label(tr("hotkeys.invalid"));
            return;
        }
        {
            let mut s = state.settings.borrow_mut();
            if !s.hotkeys.dictation_hotkeys.iter().any(|b| b.combo == combo) {
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
        record_btn: record_btn.clone(),
        add_entry: add_entry.clone(),
    });
    {
        let cap = cap.clone();
        let state = state.clone();
        record_btn.connect_clicked(move |_| {
            if cap.capturing.get() {
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
        let list = list.clone();
        let persist = persist.clone();
        let add_mode = add_mode.clone();
        ctl.connect_key_pressed(move |_, key, keycode, mod_state| {
            if !cap.capturing.get() {
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
                    cap.record_btn.set_label(tr("hotkeys.unsupported"));
                }
            }
            gtk4::glib::Propagation::Stop
        });
    }
    {
        let cap = cap.clone();
        let state = state.clone();
        let list = list.clone();
        let persist = persist.clone();
        let add_mode = add_mode.clone();
        ctl.connect_key_released(move |_, key, _, _| {
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
            "{}:{}:{};",
            m.id,
            stt::is_installed(&data_dir, m),
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
        let sub = Label::new(Some(&format!("{} · {}", m.description, m.size_label)));
        sub.add_css_class("dim");
        sub.set_xalign(0.0);
        info.append(&sub);
        card.append(&info);

        if let Some(dl) = stt::download_state(m.id) {
            let col = GtkBox::new(Orientation::Vertical, 4);
            match &dl.phase {
                stt::DlPhase::Failed(e) => {
                    let l = Label::new(Some(&trf("stt.dl.failed", &[("err", e)])));
                    l.add_css_class("dim");
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
                let state = state.clone();
                let id = m.id;
                del.connect_clicked(move |_| {
                    if let Some(data_dir) = state.db_path.parent() {
                        if let Some(entry) = stt::find_model(id) {
                            let _ = stt::delete_model(data_dir, entry);
                        }
                    }
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
            "{}:{}:{};",
            pack.id,
            stt::sherpa_installed(&data_dir, pack),
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
        let name = Label::new(Some(pack.name));
        name.add_css_class("row-label");
        name.set_xalign(0.0);
        info.append(&name);
        let sub = Label::new(Some(&format!("{} · {}", pack.description, pack.size_label)));
        sub.add_css_class("dim");
        sub.set_xalign(0.0);
        info.append(&sub);
        card.append(&info);

        if let Some(dl) = stt::download_state(pack.id) {
            let col = GtkBox::new(Orientation::Vertical, 4);
            match &dl.phase {
                stt::DlPhase::Failed(e) => {
                    let l = Label::new(Some(&trf("stt.dl.failed", &[("err", e)])));
                    l.add_css_class("dim");
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
                let state = state.clone();
                let id = pack.id;
                del.connect_clicked(move |_| {
                    if let Some(data_dir) = state.db_path.parent() {
                        if let Some(pack) = stt::SHERPA_PACKS.iter().find(|p| p.id == id) {
                            let _ = stt::delete_sherpa_pack(data_dir, pack);
                        }
                    }
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
            "{}:{}:{};",
            pack.id,
            stt::gpu_installed(&data_dir, pack),
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
        let name = Label::new(Some(pack.name));
        name.add_css_class("row-label");
        name.set_xalign(0.0);
        info.append(&name);
        // Single selection method: the backend dropdown above. Rows
        // only show install state + Download/Delete.
        let installed = stt::gpu_installed(&data_dir, pack);
        let sub = Label::new(Some(&format!(
            "{} · {}{}",
            pack.description,
            pack.size_label,
            if installed { " · Installed" } else { "" }
        )));
        sub.add_css_class("dim");
        sub.set_xalign(0.0);
        info.append(&sub);
        card.append(&info);

        if let Some(dl) = stt::download_state(pack.id) {
            let col = GtkBox::new(Orientation::Vertical, 4);
            match &dl.phase {
                stt::DlPhase::Failed(e) => {
                    let l = Label::new(Some(&trf("stt.dl.failed", &[("err", e)])));
                    l.add_css_class("dim");
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
                let state = state.clone();
                let id = pack.id;
                del.connect_clicked(move |_| {
                    if let Some(data_dir) = state.db_path.parent() {
                        if let Some(pack) = stt::GPU_PACKS.iter().find(|p| p.id == id) {
                            let _ = stt::delete_gpu_pack(data_dir, pack);
                        }
                    }
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

/// Transcription language options for the currently selected
/// model: "auto" + family list (whisper: full upstream list;
/// parakeet: per-model list), except Cohere which has no
/// auto-detect and offers its 14 codes only.
fn stt_lang_options(state: &Rc<AppState>) -> Vec<String> {
    use crate::settings::{LocalSttProvider, SttMode};
    use crate::stt;
    let s = state.settings.borrow();
    if !matches!(s.stt.mode, SttMode::Local) {
        return vec!["auto".to_string()];
    }
    let model_id: &str = match s.stt.local_provider {
        LocalSttProvider::OpenAi => {
            return {
                let mut v = vec!["auto".to_string()];
                v.extend(stt::WHISPER_LANGS.iter().map(|l| l.to_string()));
                v
            };
        }
        LocalSttProvider::Nvidia => &s.stt.parakeet_model,
        LocalSttProvider::Oruk => "orukeet-v0.1.0",
        LocalSttProvider::Cohere => "cohere-transcribe-03-2026",
    };
    if matches!(s.stt.local_provider, LocalSttProvider::Cohere) {
        return stt::model_languages(model_id)
            .iter()
            .map(|l| l.to_string())
            .collect();
    }
    let mut v = vec!["auto".to_string()];
    v.extend(stt::model_languages(model_id).iter().map(|l| l.to_string()));
    v
}

fn stt_page(state: &Rc<AppState>) -> GtkBox {
    let root = section_box(tr("settings.stt"), tr("settings.stt.sub"));

    // ---- Engine: whisper / parakeet / cloud / self-hosted ----
    // Section visibility refreshes on engine change; the holder is
    // filled after all cards exist (handler below runs later).
    let refresh_holder: Rc<std::cell::RefCell<Option<Rc<dyn Fn()>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let (card, rows) = group(tr("group.engine"));
    {
        let (b, _) = row(tr("row.engine"));
        let engine = DropDown::new(
            Some(StringList::new(&[
                tr("opt.local"),
                tr("opt.cloud"),
                tr("opt.selfhostedserver"),
            ])),
            None::<gtk4::Expression>,
        );
        {
            use crate::settings::SttMode;
            let s = state.settings.borrow();
            engine.set_selected(match s.stt.mode {
                SttMode::Cloud => 1,
                SttMode::SelfHosted => 2,
                _ => 0,
            });
        }
        {
            let state = state.clone();
            let persist = persist_prefs(state.clone());
            let refresh_holder = refresh_holder.clone();
            engine.connect_selected_notify(move |d| {
                use crate::settings::SttMode;
                {
                    let mut s = state.settings.borrow_mut();
                    s.stt.mode = match d.selected() {
                        1 => SttMode::Cloud,
                        2 => SttMode::SelfHosted,
                        _ => SttMode::Local,
                    };
                }
                persist();
                if let Some(r) = refresh_holder.borrow().as_ref() {
                    r();
                }
            });
        }
        b.append(&engine);
        rows.append(&b);
    }
    root.append(&card);

    // ---- Registry model list with download/delete/select ----
    // Vendor pill tabs on top (Oruk / OpenAI / NVIDIA / Cohere),
    // mirroring OpenWhispr's local provider picker.
    let (card, rows) = group(tr("stt.models.title"));
    {
        let pill_row = GtkBox::new(Orientation::Horizontal, 8);
        pill_row.set_halign(gtk4::Align::Start);
        rows.append(&pill_row);
        let vendors = ["Oruk", "OpenAI", "NVIDIA", "Cohere"];
        {
            use crate::settings::LocalSttProvider;
            let current = state.settings.borrow().stt.local_provider;
            let current_idx = match current {
                LocalSttProvider::Oruk => 0,
                LocalSttProvider::OpenAi => 1,
                LocalSttProvider::Nvidia => 2,
                LocalSttProvider::Cohere => 3,
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
                pill_row.append(&pill);
            }
        }
    }
    let list = GtkBox::new(Orientation::Vertical, 8);
    rows.append(&list);
    root.append(&card);
    fill_stt_models(&list, state);
    {
        let state = state.clone();
        let list = list.clone();
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
            fill_stt_models(&list, &state);
            gtk4::glib::ControlFlow::Continue
        });
    }
    let models_card = card.clone();

    // ---- Runtimes: whisper packs (+ backend picker) for the OpenAI
    // vendor, sherpa packs for Oruk / NVIDIA / Cohere. Toggled per
    // vendor in the refresh closure below.
    let (card, _rows) = group(tr("stt.gpu.title"));
    let whisper_box = GtkBox::new(Orientation::Vertical, 8);
    _rows.append(&whisper_box);
    let sherpa_box = GtkBox::new(Orientation::Vertical, 8);
    _rows.append(&sherpa_box);
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
    let sherpa_list = GtkBox::new(Orientation::Vertical, 8);
    sherpa_box.append(&sherpa_list);
    root.append(&card);
    let gpu_card = card.clone();
    fill_gpu_packs(&gpu_list, state);
    {
        let state = state.clone();
        let gpu_list = gpu_list.clone();
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
            fill_gpu_packs(&gpu_list, &state);
            gtk4::glib::ControlFlow::Continue
        });
    }
    fill_sherpa_packs(&sherpa_list, state);
    {
        let state = state.clone();
        let sherpa_list = sherpa_list.clone();
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
            fill_sherpa_packs(&sherpa_list, &state);
            gtk4::glib::ControlFlow::Continue
        });
    }
    // ---- Cloud provider (display-only for now; shown in Cloud mode) ----
    let (card, rows) = group(tr("group.cloud"));
    {
        let settings = state.settings.borrow();
        let t = &settings.stt;
        const PROVIDERS: &[&str] = &["OpenAI", "Groq", "Mistral", "Gemini", "Deepgram"];
        combo_row(
            &rows,
            tr("row.provider"),
            PROVIDERS,
            idx_of(PROVIDERS, &t.cloud_provider),
        );
        entry_row(&rows, tr("row.model"), &t.cloud_model, false);
        entry_row(&rows, tr("row.apikey"), &t.cloud_api_key, true);
    }
    root.append(&card);
    let cloud_card = card.clone();

    // ---- Self-hosted server (wired URL + model) ----
    let (card, rows) = group(tr("group.selfhosted"));
    {
        let (b, _) = row(tr("row.serverurl"));
        let url_entry = Entry::new();
        url_entry.set_text(&state.settings.borrow().stt.self_hosted_url);
        url_entry.set_hexpand(true);
        {
            let state = state.clone();
            let url_entry_conn = url_entry.clone();
            let commit = Rc::new(move || {
                let url_entry = url_entry_conn.clone();
                state.settings.borrow_mut().stt.self_hosted_url =
                    url_entry.text().trim().to_string();
                persist_prefs(state.clone())();
            });
            let commit_activate = commit.clone();
            let commit_focus = commit.clone();
            url_entry.connect_activate(move |_| commit_activate());
            let focus = gtk4::EventControllerFocus::new();
            focus.connect_leave(move |_| commit_focus());
            url_entry.add_controller(focus);
        }
        b.append(&url_entry);
        rows.append(&b);
    }
    {
        let (b, _) = row(tr("row.model"));
        let model_entry = Entry::new();
        model_entry.set_text(&state.settings.borrow().stt.self_hosted_model);
        model_entry.set_hexpand(true);
        {
            let state = state.clone();
            let model_entry_conn = model_entry.clone();
            let commit = Rc::new(move || {
                let model_entry = model_entry_conn.clone();
                state.settings.borrow_mut().stt.self_hosted_model =
                    model_entry.text().trim().to_string();
                persist_prefs(state.clone())();
            });
            let commit_activate = commit.clone();
            let commit_focus = commit.clone();
            model_entry.connect_activate(move |_| commit_activate());
            let focus = gtk4::EventControllerFocus::new();
            focus.connect_leave(move |_| commit_focus());
            model_entry.add_controller(focus);
        }
        b.append(&model_entry);
        rows.append(&b);
    }
    root.append(&card);
    let selfhosted_card = card.clone();

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
            let opts = stt_lang_options(&state);
            let (b, _) = row(tr("row.language"));
            let lang_drop = DropDown::new(
                Some(StringList::new(
                    &opts.iter().map(String::as_str).collect::<Vec<_>>(),
                )),
                None::<gtk4::Expression>,
            );
            // STT codes stay untranslated (server contract). A stored
            // code outside the new list falls back to the first entry.
            lang_drop.set_selected(idx_of(
                &opts.iter().map(String::as_str).collect::<Vec<_>>(),
                &state.settings.borrow().stt.language,
            ) as u32);
            {
                let state = state.clone();
                lang_drop.connect_selected_notify(move |d| {
                    let opts = stt_lang_options(&state);
                    let code = opts
                        .get(d.selected().min(opts.len() as u32 - 1) as usize)
                        .cloned()
                        .unwrap_or_else(|| "auto".to_string());
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
    switch_row(
        &rows,
        tr("row.livepreview"),
        state.settings.borrow().stt.live_preview,
    );
    root.append(&card);

    // ---- Engine-conditional visibility ----
    // GPU packs + model list only make sense for local whisper
    // (parakeet ships its own sherpa binaries); cloud/self-hosted
    // show their own sections. VAD always shows: it gates capture
    // before anything leaves the machine.
    let refresh: Rc<dyn Fn()> = Rc::new({
        let state = state.clone();
        let gpu_card = gpu_card.clone();
        let models_card = models_card.clone();
        let cloud_card = cloud_card.clone();
        let selfhosted_card = selfhosted_card.clone();
        let whisper_box = whisper_box.clone();
        let sherpa_box = sherpa_box.clone();
        move || {
            use crate::settings::SttMode;
            let s = state.settings.borrow();
            let (gpu, models, cloud, hosted) = match (&s.stt.mode, &s.stt.local_provider) {
                // Runtimes card shows for every local vendor; its
                // content switches (whisper packs vs sherpa packs).
                (SttMode::Local, _) => (true, true, false, false),
                (SttMode::Cloud, _) => (false, false, true, false),
                (SttMode::SelfHosted, _) => (false, false, false, true),
            };
            gpu_card.set_visible(gpu);
            models_card.set_visible(models);
            {
                use crate::settings::{LocalSttProvider, SttMode};
                let s = state.settings.borrow();
                let whisper = matches!(
                    (&s.stt.mode, &s.stt.local_provider),
                    (SttMode::Local, LocalSttProvider::OpenAi)
                );
                whisper_box.set_visible(whisper);
                sherpa_box.set_visible(!whisper);
            }
            cloud_card.set_visible(cloud);
            selfhosted_card.set_visible(hosted);
            refresh_lang();
        }
    });
    refresh();
    refresh_holder.borrow_mut().replace(refresh);

    root
}

fn language_models_page(s: &AppSettings) -> GtkBox {
    let root = section_box(tr("settings.lm"), tr("settings.lm.sub"));
    let l = &s.language_models;

    let (card, rows) = group(tr("group.provider"));
    combo_row(
        &rows,
        tr("row.mode"),
        &[tr("opt.ollama"), tr("opt.localmodel"), tr("opt.cloud")],
        match l.mode {
            crate::settings::LmMode::Ollama => 0,
            crate::settings::LmMode::Local => 1,
            crate::settings::LmMode::Cloud => 2,
        },
    );
    entry_row(&rows, tr("row.ollamaurl"), &l.ollama_url, false);
    entry_row(&rows, tr("row.ollamamodel"), &l.ollama_model, false);
    entry_row(&rows, tr("row.localmodel"), &l.local_model, false);
    switch_row(&rows, tr("row.keeploaded"), l.keep_loaded);
    root.append(&card);

    let (card, rows) = group(tr("group.cloudfallback"));
    const PROVIDERS: &[&str] = &["OpenAI", "Anthropic", "Gemini", "Groq", "OpenRouter"];
    combo_row(
        &rows,
        tr("row.provider"),
        PROVIDERS,
        idx_of(PROVIDERS, &l.cloud_provider),
    );
    entry_row(&rows, tr("row.model"), &l.cloud_model, false);
    entry_row(&rows, tr("row.apikey"), &l.cloud_api_key, true);
    root.append(&card);

    let (card, rows) = group(tr("group.cleanup"));
    switch_row(&rows, tr("row.cleanup"), l.cleanup_enabled);
    let (b, _) = row(tr("row.customprompt"));
    let prompt = TextView::new();
    prompt.set_size_request(-1, 80);
    prompt.buffer().set_text(&l.cleanup_prompt);
    prompt.set_sensitive(false); // not wired yet
    prompt.set_hexpand(true);
    b.append(&prompt);
    rows.append(&b);
    root.append(&card);

    root
}

fn privacy_page(s: &AppSettings) -> GtkBox {
    let root = section_box(tr("settings.privacy"), tr("settings.privacy.sub"));
    let p = &s.privacy;

    let (card, rows) = group(tr("group.audioret"));
    const RETENTION_DAYS: &[u32] = &[0, 1, 7, 14, 30, 60, 90];
    combo_row(
        &rows,
        tr("row.keepaudio"),
        &retention_opts(),
        RETENTION_DAYS
            .iter()
            .position(|d| *d == p.audio_retention_days)
            .unwrap_or(4),
    );
    let (b, _) = row(tr("row.storedaudio"));
    let clear = Button::with_label(tr("action.clearallaudio"));
    clear.set_sensitive(false); // not wired yet
    b.append(&clear);
    rows.append(&b);
    root.append(&card);

    let (card, rows) = group(tr("group.transcripts"));
    switch_row(&rows, tr("row.keephistory"), p.history_enabled);

    combo_row(
        &rows,
        tr("row.autodelete"),
        &delete_opts(),
        RETENTION_DAYS
            .iter()
            .position(|d| *d == p.transcript_auto_delete_days)
            .unwrap_or(0),
    );
    switch_row(
        &rows,
        tr("row.savediscarded"),
        p.save_discarded_transcriptions,
    );
    root.append(&card);

    root
}

fn system_page(s: &AppSettings) -> GtkBox {
    let root = section_box(tr("settings.system"), tr("settings.system.sub"));
    let sys = &s.system;

    let (card, rows) = group(tr("group.version"));
    let (b, _) = row(tr("row.appversion"));
    let v = Label::new(Some(env!("CARGO_PKG_VERSION")));
    v.add_css_class("dim");
    b.append(&v);
    rows.append(&b);
    root.append(&card);

    let (card, rows) = group(tr("group.modelcache"));
    entry_row(&rows, tr("row.cachepath"), &sys.model_cache_path, false);
    let (b, _) = row(tr("row.cacheactions"));
    let open = Button::with_label(tr("action.openfolder"));
    open.set_sensitive(false); // not wired yet
    let clear = Button::with_label(tr("action.clearcache"));
    clear.set_sensitive(false); // not wired yet
    b.append(&open);
    b.append(&clear);
    rows.append(&b);
    root.append(&card);

    let (card, rows) = group(tr("group.danger"));
    let (b, _) = row(tr("row.resetall"));
    let reset = Button::with_label(tr("action.resetdots"));
    reset.set_sensitive(false); // not wired yet
    b.append(&reset);
    rows.append(&b);
    root.append(&card);

    root
}

// ---------------------------------------------------------------- page

pub fn page(state: &Rc<AppState>, initial_section: &str) -> GtkBox {
    let root = GtkBox::new(Orientation::Horizontal, 0);

    let nav = GtkBox::new(Orientation::Vertical, 4);
    nav.add_css_class("sidebar");
    nav.set_size_request(170, -1);
    root.append(&nav);

    let stack = Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    root.append(&stack);

    let borrowed = state.settings.borrow();
    let s: &AppSettings = &borrowed;
    // Theme writes straight into the model and applies live.
    let on_theme: Rc<dyn Fn(Theme)> = Rc::new({
        let state = state.clone();
        move |theme: Theme| {
            state.settings.borrow_mut().preferences.theme = theme;
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
        ("Clipboard", clipboard_page(state)),
        ("Speech to Text", stt_page(state)),
        ("Language Models", language_models_page(s)),
        ("Privacy & Data", privacy_page(s)),
        ("System", system_page(s)),
    ];

    fn section_label(id: &str) -> &'static str {
        match id {
            "Preferences" => tr("settings.preferences"),
            "Hotkeys" => tr("settings.hotkeys"),
            "Clipboard" => tr("settings.clipboard"),
            "Speech to Text" => tr("settings.stt"),
            "Language Models" => tr("settings.lm"),
            "Privacy & Data" => tr("settings.privacy"),
            "System" => tr("settings.system"),
            _ => Box::leak(id.to_string().into_boxed_str()),
        }
    }

    let mut buttons: Vec<(Button, String)> = Vec::new();
    let mut pending: Vec<(Button, String)> = Vec::new();
    for (id, widget) in sections {
        let scrolled = gtk4::ScrolledWindow::new();
        scrolled.set_child(Some(&widget));
        scrolled.set_vexpand(true);
        stack.add_named(&scrolled, Some(id));
        let b = Button::with_label(section_label(id));
        b.set_halign(gtk4::Align::Fill);
        nav.append(&b);
        buttons.push((b.clone(), id.to_string()));
        pending.push((b, id.to_string()));
    }

    let set_active = Rc::new(move |active: &str| {
        for (b, id) in buttons.iter() {
            if id == active {
                b.add_css_class("active");
            } else {
                b.remove_css_class("active");
            }
        }
    });

    for (b, name) in pending {
        let stack = stack.clone();
        let set_active = set_active.clone();
        b.connect_clicked(move |_| {
            stack.set_visible_child_name(&name);
            set_active(&name);
        });
    }
    let initial = match initial_section {
        "Hotkeys" | "Clipboard" | "Speech to Text" | "Language Models" | "Privacy & Data"
        | "System" => initial_section,
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
