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
// Binding identities are scoped to an owned listener. Its replacement joins
// the old listener before any callback from the new one can run.
type OwnedCallback = Arc<dyn Fn(HotkeyEvent, usize) + Send + Sync + 'static>;

/// Generation counter: bumped on every (re)start so a stale loop
/// exits instead of double-firing after settings change.
static HOTKEY_GEN: AtomicU64 = AtomicU64::new(0);

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
pub fn parse_combo(raw: &str) -> Option<Combo> {
    if raw.len() > 256 {
        return None;
    }
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut super_ = false;
    let mut target = None;
    let mut any = false;
    for token in raw.split('+') {
        let t = token.trim();
        if t.is_empty() {
            return None;
        }
        any = true;
        let u = t.to_ascii_uppercase();
        match u.as_str() {
            "CTRL" | "CONTROL" | "COMMANDORCONTROL" | "CMDORCTRL" => ctrl = true,
            "ALT" | "OPTION" => alt = true,
            "SHIFT" => shift = true,
            "SUPER" | "META" | "WIN" | "CMD" | "COMMAND" => super_ = true,
            _ => {
                let code = key_by_name(t)?;
                if target.is_some() {
                    return None;
                }
                target = Some(code);
            }
        }
    }
    if !any {
        return None;
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

fn open_keyboards() -> (Vec<(std::path::PathBuf, Device)>, bool) {
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
fn merge_devices<D>(
    devices: &mut Vec<(std::path::PathBuf, D)>,
    found: Vec<(std::path::PathBuf, D)>,
) {
    for (path, dev) in found {
        if !devices.iter().any(|(p, _)| p == &path) {
            devices.push((path, dev));
        }
    }
    devices.retain(|(p, _)| p.exists());
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
    combo.ctrl == ctrl_held(held)
        && combo.alt == alt_held(held)
        && combo.shift == shift_held(held)
        && combo.super_ == super_held(held)
}

fn is_modifier(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::KEY_LEFTCTRL
            | KeyCode::KEY_RIGHTCTRL
            | KeyCode::KEY_LEFTALT
            | KeyCode::KEY_RIGHTALT
            | KeyCode::KEY_LEFTSHIFT
            | KeyCode::KEY_RIGHTSHIFT
            | KeyCode::KEY_LEFTMETA
            | KeyCode::KEY_RIGHTMETA
    )
}

/// One watched binding: parsed combo + its action + its own mode +
/// edge state. `active_since` bounds a Push hold: if a release is
/// ever lost (fd torn down mid-press), the watchdog force-releases
/// instead of wedging dictation on.
struct Watched {
    binding_id: usize,
    press_device: Option<std::path::PathBuf>,
    action: HotkeyAction,
    combo: Combo,
    mode: crate::settings::ActivationMode,
    active: bool,
    active_since: Option<Instant>,
}

/// Push holds wedged active longer than this are force-released.
/// Tap bindings are exempt (toggled-on is a legitimate long state).
/// 15 minutes: long-form dictation holds for many minutes, and the
/// 60 s we had before amputated exactly those sessions (log-proven).
const MAX_PUSH_SECS: u64 = 900;

#[derive(Default)]
struct HeldKeys {
    devices: HashMap<std::path::PathBuf, HashSet<KeyCode>>,
    aggregate: HashSet<KeyCode>,
}
impl HeldKeys {
    fn update(&mut self, path: &std::path::Path, key: KeyCode, down: bool) {
        let held = self.devices.entry(path.to_path_buf()).or_default();
        if down {
            held.insert(key);
        } else {
            held.remove(&key);
        }
        self.rebuild();
    }
    fn retain(&mut self, paths: &HashSet<std::path::PathBuf>) {
        self.devices.retain(|path, _| paths.contains(path));
        self.rebuild();
    }
    fn rebuild(&mut self) {
        self.aggregate = self
            .devices
            .values()
            .flat_map(|keys| keys.iter().copied())
            .collect();
    }
}

static CAPTURE_PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
type SuspendHook = Arc<dyn Fn([u8; 16]) + Send + Sync>;
static CAPTURE_SUSPEND: std::sync::OnceLock<SuspendHook> = std::sync::OnceLock::new();
pub fn configure_capture_suspend(data_dir: &std::path::Path) {
    let _ = CAPTURE_PATH.set(data_dir.join("hotkey-capture.lock"));
}
pub fn register_capture_suspend(hook: impl Fn([u8; 16]) + Send + Sync + 'static) {
    let _ = CAPTURE_SUSPEND.set(Arc::new(hook));
}

fn capture_request() -> Option<[u8; 16]> {
    let path = CAPTURE_PATH.get()?;
    crate::private_fs::read_bounded(&path.with_extension("request"), 16)
        .ok()?
        .try_into()
        .ok()
}

fn fresh_capture_request(
    previous: &mut Option<[u8; 16]>,
    current: Option<[u8; 16]>,
) -> Option<[u8; 16]> {
    let request = current?;
    if *previous == Some(request) {
        return None;
    }
    *previous = Some(request);
    Some(request)
}
pub fn capture_suspend_pending(nonce: [u8; 16]) -> bool {
    !capture_available() && capture_request() == Some(nonce)
}

/// Called by the engine thread only after retiring queued/active capture and
/// closing its overlay microphone. A stale request can never acknowledge a new one.
pub fn acknowledge_capture_suspend(nonce: [u8; 16]) -> std::io::Result<()> {
    if !capture_suspend_pending(nonce) {
        return Ok(());
    }
    let path = CAPTURE_PATH
        .get()
        .ok_or_else(|| std::io::Error::other("capture not configured"))?;
    crate::private_fs::replace(&path.with_extension("ack"), &nonce)
}
fn capture_available() -> bool {
    use std::os::unix::fs::OpenOptionsExt;
    let Some(path) = CAPTURE_PATH.get() else {
        return true;
    };
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    else {
        return false;
    };
    fs2::FileExt::try_lock_shared(&file).is_ok()
}

struct KeyInput<'a> {
    path: &'a std::path::Path,
    key: KeyCode,
    value: i32,
    now: Instant,
}

fn dispatch_key(
    input: KeyInput<'_>,
    held: &mut HeldKeys,
    watched: &mut [Watched],
    on_event: &OwnedCallback,
    enabled: bool,
    live: bool,
) {
    // Repeated press frames cannot rearm a key that stayed held through mute.
    if input.value == 1
        && held
            .devices
            .get(input.path)
            .is_some_and(|keys| keys.contains(&input.key))
    {
        return;
    }
    // value 0 = release, 1 = press, 2 = repeat (ignored).
    // Banner mute: when hotkeys are disabled, no event
    // reaches the pipeline (presses are swallowed here).
    match input.value {
        1 => {
            held.update(input.path, input.key, true);
        }
        0 => {
            held.update(input.path, input.key, false);
        }
        _ => return,
    };
    let pressed = input.value == 1;
    if !enabled {
        if watched.iter().any(|w| w.active) || live {
            on_event(HotkeyEvent::Cancel, usize::MAX);
        }
        for w in watched.iter_mut() {
            w.active = false;
            w.active_since = None;
        }
        return;
    }
    // Escape cancels a live utterance (midway cancel). Gated
    // on live state: idle Escape presses never emit.
    if pressed && input.key == KeyCode::KEY_ESC && (live || watched.iter().any(|w| w.active)) {
        eprintln!("[hotkey] escape cancels live utterance");
        on_event(HotkeyEvent::Cancel, usize::MAX);
        for w in watched.iter_mut() {
            w.active = false;
            w.active_since = None;
            w.press_device = None;
        }
        return;
    }
    for w in watched.iter_mut() {
        let c = &w.combo;
        if let Some(target) = c.target {
            if input.key != target {
                continue;
            }
            if pressed && !w.active && mods_ok(c, &held.aggregate) {
                w.press_device = Some(input.path.to_path_buf());
                w.active = true;
                w.active_since = Some(input.now);
                on_event(HotkeyEvent::Down(w.action, w.mode), w.binding_id);
            } else if !pressed && w.active && !held.aggregate.contains(&target) {
                w.active = false;
                w.active_since = None;
                on_event(HotkeyEvent::Up(w.action, w.mode), w.binding_id);
            }
        } else {
            // Modifier-only combo.
            if !is_modifier(input.key) {
                continue;
            }
            if mods_ok(c, &held.aggregate) && !w.active {
                w.press_device = Some(input.path.to_path_buf());
                w.active = true;
                w.active_since = Some(input.now);
                on_event(HotkeyEvent::Down(w.action, w.mode), w.binding_id);
            } else if !mods_ok(c, &held.aggregate) && w.active {
                w.active = false;
                w.active_since = None;
                on_event(HotkeyEvent::Up(w.action, w.mode), w.binding_id);
            }
        }
    }
}

fn disconnect_keyboard(
    path: &std::path::Path,
    held: &mut HeldKeys,
    watched: &mut [Watched],
    on_event: &OwnedCallback,
) {
    held.devices.remove(path);
    held.rebuild();
    for watched in watched {
        let combo = &watched.combo;
        let lost_required_key = watched.active
            && (combo
                .target
                .is_some_and(|key| !held.aggregate.contains(&key))
                || (combo.ctrl && !ctrl_held(&held.aggregate))
                || (combo.alt && !alt_held(&held.aggregate))
                || (combo.shift && !shift_held(&held.aggregate))
                || (combo.super_ && !super_held(&held.aggregate)));
        if watched.press_device.as_deref() == Some(path) || lost_required_key {
            // Only this binding's session can be cancelled by its unplug.
            on_event(HotkeyEvent::Cancel, watched.binding_id);
            watched.press_device = None;
            watched.active = false;
            watched.active_since = None;
        }
    }
}

trait KeyDevice {
    fn events(&mut self) -> std::io::Result<Vec<evdev::InputEvent>>;
}
impl KeyDevice for Device {
    fn events(&mut self) -> std::io::Result<Vec<evdev::InputEvent>> {
        self.fetch_events().map(Iterator::collect)
    }
}

fn poll_events<D: KeyDevice>(
    devices: &mut Vec<(std::path::PathBuf, D)>,
    held: &mut HeldKeys,
    watched: &mut [Watched],
    on_event: &OwnedCallback,
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
            dispatch_key(
                KeyInput {
                    path,
                    key,
                    value,
                    now: Instant::now(),
                },
                held,
                watched,
                on_event,
                crate::transcribe::hotkeys_listening() && capture_available(),
                crate::transcribe::has_live() || activation_active(),
            );
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
    on_event: OwnedCallback,
    on_status: Arc<dyn Fn(HotkeyStatus) + Send + Sync + 'static>,
) {
    let mut held = HeldKeys::default();
    let mut devices: Vec<(std::path::PathBuf, Device)> = Vec::new();
    let mut reported = false;
    let mut rescan_in = 0u32;
    let mut previous_enabled = true;
    let mut suspended_request = None;
    loop {
        if HOTKEY_GEN.load(Ordering::SeqCst) != generation {
            on_event(HotkeyEvent::Cancel, usize::MAX);
            return; // superseded
        }
        if rescan_in == 0 {
            let (found, denied) = if watched.is_empty() {
                (Vec::new(), false)
            } else {
                open_keyboards()
            };
            // Merge by node path: keep existing fds (with their
            // kernel-buffered events and held state), add only new
            // nodes, drop vanished ones. Replacing the whole set
            // here used to lose a release landing mid-rescan and
            // wedge the pill on.
            merge_devices(&mut devices, found);
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
        // Seed held state from the kernel so a boot-time-held key
        // cannot wedge an edge.
        let paths: HashSet<_> = devices.iter().map(|(path, _)| path.clone()).collect();
        let lost: Vec<_> = held
            .devices
            .keys()
            .filter(|path| !paths.contains(*path))
            .cloned()
            .collect();
        for path in lost {
            disconnect_keyboard(&path, &mut held, &mut watched, &on_event);
        }
        held.retain(&paths);
        let available = capture_available();
        let enabled = crate::transcribe::hotkeys_listening() && available;
        if !enabled {
            if previous_enabled || watched.iter().any(|w| w.active) || crate::transcribe::has_live()
            {
                on_event(HotkeyEvent::Cancel, usize::MAX);
            }
            for w in &mut watched {
                w.active = false;
                w.active_since = None;
            }
        }
        previous_enabled = enabled;
        if !available
            && let Some(request) = fresh_capture_request(&mut suspended_request, capture_request())
            && let Some(hook) = CAPTURE_SUSPEND.get()
        {
            hook(request);
        }
        // Keep the last nonce across lock release. A new frontend takes the
        // lock before publishing its new request; the old file in that gap
        // must not replay a completed suspension.
        poll_events(&mut devices, &mut held, &mut watched, &on_event);
        // Watchdog: a Push hold wedged by a lost release is
        // force-released instead of sticking dictation on forever.
        for w in watched.iter_mut() {
            if w.active
                && w.mode == crate::settings::ActivationMode::Push
                && w.active_since
                    .map(|t| t.elapsed() > Duration::from_secs(MAX_PUSH_SECS))
                    .unwrap_or(false)
            {
                eprintln!("[hotkey] WARNING: push stuck >{MAX_PUSH_SECS}s, force release");
                w.active = false;
                w.active_since = None;
                on_event(HotkeyEvent::Up(w.action, w.mode), w.binding_id);
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
    start_owned(
        bindings,
        Arc::new(move |event, _| on_event(event)),
        on_status,
    );
}

fn start_owned(
    bindings: HashMap<HotkeyAction, Vec<crate::settings::HotkeyBinding>>,
    on_event: OwnedCallback,
    on_status: Arc<dyn Fn(HotkeyStatus) + Send + Sync + 'static>,
) {
    let mut owner = LISTENER.lock().unwrap_or_else(|p| p.into_inner());
    HOTKEY_GEN.fetch_add(1, Ordering::SeqCst);
    if let Some(handle) = owner.take() {
        let _ = handle.join();
    }
    let generation = HOTKEY_GEN.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
    HOTKEY_GEN.store(generation, Ordering::SeqCst);
    let mut watched = Vec::new();
    for (action, binds) in bindings {
        for b in binds {
            match parse_combo(&b.combo) {
                Some(combo) => {
                    eprintln!(
                        "[hotkey] watching physical binding ({:?}) for {action:?}",
                        b.mode
                    );
                    watched.push(Watched {
                        binding_id: watched.len(),
                        press_device: None,
                        action,
                        combo,
                        mode: b.mode,
                        active: false,
                        active_since: None,
                    });
                }
                None => eprintln!("[hotkey] WARNING: skipping invalid configured shortcut"),
            }
        }
    }
    if watched.is_empty() {
        eprintln!("[hotkey] no valid bindings, listener idle");
    }
    // Dedicated loop: Builder-based so saturation logs instead of
    // panicking the caller (which may be the GTK-adjacent watcher).
    match std::thread::Builder::new()
        .name("hotkey-loop".into())
        .spawn(move || run_loop(generation, watched, on_event, on_status))
    {
        Ok(handle) => *owner = Some(handle),
        Err(e) => eprintln!("[hotkey] cannot start listener: {e}"),
    }
}

static LISTENER: std::sync::Mutex<Option<std::thread::JoinHandle<()>>> =
    std::sync::Mutex::new(None);

/// Stop the owned listener and wait until its last callback has finished.
pub fn stop() {
    let mut owner = LISTENER.lock().unwrap_or_else(|p| p.into_inner());
    HOTKEY_GEN.fetch_add(1, Ordering::SeqCst);
    if let Some(handle) = owner.take() {
        let _ = handle.join();
    }
}

/// Dictation edge hook: must be `Send` (runs on the listener
/// thread). The pill lives on the GTK thread, so service callbacks
/// typically forward through a channel + `idle_add`.
pub type DictationHook = Arc<dyn Fn() + Send + Sync + 'static>;

/// Smart-mode tap ceiling default (ms): a press shorter than this
/// toggles, a longer one holds. Overridable per install via the
/// `smart_tap_ms` setting; releases log their duration for calibration.
pub const SMART_TAP_MS: u64 = 150;

/// Smart press outcome, pure for testability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SmartDown {
    /// Was idle: dictation started, this press owns the session.
    Open,
    /// Was active: dictation stopped (toggle-off), press owns nothing.
    Close,
}

fn smart_down(dictating: bool) -> SmartDown {
    if dictating {
        SmartDown::Close
    } else {
        SmartDown::Open
    }
}

/// Whether a smart release stops dictation: only when the press that
/// opened the session was held past the tap ceiling.
fn smart_up_should_stop(opener: bool, held_ms: u64, tap_ms: u64) -> bool {
    opener && held_ms >= tap_ms
}

/// (Re)start the listener from live settings with explicit dictation
/// hooks. Each binding behaves per its own mode; the STT pipeline (or
/// the pill) hooks into `down` / `up`. Call on startup and after
/// every hotkey change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivationEffect {
    Open,
    Close,
    Cancel,
}

#[derive(Default)]
struct Activation {
    // Only the binding that opened a held session may release it.
    owner: Option<(usize, crate::settings::ActivationMode, Instant)>,
}
impl Activation {
    fn event(
        &mut self,
        event: HotkeyEvent,
        binding: usize,
        now: Instant,
        tap_ms: u64,
    ) -> Option<ActivationEffect> {
        use crate::settings::ActivationMode;
        match event {
            HotkeyEvent::Cancel => {
                if binding != usize::MAX && self.owner.is_none_or(|(owner, _, _)| owner != binding)
                {
                    return None;
                }
                self.owner = None;
                Some(ActivationEffect::Cancel)
            }
            HotkeyEvent::Down(_, ActivationMode::Push) => {
                if self.owner.is_some() {
                    return None;
                }
                self.owner = Some((binding, ActivationMode::Push, now));
                Some(ActivationEffect::Open)
            }
            HotkeyEvent::Down(_, mode) => match smart_down(self.owner.is_some()) {
                SmartDown::Close => {
                    self.owner = None;
                    Some(ActivationEffect::Close)
                }
                SmartDown::Open => {
                    self.owner = Some((binding, mode, now));
                    Some(ActivationEffect::Open)
                }
            },
            HotkeyEvent::Up(_, mode) => {
                let (owner, owner_mode, opened) = self.owner?;
                if owner != binding || owner_mode != mode {
                    return None;
                }
                let close = match mode {
                    ActivationMode::Push => true,
                    ActivationMode::Tap => false,
                    ActivationMode::Smart => smart_up_should_stop(
                        true,
                        now.saturating_duration_since(opened)
                            .as_millis()
                            .min(u64::MAX as u128) as u64,
                        tap_ms,
                    ),
                };
                if close {
                    self.owner = None;
                    Some(ActivationEffect::Close)
                } else {
                    None
                }
            }
        }
    }
}

static ACTIVATION_RESET: std::sync::Mutex<Option<std::sync::Weak<std::sync::Mutex<Activation>>>> =
    std::sync::Mutex::new(None);

fn activation_active() -> bool {
    ACTIVATION_RESET
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .and_then(std::sync::Weak::upgrade)
        .is_some_and(|activation| {
            activation
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .owner
                .is_some()
        })
}

/// A failed capture retires Toggle/Smart ownership so the next press can retry.
pub fn reset_activation() {
    if let Some(activation) = ACTIVATION_RESET
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .and_then(std::sync::Weak::upgrade)
    {
        activation.lock().unwrap_or_else(|p| p.into_inner()).owner = None;
    }
}

pub fn start_dictation(
    bindings: Vec<crate::settings::HotkeyBinding>,
    tap_ms: u64,
    down: DictationHook,
    up: DictationHook,
    cancel: DictationHook,
) {
    let mut map = HashMap::new();
    map.insert(HotkeyAction::DictationToggle, bindings);
    let activation = Arc::new(std::sync::Mutex::new(Activation::default()));
    *ACTIVATION_RESET.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::downgrade(&activation));
    let on_event: OwnedCallback = Arc::new(move |event, binding| {
        let effect = activation.lock().unwrap_or_else(|p| p.into_inner()).event(
            event,
            binding,
            Instant::now(),
            tap_ms,
        );
        // Never hold activation state across a hook: failed capture can reset it.
        match effect {
            Some(ActivationEffect::Open) => down(),
            Some(ActivationEffect::Close) => up(),
            Some(ActivationEffect::Cancel) => cancel(),
            None => {}
        }
    });
    let on_status: Arc<dyn Fn(HotkeyStatus) + Send + Sync + 'static> =
        Arc::new(|st| eprintln!("[hotkey] status: {st:?}"));
    start_owned(map, on_event, on_status);
}

/// Frontends only persist bindings; the service owns and observes its listener.
pub fn apply_from_settings(_state: &crate::app::AppState) {}

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
    }

    #[test]
    fn smart_tap_holds_toggle() {
        // Tap (under the ceiling) toggles: opener release keeps on.
        assert_eq!(smart_down(false), SmartDown::Open);
        assert!(!smart_up_should_stop(true, SMART_TAP_MS - 1, SMART_TAP_MS));
        // Hold (at/over the ceiling) behaves like push: release stops.
        assert!(smart_up_should_stop(true, SMART_TAP_MS, SMART_TAP_MS));
        assert!(smart_up_should_stop(
            true,
            SMART_TAP_MS + 5000,
            SMART_TAP_MS
        ));
        // A press that closed (toggle-off) never stops on release.
        assert_eq!(smart_down(true), SmartDown::Close);
        assert!(!smart_up_should_stop(
            false,
            SMART_TAP_MS + 5000,
            SMART_TAP_MS
        ));
        // Custom ceiling honored.
        assert!(!smart_up_should_stop(true, 100, 150));
        assert!(smart_up_should_stop(true, 150, 150));
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

    // Deterministic descriptor ownership plus the retained native integration
    // gate. Native mode explicitly requires uinput/input access; no assertions
    // are skipped or weakened to make sandboxed package builds pass.
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

    struct SyntheticKeyboard(std::collections::VecDeque<Vec<evdev::InputEvent>>);
    impl KeyDevice for SyntheticKeyboard {
        fn events(&mut self) -> std::io::Result<Vec<evdev::InputEvent>> {
            self.0
                .pop_front()
                .ok_or_else(|| std::io::ErrorKind::WouldBlock.into())
        }
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

    /// The production driver consumes the same key frames from deterministic
    /// and real uinput sources. Native mode is required by the live gate script.
    #[test]
    fn virtual_keyboard_fires_down_up() {
        use std::sync::Mutex;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let callback: OwnedCallback = Arc::new(move |event, _| sink.lock().unwrap().push(event));
        let mut watched = [Watched {
            binding_id: 0,
            press_device: None,
            action: HotkeyAction::DictationToggle,
            combo: parse_combo("F13").unwrap(),
            mode: crate::settings::ActivationMode::Push,
            active: false,
            active_since: None,
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
        for _ in 0..4 {
            poll_events(&mut devices, &mut held, &mut watched, &callback);
        }
        assert_edges(&seen.lock().unwrap());
        if std::env::var_os("MYWHISPER_NATIVE_HOTKEY_GATE").is_some() {
            seen.lock().unwrap().clear();
            let (mut keyboard, path, device) = native_keyboard();
            let mut devices = vec![(path, device)];
            for event in [press, release] {
                keyboard.emit(&[event, syn]).unwrap();
                let deadline = Instant::now() + Duration::from_secs(2);
                let expected = if event.value() == 1 { 1 } else { 2 };
                while seen.lock().unwrap().len() < expected {
                    assert!(
                        Instant::now() < deadline,
                        "native keyboard did not deliver its edge"
                    );
                    poll_events(&mut devices, &mut held, &mut watched, &callback);
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            assert_edges(&seen.lock().unwrap());
            drop(devices);
            drop(keyboard);
        }
    }

    #[test]
    fn saved_startup_mute_and_feedback_apply_before_first_input() {
        // Process isolation gives this check the real startup globals without
        // racing other tests or reading a user's configuration/keyring.
        if std::env::var_os("MYWHISPER_STARTUP_MUTE_CHILD").is_none() {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "hotkey::tests::saved_startup_mute_and_feedback_apply_before_first_input",
                    "--nocapture",
                ])
                .env("MYWHISPER_STARTUP_MUTE_CHILD", "1");
            let child = crate::child::OwnedChild::spawn(&mut command).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(status) = child.try_status().unwrap() {
                    assert!(status.success(), "isolated startup gate failed: {status}");
                    return;
                }
                assert!(Instant::now() < deadline, "startup gate exceeded deadline");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("synthetic.db");
        let store = crate::store::Store::open_with_key(&path, &[17; 32]).unwrap();
        let mut saved = store.load_settings();
        saved.hotkeys.hotkeys_enabled = false;
        saved.preferences.dictation_sounds = false;
        saved.preferences.pause_media_on_dictation = true;
        store.save_settings(&saved).unwrap();
        drop(store);
        let store = crate::store::Store::open_with_key(&path, &[17; 32]).unwrap();
        assert!(crate::transcribe::hotkeys_listening());
        assert!(crate::sound::enabled());
        assert!(!crate::media::enabled());
        crate::service::apply_input_preferences(&store.try_load_settings().unwrap());
        assert!(!crate::transcribe::hotkeys_listening());
        assert!(!crate::sound::enabled());
        assert!(crate::media::enabled());
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let callback: OwnedCallback = Arc::new(move |event, _| sink.lock().unwrap().push(event));
        let mut watched = [Watched {
            binding_id: 0,
            press_device: None,
            action: HotkeyAction::DictationToggle,
            combo: parse_combo("F13").unwrap(),
            mode: crate::settings::ActivationMode::Push,
            active: false,
            active_since: None,
        }];
        let mut held = HeldKeys::default();
        let press = evdev::InputEvent::new(EventType::KEY.0, KeyCode::KEY_F13.0, 1);
        let release = evdev::InputEvent::new(EventType::KEY.0, KeyCode::KEY_F13.0, 0);
        let mut devices = vec![(
            "synthetic-keyboard".into(),
            SyntheticKeyboard(std::collections::VecDeque::from([
                vec![press],
                vec![release],
            ])),
        )];
        poll_events(&mut devices, &mut held, &mut watched, &callback);
        poll_events(&mut devices, &mut held, &mut watched, &callback);
        assert!(
            seen.lock().unwrap().is_empty(),
            "muted startup emitted input"
        );
        if std::env::var_os("MYWHISPER_NATIVE_HOTKEY_GATE").is_some() {
            let (mut keyboard, path, device) = native_keyboard();
            let mut devices = vec![(path.clone(), device)];
            for event in [press, release] {
                keyboard.emit(&[event]).unwrap();
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    poll_events(&mut devices, &mut held, &mut watched, &callback);
                    let pressed = held
                        .devices
                        .get(&path)
                        .is_some_and(|keys| keys.contains(&KeyCode::KEY_F13));
                    if pressed == (event.value() == 1) {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "owned native input was not consumed"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                }
                assert!(
                    seen.lock().unwrap().is_empty(),
                    "native muted startup emitted input"
                );
            }
        }
        saved.hotkeys.hotkeys_enabled = true;
        saved.preferences.dictation_sounds = true;
        saved.preferences.pause_media_on_dictation = false;
        store.save_settings(&saved).unwrap();
        crate::service::apply_input_preferences(&store.try_load_settings().unwrap());
        assert!(crate::sound::enabled() && !crate::media::enabled());
        let mut devices = vec![(
            "synthetic-keyboard".into(),
            SyntheticKeyboard(std::collections::VecDeque::from([
                vec![press],
                vec![release],
            ])),
        )];
        poll_events(&mut devices, &mut held, &mut watched, &callback);
        poll_events(&mut devices, &mut held, &mut watched, &callback);
        assert_edges(&seen.lock().unwrap());
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
        assert_eq!(
            seen,
            [down, up],
            "edges must be ordered, singular and repeat-free"
        );
    }
}

#[cfg(test)]
mod mapping_regressions {
    use super::*;
    #[test]
    fn punctuation_aliases_and_all_canonical_tokens_round_trip() {
        for (token, code) in KEY_TOKENS {
            assert_eq!(key_by_name(token), Some(*code));
            assert_eq!(token_for_evdev_code(code.code()), Some(*token));
            assert_eq!(
                parse_combo(&format!("Ctrl+{token}")).unwrap().target,
                Some(*code)
            );
        }
        for (symbol, token) in [
            ("-", "Minus"),
            ("=", "Equal"),
            ("[", "LeftBracket"),
            ("]", "RightBracket"),
            (";", "Semicolon"),
            ("'", "Apostrophe"),
            ("`", "Grave"),
            ("\\", "Backslash"),
            (",", "Comma"),
            (".", "Period"),
            ("/", "Slash"),
        ] {
            assert_eq!(key_by_name(symbol), key_by_name(token));
            assert_eq!(
                parse_combo(&format!("Ctrl+{symbol}")).unwrap().target,
                parse_combo(&format!("Ctrl+{token}")).unwrap().target
            );
        }
        for invalid in ["Ctrl++", "+Ctrl", "Ctrl+", "Ctrl+A+B", "Ctrl+ñ", "Ctrl+界"] {
            assert!(parse_combo(invalid).is_none(), "{invalid}");
        }
        assert!(parse_combo(&"A".repeat(257)).is_none());
    }

    #[test]
    fn inverse_tokens_round_trip_and_nested_bindings_match_exactly() {
        for code in 0..=255 {
            if let Some(token) = token_for_evdev_code(code) {
                assert_eq!(parse_combo(token).unwrap().target.unwrap().code(), code);
            }
        }
        assert_eq!(token_for_evdev_code(46), Some("C"));
        assert_eq!(token_for_evdev_code(54), None);
        assert!(parse_combo("Ctrl+A+B").is_none());
        let combo = parse_combo("Ctrl+A").unwrap();
        let mut held = HashSet::from([KeyCode::KEY_LEFTCTRL, KeyCode::KEY_A]);
        assert!(mods_ok(&combo, &held));
        held.insert(KeyCode::KEY_LEFTSHIFT);
        assert!(!mods_ok(&combo, &held));
    }
}

#[cfg(test)]
mod device_held_regressions {
    use super::*;
    #[test]
    fn release_and_disconnect_remove_only_the_owning_keyboard() {
        let mut held = HeldKeys::default();
        let a = std::path::Path::new("a");
        let b = std::path::Path::new("b");
        held.update(a, KeyCode::KEY_LEFTCTRL, true);
        held.update(b, KeyCode::KEY_LEFTCTRL, true);
        held.update(a, KeyCode::KEY_LEFTCTRL, false);
        assert!(ctrl_held(&held.aggregate));
        held.update(a, KeyCode::KEY_LEFTSHIFT, true);
        held.retain(&HashSet::from([b.to_path_buf()]));
        assert!(ctrl_held(&held.aggregate));
        assert!(!shift_held(&held.aggregate));
        held.retain(&HashSet::new());
        assert!(held.aggregate.is_empty());
    }
}

#[cfg(test)]
mod ownership_regressions {
    use super::*;
    use crate::settings::ActivationMode::{Push, Smart, Tap};
    use std::sync::Mutex;

    fn down(mode: crate::settings::ActivationMode) -> HotkeyEvent {
        HotkeyEvent::Down(HotkeyAction::DictationToggle, mode)
    }
    fn up(mode: crate::settings::ActivationMode) -> HotkeyEvent {
        HotkeyEvent::Up(HotkeyAction::DictationToggle, mode)
    }
    fn watched(binding_id: usize, combo: &str, mode: crate::settings::ActivationMode) -> Watched {
        Watched {
            binding_id,
            press_device: None,
            action: HotkeyAction::DictationToggle,
            combo: parse_combo(combo).unwrap(),
            mode,
            active: false,
            active_since: None,
        }
    }

    #[test]
    fn cancellation_retires_every_mode_before_the_next_toggle() {
        for mode in [Push, Tap, Smart] {
            let now = Instant::now();
            let mut activation = Activation::default();
            assert_eq!(
                activation.event(down(mode), 0, now, 200),
                Some(ActivationEffect::Open)
            );
            assert_eq!(
                activation.event(HotkeyEvent::Cancel, usize::MAX, now, 200),
                Some(ActivationEffect::Cancel)
            );
            assert_eq!(
                activation.event(up(mode), 0, now + Duration::from_secs(1), 200),
                None
            );
            assert_eq!(
                activation.event(down(Tap), 1, now, 200),
                Some(ActivationEffect::Open)
            );
        }
    }

    #[test]
    fn mixed_bindings_cannot_release_a_session_they_did_not_open() {
        let mut activation = Activation::default();
        let now = Instant::now();
        assert_eq!(
            activation.event(down(Tap), 0, now, 200),
            Some(ActivationEffect::Open)
        );
        assert_eq!(activation.event(down(Push), 1, now, 200), None);
        assert_eq!(activation.event(up(Push), 1, now, 200), None);
        assert_eq!(
            activation.event(down(Smart), 2, now, 200),
            Some(ActivationEffect::Close)
        );
        assert_eq!(
            activation.event(down(Smart), 3, now, 200),
            Some(ActivationEffect::Open)
        );
        assert_eq!(
            activation.event(up(Smart), 2, now + Duration::from_secs(1), 200),
            None
        );
        assert_eq!(
            activation.event(up(Smart), 3, now + Duration::from_secs(1), 200),
            Some(ActivationEffect::Close)
        );
        assert_eq!(
            activation.event(down(Push), 4, now, 200),
            Some(ActivationEffect::Open)
        );
        assert_eq!(activation.event(down(Push), 5, now, 200), None);
        assert_eq!(activation.event(up(Push), 5, now, 200), None);
        assert_eq!(
            activation.event(up(Push), 4, now, 200),
            Some(ActivationEffect::Close)
        );
    }

    #[test]
    fn smart_timing_uses_its_own_monotonic_press_and_survives_backward_clock() {
        let mut activation = Activation::default();
        let now = Instant::now();
        let later = now + Duration::from_secs(1);
        assert_eq!(
            activation.event(down(Smart), 0, later, 200),
            Some(ActivationEffect::Open)
        );
        assert_eq!(activation.event(up(Smart), 0, now, 200), None);
        assert_eq!(
            activation.event(down(Tap), 1, later, 200),
            Some(ActivationEffect::Close)
        );
        assert_eq!(
            activation.event(down(Push), 2, later, 200),
            Some(ActivationEffect::Open)
        );
        assert_eq!(
            activation.event(up(Smart), 0, later + Duration::from_secs(1), 200),
            None
        );
        assert_eq!(
            activation.event(up(Push), 2, later, 200),
            Some(ActivationEffect::Close)
        );
    }

    #[test]
    fn overlapping_keyboards_keep_target_down_until_the_last_release() {
        let mut held = HeldKeys::default();
        let mut watched = [watched(0, "Ctrl+F13", Push)];
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let callback: OwnedCallback =
            Arc::new(move |event, binding| sink.lock().unwrap().push((event, binding)));
        let now = Instant::now();
        for (device, key, value) in [
            ("a", KeyCode::KEY_LEFTCTRL, 1),
            ("b", KeyCode::KEY_LEFTCTRL, 1),
            ("a", KeyCode::KEY_LEFTCTRL, 0),
            ("a", KeyCode::KEY_F13, 1),
            ("b", KeyCode::KEY_F13, 1),
            ("a", KeyCode::KEY_F13, 0),
        ] {
            dispatch_key(
                KeyInput {
                    path: std::path::Path::new(device),
                    key,
                    value,
                    now,
                },
                &mut held,
                &mut watched,
                &callback,
                true,
                false,
            );
        }
        assert_eq!(*seen.lock().unwrap(), vec![(down(Push), 0)]);
        dispatch_key(
            KeyInput {
                path: std::path::Path::new("b"),
                key: KeyCode::KEY_F13,
                value: 0,
                now,
            },
            &mut held,
            &mut watched,
            &callback,
            true,
            false,
        );
        assert_eq!(*seen.lock().unwrap(), vec![(down(Push), 0), (up(Push), 0)]);
    }

    #[test]
    fn unplug_only_cancels_the_binding_owned_by_that_keyboard() {
        let now = Instant::now();
        for mode in [Push, Tap, Smart] {
            let mut activation = Activation::default();
            assert_eq!(
                activation.event(down(mode), 0, now, 200),
                Some(ActivationEffect::Open)
            );
            assert_eq!(activation.event(HotkeyEvent::Cancel, 1, now, 200), None);
            assert_eq!(
                activation.event(HotkeyEvent::Cancel, 0, now, 200),
                Some(ActivationEffect::Cancel)
            );
        }
        let mut held = HeldKeys::default();
        held.update(std::path::Path::new("a"), KeyCode::KEY_LEFTCTRL, true);
        held.update(std::path::Path::new("b"), KeyCode::KEY_LEFTCTRL, true);
        let mut watched = [watched(0, "F13", Tap), watched(1, "F14", Smart)];
        watched[0].press_device = Some("a".into());
        watched[1].press_device = Some("b".into());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let callback: OwnedCallback =
            Arc::new(move |event, binding| sink.lock().unwrap().push((event, binding)));
        disconnect_keyboard(
            std::path::Path::new("a"),
            &mut held,
            &mut watched,
            &callback,
        );
        assert!(ctrl_held(&held.aggregate));
        assert_eq!(*seen.lock().unwrap(), vec![(HotkeyEvent::Cancel, 0)]);
        assert_eq!(
            watched[1].press_device.as_deref(),
            Some(std::path::Path::new("b"))
        );
    }

    #[test]
    fn unplug_a_required_modifier_cancels_only_after_its_last_owner_is_gone() {
        let mut held = HeldKeys::default();
        for path in ["a", "b"] {
            held.update(std::path::Path::new(path), KeyCode::KEY_LEFTCTRL, true);
        }
        held.update(std::path::Path::new("c"), KeyCode::KEY_F13, true);
        let mut watched = [watched(0, "Ctrl+F13", Push)];
        watched[0].active = true;
        watched[0].press_device = Some("c".into());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let callback: OwnedCallback =
            Arc::new(move |event, id| sink.lock().unwrap().push((event, id)));
        disconnect_keyboard(
            std::path::Path::new("a"),
            &mut held,
            &mut watched,
            &callback,
        );
        assert!(seen.lock().unwrap().is_empty());
        assert!(watched[0].active);
        disconnect_keyboard(
            std::path::Path::new("b"),
            &mut held,
            &mut watched,
            &callback,
        );
        assert_eq!(*seen.lock().unwrap(), vec![(HotkeyEvent::Cancel, 0)]);
        assert!(!watched[0].active);
    }

    #[test]
    fn escape_retires_a_toggle_even_before_the_engine_processes_its_press() {
        let state = Arc::new(Mutex::new(Activation::default()));
        let effects = Arc::new(Mutex::new(Vec::new()));
        let sink = effects.clone();
        let target = state.clone();
        let callback: OwnedCallback = Arc::new(move |event, id| {
            if let Some(effect) = target.lock().unwrap().event(event, id, Instant::now(), 200) {
                sink.lock().unwrap().push(effect);
            }
        });
        let mut held = HeldKeys::default();
        let mut watched = [watched(0, "F13", Tap)];
        for (key, value) in [
            (KeyCode::KEY_F13, 1),
            (KeyCode::KEY_F13, 0),
            (KeyCode::KEY_ESC, 1),
            (KeyCode::KEY_F13, 1),
        ] {
            // No recording exists yet. Input ownership still observes the
            // queued press and must cancel it before the subsequent toggle.
            let queued = state.lock().unwrap().owner.is_some();
            dispatch_key(
                KeyInput {
                    path: std::path::Path::new("a"),
                    key,
                    value,
                    now: Instant::now(),
                },
                &mut held,
                &mut watched,
                &callback,
                true,
                queued,
            );
        }
        assert_eq!(
            *effects.lock().unwrap(),
            vec![
                ActivationEffect::Open,
                ActivationEffect::Cancel,
                ActivationEffect::Open
            ]
        );
    }

    #[test]
    fn escape_mute_repeats_and_rapid_cycles_do_not_emit_stale_releases() {
        let mut held = HeldKeys::default();
        let mut watched = [watched(0, "F13", Push)];
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let callback: OwnedCallback = Arc::new(move |event, _| sink.lock().unwrap().push(event));
        let mut send = |key, value, enabled, live| {
            dispatch_key(
                KeyInput {
                    path: std::path::Path::new("a"),
                    key,
                    value,
                    now: Instant::now(),
                },
                &mut held,
                &mut watched,
                &callback,
                enabled,
                live,
            );
        };
        send(KeyCode::KEY_F13, 1, true, false);
        send(KeyCode::KEY_ESC, 1, true, true);
        send(KeyCode::KEY_F13, 0, true, false);
        assert_eq!(*seen.lock().unwrap(), vec![down(Push), HotkeyEvent::Cancel]);
        seen.lock().unwrap().clear();
        send(KeyCode::KEY_F13, 1, false, false);
        send(KeyCode::KEY_F13, 2, true, false);
        send(KeyCode::KEY_F13, 1, true, false);
        send(KeyCode::KEY_F13, 0, false, false);
        assert!(seen.lock().unwrap().is_empty());
        for _ in 0..20 {
            send(KeyCode::KEY_F13, 1, true, false);
            send(KeyCode::KEY_F13, 2, true, false);
            send(KeyCode::KEY_F13, 0, true, false);
        }
        assert_eq!(*seen.lock().unwrap(), [down(Push), up(Push)].repeat(20));
    }
}

#[cfg(test)]
mod suspend_handshake_tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    #[test]
    fn released_capture_nonce_cannot_replay_during_a_new_frontend_start() {
        let first = [1; 16];
        let second = [2; 16];
        let mut previous = None;
        assert_eq!(
            fresh_capture_request(&mut previous, Some(first)),
            Some(first)
        );
        assert_eq!(fresh_capture_request(&mut previous, Some(first)), None);
        // An available lock or temporarily absent request cannot forget an
        // acknowledged request. Reacquisition initially exposes that old file.
        assert_eq!(fresh_capture_request(&mut previous, None), None);
        assert_eq!(fresh_capture_request(&mut previous, Some(first)), None);
        assert_eq!(
            fresh_capture_request(&mut previous, Some(second)),
            Some(second)
        );
        assert_eq!(fresh_capture_request(&mut previous, Some(second)), None);
        assert_eq!(fresh_capture_request(&mut previous, None), None);
    }

    struct ChildGuard(Option<std::process::Child>);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if let Some(child) = self.0.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    fn wait_for(root: &std::path::Path, name: &str, bytes: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if crate::private_fs::read_bounded(&root.join(name), 16)
                .ok()
                .as_deref()
                == Some(bytes)
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let diagnostic =
            crate::private_fs::read_bounded(&root.join("child.log"), 4096).unwrap_or_default();
        panic!(
            "disposable capture handshake did not reach {name}: {}",
            String::from_utf8_lossy(&diagnostic)
        );
    }

    #[test]
    fn capture_requires_fresh_ack_and_recovers_after_process_exit() {
        const CHILD_ROOT: &str = "MYWHISPER_SUSPEND_TEST_ROOT";
        const ISOLATED_OWNER: &str = "MYWHISPER_SUSPEND_TEST_OWNER";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = std::path::PathBuf::from(root);
            assert!(
                root.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("mywhisper-capture-handshake-")
            );
            assert!(root.join(".disposable").is_file());
            assert_eq!(root.metadata().unwrap().uid(), unsafe { libc::geteuid() });
            assert_eq!(root.metadata().unwrap().permissions().mode() & 0o777, 0o700);
            let _engine = crate::private_fs::try_lock(&root.join("engine.lock")).unwrap();
            configure_capture_suspend(&root);
            let (tx, rx) = std::sync::mpsc::channel();
            register_capture_suspend(move |nonce| {
                let _ = tx.send(nonce);
            });
            crate::private_fs::replace(&root.join("ready"), b"ready").unwrap();
            // Even an engine with no configured bindings must acknowledge
            // capture without opening any physical keyboard.
            start(HashMap::new(), Arc::new(|_| {}), Arc::new(|_| {}));
            let first = rx.recv_timeout(Duration::from_secs(3)).unwrap();
            crate::private_fs::replace(&root.join("observed"), &first).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while capture_request() == Some(first) {
                assert!(
                    Instant::now() < deadline,
                    "new frontend did not publish its fresh request"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            acknowledge_capture_suspend(first).unwrap();
            assert!(
                !root.join("hotkey-capture.ack").exists(),
                "stale request was acknowledged"
            );
            for _ in 0..2 {
                let nonce = rx.recv_timeout(Duration::from_secs(3)).unwrap();
                acknowledge_capture_suspend(nonce).unwrap();
            }
            stop();
            crate::private_fs::replace(&root.join("done"), b"done").unwrap();
            std::thread::sleep(Duration::from_secs(3));
            return;
        }
        if std::env::var_os(ISOLATED_OWNER).is_none() {
            // Parallel tests fork/pre_exec, temporarily inheriting this
            // process's flock descriptions until exec. Isolate the actual
            // lock owner as well as its engine: all fresh-ack and immediate
            // post-exit release assertions below remain unchanged.
            let mut owner=ChildGuard(Some(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact","hotkey::suspend_handshake_tests::capture_requires_fresh_ack_and_recovers_after_process_exit","--test-threads=1"])
                .env(ISOLATED_OWNER,"1").stdin(std::process::Stdio::null()).spawn().unwrap()));
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                if let Some(status) = owner.0.as_mut().unwrap().try_wait().unwrap() {
                    assert!(status.success(), "isolated lock owner failed: {status}");
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "isolated lock owner deadline exceeded"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let root = tempfile::Builder::new()
            .prefix("mywhisper-capture-handshake-")
            .tempdir()
            .unwrap();
        crate::private_fs::directory(root.path()).unwrap();
        std::fs::write(root.path().join(".disposable"), []).unwrap();
        let capture =
            crate::private_fs::try_lock(&root.path().join("hotkey-capture.lock")).unwrap();
        let mut first = [0u8; 16];
        getrandom::fill(&mut first).unwrap();
        let mut second = first;
        second[0] ^= 1;
        let mut third = first;
        third[0] ^= 2;
        crate::private_fs::replace(&root.path().join("hotkey-capture.request"), &first).unwrap();
        let log = std::fs::File::create(root.path().join("child.log")).unwrap();
        log.set_permissions(std::fs::Permissions::from_mode(0o600))
            .unwrap();
        let mut engine = ChildGuard(Some(std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "hotkey::suspend_handshake_tests::capture_requires_fresh_ack_and_recovers_after_process_exit", "--test-threads=1"])
            .env(CHILD_ROOT, root.path()).stdin(std::process::Stdio::null())
            .stdout(log.try_clone().unwrap()).stderr(log).spawn().unwrap()));
        wait_for(root.path(), "ready", b"ready");
        wait_for(root.path(), "observed", &first);
        crate::private_fs::replace(&root.path().join("hotkey-capture.request"), &second).unwrap();
        wait_for(root.path(), "hotkey-capture.ack", &second);
        drop(capture);
        std::thread::sleep(Duration::from_millis(100));
        let capture =
            crate::private_fs::try_lock(&root.path().join("hotkey-capture.lock")).unwrap();
        // A frontend owns the lock before its atomic request write. Keep
        // that legitimate startup gap open for two native poll intervals.
        // The old nonce must not be emitted again and consume the next ack.
        std::thread::sleep(Duration::from_millis(100));
        crate::private_fs::replace(&root.path().join("hotkey-capture.request"), &third).unwrap();
        wait_for(root.path(), "hotkey-capture.ack", &third);
        wait_for(root.path(), "done", b"done");
        assert!(crate::private_fs::try_lock(&root.path().join("engine.lock")).is_err());
        let mut child = engine.0.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(crate::private_fs::try_lock(&root.path().join("engine.lock")).is_ok());
        drop(capture);
        assert!(crate::private_fs::try_lock(&root.path().join("hotkey-capture.lock")).is_ok());
    }
}
