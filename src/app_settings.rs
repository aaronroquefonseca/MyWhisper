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

use super::app::{AppState, apply_theme, log_theme_state};
use crate::settings::{AppSettings, Theme};

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
    let root = section_box("Preferences", "General application behavior.");
    let p = state.settings.borrow().preferences.clone();

    let (card, rows) = group("Appearance");
    // The theme picker is wired: it writes the model and applies the
    // variant immediately. All other controls below are display-only
    // until their backends land.
    let (b, _) = row("Theme");
    let theme_drop = DropDown::new(
        Some(StringList::new(&["System", "Light", "Dark"])),
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

    let (card, rows) = group("Sound");
    {
        let (b, _) = row("Dictation sounds");
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
        let (b, _) = row("Pause media while dictating");
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

    let (card, rows) = group("Clipboard");
    switch_row(&rows, "Auto-paste transcription", p.auto_paste);
    switch_row(
        &rows,
        "Keep transcription in clipboard",
        p.keep_transcription_in_clipboard,
    );
    root.append(&card);

    let (card, rows) = group("Floating pill");
    {
        let (b, _) = row("Start position");
        let pos = DropDown::new(
            Some(StringList::new(&[
                "Bottom center",
                "Bottom left",
                "Bottom right",
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
        let (b, _) = row("Animation");
        let mode = DropDown::new(
            Some(StringList::new(&["Tape", "History"])),
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
        let (b, _) = row("Bars");
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
        let (b, _) = row("Height");
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
        let (b, _) = row("Theme");
        let theme = DropDown::new(
            Some(StringList::new(&["White", "Grey", "Red"])),
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
        let (b, _) = row("Ornament");
        let orn = DropDown::new(
            Some(StringList::new(&["None", "Mark", "Mic", "Dot"])),
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
        let (b, _) = row("Idle animation");
        let idle = DropDown::new(
            Some(StringList::new(&["Still", "Silhouette", "Calm"])),
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

    let (card, rows) = group("Language");
    const LANGS: &[&str] = &["en", "es", "fr", "de", "pt", "it", "ru", "ja", "ar"];
    combo_row(
        &rows,
        "Interface language",
        LANGS,
        idx_of(LANGS, &p.ui_language),
    );
    const STT_LANGS: &[&str] = &["auto", "en", "es", "fr", "de", "pt", "it", "ru", "ja", "ar"];
    combo_row(
        &rows,
        "Transcription language",
        STT_LANGS,
        idx_of(STT_LANGS, &p.transcription_language),
    );
    root.append(&card);

    let (card, rows) = group("Microphone");
    combo_row(
        &rows,
        "Input",
        &["System default", "Built-in", "Specific device"],
        match p.mic_selection {
            crate::settings::MicSelection::System => 0,
            crate::settings::MicSelection::BuiltIn => 1,
            crate::settings::MicSelection::Specific => 2,
        },
    );
    entry_row(&rows, "Device id", &p.mic_device_id, false);
    const HOLD: &[&str] = &["Off", "10 seconds", "60 seconds", "15 minutes"];
    combo_row(
        &rows,
        "Keep microphone warm",
        HOLD,
        match p.mic_warm_hold_secs {
            10 => 1,
            60 => 2,
            900 => 3,
            _ => 0,
        },
    );
    root.append(&card);

    let (card, rows) = group("Dictionary");
    switch_row(
        &rows,
        "Learn words from corrections",
        p.auto_learn_dictionary,
    );
    root.append(&card);

    root
}

fn hotkeys_page(state: &Rc<AppState>) -> GtkBox {
    let root = section_box(
        "Hotkeys",
        "Global shortcuts, read straight from your keyboards.\n\
         Hold = talk while held. Toggle = press to start/stop. \
         Smart = quick tap toggles, longer press holds.",
    );
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

    let (card, rows) = group("Dictation");
    // One row per binding, each with its own Remove. Refill is a
    // plain fn (list + state passed in) so Remove closures recurse
    // without self-referential Rc tricks.
    let list = GtkBox::new(Orientation::Vertical, 4);
    rows.append(&list);
    refill_hotkey_list(&list, state, &persist);

    // Add row: text field + per-binding mode + Add/Record. Validated
    // by the same parser the listener uses; invalid combos are
    // refused with a hint.
    let (add_row, _) = row("Add combo (e.g. F13, Ctrl+Alt+D)");
    let add_entry = Entry::new();
    add_entry.set_hexpand(true);
    add_entry.set_placeholder_text(Some("F13"));
    add_row.append(&add_entry);
    let add_mode = DropDown::new(
        Some(StringList::new(&["Hold", "Toggle", "Smart"])),
        None::<gtk4::Expression>,
    );
    add_mode.set_tooltip_text(Some("Behavior for the new binding"));
    add_row.append(&add_mode);
    let add_btn = Button::with_label("Add");
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
                add_entry.set_placeholder_text(Some("Invalid combo, try F13"));
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
    let record_btn = Button::with_label("Record");
    record_btn.set_tooltip_text(Some("Click, then press the key combo"));
    add_row.append(&record_btn);
    rows.append(&add_row);

    let (b, _) = row("Reset to default");
    let reset = Button::with_label("Reset");
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

    let (smart_card, smart_rows) = group("Smart timing");
    spin_row(
        &smart_rows,
        "Tap ceiling (ms): shorter taps toggle, longer holds",
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
            Some(StringList::new(&["Hold", "Toggle", "Smart"])),
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
        let remove = Button::with_label("Remove");
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
        self.record_btn.set_label("Record");
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
        self.add_entry.set_placeholder_text(Some("Press keys now…"));
    }

    fn live_label(&self) {
        let held = *self.held.borrow();
        let shown = combo_string(&held, None);
        self.record_btn.set_label(&format!(
            "{}… (Esc cancels)",
            if shown.is_empty() {
                "Press keys".to_string()
            } else {
                shown
            }
        ));
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
            self.record_btn.set_label("Invalid, try again");
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
                    cap.record_btn.set_label("Unsupported key, try again");
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

fn stt_page(s: &AppSettings) -> GtkBox {
    let root = section_box("Speech to Text", "How your voice is transcribed.");
    let t = &s.stt;

    let (card, rows) = group("Engine");
    combo_row(
        &rows,
        "Mode",
        &["Local (on-device)", "Cloud provider", "Self-hosted"],
        match t.mode {
            crate::settings::SttMode::Local => 0,
            crate::settings::SttMode::Cloud => 1,
            crate::settings::SttMode::SelfHosted => 2,
        },
    );
    combo_row(
        &rows,
        "Local provider",
        &["Whisper", "Parakeet"],
        match t.local_provider {
            crate::settings::LocalSttProvider::Whisper => 0,
            crate::settings::LocalSttProvider::Parakeet => 1,
        },
    );
    entry_row(&rows, "Whisper model", &t.whisper_model, false);
    entry_row(&rows, "Parakeet model", &t.parakeet_model, false);
    root.append(&card);

    let (card, rows) = group("Cloud provider (BYOK)");
    const PROVIDERS: &[&str] = &["OpenAI", "Groq", "Mistral", "Gemini", "Deepgram"];
    combo_row(
        &rows,
        "Provider",
        PROVIDERS,
        idx_of(PROVIDERS, &t.cloud_provider),
    );
    entry_row(&rows, "Model", &t.cloud_model, false);
    entry_row(&rows, "API key", &t.cloud_api_key, true);
    root.append(&card);

    let (card, rows) = group("Self-hosted");
    entry_row(&rows, "Server URL", &t.self_hosted_url, false);
    entry_row(&rows, "Model", &t.self_hosted_model, false);
    root.append(&card);

    let (card, rows) = group("Voice activity");
    const STT_LANGS: &[&str] = &["auto", "en", "es", "fr", "de", "pt", "it", "ru", "ja", "ar"];
    combo_row(&rows, "Language", STT_LANGS, idx_of(STT_LANGS, &t.language));
    switch_row(&rows, "Voice activity detection", t.vad_enabled);
    spin_row(
        &rows,
        "VAD threshold",
        t.vad_threshold as f64,
        0.0,
        1.0,
        0.05,
    );
    switch_row(&rows, "Live transcription preview", t.live_preview);
    root.append(&card);

    root
}

fn language_models_page(s: &AppSettings) -> GtkBox {
    let root = section_box(
        "Language Models",
        "Cleanup and assistant models. Ollama-first.",
    );
    let l = &s.language_models;

    let (card, rows) = group("Provider");
    combo_row(
        &rows,
        "Mode",
        &["Ollama", "Local model", "Cloud provider"],
        match l.mode {
            crate::settings::LmMode::Ollama => 0,
            crate::settings::LmMode::Local => 1,
            crate::settings::LmMode::Cloud => 2,
        },
    );
    entry_row(&rows, "Ollama URL", &l.ollama_url, false);
    entry_row(&rows, "Ollama model", &l.ollama_model, false);
    entry_row(&rows, "Local model", &l.local_model, false);
    switch_row(&rows, "Keep model loaded", l.keep_loaded);
    root.append(&card);

    let (card, rows) = group("Cloud fallback (BYOK)");
    const PROVIDERS: &[&str] = &["OpenAI", "Anthropic", "Gemini", "Groq", "OpenRouter"];
    combo_row(
        &rows,
        "Provider",
        PROVIDERS,
        idx_of(PROVIDERS, &l.cloud_provider),
    );
    entry_row(&rows, "Model", &l.cloud_model, false);
    entry_row(&rows, "API key", &l.cloud_api_key, true);
    root.append(&card);

    let (card, rows) = group("Dictation cleanup");
    switch_row(&rows, "Clean up transcriptions", l.cleanup_enabled);
    let (b, _) = row("Custom prompt");
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
    let root = section_box("Privacy & Data", "Retention and history.");
    let p = &s.privacy;

    let (card, rows) = group("Audio retention");
    const RETENTION: &[&str] = &[
        "Disabled", "1 day", "7 days", "14 days", "30 days", "60 days", "90 days",
    ];
    const RETENTION_DAYS: &[u32] = &[0, 1, 7, 14, 30, 60, 90];
    combo_row(
        &rows,
        "Keep audio recordings",
        RETENTION,
        RETENTION_DAYS
            .iter()
            .position(|d| *d == p.audio_retention_days)
            .unwrap_or(4),
    );
    let (b, _) = row("Stored audio");
    let clear = Button::with_label("Clear all audio");
    clear.set_sensitive(false); // not wired yet
    b.append(&clear);
    rows.append(&b);
    root.append(&card);

    let (card, rows) = group("Transcripts");
    switch_row(&rows, "Keep dictation history", p.history_enabled);
    const DELETE: &[&str] = &[
        "Forever", "1 day", "7 days", "14 days", "30 days", "60 days", "90 days",
    ];
    combo_row(
        &rows,
        "Auto-delete transcripts",
        DELETE,
        RETENTION_DAYS
            .iter()
            .position(|d| *d == p.transcript_auto_delete_days)
            .unwrap_or(0),
    );
    switch_row(
        &rows,
        "Save discarded transcriptions",
        p.save_discarded_transcriptions,
    );
    root.append(&card);

    root
}

fn system_page(s: &AppSettings) -> GtkBox {
    let root = section_box("System", "Version and local data.");
    let sys = &s.system;

    let (card, rows) = group("Version");
    let (b, _) = row("Application version");
    let v = Label::new(Some(env!("CARGO_PKG_VERSION")));
    v.add_css_class("dim");
    b.append(&v);
    rows.append(&b);
    root.append(&card);

    let (card, rows) = group("Model cache");
    entry_row(&rows, "Cache path", &sys.model_cache_path, false);
    let (b, _) = row("Cache actions");
    let open = Button::with_label("Open folder");
    open.set_sensitive(false); // not wired yet
    let clear = Button::with_label("Clear cache");
    clear.set_sensitive(false); // not wired yet
    b.append(&open);
    b.append(&clear);
    rows.append(&b);
    root.append(&card);

    let (card, rows) = group("Danger zone");
    let (b, _) = row("Reset all application data");
    let reset = Button::with_label("Reset...");
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
    let sections = [
        ("Preferences", preferences_page(state, &on_theme)),
        ("Hotkeys", hotkeys_page(state)),
        ("Speech to Text", stt_page(s)),
        ("Language Models", language_models_page(s)),
        ("Privacy & Data", privacy_page(s)),
        ("System", system_page(s)),
    ];

    let mut buttons: Vec<Button> = Vec::new();
    let mut pending: Vec<(Button, String)> = Vec::new();
    for (name, widget) in sections {
        let scrolled = gtk4::ScrolledWindow::new();
        scrolled.set_child(Some(&widget));
        scrolled.set_vexpand(true);
        stack.add_named(&scrolled, Some(name));
        let b = Button::with_label(name);
        b.set_halign(gtk4::Align::Fill);
        nav.append(&b);
        buttons.push(b.clone());
        pending.push((b, name.to_string()));
    }

    let set_active = Rc::new(move |active: &str| {
        for b in buttons.iter() {
            if b.label().as_deref() == Some(active) {
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
        "Hotkeys" | "Speech to Text" | "Language Models" | "Privacy & Data" | "System" => {
            initial_section
        }
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
