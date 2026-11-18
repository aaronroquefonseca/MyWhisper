//! Global hotkeys via direct evdev reads (OpenWhispr method).
//!
//! A background thread opens every `/dev/input/event*` keyboard
//! (EV_KEY + KEY_A capability, same filter as OpenWhispr's
//! `linux-key-listener.c`) read-only -- never grabbed, never
//! intercepted -- and tracks held keys. Each configured combo
//! resolves to `(modifiers, target)` exactly like the C listener:
//! press emits `KeyDown`, release emits `KeyUp`.
//!
//! Actions are an enum (`DictationToggle` today) so future actions
//! reuse the same manager. Bindings are a `Vec<String>` per action:
//! any combo may fire it.

use evdev::{Device, EventSummary, EventType, KeyCode};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

/// Extensible action set. Only dictation exists today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyAction {
    DictationToggle,
}

/// Parsed combo: required modifiers + one target key.
/// Modifier-only combos (e.g. "Control+Super") set `target = None`.
#[derive(Debug, Clone)]
pub struct Combo {
    pub raw: String,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_: bool,
    pub target: Option<KeyCode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyStatus {
    Listening,
    NoPermission,
    NoDevices,
}

/// Key-down / key-up for one action binding. The mode travels with
/// the event so each binding behaves per its own setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    Down(HotkeyAction, crate::settings::ActivationMode),
    Up(HotkeyAction, crate::settings::ActivationMode),
    /// Escape pressed while a dictation is live: cancel midway.
    Cancel,
}

pub type HotkeyCallback = Arc<dyn Fn(HotkeyEvent) + Send + Sync + 'static>;
type BindingCallback = Arc<dyn Fn(HotkeyEvent, Option<usize>) + Send + Sync + 'static>;

/// Generation counter: bumped on every (re)start so a stale loop
/// exits instead of double-firing after settings change.
static HOTKEY_GEN: AtomicU64 = AtomicU64::new(0);
// A retiring loop must send its cancellation before the replacement
// can dispatch a press to the same engine.
static HOTKEY_LOOP: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ---------------------------------------------------------------- parsing

/// Physical Linux key positions. Capture, typed shortcuts and conflict checks
/// share this vocabulary; symbols are aliases rather than layout-dependent IDs.
const KEY_TOKENS: &[(&str, KeyCode)] = &[
    ("Escape", KeyCode::KEY_ESC),
    ("Tab", KeyCode::KEY_TAB),
    ("Enter", KeyCode::KEY_ENTER),
    ("Backspace", KeyCode::KEY_BACKSPACE),
    ("Space", KeyCode::KEY_SPACE),
    ("Insert", KeyCode::KEY_INSERT),
    ("Delete", KeyCode::KEY_DELETE),
    ("Home", KeyCode::KEY_HOME),
    ("End", KeyCode::KEY_END),
    ("PageUp", KeyCode::KEY_PAGEUP),
    ("PageDown", KeyCode::KEY_PAGEDOWN),
    ("Up", KeyCode::KEY_UP),
    ("Down", KeyCode::KEY_DOWN),
    ("Left", KeyCode::KEY_LEFT),
    ("Right", KeyCode::KEY_RIGHT),
    ("A", KeyCode::KEY_A),
    ("B", KeyCode::KEY_B),
    ("C", KeyCode::KEY_C),
    ("D", KeyCode::KEY_D),
    ("E", KeyCode::KEY_E),
    ("F", KeyCode::KEY_F),
    ("G", KeyCode::KEY_G),
    ("H", KeyCode::KEY_H),
    ("I", KeyCode::KEY_I),
    ("J", KeyCode::KEY_J),
    ("K", KeyCode::KEY_K),
    ("L", KeyCode::KEY_L),
    ("M", KeyCode::KEY_M),
    ("N", KeyCode::KEY_N),
    ("O", KeyCode::KEY_O),
    ("P", KeyCode::KEY_P),
    ("Q", KeyCode::KEY_Q),
    ("R", KeyCode::KEY_R),
    ("S", KeyCode::KEY_S),
    ("T", KeyCode::KEY_T),
    ("U", KeyCode::KEY_U),
    ("V", KeyCode::KEY_V),
    ("W", KeyCode::KEY_W),
    ("X", KeyCode::KEY_X),
    ("Y", KeyCode::KEY_Y),
    ("Z", KeyCode::KEY_Z),
    ("0", KeyCode::KEY_0),
    ("1", KeyCode::KEY_1),
    ("2", KeyCode::KEY_2),
    ("3", KeyCode::KEY_3),
    ("4", KeyCode::KEY_4),
    ("5", KeyCode::KEY_5),
    ("6", KeyCode::KEY_6),
    ("7", KeyCode::KEY_7),
    ("8", KeyCode::KEY_8),
    ("9", KeyCode::KEY_9),
    ("F1", KeyCode::KEY_F1),
    ("F2", KeyCode::KEY_F2),
    ("F3", KeyCode::KEY_F3),
    ("F4", KeyCode::KEY_F4),
    ("F5", KeyCode::KEY_F5),
    ("F6", KeyCode::KEY_F6),
    ("F7", KeyCode::KEY_F7),
    ("F8", KeyCode::KEY_F8),
    ("F9", KeyCode::KEY_F9),
    ("F10", KeyCode::KEY_F10),
    ("F11", KeyCode::KEY_F11),
    ("F12", KeyCode::KEY_F12),
    ("F13", KeyCode::KEY_F13),
    ("F14", KeyCode::KEY_F14),
    ("F15", KeyCode::KEY_F15),
    ("F16", KeyCode::KEY_F16),
    ("F17", KeyCode::KEY_F17),
    ("F18", KeyCode::KEY_F18),
    ("F19", KeyCode::KEY_F19),
    ("F20", KeyCode::KEY_F20),
    ("F21", KeyCode::KEY_F21),
    ("F22", KeyCode::KEY_F22),
    ("F23", KeyCode::KEY_F23),
    ("F24", KeyCode::KEY_F24),
    ("Minus", KeyCode::KEY_MINUS),
    ("Equal", KeyCode::KEY_EQUAL),
    ("LeftBracket", KeyCode::KEY_LEFTBRACE),
    ("RightBracket", KeyCode::KEY_RIGHTBRACE),
    ("Semicolon", KeyCode::KEY_SEMICOLON),
    ("Apostrophe", KeyCode::KEY_APOSTROPHE),
    ("Grave", KeyCode::KEY_GRAVE),
    ("Backslash", KeyCode::KEY_BACKSLASH),
    ("Comma", KeyCode::KEY_COMMA),
    ("Period", KeyCode::KEY_DOT),
    ("Slash", KeyCode::KEY_SLASH),
    ("IntlBackslash", KeyCode::KEY_102ND),
];

fn key_by_name(name: &str) -> Option<KeyCode> {
    let upper = name.to_ascii_uppercase();
    let token = match upper.as_str() {
        "ESC" => "Escape",
        "RETURN" => "Enter",
        "PAGE_UP" | "PRIOR" => "PageUp",
        "PAGE_DOWN" | "NEXT" => "PageDown",
        "-" => "Minus",
        "=" => "Equal",
        "[" | "BRACKETLEFT" => "LeftBracket",
        "]" | "BRACKETRIGHT" => "RightBracket",
        ";" => "Semicolon",
        "'" => "Apostrophe",
        "`" => "Grave",
        "\\" => "Backslash",
        "," => "Comma",
        "." | "DOT" => "Period",
        "/" => "Slash",
        _ => name,
    };
    KEY_TOKENS
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(token))
        .map(|(_, code)| *code)
}

/// GTK XKB hardware keycode minus eight is an evdev code on both supported
/// Linux backends. Capturing a physical position stays stable across layouts.
pub fn token_for_evdev_code(code: u16) -> Option<&'static str> {
    KEY_TOKENS
        .iter()
        .find(|(_, candidate)| candidate.code() == code)
        .map(|(token, _)| *token)
}

/// Canonical token for a supported typed or symbolic key.
pub(crate) fn canonical_key_token(name: &str) -> Option<&'static str> {
    token_for_evdev_code(key_by_name(name)?.code())
}

pub(crate) fn modifier_slot_for_evdev_code(code: u16) -> Option<usize> {
    match KeyCode(code) {
        KeyCode::KEY_LEFTCTRL | KeyCode::KEY_RIGHTCTRL => Some(0),
        KeyCode::KEY_LEFTALT | KeyCode::KEY_RIGHTALT => Some(1),
        KeyCode::KEY_LEFTSHIFT | KeyCode::KEY_RIGHTSHIFT => Some(2),
        KeyCode::KEY_LEFTMETA | KeyCode::KEY_RIGHTMETA => Some(3),
        _ => None,
    }
}

pub fn parse_combo(raw: &str) -> Option<Combo> {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut super_ = false;
    let mut target = None;
    for token in raw.split('+') {
        let t = token.trim();
        if t.is_empty() {
            return None;
        }
        let u = t.to_ascii_uppercase();
        match u.as_str() {
            "CTRL" | "CONTROL" | "COMMANDORCONTROL" | "CMDORCTRL" => ctrl = true,
            "ALT" | "OPTION" => alt = true,
            "SHIFT" => shift = true,
            "SUPER" | "META" | "WIN" | "CMD" | "COMMAND" => super_ = true,
            _ => {
                if let Some(code) = key_by_name(t) {
                    if target.replace(code).is_some() {
                        return None;
                    }
                } else {
                    eprintln!("[hotkey] WARNING: unknown key {t:?} in {raw:?}");
                    return None;
                }
            }
        }
    }
    if target.is_none() && !(ctrl || alt || shift || super_) {
        return None;
    }
    Some(Combo {
        raw: raw.to_string(),
        ctrl,
        alt,
        shift,
        super_,
        target,
    })
}

// ---------------------------------------------------------------- devices

fn is_keyboard(dev: &Device) -> bool {
    if !dev.supported_events().contains(EventType::KEY) {
        return false;
    }
    match dev.supported_keys() {
        Some(keys) => keys.contains(KeyCode::KEY_A),
        None => false,
    }
}

#[cfg(test)]
type NativeFixture = Option<Vec<(std::path::PathBuf, Device)>>;
#[cfg(test)]
static KEYBOARD_FIXTURE: std::sync::Mutex<NativeFixture> = std::sync::Mutex::new(None);
#[cfg(test)]
static NATIVE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn open_keyboards() -> (Vec<(std::path::PathBuf, Device)>, bool) {
    #[cfg(test)]
    if let Some(devices) = KEYBOARD_FIXTURE.lock().unwrap().as_mut() {
        return (std::mem::take(devices), false);
    }

    let mut devices = Vec::new();
    let mut denied = false;
    let dir = match std::fs::read_dir("/dev/input") {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return (devices, true),
        Err(_) => return (devices, false),
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("event") {
            continue;
        }
        let ft = match entry.file_type() {
            Ok(f) => f,
            Err(_) => continue,
        };
        if !ft.is_char_device() {
            continue;
        }
        match Device::open(entry.path()) {
            Ok(dev) => {
                if is_keyboard(&dev) {
                    // Nonblocking: `fetch_events` blocks by default,
                    // which would wedge the poll loop on the first
                    // quiet device while another holds new events.
                    let _ = dev.set_nonblocking(true);
                    devices.push((entry.path(), dev));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => denied = true,
            Err(_) => {}
        }
    }
    (devices, denied)
}

// ---------------------------------------------------------------- state

/// Merge a rescan into the live set: keep existing fds untouched
/// (their buffered events and held state survive), open only new
/// nodes, drop vanished ones.
fn merge_devices<D>(devices: &mut Vec<(PathBuf, D)>, found: Vec<(PathBuf, D)>) -> Vec<PathBuf> {
    for (path, dev) in found {
        if !devices.iter().any(|(p, _)| p == &path) {
            devices.push((path, dev));
        }
    }
    let mut removed = Vec::new();
    devices.retain(|(path, _)| {
        if path.exists() {
            true
        } else {
            removed.push(path.clone());
            false
        }
    });
    removed
}

fn ctrl_held(held: &HashSet<KeyCode>) -> bool {
    held.contains(&KeyCode::KEY_LEFTCTRL) || held.contains(&KeyCode::KEY_RIGHTCTRL)
}

fn alt_held(held: &HashSet<KeyCode>) -> bool {
    held.contains(&KeyCode::KEY_LEFTALT) || held.contains(&KeyCode::KEY_RIGHTALT)
}

fn shift_held(held: &HashSet<KeyCode>) -> bool {
    held.contains(&KeyCode::KEY_LEFTSHIFT) || held.contains(&KeyCode::KEY_RIGHTSHIFT)
}

fn super_held(held: &HashSet<KeyCode>) -> bool {
    held.contains(&KeyCode::KEY_LEFTMETA) || held.contains(&KeyCode::KEY_RIGHTMETA)
}

fn mods_ok(combo: &Combo, held: &HashSet<KeyCode>) -> bool {
    (!combo.ctrl || ctrl_held(held))
        && (!combo.alt || alt_held(held))
        && (!combo.shift || shift_held(held))
        && (!combo.super_ || super_held(held))
}

fn required_modifier(combo: &Combo, key: KeyCode) -> bool {
    modifier_slot_for_evdev_code(key.code())
        .is_some_and(|slot| [combo.ctrl, combo.alt, combo.shift, combo.super_][slot])
}

/// One watched binding: parsed combo + its action + its own mode +
/// edge state. The press timestamp bounds a Push hold: if a release is
/// ever lost (fd torn down mid-press), the watchdog force-releases
/// instead of wedging dictation on.
struct Watched {
    action: HotkeyAction,
    combo: Combo,
    mode: crate::settings::ActivationMode,
    press: Option<KeyboardPress>,
}

struct KeyboardPress {
    device: PathBuf,
    started: Instant,
}

#[derive(Default)]
struct HeldKeys {
    by_device: HashMap<PathBuf, HashSet<KeyCode>>,
    all: HashSet<KeyCode>,
}

impl HeldKeys {
    fn update(&mut self, path: &Path, key: KeyCode, pressed: bool) -> bool {
        if pressed {
            self.all.insert(key);
            return self
                .by_device
                .entry(path.to_path_buf())
                .or_default()
                .insert(key);
        }
        if !self
            .by_device
            .get_mut(path)
            .is_some_and(|keys| keys.remove(&key))
        {
            return false;
        }
        if !self.by_device.values().any(|keys| keys.contains(&key)) {
            self.all.remove(&key);
        }
        true
    }
}

fn disconnect_keyboard(
    path: &Path,
    held: &mut HeldKeys,
    watched: &mut [Watched],
    on_event: &BindingCallback,
) {
    if held.by_device.remove(path).is_none() {
        return;
    }
    let before = std::mem::take(&mut held.all);
    held.all = held.by_device.values().flatten().copied().collect();
    for (binding, w) in watched.iter_mut().enumerate() {
        if w.press.as_ref().is_some_and(|press| {
            press.device == path || (mods_ok(&w.combo, &before) && !mods_ok(&w.combo, &held.all))
        }) {
            w.press = None;
            on_event(HotkeyEvent::Cancel, Some(binding));
        }
    }
}

fn update_listening(
    listening: bool,
    previous: &mut bool,
    watched: &mut [Watched],
    on_event: &BindingCallback,
) {
    if std::mem::replace(previous, listening) && !listening {
        for binding in watched {
            binding.press = None;
        }
        on_event(HotkeyEvent::Cancel, None);
    }
}

/// Push holds wedged active longer than this are force-released.
/// Tap bindings are exempt (toggled-on is a legitimate long state).
/// 15 minutes: long-form dictation holds for many minutes, and the
/// 60 s we had before amputated exactly those sessions (log-proven).
const MAX_PUSH_SECS: u64 = 900;

trait KeyDevice {
    fn events(&mut self) -> std::io::Result<Vec<evdev::InputEvent>>;
}
impl KeyDevice for Device {
    fn events(&mut self) -> std::io::Result<Vec<evdev::InputEvent>> {
        self.fetch_events().map(|events| events.collect())
    }
}

fn poll_events<D: KeyDevice>(
    devices: &mut Vec<(std::path::PathBuf, D)>,
    held: &mut HeldKeys,
    watched: &mut [Watched],
    on_event: &BindingCallback,
) {
    let mut dead = Vec::new();
    for (i, (path, dev)) in devices.iter_mut().enumerate() {
        let events = match dev.events() {
            Ok(events) => events,
            // Nonblocking fds report WouldBlock when idle: not death.
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(_) => {
                dead.push(i);
                continue;
            }
        };
        for ev in events {
            let EventSummary::Key(_, key, value) = ev.destructure() else {
                continue;
            };
            // value 0 = release, 1 = press, 2 = repeat (ignored).
            match value {
                0 | 1 if held.update(path, key, value == 1) => {}
                _ => continue,
            };
            // Track physical releases during mute so an old held key
            // cannot leak into the next enabled period.
            if !crate::transcribe::hotkeys_listening() {
                continue;
            }
            let pressed = value == 1;
            // An accepted press can be queued before the engine is live.
            if pressed && key == KeyCode::KEY_ESC {
                on_event(HotkeyEvent::Cancel, None);
            }
            for (binding, w) in watched.iter_mut().enumerate() {
                let c = &w.combo;
                let mods_held = mods_ok(c, &held.all);
                let (began, ended) = match c.target {
                    Some(target) if target == key => (
                        pressed && mods_held,
                        !pressed && w.press.as_ref().is_some_and(|p| p.device == *path),
                    ),
                    None if required_modifier(c, key) => (pressed && mods_held, !mods_held),
                    _ => continue,
                };
                if began && w.press.is_none() {
                    w.press = Some(KeyboardPress {
                        device: path.clone(),
                        started: Instant::now(),
                    });
                    on_event(HotkeyEvent::Down(w.action, w.mode), Some(binding));
                } else if ended && w.press.take().is_some() {
                    on_event(HotkeyEvent::Up(w.action, w.mode), Some(binding));
                }
            }
        }
    }
    // Prune dead fds (unplugged mid-poll). Reverse order keeps
    // indexes valid.
    for i in dead.into_iter().rev() {
        eprintln!("[hotkey] device gone, dropping fd");
        let (path, _) = devices.remove(i);
        disconnect_keyboard(&path, held, watched, on_event);
    }
}

/// Blocking listener. Rereads the device dir every few seconds for
/// hotplug (mirrors the C listener's inotify rescan, cheaply).
fn run_loop(
    generation: u64,
    mut watched: Vec<Watched>,
    on_event: BindingCallback,
    on_status: Arc<dyn Fn(HotkeyStatus) + Send + Sync + 'static>,
) {
    let _owner = HOTKEY_LOOP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if HOTKEY_GEN.load(Ordering::SeqCst) != generation {
        return;
    }
    let mut held = HeldKeys::default();
    let mut devices: Vec<(std::path::PathBuf, Device)> = Vec::new();
    let mut reported = false;
    let mut rescan_in = 0u32;
    let mut listening = crate::transcribe::hotkeys_listening();
    loop {
        if HOTKEY_GEN.load(Ordering::SeqCst) != generation {
            on_event(HotkeyEvent::Cancel, None);
            return; // superseded
        }
        update_listening(
            crate::transcribe::hotkeys_listening(),
            &mut listening,
            &mut watched,
            &on_event,
        );
        if rescan_in == 0 {
            let (found, denied) = open_keyboards();
            // Merge by node path: keep existing fds (with their
            // kernel-buffered events and held state), add only new
            // nodes, drop vanished ones. Replacing the whole set
            // here used to lose a release landing mid-rescan and
            // wedge the pill on.
            for path in merge_devices(&mut devices, found) {
                disconnect_keyboard(&path, &mut held, &mut watched, &on_event);
            }
            if devices.is_empty() {
                if denied {
                    if !reported {
                        on_status(HotkeyStatus::NoPermission);
                        reported = true;
                    }
                } else if !reported {
                    on_status(HotkeyStatus::NoDevices);
                    reported = true;
                }
            } else if !reported {
                let names: Vec<String> = devices
                    .iter()
                    .map(|(_, d)| d.name().unwrap_or("?").to_string())
                    .collect();
                eprintln!(
                    "[hotkey] watching {} keyboard(s): {}",
                    devices.len(),
                    names.join(", ")
                );
                on_status(HotkeyStatus::Listening);
                reported = true;
            }
            rescan_in = 100; // ~5s at 50ms polls
        }
        rescan_in -= 1;
        poll_events(&mut devices, &mut held, &mut watched, &on_event);
        // Watchdog: a Push hold wedged by a lost release is
        // force-released instead of sticking dictation on forever.
        for (binding, w) in watched.iter_mut().enumerate() {
            if w.mode == crate::settings::ActivationMode::Push
                && w.press
                    .as_ref()
                    .is_some_and(|p| p.started.elapsed() > Duration::from_secs(MAX_PUSH_SECS))
            {
                eprintln!("[hotkey] WARNING: push stuck >{MAX_PUSH_SECS}s, force release");
                w.press = None;
                on_event(HotkeyEvent::Up(w.action, w.mode), Some(binding));
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------- manager

/// Start (or restart) the global listener. Bumps the generation so
/// any previous loop exits. `bindings` maps actions to raw combo
/// strings; unparsable entries are skipped with a warning.
pub fn start(
    bindings: HashMap<HotkeyAction, Vec<crate::settings::HotkeyBinding>>,
    on_event: HotkeyCallback,
    on_status: Arc<dyn Fn(HotkeyStatus) + Send + Sync + 'static>,
) {
    start_with_bindings(
        bindings,
        Arc::new(move |event, _| {
            if event != HotkeyEvent::Cancel || crate::transcribe::has_live() {
                on_event(event);
            }
        }),
        on_status,
    );
}

fn start_with_bindings(
    bindings: HashMap<HotkeyAction, Vec<crate::settings::HotkeyBinding>>,
    on_event: BindingCallback,
    on_status: Arc<dyn Fn(HotkeyStatus) + Send + Sync + 'static>,
) {
    let generation = HOTKEY_GEN.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
    let mut watched = Vec::new();
    for (action, binds) in bindings {
        for b in binds {
            match parse_combo(&b.combo) {
                Some(combo) => {
                    eprintln!(
                        "[hotkey] watching {:?} ({:?}) for {action:?}",
                        b.combo, b.mode
                    );
                    watched.push(Watched {
                        action,
                        combo,
                        mode: b.mode,
                        press: None,
                    });
                }
                None => eprintln!("[hotkey] WARNING: skipping invalid combo {:?}", b.combo),
            }
        }
    }
    if watched.is_empty() {
        eprintln!("[hotkey] no valid bindings, listener idle");
        return;
    }
    // Dedicated loop: Builder-based so saturation logs instead of
    // panicking the caller (which may be the GTK-adjacent watcher).
    if let Err(e) = crate::runtime::spawn_dedicated("hotkey-loop", move || {
        run_loop(generation, watched, on_event, on_status)
    }) {
        eprintln!("[hotkey] FATAL: {e} (hotkeys dead)");
    }
}

/// Stop all listeners.
pub fn stop() {
    HOTKEY_GEN.fetch_add(1, Ordering::SeqCst);
}

/// Dictation edge hook: must be `Send` (runs on the listener
/// thread). The pill lives on the GTK thread, so service callbacks
/// typically forward through a channel + `idle_add`.
pub type DictationHook = Arc<dyn Fn() + Send + Sync + 'static>;

/// Smart-mode tap ceiling default (ms): a press shorter than this
/// toggles, a longer one holds. Overridable per install via the
/// `smart_tap_ms` setting.
pub const SMART_TAP_MS: u64 = 150;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivationEffect {
    Open,
    Close,
    Cancel,
}

struct ActiveBinding {
    index: usize,
    mode: crate::settings::ActivationMode,
    pressed_at: Instant,
}

#[derive(Default)]
struct Activation {
    active: Option<ActiveBinding>,
}

impl Activation {
    fn handle(
        &mut self,
        event: HotkeyEvent,
        binding: Option<usize>,
        now: Instant,
        tap_ms: u64,
    ) -> Option<ActivationEffect> {
        use crate::settings::ActivationMode;

        match event {
            HotkeyEvent::Down(_, mode) => {
                let index = binding?;
                if self.active.is_some() {
                    if mode == ActivationMode::Push {
                        return None;
                    }
                    self.active = None;
                    return Some(ActivationEffect::Close);
                }
                self.active = Some(ActiveBinding {
                    index,
                    mode,
                    pressed_at: now,
                });
                Some(ActivationEffect::Open)
            }
            HotkeyEvent::Up(_, mode) => {
                let active = self.active.as_mut()?;
                if binding != Some(active.index) || mode != active.mode {
                    return None;
                }
                match mode {
                    ActivationMode::Tap => return None,
                    ActivationMode::Smart
                        if now.duration_since(active.pressed_at).as_millis()
                            < u128::from(tap_ms) =>
                    {
                        // A quick Smart press stays on as a toggle.
                        active.mode = ActivationMode::Tap;
                        return None;
                    }
                    _ => {}
                }
                self.active = None;
                Some(ActivationEffect::Close)
            }
            HotkeyEvent::Cancel => {
                let active = self.active.as_ref()?;
                if binding.is_some_and(|index| index != active.index) {
                    return None;
                }
                self.active = None;
                Some(ActivationEffect::Cancel)
            }
        }
    }
}

/// (Re)start the listener from live settings with explicit dictation
/// hooks. Each binding behaves per its own mode; the STT pipeline (or
/// the pill) hooks into `down` / `up`. Call on startup and after
/// every hotkey change.
pub fn start_dictation(
    bindings: Vec<crate::settings::HotkeyBinding>,
    tap_ms: u64,
    down: DictationHook,
    up: DictationHook,
    cancel: DictationHook,
) {
    let mut map = HashMap::new();
    map.insert(HotkeyAction::DictationToggle, bindings);
    let activation = std::sync::Mutex::new(Activation::default());
    let on_event: BindingCallback = Arc::new(move |event, binding| {
        let effect = activation
            .lock()
            .unwrap()
            .handle(event, binding, Instant::now(), tap_ms);
        match effect {
            Some(ActivationEffect::Open) => down(),
            Some(ActivationEffect::Close) => up(),
            Some(ActivationEffect::Cancel) => cancel(),
            None => {}
        }
    });
    let on_status: Arc<dyn Fn(HotkeyStatus) + Send + Sync + 'static> =
        Arc::new(|st| eprintln!("[hotkey] status: {st:?}"));
    start_with_bindings(map, on_event, on_status);
}

/// Log-only hooks for standalone GUI mode (no pill in this process).
fn log_hooks() -> (DictationHook, DictationHook, DictationHook) {
    (
        Arc::new(|| eprintln!("[hotkey] dictation START (pipeline hook)")),
        Arc::new(|| eprintln!("[hotkey] dictation STOP (pipeline hook)")),
        Arc::new(|| eprintln!("[hotkey] dictation CANCEL (pipeline hook)")),
    )
}

/// (Re)start the listener from live settings with log-only hooks.
/// Standalone GUI mode uses this; the service passes pill hooks to
/// `start_dictation` instead.
pub fn apply_from_settings(state: &crate::app::AppState) {
    let hotkeys = state.settings.borrow().hotkeys.clone();
    let (down, up, cancel) = log_hooks();
    start_dictation(
        hotkeys.dictation_hotkeys,
        hotkeys.smart_tap_ms,
        down,
        up,
        cancel,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_f13_and_combos() {
        let c = parse_combo("F13").unwrap();
        assert_eq!(c.target, Some(KeyCode::KEY_F13));
        assert!(!c.ctrl && !c.alt && !c.shift && !c.super_);
        let c = parse_combo("Ctrl+Shift+Space").unwrap();
        assert!(c.ctrl && c.shift && !c.alt);
        assert_eq!(c.target, Some(KeyCode::KEY_SPACE));
        assert!(parse_combo("").is_none());
        assert!(parse_combo("Nope+NotAKey").is_none());
        assert!(parse_combo("Ctrl+A+B").is_none());
        assert!(parse_combo("Ctrl++").is_none());
    }

    #[test]
    fn smart_tap_holds_toggle() {
        use crate::settings::ActivationMode::Smart;
        use ActivationEffect::{Close, Open};

        let mut activation = Activation::default();
        let action = HotkeyAction::DictationToggle;
        let down = HotkeyEvent::Down(action, Smart);
        let up = HotkeyEvent::Up(action, Smart);
        let now = Instant::now();
        let ceiling = Duration::from_millis(SMART_TAP_MS);

        assert_eq!(
            activation.handle(down, Some(0), now, SMART_TAP_MS),
            Some(Open)
        );
        assert_eq!(
            activation.handle(
                up,
                Some(0),
                now + ceiling - Duration::from_millis(1),
                SMART_TAP_MS
            ),
            None
        );
        assert_eq!(
            activation.handle(down, Some(0), now + ceiling, SMART_TAP_MS),
            Some(Close)
        );
        assert_eq!(
            activation.handle(up, Some(0), now + ceiling * 2, SMART_TAP_MS),
            None
        );

        assert_eq!(activation.handle(down, Some(0), now, 100), Some(Open));
        assert_eq!(
            activation.handle(up, Some(0), now + Duration::from_millis(100), 100),
            Some(Close)
        );
    }

    #[test]
    fn releases_only_close_the_session_their_binding_opened() {
        use crate::settings::ActivationMode::{Push, Smart, Tap};
        use ActivationEffect::{Cancel, Close, Open};

        let action = HotkeyAction::DictationToggle;
        let now = Instant::now();
        let later = now + Duration::from_secs(1);
        let down = HotkeyEvent::Down;
        let up = HotkeyEvent::Up;
        for mode in [Push, Smart, Tap] {
            let mut activation = Activation::default();
            assert_eq!(activation.handle(HotkeyEvent::Cancel, None, now, 100), None);
            assert_eq!(
                activation.handle(down(action, mode), Some(0), now, 100),
                Some(Open)
            );
            assert_eq!(
                activation.handle(down(action, Push), Some(1), now, 100),
                None
            );
            assert_eq!(
                activation.handle(up(action, Push), Some(1), later, 100),
                None
            );
            assert_eq!(
                activation.handle(HotkeyEvent::Cancel, Some(1), later, 100),
                None
            );
            assert_eq!(
                activation.handle(HotkeyEvent::Cancel, None, later, 100),
                Some(Cancel)
            );
            assert_eq!(
                activation.handle(down(action, Push), Some(2), later, 100),
                Some(Open)
            );
            assert_eq!(
                activation.handle(up(action, mode), Some(0), later, 100),
                None
            );
            assert_eq!(
                activation.handle(up(action, Push), Some(2), later, 100),
                Some(Close)
            );
        }
    }

    #[test]
    fn mods_gate_correctly() {
        let c = parse_combo("Ctrl+D").unwrap();
        let mut held = HashSet::new();
        held.insert(KeyCode::KEY_LEFTCTRL);
        assert!(mods_ok(&c, &held));
        held.clear();
        assert!(!mods_ok(&c, &held));
    }

    #[test]
    fn mute_retires_a_held_shortcut_once() {
        use crate::settings::ActivationMode::Push;
        use ActivationEffect::{Cancel, Open};

        let action = HotkeyAction::DictationToggle;
        let now = Instant::now();
        let mut watched = [Watched {
            action,
            combo: parse_combo("F13").unwrap(),
            mode: Push,
            press: Some(KeyboardPress {
                device: PathBuf::from("test-keyboard"),
                started: now,
            }),
        }];
        let state = Arc::new(std::sync::Mutex::new((Activation::default(), Vec::new())));
        let captured = state.clone();
        let callback: BindingCallback = Arc::new(move |event, binding| {
            let (activation, effects) = &mut *captured.lock().unwrap();
            effects.extend(activation.handle(event, binding, now, SMART_TAP_MS));
        });
        callback(HotkeyEvent::Down(action, Push), Some(0));
        let mut listening = true;
        update_listening(false, &mut listening, &mut watched, &callback);
        update_listening(false, &mut listening, &mut watched, &callback);
        assert!(watched[0].press.is_none());
        update_listening(true, &mut listening, &mut watched, &callback);
        callback(HotkeyEvent::Up(action, Push), Some(0));
        callback(HotkeyEvent::Down(action, Push), Some(0));
        assert_eq!(state.lock().unwrap().1, [Open, Cancel, Open]);
    }

    /// Descriptor ownership uses deterministic files in every build. The required
    /// native companion additionally uses an exclusively grabbed owned keyboard.
    #[test]
    fn rescan_merge_keeps_fds() {
        use std::os::fd::AsRawFd;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("keyboard");
        std::fs::write(&path, b"buffered event").unwrap();
        let original = std::fs::File::open(&path).unwrap();
        let fd = original.as_raw_fd();
        let mut live = vec![(path.clone(), original)];
        for _ in 0..3 {
            merge_devices(
                &mut live,
                vec![(path.clone(), std::fs::File::open(&path).unwrap())],
            );
            let paths: HashSet<_> = live.iter().map(|(p, _)| p).collect();
            assert_eq!(paths.len(), live.len(), "merge must never duplicate an fd");
            assert_eq!(
                live[0].1.as_raw_fd(),
                fd,
                "rescan must retain the original descriptor"
            );
        }
        assert!(!live.is_empty(), "synthetic keyboard is present");
        use std::io::Read;
        let mut buffered = String::new();
        live[0].1.read_to_string(&mut buffered).unwrap();
        assert_eq!(buffered, "buffered event", "rescan lost buffered events");
        std::fs::remove_file(&path).unwrap();
        merge_devices(&mut live, Vec::new());
        assert!(live.is_empty(), "unplug must drop the original descriptor");
        if std::env::var_os("MYWHISPER_NATIVE_HOTKEY_GATE").is_some() {
            let _native_owner = NATIVE_TEST_LOCK.lock().unwrap();
            let (virtual_keyboard, path, owned) = native_keyboard();
            let fd = owned.as_raw_fd();
            let mut live = vec![(path.clone(), owned)];
            for _ in 0..3 {
                let (found, _) = open_keyboards();
                merge_devices(&mut live, found);
                let paths: HashSet<_> = live.iter().map(|(p, _)| p).collect();
                assert_eq!(paths.len(), live.len(), "merge must never duplicate an fd");
                assert_eq!(
                    live.iter().find(|(p, _)| p == &path).unwrap().1.as_raw_fd(),
                    fd
                );
            }
            assert!(!live.is_empty(), "native keyboard must remain present");
            drop(live);
            drop(virtual_keyboard);
        }
    }

    #[test]
    fn evdev_codes_roundtrip_through_parser() {
        // The user's F13 key: evdev 183 must survive to "F13".
        assert_eq!(token_for_evdev_code(183), Some("F13"));
        assert_eq!(token_for_evdev_code(30), Some("A"));
        assert_eq!(token_for_evdev_code(57), Some("Space"));
        assert_eq!(token_for_evdev_code(1), Some("Escape"));
        assert_eq!(token_for_evdev_code(9999), None);
        assert_eq!(token_for_evdev_code(KeyCode::KEY_RIGHTSHIFT.code()), None);
        for &(token, code) in KEY_TOKENS {
            assert_eq!(token_for_evdev_code(code.code()), Some(token));
            assert_eq!(parse_combo(token).unwrap().target, Some(code), "{token}");
        }
        assert_eq!(
            parse_combo("Ctrl+-").unwrap().target,
            Some(KeyCode::KEY_MINUS)
        );
        // Modifier codes stay out (handled as modifiers, not keys).
        assert_eq!(token_for_evdev_code(29), None);
        for code in [183u16, 30, 57, 1, 28, 48, 16] {
            let token = token_for_evdev_code(code).unwrap();
            assert!(
                parse_combo(token).is_some(),
                "roundtrip failed for code {code}"
            );
        }
    }

    /// Production event dispatch with deterministic frames in every build and
    /// real grabbed uinput in the required native companion; all edge assertions
    /// run in both modes, with no host listener receiving synthetic events.
    struct SyntheticKeyboard(std::collections::VecDeque<Vec<evdev::InputEvent>>);
    impl KeyDevice for SyntheticKeyboard {
        fn events(&mut self) -> std::io::Result<Vec<evdev::InputEvent>> {
            self.0
                .pop_front()
                .ok_or_else(|| std::io::ErrorKind::WouldBlock.into())
        }
    }

    #[test]
    fn held_modifier_chord_only_rearms_on_a_required_press() {
        use crate::settings::ActivationMode::Push;

        let path = PathBuf::from("keyboard");
        let ctrl = KeyCode::KEY_LEFTCTRL;
        let super_key = KeyCode::KEY_LEFTMETA;
        let key = |code: KeyCode, value| evdev::InputEvent::new(EventType::KEY.0, code.0, value);
        let mut held = HeldKeys::default();
        held.update(&path, ctrl, true);
        held.update(&path, super_key, true);
        let mut devices = vec![(
            path,
            SyntheticKeyboard(
                [
                    vec![key(KeyCode::KEY_LEFTALT, 1)],
                    vec![key(ctrl, 0), key(ctrl, 1)],
                ]
                .into(),
            ),
        )];
        let mut watched = [Watched {
            action: HotkeyAction::DictationToggle,
            combo: parse_combo("Ctrl+Super").unwrap(),
            mode: Push,
            press: None,
        }];
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let callback: BindingCallback = Arc::new(move |event, _| sink.lock().unwrap().push(event));
        poll_events(&mut devices, &mut held, &mut watched, &callback);
        assert!(seen.lock().unwrap().is_empty());
        poll_events(&mut devices, &mut held, &mut watched, &callback);
        assert_eq!(
            *seen.lock().unwrap(),
            [HotkeyEvent::Down(HotkeyAction::DictationToggle, Push)]
        );
    }

    #[test]
    fn foreign_release_and_disconnect_preserve_keyboard_ownership() {
        use crate::settings::ActivationMode::Push;

        let key = |code: KeyCode, value| evdev::InputEvent::new(EventType::KEY.0, code.0, value);
        let ctrl = KeyCode::KEY_LEFTCTRL;
        let trigger = KeyCode::KEY_F13;
        let primary = PathBuf::from("primary");
        let secondary = PathBuf::from("secondary");
        let mut devices = vec![
            (
                primary.clone(),
                SyntheticKeyboard([vec![key(ctrl, 1), key(trigger, 1)]].into()),
            ),
            (
                secondary,
                SyntheticKeyboard(
                    [
                        vec![key(ctrl, 1), key(trigger, 1)],
                        vec![key(ctrl, 0), key(trigger, 0)],
                        vec![key(trigger, 1), key(trigger, 0)],
                    ]
                    .into(),
                ),
            ),
        ];
        let mut held = HeldKeys::default();
        let mut watched = [Watched {
            action: HotkeyAction::DictationToggle,
            combo: parse_combo("Ctrl+F13").unwrap(),
            mode: Push,
            press: None,
        }];
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let on_event: BindingCallback =
            Arc::new(move |event, index| sink.lock().unwrap().push((event, index)));

        poll_events(&mut devices, &mut held, &mut watched, &on_event);
        poll_events(&mut devices, &mut held, &mut watched, &on_event);
        assert!(held.all.contains(&ctrl));
        assert!(held.all.contains(&trigger));
        assert_eq!(
            *seen.lock().unwrap(),
            [(
                HotkeyEvent::Down(HotkeyAction::DictationToggle, Push),
                Some(0)
            )]
        );

        devices.remove(0);
        disconnect_keyboard(&primary, &mut held, &mut watched, &on_event);
        assert!(held.all.is_empty());
        assert!(watched[0].press.is_none());
        poll_events(&mut devices, &mut held, &mut watched, &on_event);
        assert_eq!(
            *seen.lock().unwrap(),
            [
                (
                    HotkeyEvent::Down(HotkeyAction::DictationToggle, Push),
                    Some(0)
                ),
                (HotkeyEvent::Cancel, Some(0))
            ]
        );

        let modifier = PathBuf::from("modifier");
        devices[0].1.0.push_back(vec![key(trigger, 1)]);
        devices.insert(
            0,
            (
                modifier.clone(),
                SyntheticKeyboard([vec![key(ctrl, 1)]].into()),
            ),
        );
        poll_events(&mut devices, &mut held, &mut watched, &on_event);
        devices.remove(0);
        disconnect_keyboard(&modifier, &mut held, &mut watched, &on_event);
        assert_eq!(
            &seen.lock().unwrap()[2..],
            [
                (
                    HotkeyEvent::Down(HotkeyAction::DictationToggle, Push),
                    Some(0)
                ),
                (HotkeyEvent::Cancel, Some(0))
            ]
        );
    }

    fn native_keyboard() -> (evdev::uinput::VirtualDevice, std::path::PathBuf, Device) {
        use evdev::uinput::VirtualDevice;
        let mut keys = evdev::AttributeSet::<KeyCode>::new();
        keys.insert(KeyCode::KEY_A);
        keys.insert(KeyCode::KEY_F13);
        let mut virtual_keyboard = VirtualDevice::builder()
            .unwrap()
            .name("mywhisper-disposable-hotkey-gate")
            .with_keys(&keys)
            .unwrap()
            .build()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            assert!(
                Instant::now() < deadline,
                "owned virtual keyboard did not appear"
            );
            let paths: Vec<_> = virtual_keyboard
                .enumerate_dev_nodes_blocking()
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            for path in paths {
                if let Ok(mut device) = Device::open(&path) {
                    // Exclusive ownership precedes every emitted event. No
                    // host listener can receive these synthetic shortcuts.
                    device.grab().unwrap();
                    device.set_nonblocking(true).unwrap();
                    return (virtual_keyboard, path, device);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn assert_edges(seen: &[HotkeyEvent]) {
        let down = HotkeyEvent::Down(
            HotkeyAction::DictationToggle,
            crate::settings::ActivationMode::Push,
        );
        let up = HotkeyEvent::Up(
            HotkeyAction::DictationToggle,
            crate::settings::ActivationMode::Push,
        );
        assert!(seen.contains(&down), "no Down, got {seen:?}");
        assert!(seen.contains(&up), "no Up, got {seen:?}");
        assert_eq!(seen, [down, up], "edges must occur once and in order");
    }
    #[test]
    fn virtual_keyboard_fires_down_up() {
        use std::sync::Mutex;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let callback: HotkeyCallback = Arc::new(move |event| sink.lock().unwrap().push(event));
        let mut watched = [Watched {
            action: HotkeyAction::DictationToggle,
            combo: parse_combo("F13").unwrap(),
            mode: crate::settings::ActivationMode::Push,
            press: None,
        }];
        let mut held = HeldKeys::default();
        let syn = evdev::InputEvent::new(0, 0, 0);
        let press = evdev::InputEvent::new(EventType::KEY.0, KeyCode::KEY_F13.0, 1);
        let repeat = evdev::InputEvent::new(EventType::KEY.0, KeyCode::KEY_F13.0, 2);
        let release = evdev::InputEvent::new(EventType::KEY.0, KeyCode::KEY_F13.0, 0);
        let mut devices = vec![(
            "synthetic-keyboard".into(),
            SyntheticKeyboard(std::collections::VecDeque::from([
                vec![press, syn],
                vec![repeat, syn],
                vec![release, syn],
            ])),
        )];
        let record_edge = callback.clone();
        let binding_callback: BindingCallback = Arc::new(move |event, _| record_edge(event));
        for _ in 0..4 {
            poll_events(&mut devices, &mut held, &mut watched, &binding_callback);
        }
        assert_edges(&seen.lock().unwrap());
        if std::env::var_os("MYWHISPER_NATIVE_HOTKEY_GATE").is_some() {
            let _native_owner = NATIVE_TEST_LOCK.lock().unwrap();
            struct FixtureGuard;
            impl Drop for FixtureGuard {
                fn drop(&mut self) {
                    stop();
                    *KEYBOARD_FIXTURE.lock().unwrap() = None;
                }
            }
            let _fixture = FixtureGuard;
            seen.lock().unwrap().clear();
            let (mut keyboard, path, device) = native_keyboard();
            use std::os::fd::AsRawFd;
            let fd = device.as_raw_fd();
            *KEYBOARD_FIXTURE.lock().unwrap() = Some(vec![(path.clone(), device)]);
            // Exercise original startup/rescan/dispatch through the listener
            // thread. Its first scan receives the already-grabbed owned fd;
            // it never scans or emits into a host keyboard.
            let mut bindings = HashMap::new();
            bindings.insert(
                HotkeyAction::DictationToggle,
                vec![crate::settings::HotkeyBinding {
                    combo: "F13".into(),
                    mode: crate::settings::ActivationMode::Push,
                }],
            );
            start(bindings, callback.clone(), Arc::new(|_| {}));
            for event in [press, release] {
                keyboard.emit(&[event, syn]).unwrap();
                let deadline = Instant::now() + Duration::from_secs(2);
                let expected = if event.value() == 1 { 1 } else { 2 };
                while seen.lock().unwrap().len() < expected {
                    assert!(
                        Instant::now() < deadline,
                        "native listener did not deliver its edge"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            stop();
            let deadline = Instant::now() + Duration::from_secs(2);
            while std::fs::read_link(format!("/proc/self/fd/{fd}"))
                .is_ok_and(|target| target == path)
            {
                assert!(
                    Instant::now() < deadline,
                    "native listener retained its owned descriptor"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_edges(&seen.lock().unwrap());
            drop(keyboard);
        }
    }
}
