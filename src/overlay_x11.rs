//! X11 overlay fallback for the pill when the compositor has no
//! layer-shell: X11 sessions, and GNOME/Mutter on Wayland through
//! XWayland. GTK4 dropped window types, stacking and positioning, so
//! the pill's X window gets EWMH hints and moves over its own x11rb
//! connection: notification type, always above, on every workspace,
//! out of the taskbar/pager, never focused, pinned to the bottom edge.

use crate::settings::FloatingPosition;
use gdk4_x11::{X11Display, X11Monitor, X11Surface};
use gtk4::prelude::*;
use std::cell::Cell;
use std::rc::Rc;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, EventMask, PropMode,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// Pixels between the pill and the work-area edges (matches layer-shell margins).
const BOTTOM_MARGIN: i32 = 24;
const SIDE_MARGIN: i32 = 24;

/// X11 display for the pill: the default display when it is X11,
/// otherwise `$DISPLAY` (XWayland). `None` means no X server.
pub fn display() -> Option<gtk4::gdk::Display> {
    if let Some(default) = gtk4::gdk::Display::default()
        && default.is::<X11Display>()
    {
        return Some(default);
    }
    std::env::var_os("DISPLAY").filter(|d| !d.is_empty())?;
    // Deprecated with the whole GTK4 X11 backend; still the only way in.
    #[allow(deprecated)]
    X11Display::open(None)
}

struct Atoms {
    wm_protocols: u32,
    wm_delete: u32,
    user_time: u32,
    wm_state: u32,
    above: u32,
    sticky: u32,
    window_type: u32,
    notification: u32,
}

/// Turn `window` (already on the X11 `display`) into the overlay and
/// keep it placed at `position` whenever it maps or resizes.
pub fn attach(window: &gtk4::Window, position: Rc<Cell<FloatingPosition>>) {
    let conn = match x11rb::connect(None) {
        Ok((conn, _)) => Rc::new(conn),
        Err(e) => {
            eprintln!("[poc] WARNING: X11 overlay hints unavailable: {e}");
            return;
        }
    };
    let Some(atoms) = intern(&conn) else {
        eprintln!("[poc] WARNING: X11 overlay atoms unavailable");
        return;
    };
    let atoms = Rc::new(atoms);

    // The window type only counts before the first map.
    {
        let conn = conn.clone();
        let atoms = atoms.clone();
        let position = position.clone();
        window.connect_realize(move |w| {
            let Some(surface) = w.surface().and_downcast::<X11Surface>() else {
                return;
            };
            surface.set_skip_taskbar_hint(true);
            surface.set_skip_pager_hint(true);
            let xid = surface.xid() as u32;
            let _ = conn.change_property32(
                PropMode::REPLACE,
                xid,
                atoms.window_type,
                AtomEnum::ATOM,
                &[atoms.notification],
            );
            // No WM_TAKE_FOCUS (GTK's default) and user time 0: the WM
            // must not hand the pill focus when it maps, or the paste
            // lands in the pill instead of the dictation target.
            let _ = conn.change_property32(
                PropMode::REPLACE,
                xid,
                atoms.wm_protocols,
                AtomEnum::ATOM,
                &[atoms.wm_delete],
            );
            let _ = conn.change_property32(
                PropMode::REPLACE,
                xid,
                atoms.user_time,
                AtomEnum::CARDINAL,
                &[0],
            );
            let _ = conn.flush();
            let conn = conn.clone();
            let position = position.clone();
            surface.connect_layout(move |s, w, h| place(&conn, s, w, h, position.get()));
        });
    }
    // GTK rewrites WM_HINTS and _NET_WM_STATE at map, so ours go after.
    window.connect_map(move |w| {
        let Some(surface) = w.surface().and_downcast::<X11Surface>() else {
            return;
        };
        let xid = surface.xid() as u32;
        // WM_HINTS: flags = InputHint, input = False → never focused.
        let _ = conn.change_property32(
            PropMode::REPLACE,
            xid,
            AtomEnum::WM_HINTS,
            AtomEnum::WM_HINTS,
            &[1, 0, 0, 0, 0, 0, 0, 0, 0],
        );
        let root = conn.setup().roots[0].root;
        // _NET_WM_STATE_ADD = 1, source indication 1 = normal application.
        let event = ClientMessageEvent::new(
            32,
            xid,
            atoms.wm_state,
            [1, atoms.above, atoms.sticky, 1, 0],
        );
        let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
        let _ = conn.send_event(false, root, mask, event);
        place(&conn, &surface, w.width(), w.height(), position.get());
    });
}

fn intern(conn: &RustConnection) -> Option<Atoms> {
    let atom = |name: &str| -> Option<u32> {
        Some(
            conn.intern_atom(false, name.as_bytes())
                .ok()?
                .reply()
                .ok()?
                .atom,
        )
    };
    Some(Atoms {
        wm_protocols: atom("WM_PROTOCOLS")?,
        wm_delete: atom("WM_DELETE_WINDOW")?,
        user_time: atom("_NET_WM_USER_TIME")?,
        wm_state: atom("_NET_WM_STATE")?,
        above: atom("_NET_WM_STATE_ABOVE")?,
        sticky: atom("_NET_WM_STATE_STICKY")?,
        window_type: atom("_NET_WM_WINDOW_TYPE")?,
        notification: atom("_NET_WM_WINDOW_TYPE_NOTIFICATION")?,
    })
}

/// Move the window to its spot in the primary monitor's work area.
fn place(conn: &RustConnection, surface: &X11Surface, w: i32, h: i32, pos: FloatingPosition) {
    let Some(display) = surface.display().downcast::<X11Display>().ok() else {
        return;
    };
    let Ok(monitor) = display.primary_monitor().downcast::<X11Monitor>() else {
        return;
    };
    let (x, y) = spot(monitor.workarea(), w, h, pos);
    // GDK sizes are logical; X wants device pixels.
    let scale = surface.scale_factor();
    let aux = ConfigureWindowAux::new().x(x * scale).y(y * scale);
    let _ = conn.configure_window(surface.xid() as u32, &aux);
    let _ = conn.flush();
}

fn spot(area: gtk4::gdk::Rectangle, w: i32, h: i32, pos: FloatingPosition) -> (i32, i32) {
    let x = match pos {
        FloatingPosition::BottomCenter => area.x() + (area.width() - w) / 2,
        FloatingPosition::BottomLeft => area.x() + SIDE_MARGIN,
        FloatingPosition::BottomRight => area.x() + area.width() - w - SIDE_MARGIN,
    };
    (x, area.y() + area.height() - h - BOTTOM_MARGIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pill_sits_above_the_bottom_of_the_work_area() {
        // Work area below a 32 px top bar on a 1920x1080 monitor.
        let area = gtk4::gdk::Rectangle::new(0, 32, 1920, 1048);
        assert_eq!(
            spot(area, 100, 52, FloatingPosition::BottomCenter),
            (910, 32 + 1048 - 52 - 24)
        );
        assert_eq!(spot(area, 100, 52, FloatingPosition::BottomLeft).0, 24);
        assert_eq!(
            spot(area, 100, 52, FloatingPosition::BottomRight).0,
            1920 - 100 - 24
        );
    }
}
