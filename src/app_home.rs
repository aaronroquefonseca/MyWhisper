//! Home page: greeting + dictation history grouped by day.
//!
//! Each entry shows the FINAL transcript (what was pasted). Copy
//! duplicates it to the clipboard; the ⋮ row exposes the raw STT
//! output (identical while no post-processor is enabled) and delete
//! (tombstoned, so sync never resurrects it). Per-entry WPM comes
//! from stored words/duration ("—" when duration is 0).

use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Button, Label, Orientation, Switch};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use super::app::{AppState, centered_column};
use crate::i18n::{tr, trf};
use crate::settings::HistoryEntry;

fn clear_box(container: &GtkBox) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

/// Local-midnight day key (days since epoch) for grouping.
fn day_key(epoch_secs: i64) -> i64 {
    super::app::local_day(epoch_secs)
}

fn day_label(epoch_secs: i64) -> String {
    let today = day_key(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(epoch_secs),
    );
    let day = day_key(epoch_secs);
    if day == today {
        return tr("home.today").to_string();
    }
    if day == today - 1 {
        return tr("home.yesterday").to_string();
    }
    gtk4::glib::DateTime::from_unix_utc(epoch_secs)
        .ok()
        .and_then(|d| d.to_local().ok())
        .map(|d| {
            d.format("%Y-%m-%d")
                .map(|s| s.to_string())
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

fn time_label(epoch_secs: i64) -> String {
    gtk4::glib::DateTime::from_unix_utc(epoch_secs)
        .ok()
        .and_then(|d| d.to_local().ok())
        .map(|d| d.format("%H:%M").map(|s| s.to_string()).unwrap_or_default())
        .unwrap_or_default()
}

fn entry_wpm(entry: &HistoryEntry) -> String {
    if entry.duration_secs > 0 {
        let wpm = entry.word_count as f64 * 60.0 / entry.duration_secs as f64;
        trf("home.wpm", &[("wpm", &format!("{:.0}", wpm))])
    } else {
        trf("home.wpm", &[("wpm", "—")])
    }
}

fn fill_history(
    container: &GtkBox,
    state: &Rc<AppState>,
    history: &[HistoryEntry],
    expanded: &Rc<RefCell<HashSet<String>>>,
    query: &str,
) {
    clear_box(container);
    let query = query.trim().to_lowercase();
    let filtered: Vec<_> = history
        .iter()
        .filter(|entry| {
            query.is_empty()
                || entry.text.to_lowercase().contains(&query)
                || entry.raw_text.to_lowercase().contains(&query)
        })
        .cloned()
        .collect();
    let history = filtered.as_slice();
    if history.is_empty() {
        let empty = GtkBox::new(Orientation::Vertical, 8);
        empty.add_css_class("empty-state");
        empty.set_halign(gtk4::Align::Center);
        empty.set_valign(gtk4::Align::Center);
        empty.set_vexpand(true);
        let icon = gtk4::Image::from_icon_name("audio-input-microphone-symbolic");
        icon.set_pixel_size(36);
        empty.append(&icon);
        let history_enabled = state.settings.borrow().privacy.history_enabled;
        let title = if !query.is_empty() {
            "home.noresults"
        } else if !history_enabled {
            "home.historyoff"
        } else {
            "home.empty.title"
        };
        let t = Label::new(Some(tr(title)));
        t.add_css_class("section-title");
        let hotkeys = state
            .settings
            .borrow()
            .hotkeys
            .effective_hotkeys()
            .iter()
            .map(|b| b.combo.clone())
            .collect::<Vec<_>>()
            .join(" / ");
        let hint = if !query.is_empty() {
            tr("home.noresults.hint").to_string()
        } else if !history_enabled {
            tr("home.historyoff.hint").to_string()
        } else {
            trf("home.empty.hint", &[("hotkey", &hotkeys)])
        };
        let h = Label::new(Some(&hint));
        h.set_wrap(true);
        h.set_max_width_chars(55);
        h.add_css_class("dim");
        h.set_justify(gtk4::Justification::Center);
        empty.append(&t);
        empty.append(&h);
        container.append(&empty);
        return;
    }
    // Group newest-first entries by local day, preserving order.
    let mut days: Vec<i64> = Vec::new();
    let mut by_day: HashMap<i64, Vec<&HistoryEntry>> = HashMap::new();
    for entry in history {
        let day = day_key(entry.recorded_at);
        by_day.entry(day).or_insert_with(|| {
            days.push(day);
            Vec::new()
        });
        by_day.entry(day).or_default().push(entry);
    }
    for day in days {
        let header = Label::new(Some(day_label(by_day[&day][0].recorded_at).as_str()));
        header.add_css_class("page-subtitle");
        header.set_xalign(0.0);
        container.append(&header);
        for entry in by_day[&day].iter() {
            container.append(&entry_card(state, entry, expanded));
        }
    }
}

fn entry_card(
    state: &Rc<AppState>,
    entry: &HistoryEntry,
    expanded: &Rc<RefCell<HashSet<String>>>,
) -> GtkBox {
    if entry.discarded {
        return discarded_card(state, entry);
    }
    let card = GtkBox::new(Orientation::Vertical, 2);
    card.add_css_class("card");
    card.add_css_class("history-entry");

    // Header row: time · wpm left, copy + ⋮ right.
    let head = GtkBox::new(Orientation::Horizontal, 8);
    let meta = Label::new(Some(
        format!("{} · {}", time_label(entry.recorded_at), entry_wpm(entry)).as_str(),
    ));
    meta.set_xalign(0.0);
    meta.add_css_class("dim");
    meta.set_hexpand(true);
    head.append(&meta);
    let copy = Button::with_label(tr("home.copy"));
    {
        let text = entry.text.clone();
        copy.connect_clicked(
            move |button| match crate::clipboard::copy_text(&text, false) {
                Ok(()) => super::app::toast(button, tr("feedback.copied")),
                Err(e) => {
                    eprintln!("[home] copy: {e}");
                    super::app::toast(button, tr("feedback.copyfailed"));
                }
            },
        );
    }
    head.append(&copy);
    let more = Button::from_icon_name("view-more-symbolic");
    more.set_tooltip_text(Some(tr("home.more")));
    more.update_property(&[gtk4::accessible::Property::Label(tr("home.more"))]);
    more.add_css_class("flat");
    head.append(&more);
    card.append(&head);

    let text = Label::new(Some(entry.text.as_str()));
    text.set_xalign(0.0);
    text.set_wrap(true);
    text.set_selectable(true);
    text.add_css_class("history-text");
    card.append(&text);

    // Action row (revealed by ⋮): raw toggle + delete.
    let actions = GtkBox::new(Orientation::Horizontal, 8);
    actions.set_visible(false);
    let raw_btn = Button::with_label(tr("home.viewraw"));
    let del_btn = Button::with_label(tr("home.delete"));
    del_btn.add_css_class("destructive-action");
    actions.append(&raw_btn);
    actions.append(&del_btn);
    card.append(&actions);
    {
        let actions = actions.clone();
        more.connect_clicked(move |_| {
            actions.set_visible(!actions.is_visible());
        });
    }
    {
        let state = state.clone();
        let uuid = entry.uuid.clone();
        del_btn.connect_clicked(move |button| {
            let state = state.clone();
            let uuid = uuid.clone();
            let button = button.clone();
            let card = button.parent().and_then(|p| p.parent());
            super::app::confirm(
                &button.clone(),
                tr("confirm.delete.title"),
                tr("confirm.delete.body"),
                tr("action.delete"),
                move || match state.store.delete_history(&uuid) {
                    Ok(()) => {
                        super::app::toast(&button, tr("feedback.deleted"));
                        if let Some(card) = &card {
                            card.set_visible(false);
                        }
                        crate::net::sync_after_change(state.db_path.clone(), state.creds.clone());
                    }
                    Err(e) => {
                        eprintln!("[home] delete: {e}");
                        super::app::toast(&button, tr("feedback.deletefailed"));
                    }
                },
            );
        });
    }

    // Raw transcript section (OpenWhispr-style inline expansion).
    let raw_box = GtkBox::new(Orientation::Vertical, 6);
    raw_box.add_css_class("raw-transcript");
    raw_box.set_visible(expanded.borrow().contains(&entry.uuid));
    let raw_title = Label::new(Some(tr("home.raw.title")));
    raw_title.set_xalign(0.0);
    raw_title.add_css_class("dim");
    raw_box.append(&raw_title);
    let raw = Label::new(Some(entry.raw_text.as_str()));
    raw.set_xalign(0.0);
    raw.set_wrap(true);
    raw.set_selectable(true);
    raw.add_css_class("dim");
    raw_box.append(&raw);
    if entry.raw_text == entry.text {
        let note = Label::new(Some(tr("home.raw.passthrough")));
        note.set_xalign(0.0);
        note.add_css_class("dim");
        raw_box.append(&note);
    }
    card.append(&raw_box);
    {
        let raw_box = raw_box.clone();
        let expanded = expanded.clone();
        let uuid = entry.uuid.clone();
        raw_btn.set_label(if raw_box.is_visible() {
            tr("home.hideraw")
        } else {
            tr("home.viewraw")
        });
        raw_btn.connect_clicked(move |button| {
            let show = !raw_box.is_visible();
            raw_box.set_visible(show);
            button.set_label(if show {
                tr("home.hideraw")
            } else {
                tr("home.viewraw")
            });
            if let Ok(mut set) = expanded.try_borrow_mut() {
                if show {
                    set.insert(uuid.clone());
                } else {
                    set.remove(&uuid);
                }
            }
        });
    }
    card
}

/// Midway-cancelled utterance: timestamp + duration, dimmed, no
/// text (never transcribed), delete only. Shown iff the discarded
/// toggle is on.
fn discarded_card(state: &Rc<AppState>, entry: &HistoryEntry) -> GtkBox {
    let card = GtkBox::new(Orientation::Vertical, 2);
    card.add_css_class("card");
    card.add_css_class("history-entry");
    let head = GtkBox::new(Orientation::Horizontal, 8);
    let meta = Label::new(Some(
        format!(
            "{} · {}s · {}",
            time_label(entry.recorded_at),
            entry.duration_secs,
            tr("home.discarded")
        )
        .as_str(),
    ));
    meta.set_xalign(0.0);
    meta.add_css_class("dim");
    meta.set_hexpand(true);
    head.append(&meta);
    let del_btn = Button::with_label(tr("home.delete"));
    del_btn.add_css_class("destructive-action");
    {
        let state = state.clone();
        let uuid = entry.uuid.clone();
        del_btn.connect_clicked(move |button| {
            let state = state.clone();
            let uuid = uuid.clone();
            let button = button.clone();
            let card = button.parent().and_then(|p| p.parent());
            super::app::confirm(
                &button.clone(),
                tr("confirm.delete.title"),
                tr("confirm.delete.body"),
                tr("action.delete"),
                move || match state.store.delete_history(&uuid) {
                    Ok(()) => {
                        super::app::toast(&button, tr("feedback.deleted"));
                        if let Some(card) = &card {
                            card.set_visible(false);
                        }
                        crate::net::sync_after_change(state.db_path.clone(), state.creds.clone());
                    }
                    Err(e) => {
                        eprintln!("[home] delete: {e}");
                        super::app::toast(&button, tr("feedback.deletefailed"));
                    }
                },
            );
        });
    }
    head.append(&del_btn);
    card.append(&head);
    card
}

pub fn page(state: &Rc<AppState>) -> GtkBox {
    let root = GtkBox::new(Orientation::Vertical, 16);
    root.add_css_class("page");
    let col = centered_column(&root, 860);
    super::app::page_heading(&col, tr("home.greeting"), tr("home.subtitle"));

    let state_dir = crate::paths::AppPaths::create()
        .map(|p| p.state_dir)
        .unwrap_or_default();
    let banner = GtkBox::new(Orientation::Horizontal, 12);
    banner.add_css_class("card");
    banner.add_css_class("status-banner");
    let dot = Label::new(Some("●"));
    banner.append(&dot);
    let service_info = GtkBox::new(Orientation::Vertical, 4);
    service_info.set_hexpand(true);
    let banner_label = Label::new(None);
    banner_label.add_css_class("row-label");
    banner_label.set_xalign(0.0);
    service_info.append(&banner_label);
    let shortcut_hint = Label::new(None);
    shortcut_hint.add_css_class("dim");
    shortcut_hint.set_xalign(0.0);
    shortcut_hint.set_wrap(true);
    service_info.append(&shortcut_hint);
    banner.append(&service_info);
    let start = Button::with_label(tr("home.start"));
    start.set_valign(gtk4::Align::Center);
    {
        start.connect_clicked(move |button| {
            button.set_sensitive(false);
            button.set_label(tr("home.starting"));
            let result = std::sync::Arc::new(std::sync::Mutex::new(None));
            let sender = result.clone();
            std::thread::spawn(move || {
                let success = std::process::Command::new("systemctl")
                    .args(["--user", "start", "mywhisper.service"])
                    .stdin(std::process::Stdio::null())
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                *sender.lock().unwrap() = Some(success);
            });
            let button = button.clone();
            gtk4::glib::timeout_add_local(Duration::from_millis(150), move || {
                if let Some(success) = result.lock().unwrap().take() {
                    button.set_sensitive(true);
                    button.set_label(tr("home.start"));
                    super::app::toast(
                        &button,
                        tr(if success {
                            "home.start.ok"
                        } else {
                            "home.start.failed"
                        }),
                    );
                    gtk4::glib::ControlFlow::Break
                } else {
                    gtk4::glib::ControlFlow::Continue
                }
            });
        });
    }
    banner.append(&start);
    let hk_label = Label::new(Some(tr("home.hotkeys")));
    hk_label.add_css_class("dim");
    banner.append(&hk_label);
    let hk_switch = Switch::new();
    hk_switch.set_active(state.settings.borrow().hotkeys.hotkeys_enabled);
    hk_switch.set_valign(gtk4::Align::Center);
    hk_switch.update_property(&[gtk4::accessible::Property::Label(tr("home.hotkeys"))]);
    {
        let state = state.clone();
        hk_switch.connect_state_set(move |_, active| {
            state.settings.borrow_mut().hotkeys.hotkeys_enabled = active;
            super::app_settings::persist_prefs(state.clone())();
            gtk4::glib::Propagation::Proceed
        });
    }
    banner.append(&hk_switch);
    col.append(&banner);

    let setup_row = GtkBox::new(Orientation::Horizontal, 8);
    for (label, section) in [
        (tr("home.setup"), "Speech to Text"),
        (tr("home.shortcuts"), "Hotkeys"),
    ] {
        let button = Button::with_label(label);
        button.add_css_class("flat");
        button.connect_clicked(move |button| super::app::navigate(button, "settings", section));
        setup_row.append(&button);
    }
    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    setup_row.append(&spacer);
    let privacy = Button::with_label(tr("home.privacy"));
    privacy.add_css_class("flat");
    privacy.connect_clicked(|b| super::app::navigate(b, "settings", "Privacy & Data"));
    setup_row.append(&privacy);
    col.append(&setup_row);

    let update_label = Label::new(None);
    update_label.add_css_class("dim");
    update_label.set_xalign(0.0);
    update_label.set_wrap(true);
    update_label.set_visible(false);
    col.append(&update_label);
    let refresh_banner: Rc<dyn Fn()> = Rc::new({
        let state = state.clone();
        let hk_switch = hk_switch.clone();
        move || {
            if let Some(v) = crate::update::available_update() {
                update_label.set_text(&trf("home.update", &[("version", &v)]));
                update_label.set_visible(true);
            }
            let settings = state.settings.borrow();
            let enabled = settings.hotkeys.hotkeys_enabled;
            let binding = settings.hotkeys.effective_hotkeys().into_iter().next();
            if let Some(binding) = binding {
                let key = match binding.mode {
                    crate::settings::ActivationMode::Push => "home.shortcut.hold",
                    crate::settings::ActivationMode::Tap => "home.shortcut.toggle",
                    crate::settings::ActivationMode::Smart => "home.shortcut.smart",
                };
                shortcut_hint.set_text(&trf(key, &[("key", &binding.combo)]));
            }
            drop(settings);
            if hk_switch.is_active() != enabled {
                hk_switch.set_active(enabled);
            }
            let heartbeat = crate::service::read_heartbeat(&state_dir);
            state.service_alive.set(heartbeat.is_some());
            start.set_visible(heartbeat.is_none());
            let key = match heartbeat {
                None => "home.svc.off",
                Some(hb)
                    if hb
                        .get("engine_failures")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0)
                        >= 3 =>
                {
                    "home.svc.unhealthy"
                }
                Some(_) if !enabled => "home.paused",
                Some(_) => "home.svc.on",
            };
            banner_label.set_text(tr(key));
            dot.remove_css_class("cap-ok");
            dot.remove_css_class("cap-no");
            dot.remove_css_class("dim");
            dot.add_css_class(if key == "home.svc.on" {
                "cap-ok"
            } else if key == "home.paused" {
                "dim"
            } else {
                "cap-no"
            });
        }
    });
    refresh_banner();

    let history_header = GtkBox::new(Orientation::Horizontal, 12);
    let title = Label::new(Some(tr("home.history")));
    title.add_css_class("section-title");
    title.set_xalign(0.0);
    title.set_hexpand(true);
    history_header.append(&title);
    let toggle = gtk4::CheckButton::with_label(tr("home.showdiscarded"));
    history_header.append(&toggle);
    col.append(&history_header);
    let search = gtk4::SearchEntry::new();
    search.set_placeholder_text(Some(tr("home.search")));
    search.update_property(&[gtk4::accessible::Property::Label(tr("home.search"))]);
    col.append(&search);
    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scrolled.set_vexpand(true);
    col.append(&scrolled);
    let list = GtkBox::new(Orientation::Vertical, 10);
    list.set_vexpand(true);
    scrolled.set_child(Some(&list));
    let expanded: Rc<RefCell<HashSet<String>>> = Rc::new(RefCell::new(HashSet::new()));
    let refresh: Rc<dyn Fn()> = Rc::new({
        let state = state.clone();
        let search = search.clone();
        let toggle = toggle.clone();
        move || match state.store.load_history(200, toggle.is_active()) {
            Ok(history) => fill_history(&list, &state, &history, &expanded, &search.text()),
            Err(e) => {
                eprintln!("[home] history: {e}");
                super::app::toast(&list, tr("feedback.savefailed"));
            }
        }
    });
    refresh();
    {
        let refresh = refresh.clone();
        search.connect_search_changed(move |_| refresh());
    }
    {
        let refresh = refresh.clone();
        toggle.connect_toggled(move |_| refresh());
    }
    {
        let state = state.clone();
        let seen = Rc::new(Cell::new((
            crate::store::data_gen(),
            crate::store::read_gen(&state.store),
        )));
        let weak_root = root.downgrade();
        gtk4::glib::timeout_add_local(Duration::from_millis(1500), move || {
            if weak_root.upgrade().is_none() {
                return gtk4::glib::ControlFlow::Break;
            }
            let token = (
                crate::store::data_gen(),
                crate::store::read_gen(&state.store),
            );
            refresh_banner();
            if seen.replace(token) != token {
                refresh();
            }
            gtk4::glib::ControlFlow::Continue
        });
    }
    root
}
