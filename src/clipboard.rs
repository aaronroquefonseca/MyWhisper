//! Clipboard write + paste injection for dictation output.
//!
//! Copy side owns an ext-data-control/wlr-data-control Wayland source and
//! publishes a sensitive hint when requested. Paste side: `wtype`
//! (`wtype` package, virtual-keyboard protocol) or `ydotool`
//! (`ydotool` package + running `ydotoold` + input group). The chord
//! is configurable: Ctrl+Shift+V (default), Ctrl+V, Shift+Insert.

use crate::settings::{PasteBackend, PasteCombo, Preferences};
use std::process::Command;
use std::time::Duration;

fn have(binary: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        use std::os::unix::fs::PermissionsExt;
        std::env::split_paths(&paths).any(|directory| {
            let file = directory.join(binary);
            file.metadata().is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
    })
}

/// Dependency strings for the settings UI, per backend.
pub fn copy_backend_deps() -> &'static str {
    "Wayland data-control; wl-copy/wl-paste fallback for copy or Keep in clipboard on"
}

pub fn paste_backend_deps(backend: PasteBackend) -> &'static str {
    match backend {
        PasteBackend::Wtype => "wtype (wtype package; Wayland virtual-keyboard protocol)",
        PasteBackend::Ydotool => {
            "ydotool (ydotool package) + running ydotoold service + input group"
        }
    }
}

/// What's installed right now, for status lines in the UI.
#[derive(Debug, Clone, Copy)]
pub struct BackendStatus {
    pub wayland: bool,
    pub wl_copy: bool,
    pub wtype: bool,
    pub ydotool: bool,
    pub ydotoold_socket: bool,
}

pub fn probe() -> BackendStatus {
    let socket = std::env::var("XDG_RUNTIME_DIR")
        .map(|d| std::path::PathBuf::from(format!("{d}/.ydotool_socket")))
        .map(|p| p.exists())
        .unwrap_or(false);
    BackendStatus {
        wl_copy: have("wl-copy"),
        wayland: std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var_os("WAYLAND_SOCKET").is_some(),
        wtype: have("wtype"),
        ydotool: have("ydotool"),
        ydotoold_socket: socket,
    }
}

/// Exact MIME offers, including a genuinely empty selection. Snapshot errors
/// are returned rather than being mistaken for an empty clipboard.
#[derive(Clone)]
pub enum SavedClipboard {
    Empty,
    Text(String),
    Raw { mime: String, bytes: Vec<u8> },
    Formats(Vec<(String, Vec<u8>)>),
}

struct Request {
    operation: Operation,
    guard: crate::selection::Guard,
    response: std::sync::mpsc::Sender<Result<crate::selection::Payload, String>>,
}
enum Operation {
    Snapshot,
    Copy(crate::selection::Payload, bool),
    Pipeline(String, Preferences),
}
struct Worker {
    sender: std::sync::mpsc::SyncSender<Request>,
    // The process owns one selection worker, rather than a child/reaper per
    // dictation. Its only retained data is the currently requested selection.
    _thread: crate::runtime::OwnedThread,
}
static WORKER: std::sync::OnceLock<Worker> = std::sync::OnceLock::new();
static WORKER_INIT: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn worker() -> Result<&'static Worker, String> {
    use std::sync::mpsc;
    let _lock = WORKER_INIT
        .lock()
        .map_err(|_| "clipboard worker initialization failed")?;
    if crate::runtime::closing() {
        return Err("clipboard worker stopped".into());
    }
    if WORKER.get().is_none() {
        let (sender, receiver) = mpsc::sync_channel::<Request>(4);
        let thread = crate::runtime::spawn_dedicated("clipboard-selection", move || {
            let mut selection: Option<crate::selection::Selection> = None;
            let mut fallback: Option<crate::selection::CliSelection> = None;
            loop {
                if crate::runtime::closing() {
                    if let Some(selection) = selection.as_mut() {
                        selection.clear_if_owned();
                    }
                    return;
                }
                match receiver.recv_timeout(Duration::from_millis(2)) {
                    Ok(request) => {
                        let pipeline = matches!(&request.operation, Operation::Pipeline(..));
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            if !request.guard.as_ref()() {
                                return Err("clipboard delivery cancelled".into());
                            }
                            if selection.is_none() {
                                match crate::selection::Selection::open(&request.guard) {
                                    Ok(native) => {
                                        selection = Some(native);
                                        fallback = None;
                                    }
                                    Err(error) if error == crate::selection::UNSUPPORTED => {
                                        let (payload, sensitive, paste_prefs) = match &request.operation {
                                                Operation::Copy(payload, sensitive) => (payload.clone(), *sensitive, None),
                                                Operation::Pipeline(text, prefs) if prefs.keep_transcription_in_clipboard => (text_payload(text), prefs.hide_from_history, Some(prefs)),
                                                _ => return Err("This compositor needs Keep in clipboard enabled; safe borrowing requires Wayland data-control support".into()),
                                            };
                                        fallback = Some(crate::selection::CliSelection::copy(
                                            &payload,
                                            sensitive,
                                            &request.guard,
                                        )?);
                                        if let Some(prefs) = paste_prefs
                                            && prefs.auto_paste
                                        {
                                            paste_guarded(
                                                prefs.paste_backend,
                                                prefs.paste_combo,
                                                &request.guard,
                                            )?;
                                        }
                                        return Ok(Vec::new());
                                    }
                                    Err(error) => return Err(error),
                                }
                            }
                            let selection = selection.as_mut().ok_or("clipboard unavailable")?;
                            match request.operation {
                                Operation::Snapshot => selection.snapshot(&request.guard),
                                Operation::Copy(payload, sensitive) => {
                                    selection.copy(payload, sensitive, &request.guard)?;
                                    Ok(Vec::new())
                                }
                                Operation::Pipeline(text, prefs) => {
                                    pipeline_owned(selection, &text, &prefs, &request.guard)?;
                                    Ok(Vec::new())
                                }
                            }
                        }));
                        let result = match result {
                            Ok(result) => result,
                            Err(_) => {
                                selection = None;
                                fallback = None;
                                Err("clipboard worker failed; delivery aborted".into())
                            }
                        };
                        if !request.guard.as_ref()() {
                            fallback = None;
                        }
                        if !pipeline && !request.guard.as_ref()() {
                            if let Some(selection) = selection.as_mut() {
                                selection.clear_if_owned();
                            }
                            selection = None;
                        }
                        let _ = request.response.send(result);
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if fallback.as_ref().is_some_and(|owner| !owner.alive()) {
                            fallback = None;
                        }
                        if selection
                            .as_mut()
                            .is_some_and(|selection| selection.tick().is_err())
                        {
                            selection = None;
                        }
                    }
                }
            }
        })?;
        let _ = WORKER.set(Worker {
            sender,
            _thread: thread,
        });
    }
    WORKER
        .get()
        .ok_or_else(|| "clipboard worker unavailable".into())
}

fn request(
    operation: Operation,
    guard: crate::selection::Guard,
) -> Result<crate::selection::Payload, String> {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    let worker = worker()?;
    let stopped = Arc::new(AtomicBool::new(false));
    struct StopOnDrop(Arc<AtomicBool>);
    impl Drop for StopOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let _stop_on_return = StopOnDrop(stopped.clone());
    let stop = stopped.clone();
    let guarded: crate::selection::Guard =
        Arc::new(move || !crate::runtime::closing() && !stop.load(Ordering::Acquire) && guard());
    let (response, result) = mpsc::channel();
    worker
        .sender
        .try_send(Request {
            operation,
            guard: guarded,
            response,
        })
        .map_err(|_| "clipboard delivery queue is full")?;
    // At most sixteen snapshot formats plus ownership/paste/consumption steps;
    // the caller's cancellation also interrupts every individual operation.
    let outcome = result
        .recv_timeout(Duration::from_secs(100))
        .map_err(|_| "clipboard worker deadline exceeded")?;
    stopped.store(true, Ordering::Release);
    outcome
}
pub(crate) struct CopyTask {
    result: std::sync::mpsc::Receiver<Result<crate::selection::Payload, String>>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl CopyTask {
    pub fn poll(&self) -> Option<Result<(), String>> {
        match self.result.try_recv() {
            Ok(result) => Some(result.map(|_| ())),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err("clipboard worker stopped".into()))
            }
        }
    }
}
impl Drop for CopyTask {
    fn drop(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
    }
}
pub(crate) fn copy_text_async(text: &str, sensitive: bool) -> Result<CopyTask, String> {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    let stop = cancelled.clone();
    let guard: crate::selection::Guard =
        Arc::new(move || !crate::runtime::closing() && !stop.load(Ordering::Acquire));
    let (response, result) = mpsc::channel();
    worker()?
        .sender
        .try_send(Request {
            operation: Operation::Copy(text_payload(text), sensitive),
            guard,
            response,
        })
        .map_err(|_| "clipboard delivery queue is full")?;
    Ok(CopyTask { result, cancelled })
}

fn pipeline_owned(
    selection: &mut crate::selection::Selection,
    text: &str,
    prefs: &Preferences,
    guard: &crate::selection::Guard,
) -> Result<(), String> {
    // Borrowed transcripts are temporary: always publish the sensitive hint
    // so compliant clipboard managers skip them even when the user allows
    // kept transcripts in history. Managers that ignore the hint still read
    // the offer; that eager read counts as completion (see `consumed`), so a
    // manager-heavy session can restore before the focused target consumes
    // the paste. That is a protocol limitation, not a scheduling bug: with
    // data-control the reader is anonymous, and `set_selection` offers no
    // atomic ownership-conditional restore. The nested niri gate covers the
    // no-manager case only.
    let borrowed = !prefs.keep_transcription_in_clipboard;
    let sensitive = prefs.hide_from_history || borrowed;
    let saved = if borrowed {
        Some(selection.snapshot_tracked(guard)?)
    } else {
        None
    };
    let result = (|| {
        match &saved {
            // Never overwrite a user's replacement that landed between the
            // snapshot and the copy with the stale snapshot's predecessor.
            Some((_, token)) => {
                selection.copy_if_selection_matches(text_payload(text), sensitive, guard, token)?
            }
            None => selection.copy(text_payload(text), sensitive, guard)?,
        }
        if prefs.auto_paste {
            selection.arm(guard)?;
            paste_guarded(prefs.paste_backend, prefs.paste_combo, guard)?;
            if saved.is_some() {
                selection.consumed(guard)?;
            }
        }
        Ok(())
    })();
    if let Some((saved, _)) = saved {
        // Returning the user's earlier selection is cleanup, even when the
        // transcription was cancelled. This never republishes the transcript.
        // Restoration itself is check-then-set: `restore_if_owned` syncs and
        // skips when another client owns the selection, but a replacement
        // that lands between that sync and `set_selection` cannot be detected
        // atomically and would be overwritten. The window is one roundtrip on
        // the same connection; document it rather than claim atomicity.
        let cleanup: crate::selection::Guard = std::sync::Arc::new(|| true);
        if let Err(error) = selection.restore_if_owned(saved, &cleanup) {
            selection.clear_if_owned();
            return Err(error);
        }
    } else if result.is_err() && !guard() {
        selection.clear_if_owned();
    }
    result
}

fn text_payload(text: &str) -> crate::selection::Payload {
    let bytes = std::sync::Arc::new(text.as_bytes().to_vec());
    [
        "text/plain;charset=utf-8",
        "text/plain",
        "UTF8_STRING",
        "TEXT",
        "STRING",
    ]
    .into_iter()
    .map(|mime| (mime.into(), bytes.clone()))
    .collect()
}

pub fn save_clipboard() -> Option<SavedClipboard> {
    request(Operation::Snapshot, std::sync::Arc::new(|| true))
        .ok()
        .map(|payload| {
            if payload.is_empty() {
                SavedClipboard::Empty
            } else {
                SavedClipboard::Formats(
                    payload
                        .into_iter()
                        .map(|(mime, bytes)| (mime, (*bytes).clone()))
                        .collect(),
                )
            }
        })
}
pub fn restore_clipboard(saved: &SavedClipboard) -> Result<(), String> {
    let payload = match saved {
        SavedClipboard::Empty => Vec::new(),
        SavedClipboard::Text(text) => text_payload(text),
        SavedClipboard::Raw { mime, bytes } => {
            vec![(mime.clone(), std::sync::Arc::new(bytes.clone()))]
        }
        SavedClipboard::Formats(formats) => formats
            .iter()
            .map(|(mime, bytes)| (mime.clone(), std::sync::Arc::new(bytes.clone())))
            .collect(),
    };
    request(
        Operation::Copy(payload, false),
        std::sync::Arc::new(|| true),
    )
    .map(|_| ())
}
pub fn previous_text() -> Option<String> {
    match save_clipboard()? {
        SavedClipboard::Text(text) => Some(text),
        SavedClipboard::Formats(formats) => formats
            .into_iter()
            .find(|(mime, _)| mime.starts_with("text/plain"))
            .and_then(|(_, bytes)| String::from_utf8(bytes).ok()),
        _ => None,
    }
}
pub fn copy_text(text: &str, hide_from_history: bool) -> Result<(), String> {
    request(
        Operation::Copy(text_payload(text), hide_from_history),
        std::sync::Arc::new(|| true),
    )
    .map(|_| ())
}

/// Inject the configured paste chord into the focused window.
pub fn paste(backend: PasteBackend, combo: PasteCombo) -> Result<(), String> {
    let guard: crate::selection::Guard = std::sync::Arc::new(|| true);
    paste_guarded(backend, combo, &guard)
}
fn paste_guarded(
    backend: PasteBackend,
    combo: PasteCombo,
    guard: &crate::selection::Guard,
) -> Result<(), String> {
    match backend {
        PasteBackend::Wtype => {
            if !have("wtype") {
                return Err("wtype not found (install wtype)".to_string());
            }
            let args: &[&str] = match combo {
                PasteCombo::CtrlShiftV => &[
                    "-M", "ctrl", "-M", "shift", "-k", "v", "-m", "shift", "-m", "ctrl",
                ],
                PasteCombo::CtrlV => &["-M", "ctrl", "-k", "v", "-m", "ctrl"],
                PasteCombo::ShiftInsert => &["-M", "shift", "-k", "Insert", "-m", "shift"],
            };
            crate::child::run_guarded(
                Command::new("wtype").args(args),
                &[],
                Duration::from_secs(5),
                1024,
                || guard(),
            )
            .map(|_| ())
        }
        PasteBackend::Ydotool => {
            if !have("ydotool") {
                return Err("ydotool not found (install ydotool)".to_string());
            }
            // `ydotool key` takes KEYCODE:STATE pairs (see
            // input-event-codes.h), NOT names: 29 ctrl, 42 shift,
            // 47 v, 110 insert. Unknown tokens are silently
            // swallowed (exit 0, nothing injected), so keep these
            // numeric.
            let seq = match combo {
                PasteCombo::CtrlShiftV => "29:1 42:1 47:1 47:0 42:0 29:0",
                PasteCombo::CtrlV => "29:1 47:1 47:0 29:0",
                PasteCombo::ShiftInsert => "42:1 110:1 110:0 42:0",
            };
            let mut args = vec!["key"];
            args.extend(seq.split(' '));
            let result = crate::child::run_guarded(
                Command::new("ydotool").args(&args),
                &[],
                Duration::from_secs(5),
                1024,
                || guard(),
            )
            .map(|_| ());
            if result.is_err() {
                // uinput persists key state beyond an aborted tool process.
                // Release every key this operation could have pressed. Guarded
                // like the paste itself: no cleanup fork after cancellation.
                let _ = crate::child::run_guarded(
                    Command::new("ydotool").args(["key", "47:0", "110:0", "42:0", "29:0"]),
                    &[],
                    Duration::from_millis(500),
                    1024,
                    || guard(),
                );
            }
            result
        }
    }
}

/// FIFO finalization calls this with its capture/privacy guard. A failed copy
/// never injects a paste; temporary selections are restored after a completed
/// transfer only if this generation still owns the clipboard.
pub fn run_pipeline(
    text: &str,
    prefs: &Preferences,
    guard: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
) -> Result<(), String> {
    if !prefs.auto_paste && !prefs.keep_transcription_in_clipboard {
        return Ok(());
    }
    request(Operation::Pipeline(text.into(), prefs.clone()), guard).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_dep_strings_present() {
        assert!(copy_backend_deps().contains("data-control"));
        assert!(paste_backend_deps(PasteBackend::Wtype).contains("wtype"));
        assert!(paste_backend_deps(PasteBackend::Ydotool).contains("ydotool"));
    }
}
