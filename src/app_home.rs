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

use super::app::{centered_column, AppState};
use crate::i18n::{tr, trf};
use crate::settings::HistoryEntry;

fn clear_box(container: &GtkBox) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

/// Local-midnight day key (days since epoch) for grouping.
fn day_key(epoch_secs: i64) -> i64 {
    // Local offset, best effort (whole minutes): groups "today" the
    // way the user experiences it, not in UTC.
    (epoch_secs + local_offset_minutes() * 60) / 86400
}

fn local_offset_minutes() -> i64 {
    // g_get_timezone difference, cheaply: compare glib local vs UTC
    // for "now". Falls back to 0 without glib.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let dt = gtk4::glib::DateTime::from_unix_utc(now).ok();
    let local = dt.as_ref().and_then(|d| d.to_local().ok());
    match (dt, local) {
        (Some(u), Some(l)) => (l.to_unix() - u.to_unix()) / 60,
        _ => 0,
    }
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
        .map(|d| {
            d.format("%I:%M %p")
                .map(|s| s.to_string())
                .unwrap_or_default()
        })
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
) {
    clear_box(container);
    if history.is_empty() {
        let empty = GtkBox::new(Orientation::Vertical, 8);
        empty.add_css_class("card");
        empty.set_halign(gtk4::Align::Center);
        empty.set_valign(gtk4::Align::Center);
        empty.set_vexpand(true);
        let t = Label::new(Some(tr("home.empty.title")));
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
        let h = Label::new(Some(
            trf("home.empty.hint", &[("hotkey", &hotkeys)]).as_str(),
        ));
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
        copy.connect_clicked(move |_| {
            if let Err(e) = crate::clipboard::copy_text(&text, false) {
                eprintln!("[home] copy: {e}");
            }
        });
    }
    head.append(&copy);
    let more = Button::with_label("⋮");
    head.append(&more);
    card.append(&head);

    let text = Label::new(Some(entry.text.as_str()));
    text.set_xalign(0.0);
    text.set_wrap(true);
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
        del_btn.connect_clicked(move |_| {
            if let Err(e) = state.store.delete_history(&uuid) {
                eprintln!("[home] delete: {e}");
            }
        });
    }

    // Raw transcript section (OpenWhispr-style inline expansion).
    let raw_box = GtkBox::new(Orientation::Vertical, 2);
    raw_box.set_visible(expanded.borrow().contains(&entry.uuid));
    let raw_title = Label::new(Some(tr("home.raw.title")));
    raw_title.set_xalign(0.0);
    raw_title.add_css_class("dim");
    raw_box.append(&raw_title);
    let raw = Label::new(Some(entry.raw_text.as_str()));
    raw.set_xalign(0.0);
    raw.set_wrap(true);
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
        raw_btn.connect_clicked(move |_| {
            let show = !raw_box.is_visible();
            raw_box.set_visible(show);
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
        del_btn.connect_clicked(move |_| {
            if let Err(e) = state.store.delete_history(&uuid) {
                eprintln!("[home] delete: {e}");
            }
        });
    }
    head.append(&del_btn);
    card.append(&head);
    card
}

pub fn page(state: &Rc<AppState>) -> GtkBox {
    let root = GtkBox::new(Orientation::Vertical, 16);
    root.add_css_class("page");

    let col = centered_column(&root, 880);

    // Service banner (top): health from the heartbeat file + the
    // hotkey-listen toggle. Refreshed on the timer below.
    let state_dir = crate::paths::AppPaths::create()
        .map(|p| p.state_dir)
        .unwrap_or_default();
    let banner = GtkBox::new(Orientation::Horizontal, 8);
    banner.add_css_class("card");
    let dot = Label::new(Some("●"));
    dot.add_css_class("cap-no");
    banner.append(&dot);
    let banner_label = Label::new(Some(tr("home.svc.off")));
    banner_label.set_xalign(0.0);
    banner_label.set_hexpand(true);
    banner.append(&banner_label);
    let hk_label = Label::new(Some(tr("home.hotkeys")));
    hk_label.add_css_class("dim");
    banner.append(&hk_label);
    let hk_switch = Switch::new();
    hk_switch.set_active(state.settings.borrow().hotkeys.hotkeys_enabled);
    hk_switch.set_valign(gtk4::Align::Center);
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
    let refresh_banner: Rc<dyn Fn()> = Rc::new({
        let dot = dot.clone();
        let banner_label = banner_label.clone();
        let state_dir = state_dir.clone();
        move || match crate::service::read_heartbeat(&state_dir) {
            None => {
                dot.set_text("●");
                dot.add_css_class("cap-no");
                dot.remove_css_class("cap-ok");
                banner_label.set_text(tr("home.svc.off"));
            }
            Some(hb) => {
                let fails = hb
                    .get("engine_failures")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                dot.set_text("●");
                if fails >= 3 {
                    dot.add_css_class("cap-no");
                    dot.remove_css_class("cap-ok");
                    banner_label.set_text(tr("home.svc.unhealthy"));
                } else {
                    dot.add_css_class("cap-ok");
                    dot.remove_css_class("cap-no");
                    banner_label.set_text(tr("home.svc.on"));
                }
            }
        }
    });
    refresh_banner();
    let greeting = Label::new(Some(tr("home.greeting")));
    greeting.add_css_class("page-title");
    greeting.set_xalign(0.0);
    col.append(&greeting);

    let sub = Label::new(Some(tr("home.subtitle")));
    sub.add_css_class("page-subtitle");
    sub.set_xalign(0.0);
    col.append(&sub);

    // Show-discarded toggle (in-memory, default off like the
    // reference): discarded rows are midway cancels, not dictations.
    let show_discarded: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let toggle = Button::with_label(tr("home.showdiscarded"));
    toggle.set_halign(gtk4::Align::End);
    col.append(&toggle);
    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    col.append(&scrolled);
    let list = GtkBox::new(Orientation::Vertical, 8);
    scrolled.set_child(Some(&list));
    // Raw-expander state survives the 1500 ms refills below.
    let expanded: Rc<RefCell<HashSet<String>>> = Rc::new(RefCell::new(HashSet::new()));
    fill_history(&list, state, &state.history, &expanded);
    {
        let state = state.clone();
        let list = list.clone();
        let expanded = expanded.clone();
        let show_discarded = show_discarded.clone();
        toggle.connect_clicked(move |t| {
            let show = !show_discarded.get();
            show_discarded.set(show);
            t.set_label(if show {
                tr("home.hidediscarded")
            } else {
                tr("home.showdiscarded")
            });
            let history = state.store.load_history(200, show).unwrap_or_default();
            fill_history(&list, &state, &history, &expanded);
        });
    }

    {
        let state = state.clone();
        let expanded = expanded.clone();
        let show_discarded = show_discarded.clone();
        let refresh_banner = refresh_banner.clone();
        let seen = Rc::new(Cell::new((crate::store::data_gen(), 0i64)));
        gtk4::glib::timeout_add_local(Duration::from_millis(1500), move || {
            let token = (
                crate::store::data_gen(),
                crate::store::read_gen(&state.store),
            );
            refresh_banner();
            if seen.get() != token {
                seen.set(token);
                let history = state
                    .store
                    .load_history(200, show_discarded.get())
                    .unwrap_or_default();
                fill_history(&list, &state, &history, &expanded);
            }
            gtk4::glib::ControlFlow::Continue
        });
    }

    root
}
