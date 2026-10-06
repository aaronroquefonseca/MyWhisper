//! Home page: greeting + dictation history.
//!
//! Model: `AppState.history` (empty by default -> empty state with
//! the dictation hotkey hint). Entries render newest-first.

use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Label, ListBox, Orientation};
use std::rc::Rc;

use super::app::{AppState, centered_column};

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

    if state.history.is_empty() {
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
                state.settings.borrow().hotkeys.dictation_hotkey
            )
            .as_str(),
        ));
        h.add_css_class("dim");
        h.set_justify(gtk4::Justification::Center);
        empty.append(&t);
        empty.append(&h);
        col.append(&empty);
    } else {
        let list = ListBox::new();
        list.add_css_class("card");
        for entry in state.history.iter().rev() {
            let row = GtkBox::new(Orientation::Vertical, 2);
            let text = Label::new(Some(entry.text.as_str()));
            text.set_xalign(0.0);
            text.set_wrap(true);
            let meta = Label::new(Some(
                format!("{} · {} words", entry.date_label, entry.word_count).as_str(),
            ));
            meta.set_xalign(0.0);
            meta.add_css_class("dim");
            row.append(&text);
            row.append(&meta);
            list.append(&row);
        }
        col.append(&list);
    }

    root
}
