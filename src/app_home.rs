//! Home page: greeting + dictation history, straight from the DB.
//!
//! Renders `AppState.history` newest-first; a generation poll refills
//! the list when synced entries land while the app is open.

use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Label, Orientation};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use super::app::{AppState, centered_column};
use crate::settings::HistoryEntry;

fn clear_box(container: &GtkBox) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn fill_history(container: &GtkBox, state: &Rc<AppState>, history: &[HistoryEntry]) {
    clear_box(container);
    if history.is_empty() {
        let empty = GtkBox::new(Orientation::Vertical, 8);
        empty.add_css_class("card");
        empty.set_halign(gtk4::Align::Center);
        empty.set_valign(gtk4::Align::Center);
        empty.set_vexpand(true);
        let t = Label::new(Some("No dictations yet"));
        t.add_css_class("section-title");
        let h = Label::new(Some(
            format!(
                "Your history will appear here.\nDefault hotkey: {}",
                state
                    .settings
                    .borrow()
                    .hotkeys
                    .effective_hotkeys()
                    .iter()
                    .map(|b| b.combo.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            )
            .as_str(),
        ));
        h.add_css_class("dim");
        h.set_justify(gtk4::Justification::Center);
        empty.append(&t);
        empty.append(&h);
        container.append(&empty);
        return;
    }
    for entry in history.iter().rev() {
        let card = GtkBox::new(Orientation::Vertical, 2);
        card.add_css_class("card");
        let text = Label::new(Some(entry.text.as_str()));
        text.set_xalign(0.0);
        text.set_wrap(true);
        let meta = Label::new(Some(
            format!("{} · {} words", entry.date_label, entry.word_count).as_str(),
        ));
        meta.set_xalign(0.0);
        meta.add_css_class("dim");
        card.append(&text);
        card.append(&meta);
        container.append(&card);
    }
}

pub fn page(state: &Rc<AppState>) -> GtkBox {
    let root = GtkBox::new(Orientation::Vertical, 16);
    root.add_css_class("page");

    let col = centered_column(&root, 880);

    let greeting = Label::new(Some("Welcome back"));
    greeting.add_css_class("page-title");
    greeting.set_xalign(0.0);
    col.append(&greeting);

    let sub = Label::new(Some(
        "Press your dictation hotkey anywhere to start writing with your voice.",
    ));
    sub.add_css_class("page-subtitle");
    sub.set_xalign(0.0);
    col.append(&sub);

    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    col.append(&scrolled);
    let list = GtkBox::new(Orientation::Vertical, 8);
    scrolled.set_child(Some(&list));
    fill_history(&list, state, &state.history);

    // Live refresh on synced arrivals (same generation signal as the
    // dictionary page).
    {
        let state = state.clone();
        let seen = Rc::new(Cell::new(crate::store::data_gen()));
        gtk4::glib::timeout_add_local(Duration::from_millis(1500), move || {
            if seen.get() != crate::store::data_gen() {
                seen.set(crate::store::data_gen());
                let history = state.store.load_history(200).unwrap_or_default();
                fill_history(&list, &state, &history);
            }
            gtk4::glib::ControlFlow::Continue
        });
    }

    root
}
