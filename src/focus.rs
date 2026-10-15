//! Retain focus owners across reentrant GTK focus transitions.
use gtk4::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default)]
struct Owners {
    current: Option<gtk4::Widget>,
    retired: Vec<gtk4::Widget>,
    scheduled: bool,
    closed: bool,
}

fn after_dispatch(owners: Vec<gtk4::Widget>) {
    let mut owners = Some(owners);
    gtk4::glib::idle_add_local_full(gtk4::glib::Priority::DEFAULT, move || {
        drop(owners.take());
        gtk4::glib::ControlFlow::Break
    });
}

pub(crate) fn install(window: &impl IsA<gtk4::Window>) {
    let window: &gtk4::Window = window.as_ref();
    let owners = Rc::new(RefCell::new(Owners::default()));
    window.connect_focus_widget_notify({
        let owners = owners.clone();
        move |window| {
            // GTK 4.24's GtkLabel::grab_focus borrows prev_focus, then calls
            // the parent focus implementation before using that pointer.
            // Dismissal/refresh can destroy the previous widget reentrantly.
            // Keep it alive through the current dispatch without disabling
            // selectable labels, dialog dismissal or focus restoration.
            let schedule = {
                let mut state = owners.borrow_mut();
                if state.closed {
                    return;
                }
                let focused = gtk4::prelude::GtkWindowExt::focus(window);
                if state.current == focused {
                    return;
                }
                if let Some(previous) = std::mem::replace(&mut state.current, focused) {
                    state.retired.push(previous);
                }
                let schedule = !state.scheduled && !state.retired.is_empty();
                state.scheduled |= schedule;
                schedule
            };
            if schedule {
                let weak = Rc::downgrade(&owners);
                gtk4::glib::idle_add_local_full(gtk4::glib::Priority::DEFAULT, move || {
                    if let Some(owners) = weak.upgrade() {
                        let retired = {
                            let mut state = owners.borrow_mut();
                            state.scheduled = false;
                            std::mem::take(&mut state.retired)
                        };
                        // Widget destruction may emit focus notifications.
                        // Never hold the state borrow across that destruction.
                        drop(retired);
                    }
                    gtk4::glib::ControlFlow::Break
                });
            }
        }
    });
    window.connect_destroy(move |_| {
        let retired = {
            let mut state = owners.borrow_mut();
            state.closed = true;
            if let Some(current) = state.current.take() {
                state.retired.push(current);
            }
            std::mem::take(&mut state.retired)
        };
        after_dispatch(retired);
    });
}
