//! Settings page: sub-nav + sections.
//!
//! Every control initializes from [`AppSettings`] but nothing is
//! wired yet: flipping a switch changes no model and persists
//! nothing. Wiring later = connect each control to its model field
//! (+ persistence + backend apply).

use gtk4::prelude::*;
use gtk4::{
    Box as GtkBox, Button, DropDown, Entry, Label, Orientation, SpinButton, Stack, StringList,
    Switch, TextView,
};
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
    b.append(&s);
    parent.append(&b);
}

/// Index of `value` in `options`, clamped to 0.
fn idx_of(options: &[&str], value: &str) -> usize {
    options.iter().position(|o| *o == value).unwrap_or(0)
}

// ---------------------------------------------------------------- sections

fn preferences_page(s: &AppSettings, on_theme: &Rc<dyn Fn(Theme)>) -> GtkBox {
    let root = section_box("Preferences", "General application behavior.");
    let p = &s.preferences;

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
    switch_row(&rows, "Dictation sounds", p.dictation_sounds);
    switch_row(
        &rows,
        "Pause media while dictating",
        p.pause_media_on_dictation,
    );
    root.append(&card);

    let (card, rows) = group("Clipboard");
    switch_row(&rows, "Auto-paste transcription", p.auto_paste);
    switch_row(
        &rows,
        "Keep transcription in clipboard",
        p.keep_transcription_in_clipboard,
    );
    root.append(&card);

    let (card, rows) = group("Floating icon");
    switch_row(&rows, "Auto-hide", p.floating_icon_auto_hide);
    combo_row(
        &rows,
        "Start position",
        &["Bottom right", "Bottom center", "Bottom left"],
        match p.floating_icon_position {
            crate::settings::FloatingPosition::BottomRight => 0,
            crate::settings::FloatingPosition::BottomCenter => 1,
            crate::settings::FloatingPosition::BottomLeft => 2,
        },
    );
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

    let (card, rows) = group("Startup");
    switch_row(&rows, "Launch at login", p.launch_at_login);
    switch_row(&rows, "Start minimized", p.start_minimized);
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

fn hotkeys_page(s: &AppSettings) -> GtkBox {
    let root = section_box("Hotkeys", "Global shortcuts. Capture comes later.");
    let h = &s.hotkeys;

    let (card, rows) = group("Dictation");
    entry_row(&rows, "Dictation hotkey", &h.dictation_hotkey, false);
    combo_row(
        &rows,
        "Activation",
        &["Hold to talk", "Press to toggle"],
        match h.activation_mode {
            crate::settings::ActivationMode::Push => 0,
            crate::settings::ActivationMode::Tap => 1,
        },
    );
    let (b, _) = row("Reset to default");
    let reset = Button::with_label("Reset");
    reset.set_sensitive(false); // not wired yet
    b.append(&reset);
    rows.append(&b);
    root.append(&card);

    root
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
        ("Preferences", preferences_page(s, &on_theme)),
        ("Hotkeys", hotkeys_page(s)),
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
