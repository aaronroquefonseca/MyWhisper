//! Words bias supported engines; spoken snippets replace transcript phrases.
//! CRUD persists to versioned tables. Editing survives background refreshes.
use crate::app::{AppState, confirm, page_heading, toast};
use crate::i18n::tr;
use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Button, Entry, Label, Notebook, Orientation, TextView};
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

fn clear_box(container: &GtkBox) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn hint(parent: &GtkBox, text: &str) {
    let label = Label::new(Some(text));
    label.add_css_class("dim");
    label.set_xalign(0.0);
    label.set_wrap(true);
    parent.append(&label);
}

fn field_label(parent: &GtkBox, text: &str) {
    let label = Label::new(Some(text));
    label.add_css_class("row-label");
    label.set_xalign(0.0);
    parent.append(&label);
}

fn action(text: &str, primary: bool) -> Button {
    let button = Button::with_label(text);
    button.set_valign(gtk4::Align::Center);
    if primary {
        button.add_css_class("suggested-action");
    }
    button
}

fn changed(state: &Rc<AppState>) {
    let mut settings = state.settings.borrow_mut();
    settings.dictionary.words = state.store.load_words().unwrap_or_default();
    settings.dictionary.snippets = state.store.load_snippets().unwrap_or_default();
    drop(settings);
    crate::net::sync_after_change(state.db_path.clone(), state.creds.clone());
}

fn invalid(entry: &Entry, message: &str) {
    entry.add_css_class("error");
    entry.grab_focus();
    toast(entry, message);
}

fn labeled_entry(text: &str, label: &str) -> Entry {
    let entry = Entry::new();
    entry.set_text(text);
    entry.set_hexpand(true);
    entry.update_property(&[gtk4::accessible::Property::Label(label)]);
    entry.connect_changed(|entry| entry.remove_css_class("error"));
    entry
}

fn duplicate_word(state: &AppState, value: &str, original: Option<&str>) -> bool {
    state
        .store
        .load_words()
        .unwrap_or_default()
        .iter()
        .any(|word| Some(word.as_str()) != original && word.to_lowercase() == value.to_lowercase())
}

fn duplicate_snippet(state: &AppState, value: &str, original: Option<&str>) -> bool {
    let triggers: HashSet<_> = value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect();
    state
        .store
        .load_snippets()
        .unwrap_or_default()
        .iter()
        .any(|snippet| {
            Some(snippet.trigger.as_str()) != original
                && snippet
                    .triggers()
                    .iter()
                    .any(|trigger| triggers.contains(&trigger.to_lowercase()))
        })
}

fn word_card(state: &Rc<AppState>, card: &GtkBox, word: &str, editing: bool) {
    clear_box(card);
    if editing {
        card.add_css_class("editing");
    } else {
        card.remove_css_class("editing");
    }
    if !editing {
        let label = Label::new(Some(word));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_wrap(true);
        label.set_selectable(true);
        card.append(&label);
        let edit = action(tr("action.edit"), false);
        {
            let state = state.clone();
            let card = card.clone();
            let word = word.to_string();
            edit.connect_clicked(move |_| word_card(&state, &card, &word, true));
        }
        card.append(&edit);
        let delete = action(tr("action.delete"), false);
        delete.add_css_class("destructive-action");
        {
            let state = state.clone();
            let card = card.clone();
            let word = word.to_string();
            delete.connect_clicked(move |button| {
                let state = state.clone();
                let card = card.clone();
                let word = word.clone();
                let button = button.clone();
                confirm(
                    &button.clone(),
                    tr("confirm.delete.title"),
                    tr("confirm.delete.body"),
                    tr("action.delete"),
                    move || {
                        if state.store.delete_word(&word, &state.node_id).is_err() {
                            toast(&button, tr("feedback.deletefailed"));
                            return;
                        }
                        toast(&button, tr("feedback.deleted"));
                        card.set_visible(false);
                        changed(&state);
                    },
                );
            });
        }
        card.append(&delete);
    } else {
        let entry = labeled_entry(word, tr("dict.words.label"));
        card.append(&entry);
        let save = action(tr("action.save"), true);
        let commit: Rc<dyn Fn()> = Rc::new({
            let state = state.clone();
            let card = card.clone();
            let entry = entry.clone();
            let original = word.to_string();
            move || {
                let value = entry.text().trim().to_string();
                if value.is_empty() {
                    invalid(&entry, tr("dict.required"));
                    return;
                }
                if duplicate_word(&state, &value, Some(&original)) {
                    invalid(&entry, tr("dict.duplicate"));
                    return;
                }
                if value != original {
                    // Save the replacement first so a failed save never loses the original.
                    if state
                        .store
                        .upsert_word(&value, &state.node_id)
                        .and_then(|_| state.store.delete_word(&original, &state.node_id))
                        .is_err()
                    {
                        toast(&entry, tr("feedback.savefailed"));
                        return;
                    }
                    changed(&state);
                }
                toast(&entry, tr("feedback.saved"));
                word_card(&state, &card, &value, false);
            }
        });
        let enter = commit.clone();
        entry.connect_activate(move |_| enter());
        save.connect_clicked(move |_| commit());
        card.append(&save);
        let cancel = action(tr("action.cancel"), false);
        {
            let state = state.clone();
            let card = card.clone();
            let word = word.to_string();
            cancel.connect_clicked(move |_| word_card(&state, &card, &word, false));
        }
        card.append(&cancel);
        entry.grab_focus();
    }
}

fn fill_words(list: &GtkBox, state: &Rc<AppState>) {
    clear_box(list);
    let words = state.store.load_words().unwrap_or_default();
    if words.is_empty() {
        hint(list, tr("dict.words.empty"));
    }
    for word in words {
        let card = GtkBox::new(Orientation::Horizontal, 8);
        card.add_css_class("card");
        word_card(state, &card, &word, false);
        list.append(&card);
    }
}

fn words_tab(state: &Rc<AppState>) -> (GtkBox, GtkBox) {
    let root = GtkBox::new(Orientation::Vertical, 14);
    hint(&root, tr("dict.words.hint"));
    field_label(&root, tr("dict.words.label"));
    let row = GtkBox::new(Orientation::Horizontal, 8);
    let entry = labeled_entry("", tr("dict.words.label"));
    entry.set_widget_name("new-word");
    entry.set_placeholder_text(Some(tr("dict.words.add")));
    row.append(&entry);
    let add = action(tr("dict.words.create"), true);
    row.append(&add);
    root.append(&row);
    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scrolled.set_vexpand(true);
    root.append(&scrolled);
    let list = GtkBox::new(Orientation::Vertical, 8);
    scrolled.set_child(Some(&list));
    fill_words(&list, state);
    let commit: Rc<dyn Fn()> = Rc::new({
        let state = state.clone();
        let list = list.clone();
        let entry = entry.clone();
        move || {
            let value = entry.text().trim().to_string();
            if value.is_empty() {
                invalid(&entry, tr("dict.required"));
                return;
            }
            if duplicate_word(&state, &value, None) {
                invalid(&entry, tr("dict.duplicate"));
                return;
            }
            if state.store.upsert_word(&value, &state.node_id).is_err() {
                toast(&entry, tr("feedback.savefailed"));
                return;
            }
            entry.set_text("");
            changed(&state);
            fill_words(&list, &state);
            toast(&entry, tr("feedback.saved"));
            entry.grab_focus();
        }
    });
    let enter = commit.clone();
    entry.connect_activate(move |_| enter());
    add.connect_clicked(move |_| commit());
    (root, list)
}

fn snippet_text(view: &TextView) -> String {
    let buffer = view.buffer();
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .to_string()
}

fn text_field(text: &str) -> TextView {
    let view = TextView::new();
    view.buffer().set_text(text);
    view.set_wrap_mode(gtk4::WrapMode::WordChar);
    view.set_size_request(-1, 90);
    view.update_property(&[gtk4::accessible::Property::Label(tr(
        "dict.snippets.expansion",
    ))]);
    view
}

fn snippet_card(
    state: &Rc<AppState>,
    card: &GtkBox,
    trigger: &str,
    replacement: &str,
    editing: bool,
) {
    clear_box(card);
    if editing {
        card.add_css_class("editing");
    } else {
        card.remove_css_class("editing");
    }
    if !editing {
        let row = GtkBox::new(Orientation::Horizontal, 8);
        let label = Label::new(Some(trigger));
        label.add_css_class("row-label");
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_wrap(true);
        row.append(&label);
        let edit = action(tr("action.edit"), false);
        {
            let state = state.clone();
            let card = card.clone();
            let trigger = trigger.to_string();
            let replacement = replacement.to_string();
            edit.connect_clicked(move |_| {
                snippet_card(&state, &card, &trigger, &replacement, true)
            });
        }
        row.append(&edit);
        let delete = action(tr("action.delete"), false);
        delete.add_css_class("destructive-action");
        {
            let state = state.clone();
            let card = card.clone();
            let trigger = trigger.to_string();
            delete.connect_clicked(move |button| {
                let state = state.clone();
                let card = card.clone();
                let trigger = trigger.clone();
                let button = button.clone();
                confirm(
                    &button.clone(),
                    tr("confirm.delete.title"),
                    tr("confirm.delete.body"),
                    tr("action.delete"),
                    move || {
                        if state
                            .store
                            .delete_snippet(&trigger, &state.node_id)
                            .is_err()
                        {
                            toast(&button, tr("feedback.deletefailed"));
                            return;
                        }
                        toast(&button, tr("feedback.deleted"));
                        card.set_visible(false);
                        changed(&state);
                    },
                );
            });
        }
        row.append(&delete);
        card.append(&row);
        let body = Label::new(Some(replacement));
        body.set_xalign(0.0);
        body.set_wrap(true);
        body.set_selectable(true);
        body.add_css_class("dim");
        card.append(&body);
    } else {
        field_label(card, tr("dict.snippets.label"));
        let input = labeled_entry(trigger, tr("dict.snippets.label"));
        card.append(&input);
        field_label(card, tr("dict.snippets.expansion"));
        let expansion = text_field(replacement);
        card.append(&expansion);
        let row = GtkBox::new(Orientation::Horizontal, 8);
        row.set_halign(gtk4::Align::End);
        let cancel = action(tr("action.cancel"), false);
        {
            let state = state.clone();
            let card = card.clone();
            let trigger = trigger.to_string();
            let replacement = replacement.to_string();
            cancel.connect_clicked(move |_| {
                snippet_card(&state, &card, &trigger, &replacement, false)
            });
        }
        row.append(&cancel);
        let save = action(tr("action.save"), true);
        {
            let state = state.clone();
            let card = card.clone();
            let original = trigger.to_string();
            let input = input.clone();
            save.connect_clicked(move |button| {
                let trigger = input.text().trim().to_string();
                let replacement = snippet_text(&expansion).trim().to_string();
                if trigger.split(',').all(|part| part.trim().is_empty()) || replacement.is_empty() {
                    invalid(&input, tr("dict.snippets.required"));
                    return;
                }
                if duplicate_snippet(&state, &trigger, Some(&original)) {
                    invalid(&input, tr("dict.duplicate"));
                    return;
                }
                let result = state
                    .store
                    .upsert_snippet(&trigger, &replacement, &state.node_id)
                    .and_then(|_| {
                        if trigger != original {
                            state.store.delete_snippet(&original, &state.node_id)
                        } else {
                            Ok(())
                        }
                    });
                if result.is_err() {
                    toast(button, tr("feedback.savefailed"));
                    return;
                }
                toast(button, tr("feedback.saved"));
                changed(&state);
                snippet_card(&state, &card, &trigger, &replacement, false);
            });
        }
        row.append(&save);
        card.append(&row);
        input.grab_focus();
    }
}

fn fill_snippets(list: &GtkBox, state: &Rc<AppState>) {
    clear_box(list);
    let snippets = state.store.load_snippets().unwrap_or_default();
    if snippets.is_empty() {
        hint(list, tr("dict.snippets.empty"));
    }
    for snippet in snippets {
        let card = GtkBox::new(Orientation::Vertical, 10);
        card.add_css_class("card");
        snippet_card(state, &card, &snippet.trigger, &snippet.replacement, false);
        list.append(&card);
    }
}

fn snippets_tab(state: &Rc<AppState>) -> (GtkBox, GtkBox) {
    let root = GtkBox::new(Orientation::Vertical, 12);
    hint(&root, tr("dict.snippets.hint"));
    let form = GtkBox::new(Orientation::Vertical, 10);
    form.add_css_class("card");
    field_label(&form, tr("dict.snippets.label"));
    let trigger = labeled_entry("", tr("dict.snippets.label"));
    trigger.set_widget_name("snippet-trigger");
    trigger.set_placeholder_text(Some(tr("dict.snippets.trigger")));
    form.append(&trigger);
    field_label(&form, tr("dict.snippets.expansion"));
    let expansion = text_field("");
    expansion.set_widget_name("snippet-expansion");
    form.append(&expansion);
    let create = action(tr("dict.snippets.create"), true);
    create.set_halign(gtk4::Align::End);
    form.append(&create);
    root.append(&form);
    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    scrolled.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    root.append(&scrolled);
    let list = GtkBox::new(Orientation::Vertical, 8);
    scrolled.set_child(Some(&list));
    fill_snippets(&list, state);
    {
        let state = state.clone();
        let list = list.clone();
        create.connect_clicked(move |button| {
            let value = trigger.text().trim().to_string();
            let replacement = snippet_text(&expansion).trim().to_string();
            if value.split(',').all(|part| part.trim().is_empty()) || replacement.is_empty() {
                invalid(&trigger, tr("dict.snippets.required"));
                return;
            }
            if duplicate_snippet(&state, &value, None) {
                invalid(&trigger, tr("dict.duplicate"));
                return;
            }
            if state
                .store
                .upsert_snippet(&value, &replacement, &state.node_id)
                .is_err()
            {
                toast(button, tr("feedback.savefailed"));
                return;
            }
            trigger.set_text("");
            expansion.buffer().set_text("");
            changed(&state);
            fill_snippets(&list, &state);
            toast(button, tr("feedback.saved"));
            trigger.grab_focus();
        });
    }
    (root, list)
}

fn editing(list: &GtkBox) -> bool {
    let mut child = list.first_child();
    while let Some(card) = child {
        if card.has_css_class("editing") {
            return true;
        }
        child = card.next_sibling();
    }
    false
}

pub fn page(state: &Rc<AppState>) -> GtkBox {
    let root = GtkBox::new(Orientation::Vertical, 16);
    root.add_css_class("page");
    let col = super::app::centered_column(&root, 860);
    page_heading(&col, tr("dict.title"), tr("dict.subtitle"));
    let tabs = Notebook::new();
    let (words_root, words_list) = words_tab(state);
    let (snippets_root, snippets_list) = snippets_tab(state);
    tabs.append_page(&words_root, Some(&Label::new(Some(tr("dict.tab.words")))));
    tabs.append_page(
        &snippets_root,
        Some(&Label::new(Some(tr("dict.tab.snippets")))),
    );
    tabs.set_vexpand(true);
    col.append(&tabs);
    {
        let state = state.clone();
        let seen = Cell::new((
            crate::store::data_gen(),
            crate::store::read_gen(&state.store),
        ));
        let weak_root = root.downgrade();
        gtk4::glib::timeout_add_local(Duration::from_millis(1000), move || {
            if weak_root.upgrade().is_none() {
                return gtk4::glib::ControlFlow::Break;
            }
            let token = (
                crate::store::data_gen(),
                crate::store::read_gen(&state.store),
            );
            if seen.get() != token && !editing(&words_list) && !editing(&snippets_list) {
                seen.set(token);
                let mut settings = state.settings.borrow_mut();
                settings.dictionary.words = state.store.load_words().unwrap_or_default();
                settings.dictionary.snippets = state.store.load_snippets().unwrap_or_default();
                drop(settings);
                fill_words(&words_list, &state);
                fill_snippets(&snippets_list, &state);
            }
            gtk4::glib::ControlFlow::Continue
        });
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_validation_respects_aliases_case_and_the_edited_record() {
        let state = AppState::fresh();
        state.store.upsert_word("MyWhisper", "test").unwrap();
        assert!(duplicate_word(&state, "mywhisper", None));
        assert!(!duplicate_word(&state, "MYWHISPER", Some("MyWhisper")));
        state
            .store
            .upsert_snippet("signature, my sign off", "First line\nSecond line", "test")
            .unwrap();
        assert!(duplicate_snippet(&state, "other phrase, MY SIGN OFF", None));
        assert!(!duplicate_snippet(&state, "new phrase", None));
        assert!(!duplicate_snippet(
            &state,
            "SIGNATURE",
            Some("signature, my sign off")
        ));
    }
}
