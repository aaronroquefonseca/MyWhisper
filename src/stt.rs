//! Local speech-to-text: model registry, downloads, storage.
//!
//! Same model versions OpenWhispr ships (same upstreams, same files),
//! managed locally under `<data>/models/`:
//!
//! * whisper: single `ggml-*.bin` from `ggerganov/whisper.cpp` (HF)
//! * parakeet: `.tar.bz2` archives from k2-fsa sherpa-onnx releases
//!   (or the pinned Orukeet HF commit), extracted + verified by
//!   required files
//!
//! Nothing is preinstalled: everything downloads on demand with
//! progress, size verification and cancel. GPU runtimes and the
//! server processes are the next layer, not this module.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

// ---------------------------------------------------------------- registry

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    Whisper,
    Parakeet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParakeetRuntime {
    Offline,
    Online,
}

#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub id: &'static str,
    pub kind: ModelKind,
    pub name: &'static str,
    pub description: &'static str,
    pub size_label: &'static str,
    pub size_mb: u32,
    /// Expected bytes when the upstream states it (whisper .bin and
    /// Orukeet); otherwise verified by required files after extract.
    pub expected_bytes: Option<u64>,
    pub download_url: &'static str,
    /// Archive member top dir (parakeet only).
    pub extract_dir: Option<&'static str>,
    /// Files that must exist afterwards (parakeet only).
    pub required_files: &'static [&'static str],
    pub runtime: Option<ParakeetRuntime>,
    pub recommended: bool,
}

const TRANSDUCER_FILES: &[&str] = &[
    "encoder.int8.onnx",
    "decoder.int8.onnx",
    "joiner.int8.onnx",
    "tokens.txt",
];

const COHERE_FILES: &[&str] = &[
    "encoder.int8.onnx",
    "encoder.int8.onnx.data",
    "decoder.int8.onnx",
    "tokens.txt",
];

pub const WHISPER_MODELS: &[ModelEntry] = &[
    ModelEntry {
        id: "tiny",
        kind: ModelKind::Whisper,
        name: "Tiny",
        description: "Fastest, lower quality",
        size_label: "75MB",
        size_mb: 75,
        expected_bytes: Some(78_000_000),
        download_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin",
        extract_dir: None,
        required_files: &[],
        runtime: None,
        recommended: false,
    },
    ModelEntry {
        id: "base",
        kind: ModelKind::Whisper,
        name: "Base",
        description: "Good balance",
        size_label: "142MB",
        size_mb: 142,
        expected_bytes: Some(148_000_000),
        download_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
        extract_dir: None,
        required_files: &[],
        runtime: None,
        recommended: true,
    },
    ModelEntry {
        id: "small",
        kind: ModelKind::Whisper,
        name: "Small",
        description: "Better quality, slower",
        size_label: "466MB",
        size_mb: 466,
        expected_bytes: Some(488_000_000),
        download_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
        extract_dir: None,
        required_files: &[],
        runtime: None,
        recommended: false,
    },
    ModelEntry {
        id: "medium",
        kind: ModelKind::Whisper,
        name: "Medium",
        description: "High quality",
        size_label: "1.5GB",
        size_mb: 1500,
        expected_bytes: Some(1_570_000_000),
        download_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.bin",
        extract_dir: None,
        required_files: &[],
        runtime: None,
        recommended: false,
    },
    ModelEntry {
        id: "large",
        kind: ModelKind::Whisper,
        name: "Large",
        description: "Best quality, slowest",
        size_label: "3GB",
        size_mb: 3000,
        expected_bytes: Some(3_140_000_000),
        download_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin",
        extract_dir: None,
        required_files: &[],
        runtime: None,
        recommended: false,
    },
    ModelEntry {
        id: "turbo",
        kind: ModelKind::Whisper,
        name: "Turbo",
        description: "Fast with good quality",
        size_label: "1.6GB",
        size_mb: 1600,
        expected_bytes: Some(1_670_000_000),
        download_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
        extract_dir: None,
        required_files: &[],
        runtime: None,
        recommended: false,
    },
];

pub const PARAKEET_MODELS: &[ModelEntry] = &[
    ModelEntry {
        id: "parakeet-tdt-0.6b-v3",
        kind: ModelKind::Parakeet,
        name: "Parakeet TDT 0.6B",
        description: "Fast multilingual ASR with auto language detection",
        size_label: "680MB",
        size_mb: 680,
        expected_bytes: None,
        download_url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2",
        extract_dir: Some("sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"),
        required_files: TRANSDUCER_FILES,
        runtime: None,
        recommended: false,
    },
    ModelEntry {
        id: "parakeet-unified-en-0.6b",
        kind: ModelKind::Parakeet,
        name: "Parakeet Unified EN 0.6B",
        description: "English-only, fast",
        size_label: "631MB",
        size_mb: 631,
        expected_bytes: None,
        download_url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming.tar.bz2",
        extract_dir: Some("sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming"),
        required_files: TRANSDUCER_FILES,
        runtime: None,
        recommended: false,
    },
    ModelEntry {
        id: "nemotron-speech-streaming-en-0.6b",
        kind: ModelKind::Parakeet,
        name: "Nemotron Speech Streaming EN 0.6B",
        description: "English streaming",
        size_label: "632MB",
        size_mb: 632,
        expected_bytes: None,
        download_url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25.tar.bz2",
        extract_dir: Some("sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25"),
        required_files: TRANSDUCER_FILES,
        runtime: Some(ParakeetRuntime::Online),
        recommended: false,
    },
    ModelEntry {
        id: "nemotron-3.5-asr-streaming-0.6b",
        kind: ModelKind::Parakeet,
        name: "Nemotron 3.5 ASR Streaming 0.6B",
        description: "Multilingual streaming, 15 languages",
        size_label: "650MB",
        size_mb: 650,
        expected_bytes: None,
        download_url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-560ms-int8-2026-06-11.tar.bz2",
        extract_dir: Some("sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-560ms-int8-2026-06-11"),
        required_files: TRANSDUCER_FILES,
        runtime: Some(ParakeetRuntime::Online),
        recommended: false,
    },
    ModelEntry {
        id: "cohere-transcribe-03-2026",
        kind: ModelKind::Parakeet,
        name: "Cohere Transcribe 2B",
        description: "14 languages, no auto-detect (single-language server)",
        size_label: "2.7GB",
        size_mb: 2700,
        expected_bytes: None,
        download_url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-cohere-transcribe-14-lang-int8-2026-04-01.tar.bz2",
        extract_dir: Some("sherpa-onnx-cohere-transcribe-14-lang-int8-2026-04-01"),
        required_files: COHERE_FILES,
        runtime: None,
        recommended: false,
    },
    ModelEntry {
        id: "orukeet-v0.1.0",
        kind: ModelKind::Parakeet,
        name: "Orukeet",
        description: "Multilingual Parakeet fine-tune (CC-BY-SA-4.0)",
        size_label: "672MB",
        size_mb: 672,
        expected_bytes: Some(486_807_585),
        download_url: "https://huggingface.co/oruk/orukeet/resolve/55a984d46f68323301837194ce647c702f55facc/onnx/sherpa-onnx-orukeet-v0.1.0-int8.tar.bz2",
        extract_dir: Some("sherpa-onnx-orukeet-v0.1.0-int8"),
        required_files: TRANSDUCER_FILES,
        runtime: Some(ParakeetRuntime::Offline),
        recommended: true,
    },
];

/// Full whisper.cpp language list (99 codes + Cantonese), pulled from
/// upstream whisper.cpp. The UI prepends "auto".
pub const WHISPER_LANGS: &[&str] = &[
    "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it",
    "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no", "th", "ur",
    "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn",
    "et", "mk", "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si",
    "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo",
    "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "ln", "ha", "ba",
    "jw", "su", "yue",
];

const PARAKEET_25: &[&str] = &[
    "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt", "mt",
    "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk",
];

const NEMOTRON_15: &[&str] = &[
    "en", "es", "fr", "it", "pt", "nl", "de", "tr", "ru", "ar", "hi", "ja", "ko", "vi", "uk",
];

const COHERE_14: &[&str] = &[
    "ar", "de", "el", "en", "es", "fr", "it", "ja", "ko", "nl", "pl", "pt", "vi", "zh",
];

/// Supported transcription languages for one registry model.
/// Whisper reads the full upstream list; sherpa models carry their
/// own list (single-language models return just that one).
pub fn model_languages(model_id: &str) -> &'static [&'static str] {
    match model_id {
        "parakeet-tdt-0.6b-v3" | "orukeet-v0.1.0" => PARAKEET_25,
        "parakeet-unified-en-0.6b" | "nemotron-speech-streaming-en-0.6b" => &["en"],
        "nemotron-3.5-asr-streaming-0.6b" => NEMOTRON_15,
        "cohere-transcribe-03-2026" => COHERE_14,
        _ => WHISPER_LANGS,
    }
}

/// Registry models behind one vendor tab.
pub fn models_for_vendor(vendor: crate::settings::LocalSttProvider) -> Vec<&'static ModelEntry> {
    use crate::settings::LocalSttProvider as V;
    match vendor {
        V::Oruk => PARAKEET_MODELS
            .iter()
            .filter(|m| m.id == "orukeet-v0.1.0")
            .collect(),
        V::OpenAi => WHISPER_MODELS.iter().collect(),
        V::Nvidia => PARAKEET_MODELS
            .iter()
            .filter(|m| m.id != "cohere-transcribe-03-2026" && m.id != "orukeet-v0.1.0")
            .collect(),
        V::Cohere => PARAKEET_MODELS
            .iter()
            .filter(|m| m.id == "cohere-transcribe-03-2026")
            .collect(),
    }
}

/// Registry models backing one local provider tab.
pub fn models_for_parakeet() -> Vec<&'static ModelEntry> {
    PARAKEET_MODELS
        .iter()
        .filter(|m| m.id != "cohere-transcribe-03-2026")
        .collect()
}

#[cfg(test)]
mod lang_tests {
    #[test]
    fn whisper_list_is_complete() {
        assert_eq!(super::WHISPER_LANGS.len(), 99);
    }

    #[test]
    fn every_model_has_languages() {
        for m in super::WHISPER_MODELS
            .iter()
            .chain(super::PARAKEET_MODELS.iter())
        {
            let langs = if m.kind == super::ModelKind::Whisper {
                super::WHISPER_LANGS
            } else {
                super::model_languages(m.id)
            };
            assert!(!langs.is_empty(), "{}", m.id);
        }
    }
}

pub fn find_model(id: &str) -> Option<&'static ModelEntry> {
    WHISPER_MODELS
        .iter()
        .chain(PARAKEET_MODELS.iter())
        .find(|m| m.id == id)
}

// ---------------------------------------------------------------- storage

/// `<data>/models/whisper`, `<data>/models/parakeet`.
pub fn models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models")
}

pub fn model_dir(data_dir: &Path, entry: &ModelEntry) -> PathBuf {
    let sub = match entry.kind {
        ModelKind::Whisper => "whisper",
        ModelKind::Parakeet => "parakeet",
    };
    models_dir(data_dir).join(sub).join(entry.id)
}

/// Whisper: the .bin exists and (when known) matches expected bytes.
/// Parakeet: all required files exist under the model dir.
pub fn is_installed(data_dir: &Path, entry: &ModelEntry) -> bool {
    let dir = model_dir(data_dir, entry);
    match entry.kind {
        ModelKind::Whisper => {
            let file = dir.join(format!("ggml-{}.bin", whisper_file_stem(entry.id)));
            let Ok(meta) = std::fs::metadata(&file) else {
                return false;
            };
            match entry.expected_bytes {
                Some(n) => meta.len() >= (n * 95) / 100,
                None => meta.len() > 0,
            }
        }
        ModelKind::Parakeet => entry.required_files.iter().all(|f| dir.join(f).exists()),
    }
}

fn whisper_file_stem(id: &str) -> &str {
    match id {
        "large" => "large-v3",
        "turbo" => "large-v3-turbo",
        other => other,
    }
}

pub fn delete_model(data_dir: &Path, entry: &ModelEntry) -> std::io::Result<()> {
    let dir = model_dir(data_dir, entry);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- download

#[derive(Debug, Clone)]
pub enum DownloadEvent {
    Progress { received: u64, total: Option<u64> },
    Installing,
    Done,
}

pub type DownloadCallback = Arc<dyn Fn(DownloadEvent) + Send + Sync + 'static>;

/// Download + install one registry model. Blocking; run on a worker
/// thread. `cancel` is polled between chunks. Partial files are
/// removed on cancel/failure so the next attempt starts clean.
pub fn download_model(
    data_dir: &Path,
    entry: &ModelEntry,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
) -> Result<(), String> {
    let dir = model_dir(data_dir, entry);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;

    let tmp = dir.join(format!(".{}.part", entry.id));
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::File::create(&tmp).map_err(|e| format!("create: {e}"))?;

    let client = reqwest::blocking::Client::builder()
        .timeout(None)
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut resp = client
        .get(entry.download_url)
        .send()
        .map_err(|e| format!("request: {e}"))?;
    if !resp.status().is_success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("http status: {}", resp.status()));
    }
    let total = resp.content_length();
    let mut received = 0u64;
    let mut buf = [0u8; 65536];
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = std::fs::remove_file(&tmp);
            return Err("cancelled".to_string());
        }
        let n = resp.read(&mut buf).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("write: {e}"))?;
        received += n as u64;
        on_event(DownloadEvent::Progress { received, total });
    }
    drop(file);

    if let Some(expected) = entry.expected_bytes {
        if received < (expected * 95) / 100 {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!(
                "size mismatch: got {received} bytes, expected ~{expected}"
            ));
        }
    }

    on_event(DownloadEvent::Installing);
    match entry.kind {
        ModelKind::Whisper => {
            let stem = whisper_file_stem(entry.id);
            let dest = dir.join(format!("ggml-{stem}.bin"));
            std::fs::rename(&tmp, &dest).map_err(|e| format!("install: {e}"))?;
        }
        ModelKind::Parakeet => {
            extract_tar_bz2(&tmp, &dir).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                e
            })?;
            let _ = std::fs::remove_file(&tmp);
            // Flatten one level when the archive nests everything
            // under its extract dir (as all registry archives do).
            if let Some(top) = entry.extract_dir {
                let nested = dir.join(top);
                if nested.is_dir() {
                    flatten_dir(&nested, &dir).map_err(|e| format!("flatten: {e}"))?;
                }
            }
            let missing: Vec<_> = entry
                .required_files
                .iter()
                .filter(|f| !dir.join(f).exists())
                .collect();
            if !missing.is_empty() {
                return Err(format!("missing after extract: {missing:?}"));
            }
        }
    }
    on_event(DownloadEvent::Done);
    Ok(())
}

fn extract_tar_bz2(archive: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| format!("open archive: {e}"))?;
    let bz = bzip2::read::BzDecoder::new(file);
    let mut tar = tar::Archive::new(bz);
    tar.unpack(dest).map_err(|e| format!("extract: {e}"))?;
    Ok(())
}

fn flatten_dir(src: &Path, dest: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(src).map_err(|e| format!("read extract dir: {e}"))? {
        let entry = entry.map_err(|e| format!("read entry: {e}"))?;
        let to = dest.join(entry.file_name());
        let from = entry.path();
        if to.exists() {
            let _ = std::fs::remove_file(&to);
        }
        std::fs::rename(&from, &to).map_err(|e| format!("move file: {e}"))?;
    }
    let _ = std::fs::remove_dir(src);
    Ok(())
}

// ---------------------------------------------------------------- downloads

#[derive(Debug, Clone)]
pub enum DlPhase {
    Downloading,
    Installing,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct DlSnapshot {
    pub received: u64,
    pub total: Option<u64>,
    pub phase: DlPhase,
}

static DOWNLOADS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, (DlSnapshot, Arc<AtomicBool>)>>,
> = std::sync::OnceLock::new();

fn downloads()
-> &'static std::sync::Mutex<std::collections::HashMap<String, (DlSnapshot, Arc<AtomicBool>)>> {
    DOWNLOADS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Snapshot of an in-flight (or just-failed) download, if any.
pub fn download_state(id: &str) -> Option<DlSnapshot> {
    downloads()
        .lock()
        .ok()?
        .get(id)
        .map(|(snap, _)| snap.clone())
}

/// Start a download on a worker thread. No-op if one is running.
pub fn start_download(data_dir: PathBuf, id: &'static str) {
    {
        let map = downloads().lock().unwrap();
        if let Some((snap, _)) = map.get(id) {
            if matches!(snap.phase, DlPhase::Downloading | DlPhase::Installing) {
                return;
            }
        }
    }
    let Some(entry) = find_model(id) else {
        return;
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_worker = cancel.clone();
    downloads().lock().unwrap().insert(
        id.to_string(),
        (
            DlSnapshot {
                received: 0,
                total: None,
                phase: DlPhase::Downloading,
            },
            cancel,
        ),
    );
    std::thread::spawn(move || {
        let cb: DownloadCallback = Arc::new(move |ev| {
            if let Ok(mut map) = downloads().lock() {
                if let Some((snap, _)) = map.get_mut(id) {
                    match ev {
                        DownloadEvent::Progress { received, total } => {
                            snap.received = received;
                            snap.total = total;
                            snap.phase = DlPhase::Downloading;
                        }
                        DownloadEvent::Installing => snap.phase = DlPhase::Installing,
                        DownloadEvent::Done => {}
                    }
                }
            }
        });
        let result = download_model(&data_dir, entry, &cancel_worker, &cb);
        if let Ok(mut map) = downloads().lock() {
            match result {
                Ok(()) => {
                    map.remove(id);
                }
                Err(e) => {
                    if let Some((snap, _)) = map.get_mut(id) {
                        snap.phase = DlPhase::Failed(e);
                    }
                }
            }
        }
    });
}

/// Cancel an in-flight download.
pub fn cancel_download(id: &str) {
    if let Ok(map) = downloads().lock() {
        if let Some((_, cancel)) = map.get(id) {
            cancel.store(true, Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_complete_and_consistent() {
        assert_eq!(WHISPER_MODELS.len(), 6);
        assert_eq!(PARAKEET_MODELS.len(), 6);
        let mut recommended = 0;
        for m in WHISPER_MODELS.iter().chain(PARAKEET_MODELS.iter()) {
            assert!(m.download_url.starts_with("https://"), "{}", m.id);
            assert!(m.size_mb > 0, "{}", m.id);
            if m.recommended {
                recommended += 1;
            }
            if m.kind == ModelKind::Parakeet {
                assert!(!m.required_files.is_empty(), "{}", m.id);
                assert!(m.extract_dir.is_some(), "{}", m.id);
            }
        }
        // Exactly one recommended default per family.
        assert_eq!(recommended, 2);
    }

    #[test]
    fn find_model_resolves_all_ids() {
        for id in [
            "tiny",
            "base",
            "turbo",
            "orukeet-v0.1.0",
            "cohere-transcribe-03-2026",
        ] {
            assert!(find_model(id).is_some(), "{id}");
        }
        assert!(find_model("nope").is_none());
    }

    /// Full download round-trip against HuggingFace (75MB).
    /// Ignored by default: needs network. Proves streaming,
    /// progress events, size check and install layout.
    #[test]
    #[ignore]
    fn downloads_tiny_model() {
        let data = std::env::temp_dir().join(format!("mywhisper-dl-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let entry = find_model("tiny").unwrap();
        let cancel = AtomicBool::new(false);
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let ev = events.clone();
        let cb: DownloadCallback = std::sync::Arc::new(move |e| {
            ev.lock().unwrap().push(format!("{e:?}"));
        });
        download_model(&data, entry, &cancel, &cb).expect("tiny download");
        assert!(is_installed(&data, entry));
        let log = events.lock().unwrap();
        assert!(
            log.iter().any(|e| e.contains("Progress")),
            "no progress events"
        );
        assert!(log.last().unwrap().contains("Done"));
        let _ = std::fs::remove_dir_all(&data);
    }

    /// Full GPU pack round-trip (CPU pack, ~1.3MB). Ignored by
    /// default: needs network. Proves URL, sha256, zip extract,
    /// chmod and install layout.
    #[test]
    #[ignore]
    fn downloads_cpu_pack() {
        let data = std::env::temp_dir().join(format!("mywhisper-gpu-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let pack = GPU_PACKS.iter().find(|p| p.id == "whisper-cpu").unwrap();
        let cancel = AtomicBool::new(false);
        let cb: DownloadCallback = std::sync::Arc::new(|_| {});
        download_gpu_pack(&data, pack, &cancel, &cb).expect("cpu pack");
        assert!(gpu_installed(&data, pack));
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn whisper_stems_map_to_filenames() {
        assert_eq!(whisper_file_stem("base"), "base");
        assert_eq!(whisper_file_stem("large"), "large-v3");
        assert_eq!(whisper_file_stem("turbo"), "large-v3-turbo");
    }
}

// ---------------------------------------------------------------- GPU packs

/// whisper-server binary packs from the pinned OpenWhispr/whisper.cpp
/// release. CPU is tiny (~1MB); CUDA (~219MB) and Vulkan (~24MB) are
/// large downloads, hence on-demand. Same tag + digests OpenWhispr
/// pins, Linux x64 only (our platform).
pub const WHISPER_CPP_TAG: &str = "0.0.10";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuBackend {
    Cpu,
    Cuda,
    Vulkan,
}

#[derive(Debug, Clone)]
pub struct GpuPack {
    pub id: &'static str,
    pub backend: GpuBackend,
    pub name: &'static str,
    pub description: &'static str,
    pub size_label: &'static str,
    pub size_bytes: u64,
    pub asset: &'static str,
    pub sha256: &'static str,
    /// Install subdir under `<data>/bin/`, also the binary dir.
    pub subdir: &'static str,
}

pub const GPU_PACKS: &[GpuPack] = &[
    GpuPack {
        id: "whisper-cpu",
        backend: GpuBackend::Cpu,
        name: "CPU server",
        description: "Portable whisper-server, works everywhere",
        size_label: "1.3MB",
        size_bytes: 1_297_401,
        asset: "whisper-server-linux-x64-cpu.zip",
        sha256: "d10561f59eabb6e3273d721bc31e17092aa30ec543d9f3931e8ae04c58be6dd4",
        subdir: "whisper-cpu",
    },
    GpuPack {
        id: "whisper-cuda",
        backend: GpuBackend::Cuda,
        name: "CUDA server",
        description: "NVIDIA GPUs (Pascal 6.1+, needs proprietary driver)",
        size_label: "219MB",
        size_bytes: 218_743_566,
        asset: "whisper-server-linux-x64-cuda.zip",
        sha256: "9282d706a3c1853e10f563be5fab3d7e9e544142cc73fe9e6470aaeeaf0a5454",
        subdir: "whisper-cuda",
    },
    GpuPack {
        id: "whisper-vulkan",
        backend: GpuBackend::Vulkan,
        name: "Vulkan server",
        description: "AMD/Intel/discrete GPUs via Vulkan",
        size_label: "24MB",
        size_bytes: 24_013_074,
        asset: "whisper-server-linux-x64-vulkan.zip",
        sha256: "7bcd8b226cae9b0384613ce315fd8afab533fbdc5c3731343a09de8693eca203",
        subdir: "whisper-vulkan",
    },
];

fn gpu_url(asset: &str) -> String {
    format!("https://github.com/OpenWhispr/whisper.cpp/releases/download/{WHISPER_CPP_TAG}/{asset}")
}

pub fn gpu_bin_dir(data_dir: &Path, pack: &GpuPack) -> PathBuf {
    models_dir(data_dir)
        .parent()
        .unwrap_or(data_dir)
        .join("bin")
        .join(pack.subdir)
}

/// Installed = whisper-server binary present in the pack dir.
pub fn gpu_installed(data_dir: &Path, pack: &GpuPack) -> bool {
    gpu_bin_dir(data_dir, pack).join("whisper-server").exists()
}

/// Detection is advisory only (never auto-selects): NVIDIA via
/// nvidia-smi, Vulkan via vulkaninfo presence.
#[derive(Debug, Clone, Default)]
pub struct GpuDetect {
    pub nvidia_gpu: Option<String>,
    pub vulkan_tools: bool,
}

pub fn detect_gpu() -> GpuDetect {
    let mut out = GpuDetect::default();
    if let Ok(info) = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=name", "--format=csv,noheader"])
        .output()
    {
        if info.status.success() {
            let name = String::from_utf8_lossy(&info.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !name.is_empty() {
                out.nvidia_gpu = Some(name);
            }
        }
    }
    out.vulkan_tools = std::process::Command::new("vulkaninfo")
        .arg("--summary")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    out
}

/// Human recommendation line for the picker. Manual choice always wins.
pub fn gpu_recommendation(det: &GpuDetect) -> &'static str {
    if det.nvidia_gpu.is_some() {
        "Recommended for this machine: CUDA (NVIDIA detected)"
    } else if det.vulkan_tools {
        "Recommended for this machine: Vulkan"
    } else {
        "No GPU runtimes detected: CPU is the safe choice"
    }
}

/// Download + sha256-verify + unzip a GPU pack. Blocking; run on a
/// worker thread. Reuses the download event/cancel machinery.
pub fn download_gpu_pack(
    data_dir: &Path,
    pack: &GpuPack,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
) -> Result<(), String> {
    use sha2::Digest;
    let dir = gpu_bin_dir(data_dir, pack);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let tmp = dir.join(format!(".{}.part", pack.id));
    let _ = std::fs::remove_file(&tmp);

    let client = reqwest::blocking::Client::builder()
        .timeout(None)
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut resp = client
        .get(gpu_url(pack.asset))
        .send()
        .map_err(|e| format!("request: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("http status: {}", resp.status()));
    }
    let total = resp.content_length();
    let file = std::fs::File::create(&tmp).map_err(|e| format!("create: {e}"))?;
    let mut hasher = sha2::Sha256::new();
    let mut writer = HashWriter {
        file,
        hasher: &mut hasher,
    };
    let mut received = 0u64;
    let mut buf = [0u8; 65536];
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = std::fs::remove_file(&tmp);
            return Err("cancelled".to_string());
        }
        let n = resp.read(&mut buf).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&buf[..n])
            .map_err(|e| format!("write: {e}"))?;
        received += n as u64;
        on_event(DownloadEvent::Progress { received, total });
    }
    drop(writer);
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if digest != pack.sha256 {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "sha256 mismatch (got {digest}, expected {})",
            pack.sha256
        ));
    }

    on_event(DownloadEvent::Installing);
    let file = std::fs::File::open(&tmp).map_err(|e| format!("open archive: {e}"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("open zip: {e}"))?;
    zip.extract(&dir).map_err(|e| format!("extract zip: {e}"))?;
    let _ = std::fs::remove_file(&tmp);
    // The archives ship the binary under its release filename
    // (`whisper-server-linux-x64-cuda`, …): normalize to
    // `whisper-server` so runners have one stable path.
    if !dir.join("whisper-server").exists() {
        let candidates: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|e| format!("read bin dir: {e}"))?
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("whisper-server")
            })
            .collect();
        if candidates.len() == 1 {
            std::fs::rename(candidates[0].path(), dir.join("whisper-server"))
                .map_err(|e| format!("normalize binary: {e}"))?;
        }
    }
    // Server binary must be executable after unzip.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("whisper-server");
        if bin.exists() {
            let mut perms = std::fs::metadata(&bin)
                .map_err(|e| format!("stat binary: {e}"))?
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin, perms).map_err(|e| format!("chmod: {e}"))?;
        }
    }
    if !gpu_installed(data_dir, pack) {
        return Err("whisper-server missing after extract".to_string());
    }
    on_event(DownloadEvent::Done);
    Ok(())
}

struct HashWriter<'a> {
    file: std::fs::File,
    hasher: &'a mut sha2::Sha256,
}

impl Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest;
        self.hasher.update(buf);
        self.file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

pub fn delete_gpu_pack(data_dir: &Path, pack: &GpuPack) -> std::io::Result<()> {
    let dir = gpu_bin_dir(data_dir, pack);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

/// Start a GPU pack download on a worker thread. No-op if running.
/// Shares the download state map with model downloads.
pub fn start_gpu_download(data_dir: PathBuf, id: &'static str) {
    {
        let map = downloads().lock().unwrap();
        if let Some((snap, _)) = map.get(id) {
            if matches!(snap.phase, DlPhase::Downloading | DlPhase::Installing) {
                return;
            }
        }
    }
    let Some(pack) = GPU_PACKS.iter().find(|p| p.id == id) else {
        return;
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_worker = cancel.clone();
    downloads().lock().unwrap().insert(
        id.to_string(),
        (
            DlSnapshot {
                received: 0,
                total: Some(pack.size_bytes),
                phase: DlPhase::Downloading,
            },
            cancel,
        ),
    );
    std::thread::spawn(move || {
        let cb: DownloadCallback = Arc::new(move |ev| {
            if let Ok(mut map) = downloads().lock() {
                if let Some((snap, _)) = map.get_mut(id) {
                    match ev {
                        DownloadEvent::Progress { received, total } => {
                            snap.received = received;
                            snap.total = total;
                            snap.phase = DlPhase::Downloading;
                        }
                        DownloadEvent::Installing => snap.phase = DlPhase::Installing,
                        DownloadEvent::Done => {}
                    }
                }
            }
        });
        let result = download_gpu_pack(&data_dir, pack, &cancel_worker, &cb);
        if let Ok(mut map) = downloads().lock() {
            match result {
                Ok(()) => {
                    map.remove(id);
                }
                Err(e) => {
                    if let Some((snap, _)) = map.get_mut(id) {
                        snap.phase = DlPhase::Failed(e);
                    }
                }
            }
        }
    });
}

/// Cancel an in-flight GPU download.
pub fn cancel_gpu_download(id: &str) {
    cancel_download(id);
}

// ---------------------------------------------------------------- sherpa runtimes

/// sherpa-onnx server runtimes for parakeet/orukeet/cohere models
/// (whisper models use the whisper-server packs above instead).
/// Same v1.13.8 upstream OpenWhispr fetches.
pub const SHERPA_TAG: &str = "v1.13.8";

#[derive(Debug, Clone)]
pub struct SherpaPack {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub size_label: &'static str,
    pub size_bytes: u64,
    pub asset: &'static str,
    pub sha256: &'static str,
    pub subdir: &'static str,
    /// Binaries that must exist after install (relative to subdir).
    pub binaries: &'static [&'static str],
}

pub const SHERPA_PACKS: &[SherpaPack] = &[SherpaPack {
    id: "sherpa-cpu",
    name: "Sherpa CPU server",
    description: "Offline + online websocket servers, onnxruntime included",
    size_label: "25MB",
    size_bytes: 24_802_494,
    asset: "sherpa-onnx-v1.13.8-linux-x64-shared-no-tts.tar.bz2",
    sha256: "d0f96c8b65c6cd0974fada22737e337de81bc8cd2abbec2e39caf358b1eec5fc",
    subdir: "sherpa-cpu",
    binaries: &[
        "bin/sherpa-onnx-offline-websocket-server",
        "bin/sherpa-onnx-online-websocket-server",
        "lib/libonnxruntime.so",
    ],
}];

fn sherpa_url(asset: &str) -> String {
    format!("https://github.com/k2-fsa/sherpa-onnx/releases/download/{SHERPA_TAG}/{asset}")
}

pub fn sherpa_bin_dir(data_dir: &Path, pack: &SherpaPack) -> PathBuf {
    models_dir(data_dir)
        .parent()
        .unwrap_or(data_dir)
        .join("bin")
        .join(pack.subdir)
}

/// Installed = all required binaries present.
pub fn sherpa_installed(data_dir: &Path, pack: &SherpaPack) -> bool {
    let dir = sherpa_bin_dir(data_dir, pack);
    pack.binaries.iter().all(|b| dir.join(b).exists())
}

/// Download + sha256-verify + extract a sherpa pack. Blocking; run on
/// a worker thread. Archives nest everything under one top dir, which
/// is flattened like parakeet archives.
pub fn download_sherpa_pack(
    data_dir: &Path,
    pack: &SherpaPack,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
) -> Result<(), String> {
    use sha2::Digest;
    let dir = sherpa_bin_dir(data_dir, pack);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let tmp = dir.join(format!(".{}.part", pack.id));
    let _ = std::fs::remove_file(&tmp);

    let client = reqwest::blocking::Client::builder()
        .timeout(None)
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut resp = client
        .get(sherpa_url(pack.asset))
        .send()
        .map_err(|e| format!("request: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("http status: {}", resp.status()));
    }
    let total = resp.content_length();
    let file = std::fs::File::create(&tmp).map_err(|e| format!("create: {e}"))?;
    let mut hasher = sha2::Sha256::new();
    let mut writer = HashWriter {
        file,
        hasher: &mut hasher,
    };
    let mut received = 0u64;
    let mut buf = [0u8; 65536];
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = std::fs::remove_file(&tmp);
            return Err("cancelled".to_string());
        }
        let n = resp.read(&mut buf).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&buf[..n])
            .map_err(|e| format!("write: {e}"))?;
        received += n as u64;
        on_event(DownloadEvent::Progress { received, total });
    }
    drop(writer);
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if digest != pack.sha256 {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "sha256 mismatch (got {digest}, expected {})",
            pack.sha256
        ));
    }

    on_event(DownloadEvent::Installing);
    extract_tar_bz2(&tmp, &dir).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e
    })?;
    let _ = std::fs::remove_file(&tmp);
    // Flatten the single top dir when present.
    if let Ok(mut entries) = std::fs::read_dir(&dir) {
        let tops: Vec<_> = entries
            .by_ref()
            .flatten()
            .filter(|e| e.path().is_dir())
            .collect();
        if tops.len() == 1 {
            let _ = flatten_dir(&tops[0].path(), &dir);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for b in pack.binaries {
            if b.ends_with(".so") {
                continue;
            }
            let bin = dir.join(b);
            if bin.exists() {
                if let Ok(meta) = std::fs::metadata(&bin) {
                    let mut perms = meta.permissions();
                    perms.set_mode(0o755);
                    let _ = std::fs::set_permissions(&bin, perms);
                }
            }
        }
    }
    let missing: Vec<_> = pack
        .binaries
        .iter()
        .filter(|b| !dir.join(b).exists())
        .collect();
    if !missing.is_empty() {
        return Err(format!("missing after extract: {missing:?}"));
    }
    on_event(DownloadEvent::Done);
    Ok(())
}

pub fn delete_sherpa_pack(data_dir: &Path, pack: &SherpaPack) -> std::io::Result<()> {
    let dir = sherpa_bin_dir(data_dir, pack);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

/// Start a sherpa pack download on a worker thread. No-op if running.
pub fn start_sherpa_download(data_dir: PathBuf, id: &'static str) {
    {
        let map = downloads().lock().unwrap();
        if let Some((snap, _)) = map.get(id) {
            if matches!(snap.phase, DlPhase::Downloading | DlPhase::Installing) {
                return;
            }
        }
    }
    let Some(pack) = SHERPA_PACKS.iter().find(|p| p.id == id) else {
        return;
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_worker = cancel.clone();
    downloads().lock().unwrap().insert(
        id.to_string(),
        (
            DlSnapshot {
                received: 0,
                total: Some(pack.size_bytes),
                phase: DlPhase::Downloading,
            },
            cancel,
        ),
    );
    std::thread::spawn(move || {
        let cb: DownloadCallback = Arc::new(move |ev| {
            if let Ok(mut map) = downloads().lock() {
                if let Some((snap, _)) = map.get_mut(id) {
                    match ev {
                        DownloadEvent::Progress { received, total } => {
                            snap.received = received;
                            snap.total = total;
                            snap.phase = DlPhase::Downloading;
                        }
                        DownloadEvent::Installing => snap.phase = DlPhase::Installing,
                        DownloadEvent::Done => {}
                    }
                }
            }
        });
        let result = download_sherpa_pack(&data_dir, pack, &cancel_worker, &cb);
        if let Ok(mut map) = downloads().lock() {
            match result {
                Ok(()) => {
                    map.remove(id);
                }
                Err(e) => {
                    if let Some((snap, _)) = map.get_mut(id) {
                        snap.phase = DlPhase::Failed(e);
                    }
                }
            }
        }
    });
}
