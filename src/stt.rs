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
    Whistle,
    Qwen,
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

/// Cactus Whistle (own engine, CLI-driven): 16.9 MB, 7 languages
/// with auto-detect, up to 30 s per pass. Model URL unconfirmed
/// (natural HF path from the release post — TO-VERIFY on first
/// download; the engine resolves `models/whistle/whistle.cact`
/// whatever the source, so manual placement always works).
pub const WHISTLE_LANGS: &[&str] = &["en", "de", "fr", "es", "it", "nl", "pl"];

pub const WHISTLE_MODELS: &[ModelEntry] = &[ModelEntry {
    id: "whistle-v1",
    kind: ModelKind::Whistle,
    name: "Whistle v1",
    description: "16.9 MB STT, 7 languages, own CPU engine",
    size_label: "17MB",
    size_mb: 17,
    expected_bytes: Some(16_919_407),
    download_url: "https://huggingface.co/Cactus-Compute/whistle/resolve/main/whistle.cact",
    extract_dir: None,
    required_files: &["whistle.cact"],
    runtime: None,
    recommended: true,
}];

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

/// Qwen3-ASR files (flat HuggingFace snapshot, safetensors BF16).
/// 1.7B ships two weight shards + index; 0.6B a single shard.
/// `download_url` is the repo `.../resolve/main` base; every entry
/// of `required_files` is fetched from it (see `download_model`).
const QWEN_1_7B_FILES: &[&str] = &[
    "config.json",
    "generation_config.json",
    "preprocessor_config.json",
    "tokenizer_config.json",
    "vocab.json",
    "merges.txt",
    "chat_template.json",
    "model.safetensors.index.json",
    "model-00001-of-00002.safetensors",
    "model-00002-of-00002.safetensors",
];

const QWEN_0_6B_FILES: &[&str] = &[
    "config.json",
    "generation_config.json",
    "preprocessor_config.json",
    "tokenizer_config.json",
    "vocab.json",
    "merges.txt",
    "chat_template.json",
    "model.safetensors",
];

pub const QWEN_MODELS: &[ModelEntry] = &[
    ModelEntry {
        id: "qwen3-asr-1.7b",
        kind: ModelKind::Qwen,
        name: "Qwen3-ASR 1.7B",
        description: "LLM-based multilingual ASR, 30 languages + dialects",
        size_label: "4.7GB",
        size_mb: 4700,
        expected_bytes: Some(4_699_896_190),
        download_url: "https://huggingface.co/Qwen/Qwen3-ASR-1.7B/resolve/main",
        extract_dir: None,
        required_files: QWEN_1_7B_FILES,
        runtime: None,
        recommended: true,
    },
    ModelEntry {
        id: "qwen3-asr-0.6b",
        kind: ModelKind::Qwen,
        name: "Qwen3-ASR 0.6B",
        description: "Lightweight LLM-based ASR, 30 languages + dialects",
        size_label: "1.9GB",
        size_mb: 1900,
        expected_bytes: Some(1_876_091_704),
        download_url: "https://huggingface.co/Qwen/Qwen3-ASR-0.6B/resolve/main",
        extract_dir: None,
        required_files: QWEN_0_6B_FILES,
        runtime: None,
        recommended: false,
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

/// Qwen3-ASR's 30 supported languages (model card; dialects and
/// accents follow the selected language).
const QWEN_30: &[&str] = &[
    "zh", "en", "yue", "ar", "de", "fr", "es", "pt", "id", "it", "ko", "ru", "th", "vi", "ja",
    "tr", "hi", "ms", "nl", "sv", "da", "fi", "pl", "cs", "fil", "fa", "el", "hu", "mk", "ro",
];

/// Supported transcription languages for one registry model.
/// Whisper reads the full upstream list; sherpa models carry their
/// own list (single-language models return just that one).
pub fn model_languages(model_id: &str) -> &'static [&'static str] {
    match model_id {
        "parakeet-tdt-0.6b-v3" | "orukeet-v0.1.0" => PARAKEET_25,
        "whistle-v1" => WHISTLE_LANGS,
        "qwen3-asr-1.7b" | "qwen3-asr-0.6b" => QWEN_30,
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
        V::Whistle => WHISTLE_MODELS.iter().collect(),
        V::OpenAi => WHISPER_MODELS.iter().collect(),
        V::Nvidia => PARAKEET_MODELS
            .iter()
            .filter(|m| m.id != "cohere-transcribe-03-2026" && m.id != "orukeet-v0.1.0")
            .collect(),
        V::Cohere => PARAKEET_MODELS
            .iter()
            .filter(|m| m.id == "cohere-transcribe-03-2026")
            .collect(),
        V::Qwen => QWEN_MODELS.iter().collect(),
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
            .chain(super::WHISTLE_MODELS.iter())
            .chain(super::QWEN_MODELS.iter())
        {
            let langs = match m.kind {
                super::ModelKind::Whisper => super::WHISPER_LANGS,
                super::ModelKind::Whistle => super::WHISTLE_LANGS,
                _ => super::model_languages(m.id),
            };
            assert!(!langs.is_empty(), "{}", m.id);
        }
    }
}

/// Capability matrix: (live streaming, dictionary bias).
/// Whisper prompts natively; sherpa transducers take hotwords files;
/// whistle takes keyword files; orukeet/cohere expose neither path;
/// Qwen takes `context` biasing, and streams live on the vLLM
/// runtime (the transformers runtime decodes after release).
/// The matrix advertises per-model capability; the runtime picker
/// reports the backend truth (transformers = offline only).
pub fn capabilities(entry: &ModelEntry) -> (bool, bool) {
    match entry.kind {
        ModelKind::Whisper => (false, true),
        ModelKind::Whistle => (true, true),
        ModelKind::Qwen => (true, true),
        ModelKind::Parakeet => match entry.id {
            "orukeet-v0.1.0" => (false, false),
            _ if entry.runtime == Some(ParakeetRuntime::Online) => (true, true),
            _ => (false, true),
        },
    }
}

pub fn find_model(id: &str) -> Option<&'static ModelEntry> {
    WHISPER_MODELS
        .iter()
        .chain(PARAKEET_MODELS.iter())
        .chain(WHISTLE_MODELS.iter())
        .chain(QWEN_MODELS.iter())
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
        ModelKind::Whistle => "whistle",
        ModelKind::Qwen => "qwen",
    };
    models_dir(data_dir).join(sub).join(entry.id)
}

/// Whisper: the .bin exists and (when known) matches expected bytes.
/// Parakeet: all required files exist under the model dir.
/// Qwen: all snapshot files exist and together match expected bytes.
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
        ModelKind::Qwen => {
            if !entry.required_files.iter().all(|f| dir.join(f).exists()) {
                return false;
            }
            match entry.expected_bytes {
                Some(n) => {
                    let total: u64 = entry
                        .required_files
                        .iter()
                        .map(|f| std::fs::metadata(dir.join(f)).map(|m| m.len()).unwrap_or(0))
                        .sum();
                    total >= (n * 95) / 100
                }
                None => true,
            }
        }
        ModelKind::Parakeet | ModelKind::Whistle => {
            entry.required_files.iter().all(|f| dir.join(f).exists())
        }
    }
}

pub fn whisper_file_stem(id: &str) -> &str {
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

/// Recursive byte size of owned staging/runtime trees (symlinks not
/// followed, errors ignored). Feeds the install progress meter.
pub fn dir_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(md) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if md.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&p) {
                for e in entries.flatten() {
                    stack.push(e.path());
                }
            }
        } else if md.is_file() {
            total = total.saturating_add(md.len());
        }
    }
    total
}

/// Download + install one registry model. Blocking; run on a worker
/// thread. `cancel` is polled between chunks. Partial files are
/// removed on cancel/failure so the next attempt starts clean.
pub fn download_model(
    data_dir: &Path,
    entry: &ModelEntry,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
) -> Result<(), String> {
    if entry.kind == ModelKind::Qwen {
        return download_qwen_snapshot(data_dir, entry, cancel, on_event);
    }
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

    if let Some(expected) = entry.expected_bytes
        && received < (expected * 95) / 100
    {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "size mismatch: got {received} bytes, expected ~{expected}"
        ));
    }

    on_event(DownloadEvent::Installing);
    match entry.kind {
        ModelKind::Whisper => {
            let stem = whisper_file_stem(entry.id);
            let dest = dir.join(format!("ggml-{stem}.bin"));
            std::fs::rename(&tmp, &dest).map_err(|e| format!("install: {e}"))?;
        }
        ModelKind::Whistle => {
            // Single-file model: move into place, then verify.
            let dest = dir.join("whistle.cact");
            std::fs::rename(&tmp, &dest).map_err(|e| format!("install: {e}"))?;
            if !dest.exists() || std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0) == 0 {
                return Err("whistle.cact missing or empty after download".to_string());
            }
        }
        ModelKind::Parakeet => {
            extract_tar_bz2(&tmp, &dir).inspect_err(|_e| {
                let _ = std::fs::remove_file(&tmp);
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
        // Qwen snapshots return before this match (multi-file path
        // above); the arm exists so the match stays exhaustive.
        ModelKind::Qwen => {
            return Err("qwen uses the snapshot downloader".to_string());
        }
    }
    on_event(DownloadEvent::Done);
    Ok(())
}

/// Qwen snapshot download: every `required_files` entry is fetched
/// from the repo `.../resolve/main` base straight into the model
/// dir (flat layout, no extraction). Blocking; run on a worker
/// thread. Partial `.part` files are removed on cancel/failure so
/// the next attempt starts that file clean. Integrity = every file
/// present + snapshot byte total within 5%.
fn download_qwen_snapshot(
    data_dir: &Path,
    entry: &ModelEntry,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
) -> Result<(), String> {
    let dir = model_dir(data_dir, entry);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let client = reqwest::blocking::Client::builder()
        .timeout(None)
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut received = 0u64;
    for file in entry.required_files {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".to_string());
        }
        let dest = dir.join(file);
        let tmp = dir.join(format!(".{}.{file}.part", entry.id));
        let _ = std::fs::remove_file(&tmp);
        let mut out = std::fs::File::create(&tmp).map_err(|e| format!("create: {e}"))?;
        let url = format!("{}/{file}", entry.download_url);
        let mut resp = client
            .get(&url)
            .send()
            .map_err(|e| format!("request {file}: {e}"))?;
        if !resp.status().is_success() {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("http status for {file}: {}", resp.status()));
        }
        let total = entry.expected_bytes.filter(|n| *n > 0);
        let mut buf = [0u8; 65536];
        loop {
            if cancel.load(Ordering::SeqCst) {
                let _ = std::fs::remove_file(&tmp);
                return Err("cancelled".to_string());
            }
            let n = resp
                .read(&mut buf)
                .map_err(|e| format!("read {file}: {e}"))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])
                .map_err(|e| format!("write {file}: {e}"))?;
            received += n as u64;
            on_event(DownloadEvent::Progress { received, total });
        }
        drop(out);
        if dest.exists() {
            let _ = std::fs::remove_file(&dest);
        }
        std::fs::rename(&tmp, &dest).map_err(|e| format!("install {file}: {e}"))?;
    }
    if let Some(expected) = entry.expected_bytes
        && received < (expected * 95) / 100
    {
        return Err(format!(
            "size mismatch: got {received} bytes, expected ~{expected}"
        ));
    }
    let missing: Vec<_> = entry
        .required_files
        .iter()
        .filter(|f| !dir.join(f).exists())
        .collect();
    if !missing.is_empty() {
        return Err(format!("missing after download: {missing:?}"));
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

type DownloadMap =
    std::sync::Mutex<std::collections::HashMap<String, (DlSnapshot, Arc<AtomicBool>)>>;
static DOWNLOADS: std::sync::OnceLock<DownloadMap> = std::sync::OnceLock::new();

fn downloads() -> &'static DownloadMap {
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
        if let Some((snap, _)) = map.get(id)
            && matches!(snap.phase, DlPhase::Downloading | DlPhase::Installing)
        {
            return;
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
            if let Ok(mut map) = downloads().lock()
                && let Some((snap, _)) = map.get_mut(id)
            {
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
    if let Ok(map) = downloads().lock()
        && let Some((_, cancel)) = map.get(id)
    {
        cancel.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_matrix() {
        let w = find_model("tiny").unwrap();
        assert_eq!(capabilities(w), (false, true));
        let o = find_model("orukeet-v0.1.0").unwrap();
        assert_eq!(capabilities(o), (false, false));
        let t = find_model("parakeet-tdt-0.6b-v3").unwrap();
        assert_eq!(capabilities(t), (false, true));
        let n = find_model("nemotron-speech-streaming-en-0.6b").unwrap();
        assert_eq!(capabilities(n), (true, true));
        let wh = find_model("whistle-v1").unwrap();
        assert_eq!(capabilities(wh), (true, true));
        // Qwen advertises streaming + dictionary (backend picker
        // reports the per-backend truth: transformers offline only).
        let q = find_model("qwen3-asr-1.7b").unwrap();
        assert_eq!(capabilities(q), (true, true));
        let q6 = find_model("qwen3-asr-0.6b").unwrap();
        assert_eq!(capabilities(q6), (true, true));
    }

    #[test]
    fn qwen_registry_is_pinned() {
        assert_eq!(QWEN_MODELS.len(), 2);
        for m in QWEN_MODELS.iter() {
            assert!(m.download_url.starts_with("https://"), "{}", m.id);
            assert!(!m.required_files.is_empty(), "{}", m.id);
            assert!(model_languages(m.id).contains(&"en"), "{}", m.id);
        }
        assert_eq!(model_languages("qwen3-asr-1.7b").len(), 30);
        assert!(find_model("qwen3-asr-1.7b").is_some());
        assert!(find_model("qwen3-asr-0.6b").is_some());
    }

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

    /// zip 9 hardening regression: the production `ZipArchive::extract`
    /// path must still install a normal archive and must not let a
    /// `../` entry escape the destination (RUSTSEC-2026-0368/0369).
    #[test]
    fn zip_extract_installs_layout_without_escape() {
        use std::io::Write;
        let root = std::env::temp_dir().join(format!("mywhisper-zip-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        // Normal archive: same two calls as `download_gpu_pack`.
        let archive_path = root.join("pack.zip");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file(
                "whisper-server-test",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(b"binary").unwrap();
            zip.finish().unwrap();
        }
        let dest = root.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        {
            let file = std::fs::File::open(&archive_path).unwrap();
            let mut zip = zip::ZipArchive::new(file).unwrap();
            zip.extract(&dest).unwrap();
        }
        assert_eq!(
            std::fs::read(dest.join("whisper-server-test")).unwrap(),
            b"binary"
        );

        // Malicious archives: `../` and absolute entries must not escape.
        for name in ["../evil.txt", "/tmp/mywhisper-zip-escape-probe.txt"] {
            let evil_archive = root.join(format!("evil-{}.zip", name.len()));
            {
                let file = std::fs::File::create(&evil_archive).unwrap();
                let mut zip = zip::ZipWriter::new(file);
                zip.start_file(name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                zip.write_all(b"evil").unwrap();
                zip.finish().unwrap();
            }
            let evil_dest = root.join("evil-out");
            std::fs::create_dir_all(&evil_dest).unwrap();
            {
                let file = std::fs::File::open(&evil_archive).unwrap();
                let mut zip = zip::ZipArchive::new(file).unwrap();
                let _ = zip.extract(&evil_dest);
            }
        }
        assert!(
            !root.join("evil.txt").exists(),
            "zip extract must not escape destination"
        );
        assert!(
            !std::path::Path::new("/tmp/mywhisper-zip-escape-probe.txt").exists(),
            "zip extract must not write absolute paths"
        );
        let _ = std::fs::remove_dir_all(&root);
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
        && info.status.success()
    {
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
        if let Some((snap, _)) = map.get(id)
            && matches!(snap.phase, DlPhase::Downloading | DlPhase::Installing)
        {
            return;
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
            if let Ok(mut map) = downloads().lock()
                && let Some((snap, _)) = map.get_mut(id)
            {
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

pub const SHERPA_PACKS: &[SherpaPack] = &[
    SherpaPack {
        id: "sherpa-cuda",
        name: "Sherpa CUDA server",
        description: "Offline + online websocket servers, CUDA 13 (needs system CUDA libs)",
        size_label: "255MB",
        size_bytes: 254_858_007,
        asset: "sherpa-onnx-v1.13.8-cuda-13.x-cudnn-9.x-onnxruntime1.28.2-linux-x64-gpu.tar.bz2",
        sha256: "7eddaee14a42d143ec995ed68e9ec3623dd6ca9d22076139d4c0879c3121c819",
        subdir: "sherpa-cuda",
        binaries: &[
            "bin/sherpa-onnx-offline-websocket-server",
            "bin/sherpa-onnx-online-websocket-server",
            "lib/libonnxruntime.so",
        ],
    },
    SherpaPack {
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
    },
];

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
    extract_tar_bz2(&tmp, &dir).inspect_err(|_e| {
        let _ = std::fs::remove_file(&tmp);
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
            if bin.exists()
                && let Ok(meta) = std::fs::metadata(&bin)
            {
                let mut perms = meta.permissions();
                perms.set_mode(0o755);
                let _ = std::fs::set_permissions(&bin, perms);
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

// ---------------------------------------------------------------- needle engine
//
// Whistle's `needle` binary: one file per arch, no archive. Hashes
// are the upstream Xet ETags (verified 2026-10-07).

pub const NEEDLE_ENGINE_ID: &str = "needle-engine";

struct NeedleAsset {
    folder: &'static str,
    sha256: &'static str,
    size_bytes: u64,
}

fn needle_asset() -> Result<NeedleAsset, String> {
    match std::env::consts::ARCH {
        "x86_64" => Ok(NeedleAsset {
            folder: "linux-x86_64",
            sha256: "f38dc4b0345d66b4e385734ad0f12af43ac6e2cfa1752d0795af5c137200c8e4",
            size_bytes: 1_541_680,
        }),
        "aarch64" => Ok(NeedleAsset {
            folder: "linux-arm64",
            sha256: "6fc25a97def475e1c7d1933a4a219d4e37076be77974e76dd331d9e0cd97f3cb",
            size_bytes: 1_431_816,
        }),
        other => Err(format!("needle engine has no build for {other}")),
    }
}

fn needle_url(folder: &str) -> String {
    format!("https://huggingface.co/Cactus-Compute/needle3/resolve/main/{folder}/needle")
}

pub fn needle_bin_dir(data_dir: &Path) -> PathBuf {
    models_dir(data_dir)
        .parent()
        .unwrap_or(data_dir)
        .join("bin")
        .join("needle")
}

/// Installed = engine binary present (executable bit set on install).
pub fn needle_installed(data_dir: &Path) -> bool {
    needle_bin_dir(data_dir).join("needle").exists()
}

/// Download + sha256-verify a needle engine binary. Blocking; run on
/// a worker thread. Single file, no extraction; chmod +x on install.
pub fn download_needle_pack(
    data_dir: &Path,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
) -> Result<(), String> {
    use sha2::Digest;
    let asset = needle_asset()?;
    let dir = needle_bin_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let tmp = dir.join(".needle.part");
    let _ = std::fs::remove_file(&tmp);

    let client = reqwest::blocking::Client::builder()
        .timeout(None)
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut resp = client
        .get(needle_url(asset.folder))
        .send()
        .map_err(|e| format!("request: {e}"))?;
    if !resp.status().is_success() {
        let _ = std::fs::remove_file(&tmp);
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
    if digest != asset.sha256 {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "sha256 mismatch (got {digest}, expected {})",
            asset.sha256
        ));
    }

    on_event(DownloadEvent::Installing);
    let dest = dir.join("needle");
    std::fs::rename(&tmp, &dest).map_err(|e| format!("install: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod: {e}"))?;
    }
    on_event(DownloadEvent::Done);
    Ok(())
}

pub fn delete_needle_pack(data_dir: &Path) -> std::io::Result<()> {
    let dir = needle_bin_dir(data_dir);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

/// Start a needle engine download on the task runtime. No-op if
/// running or when this arch has no build.
pub fn start_needle_download(data_dir: PathBuf) {
    {
        let map = downloads().lock().unwrap();
        if let Some((snap, _)) = map.get(NEEDLE_ENGINE_ID)
            && matches!(snap.phase, DlPhase::Downloading | DlPhase::Installing)
        {
            return;
        }
    }
    let size = needle_asset().map(|a| a.size_bytes).unwrap_or(0);
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_worker = cancel.clone();
    downloads().lock().unwrap().insert(
        NEEDLE_ENGINE_ID.to_string(),
        (
            DlSnapshot {
                received: 0,
                total: Some(size),
                phase: DlPhase::Downloading,
            },
            cancel,
        ),
    );
    let id = NEEDLE_ENGINE_ID;
    if crate::runtime::submit("dl-needle", move || {
        let cb: DownloadCallback = Arc::new(move |ev| {
            if let Ok(mut map) = downloads().lock()
                && let Some((snap, _)) = map.get_mut(id)
            {
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
        });
        let result = download_needle_pack(&data_dir, &cancel_worker, &cb);
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
    })
    .is_err()
        && let Ok(mut map) = downloads().lock()
        && let Some((snap, _)) = map.get_mut(id)
    {
        snap.phase = DlPhase::Failed("task runtime saturated".to_string());
    }
}

/// Start a sherpa pack download on a worker thread. No-op if running.
pub fn start_sherpa_download(data_dir: PathBuf, id: &'static str) {
    {
        let map = downloads().lock().unwrap();
        if let Some((snap, _)) = map.get(id)
            && matches!(snap.phase, DlPhase::Downloading | DlPhase::Installing)
        {
            return;
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
            if let Ok(mut map) = downloads().lock()
                && let Some((snap, _)) = map.get_mut(id)
            {
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

// ---------------------------------------------------------------- Qwen runtime
//
// Qwen3-ASR runs on the upstream `qwen-asr` Python package (torch),
// not a native binary: a "runtime pack" here is a managed Python
// 3.12 environment under `<data>/bin/<subdir>/` (private interpreter
// + `venv` + locked pip specs) plus the sidecar server script
// (embedded in `crate::transcribe`, written on server ensure). Two
// packs mirror the two `qwen-asr` backends: transformers (CPU or
// CUDA, offline decode) and vLLM (NVIDIA GPU, offline + live
// streaming).
//
// Provisioning is self-contained: a private pinned `uv` binary
// fetches a pinned portable CPython 3.12 into owned staging
// storage — system `python3`, `pip`, `python3-venv`, user
// environments and shell configuration are never consulted, and
// nothing is added to the global `PATH`. See `crate::qwen_runtime`
// for pins, the lock audit, staging/activation and cancellation.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QwenRuntime {
    Transformers,
    Vllm,
}

#[derive(Debug, Clone)]
pub struct QwenPack {
    pub id: &'static str,
    pub backend: QwenRuntime,
    pub name: &'static str,
    pub description: &'static str,
    pub size_label: &'static str,
    pub size_bytes: u64,
    pub subdir: &'static str,
}

impl QwenPack {
    /// Locked pip specs for this backend (see `crate::qwen_runtime`).
    pub fn pip_specs(&self) -> &'static [&'static str] {
        match self.backend {
            QwenRuntime::Transformers => crate::qwen_runtime::locked_specs_transformers(),
            QwenRuntime::Vllm => crate::qwen_runtime::locked_specs_vllm(),
        }
    }

    pub fn import_checks(&self) -> &'static [&'static str] {
        match self.backend {
            QwenRuntime::Transformers => crate::qwen_runtime::import_checks_transformers(),
            QwenRuntime::Vllm => crate::qwen_runtime::import_checks_vllm(),
        }
    }
}

pub const QWEN_PACKS: &[QwenPack] = &[
    QwenPack {
        id: "qwen-transformers",
        backend: QwenRuntime::Transformers,
        name: "Qwen transformers runtime",
        description: "CPU or NVIDIA GPU, offline decode (no live streaming); self-contained Python 3.12",
        size_label: "~4GB",
        size_bytes: 4_000_000_000,
        subdir: "qwen-transformers",
    },
    QwenPack {
        id: "qwen-vllm",
        backend: QwenRuntime::Vllm,
        name: "Qwen vLLM runtime",
        description: "NVIDIA GPU only, offline + live streaming; self-contained Python 3.12",
        size_label: "~9GB",
        size_bytes: 9_000_000_000,
        subdir: "qwen-vllm",
    },
];

pub fn qwen_runtime_dir(data_dir: &Path, pack: &QwenPack) -> PathBuf {
    crate::qwen_runtime::runtime_dir(data_dir, pack.subdir)
}

pub fn qwen_venv_python(data_dir: &Path, pack: &QwenPack) -> PathBuf {
    crate::qwen_runtime::venv_python(&qwen_runtime_dir(data_dir, pack))
}

/// Installed = venv provisioned by the managed installer AND
/// verified (v2 marker written after post-activation import checks
/// at the final path) AND self-contained (the venv interpreter
/// resolves inside the runtime dir — a venv left pointing at deleted
/// staging is broken, never trusted). No per-poll python spawn: the
/// marker pins the backend spec set + interpreter, so a spec bump or
/// an old system-python (e.g. 3.14) venv reinstalls instead of being
/// trusted. Incomplete installs never write the marker.
pub fn qwen_installed(data_dir: &Path, pack: &QwenPack) -> bool {
    let dir = qwen_runtime_dir(data_dir, pack);
    if !qwen_venv_python(data_dir, pack).exists() {
        return false;
    }
    if !crate::qwen_runtime::venv_python_self_contained(&dir) {
        return false;
    }
    match std::fs::read_to_string(dir.join(".installed")) {
        Ok(mark) => crate::qwen_runtime::marker_valid(&mark, pack.subdir, pack.pip_specs()),
        Err(_) => false,
    }
}

/// Approximate unpack meter scoped to exactly the dirs being
/// written. Returns the stop flag + join handle; the caller must set
/// the flag and join (stopping the thread) once the child exits.
fn spawn_meter(
    dirs: Vec<PathBuf>,
    total: u64,
    on_event: &DownloadCallback,
) -> (Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    let cb = on_event.clone();
    let handle = std::thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let mut sum = 0u64;
            for dir in &dirs {
                sum = sum.saturating_add(dir_bytes(dir));
            }
            cb(DownloadEvent::Progress {
                received: sum.min(total),
                total: Some(total),
            });
            if flag.load(Ordering::SeqCst) {
                break;
            }
        }
    });
    (done, handle)
}

/// Download resolved wheels with exact byte progress (sizes via
/// HEAD; unknown sizes still download, the bar clamps at 100%),
/// then install offline from the local wheel dir. Unpack progress
/// rides a meter scoped to the venv tree against the pack estimate.
#[allow(clippy::too_many_arguments)]
fn install_wheels_offline(
    pip: &Path,
    refs: &[crate::qwen_runtime::WheelRef],
    wheels: &Path,
    runtime: &Path,
    pack: &QwenPack,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
    log_path: &Path,
) -> Result<(), String> {
    use crate::qwen_runtime as qr;
    let sizes = qr::wheel_sizes(refs, cancel)?;
    let total: u64 = sizes.iter().flatten().sum::<u64>().max(1);
    let mut base = 0u64;
    for (w, _size) in refs.iter().zip(sizes.iter()) {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".to_string());
        }
        eprintln!("[qwen] {}: fetching {}=={}", pack.id, w.name, w.version);
        let dest = wheels.join(qr::filename_of_url(&w.url));
        // Transient network blips must not fail a multi-GB fetch:
        // retry the single GET (pip itself retries 5x). Cancel still
        // aborts between attempts; the last error surfaces.
        let mut attempt = 0;
        loop {
            attempt += 1;
            let cb = |rec: u64, _tot: Option<u64>| {
                on_event(DownloadEvent::Progress {
                    received: base.saturating_add(rec),
                    total: Some(total),
                });
            };
            match qr::download_verified(&w.url, &dest, &w.sha256, cancel, &cb) {
                Ok(()) => break,
                Err(e) if qr::is_cancel_error(&e) || attempt >= 3 => {
                    if attempt >= 3 && !qr::is_cancel_error(&e) {
                        eprintln!(
                            "[qwen] {}: {}=={} failed after 3 attempts",
                            pack.id, w.name, w.version
                        );
                    }
                    return Err(e);
                }
                Err(e) => {
                    eprintln!(
                        "[qwen] {}: {}=={} attempt {attempt}/3 failed ({e}); retrying",
                        pack.id, w.name, w.version
                    );
                    std::thread::sleep(std::time::Duration::from_secs(2 * attempt as u64));
                }
            }
        }
        base = base.saturating_add(std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0));
    }
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled".to_string());
    }
    let wheels_s = wheels.to_string_lossy().to_string();
    let (done, meter) = spawn_meter(vec![runtime.join("venv")], pack.size_bytes, on_event);
    let mut cmd = std::process::Command::new(pip);
    cmd.args([
        "install",
        "--no-index",
        "--find-links",
        &wheels_s,
        "--prefer-binary",
        "--disable-pip-version-check",
    ])
    .args(pack.pip_specs())
    .env("PIP_NO_CACHE_DIR", "1")
    .env_remove("PYTHONHOME")
    .env_remove("PYTHONPATH")
    .env_remove("VIRTUAL_ENV");
    let r = qr::run_cancellable(&mut cmd, cancel, log_path, "pip install (offline)");
    done.store(true, Ordering::SeqCst);
    let _ = meter.join();
    r
}

/// Fallback when wheels cannot be prefetched: direct install with a
/// meter scoped to exactly the dirs pip writes (its cache + the venv
/// tree) against the pack estimate.
fn install_direct_with_meter(
    pip: &Path,
    pack: &QwenPack,
    staging: &Path,
    runtime: &Path,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
    log_path: &Path,
) -> Result<(), String> {
    use crate::qwen_runtime as qr;
    let cache = staging.join("pip-cache");
    let (done, meter) = spawn_meter(
        vec![cache.clone(), runtime.join("venv")],
        pack.size_bytes,
        on_event,
    );
    let mut cmd = std::process::Command::new(pip);
    cmd.args(["install", "--prefer-binary", "--disable-pip-version-check"])
        .args(pack.pip_specs())
        .env("PIP_CACHE_DIR", &cache)
        .env_remove("PYTHONHOME")
        .env_remove("PYTHONPATH")
        .env_remove("VIRTUAL_ENV");
    let r = qr::run_cancellable(&mut cmd, cancel, log_path, "pip install");
    done.store(true, Ordering::SeqCst);
    let _ = meter.join();
    r
}

/// Provision the managed runtime: private Python 3.12 + venv at the
/// final path + locked deps + verification, transactionally.
/// Blocking; run on a worker thread. Reuses the download
/// event/cancel machinery (pip yields no byte counts, so dependency
/// installation reports the Installing phase with stages in
/// install.log).
///
/// Guarantees: GPU-gated before large vLLM downloads; staging under
/// owned storage with sha/version verification; venv created at its
/// final path (never relocated); previous working runtime restored
/// on failure/cancel; `"cancelled"` preserved distinctly; detailed
/// diagnostics in `<staging>/install.log` (+ `<runtime>/install.log`
/// on success), concise error strings otherwise.
pub fn download_qwen_runtime(
    data_dir: &Path,
    pack: &QwenPack,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
) -> Result<(), String> {
    use crate::qwen_runtime as qr;
    qr::supported_platform()?;
    qr::check_locked_compat()?;
    if pack.backend == QwenRuntime::Vllm {
        qr::check_gpu_for_vllm()?;
    }
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled".to_string());
    }
    let staging = qr::staging_dir(data_dir, pack.subdir);
    let runtime = qr::runtime_dir(data_dir, pack.subdir);
    let backup = qr::backup_dir(data_dir, pack.subdir);
    // Stale staging from an interrupted (SIGKILL) run: owned, drop it.
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("mkdir staging: {e}"))?;
    let log_path = staging.join("install.log");
    let _ = std::fs::write(&log_path, format!("qwen {} install\n", pack.id));
    let stage = |s: qr::Stage| {
        eprintln!("[qwen] {}: {}", pack.id, s.label());
        on_event(DownloadEvent::Installing);
    };
    let result: Result<(), String> = (|| {
        stage(qr::Stage::FetchUv);
        let progress = |received: u64, total: Option<u64>| {
            on_event(DownloadEvent::Progress { received, total });
        };
        let uv = qr::ensure_private_uv(&staging, cancel, &progress)?;
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".to_string());
        }
        // Preserve a working runtime across replacement; partial,
        // legacy (system-python/3.14), or staging-linked (venv python
        // resolving outside the runtime) installs are not working.
        let had_working = qwen_installed(data_dir, pack);
        if runtime.exists() {
            if had_working {
                let _ = std::fs::remove_dir_all(&backup);
                std::fs::rename(&runtime, &backup)
                    .map_err(|e| format!("stage previous runtime: {e}"))?;
            } else {
                std::fs::remove_dir_all(&runtime)
                    .map_err(|e| format!("clear stale runtime: {e}"))?;
            }
        }
        std::fs::create_dir_all(&runtime).map_err(|e| format!("mkdir runtime: {e}"))?;
        // Managed interpreter INSIDE the final runtime root: venvs
        // symlink (and share the stdlib of) their base interpreter,
        // so a base under staging would dangle as soon as staging is
        // cleaned. Only downloads live in staging.
        stage(qr::Stage::InstallPython);
        let python =
            qr::ensure_managed_python(&uv, &runtime.join("python"), &staging, cancel, &log_path)?;
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".to_string());
        }
        // Venv at the FINAL path (venvs embed absolute paths), from
        // the FINAL interpreter above.
        stage(qr::Stage::CreateVenv);
        let mut venv_cmd = std::process::Command::new(&python);
        venv_cmd
            .args(["-m", "venv", "venv"])
            .current_dir(&runtime)
            .env_remove("PYTHONHOME")
            .env_remove("PYTHONPATH")
            .env_remove("VIRTUAL_ENV");
        qr::run_cancellable(&mut venv_cmd, cancel, &log_path, "create venv")?;
        let venv_python = qr::venv_python(&runtime);
        if !venv_python.exists() {
            return Err("venv python missing after create (see install.log)".to_string());
        }
        // Locked deps via the venv pip (private, prefer wheels).
        // Two phases so the multi-GB fetch has exact byte progress:
        // pip only draws % on a TTY (a log file is not one), so we
        // first resolve to pinned wheel URLs (`--dry-run --report`),
        // download each wheel ourselves with per-byte events, then
        // install offline from the local wheel dir. Anything the
        // resolver cannot pin to sha-verified wheels (sdists, report
        // failures) falls back to a direct install whose meter is
        // scoped to exactly the dirs pip writes (cache + venv).
        stage(qr::Stage::InstallDeps);
        let pip = runtime.join("venv").join("bin").join("pip");
        let wheels = staging.join("wheels");
        std::fs::create_dir_all(&wheels).map_err(|e| format!("mkdir wheels: {e}"))?;
        let report = staging.join("pip-report.json");
        let mut resolve_cmd = std::process::Command::new(&pip);
        resolve_cmd
            .args([
                "install",
                "--dry-run",
                "--report",
                &report.to_string_lossy(),
                "--prefer-binary",
                "--disable-pip-version-check",
            ])
            .args(pack.pip_specs())
            .env("PIP_NO_CACHE_DIR", "1")
            .env_remove("PYTHONHOME")
            .env_remove("PYTHONPATH")
            .env_remove("VIRTUAL_ENV");
        let resolved = qr::run_cancellable(&mut resolve_cmd, cancel, &log_path, "pip resolve")
            .and_then(|()| {
                if cancel.load(Ordering::SeqCst) {
                    return Err("cancelled".to_string());
                }
                qr::parse_pip_report(&report)
            });
        match resolved {
            Ok(refs) if refs.iter().all(|w| w.is_wheel) => install_wheels_offline(
                &pip, &refs, &wheels, &runtime, pack, cancel, on_event, &log_path,
            )?,
            Ok(_) => {
                eprintln!(
                    "[qwen] {}: sdist in resolution set; direct install (see install.log)",
                    pack.id
                );
                install_direct_with_meter(
                    &pip, pack, &staging, &runtime, cancel, on_event, &log_path,
                )?;
            }
            Err(e) => {
                eprintln!(
                    "[qwen] {}: wheel prefetch unavailable ({e}); direct install",
                    pack.id
                );
                install_direct_with_meter(
                    &pip, pack, &staging, &runtime, cancel, on_event, &log_path,
                )?;
            }
        }
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".to_string());
        }
        // Post-activation verification AT the final path.
        stage(qr::Stage::Verify);
        let check = format!("import {}", pack.import_checks().join(", "));
        let mut check_cmd = std::process::Command::new(&venv_python);
        check_cmd.args(["-c", &check]);
        check_cmd
            .env_remove("PYTHONHOME")
            .env_remove("PYTHONPATH")
            .env_remove("VIRTUAL_ENV");
        qr::run_cancellable(&mut check_cmd, cancel, &log_path, "import check")?;
        // Interpreter identity at the final path (never 3.14).
        let out = std::process::Command::new(&venv_python)
            .arg("--version")
            .env_remove("PYTHONHOME")
            .env_remove("PYTHONPATH")
            .env_remove("VIRTUAL_ENV")
            .output()
            .map_err(|e| format!("runtime python --version: {e}"))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        match qr::parse_python_version(&text) {
            Some((maj, min)) if qr::managed_python_ok(maj, min) => {}
            _ => {
                return Err(format!(
                    "runtime python is not 3.12 (saw {text:?}; remove and reinstall)"
                ));
            }
        }
        stage(qr::Stage::Activate);
        std::fs::write(
            runtime.join(".installed"),
            qr::marker_text(pack.subdir, pack.pip_specs()),
        )
        .map_err(|e| format!("marker: {e}"))?;
        std::fs::write(
            runtime.join(".python-version"),
            crate::qwen_runtime::QWEN_PYTHON_VERSION,
        )
        .map_err(|e| format!("marker: {e}"))?;
        // Keep diagnostics with the runtime; drop the backup.
        let _ = std::fs::copy(&log_path, qr::install_log(&runtime));
        let _ = std::fs::remove_dir_all(&backup);
        let _ = std::fs::remove_dir_all(&staging);
        // Post-cleanup sanity: the venv must work with staging gone
        // (this is the exact regression that shipped staging-linked
        // venvs: activation looked fine until cleanup deleted their
        // interpreter). Fail the install rather than marking it.
        if !qr::venv_python_self_contained(&runtime) {
            return Err("venv interpreter escapes the runtime dir (see install.log)".to_string());
        }
        let sanity = std::process::Command::new(qr::venv_python(&runtime))
            .arg("--version")
            .env_remove("PYTHONHOME")
            .env_remove("PYTHONPATH")
            .env_remove("VIRTUAL_ENV")
            .output()
            .map_err(|e| format!("post-cleanup python check: {e}"))?;
        if !sanity.status.success() {
            return Err("venv python broken after cleanup (see install.log)".to_string());
        }
        on_event(DownloadEvent::Done);
        Ok(())
    })();
    match result {
        Ok(()) => {
            let _ = std::fs::remove_file(qr::failed_log(data_dir, pack.subdir));
            Ok(())
        }
        Err(e) => {
            // Restore the previous working runtime; never leave a
            // half-installed tree marked installed (marker is only
            // written on success, and partial trees are removed).
            let had_backup = backup.exists();
            let _ = std::fs::remove_dir_all(&runtime);
            if had_backup {
                let _ = std::fs::rename(&backup, &runtime);
            }
            // Persist diagnostics BEFORE staging is cleaned: the
            // concise error references this file, which survives
            // restarts (the in-memory Failed state does not).
            let failed = qr::failed_log(data_dir, pack.subdir);
            if !qr::is_cancel_error(&e) {
                let _ = qr::copy_log_tail(&log_path, &failed, 262_144);
            }
            let _ = std::fs::remove_dir_all(&staging);
            if qr::is_cancel_error(&e) {
                return Err(e);
            }
            Err(format!("{e} (details: {})", failed.display()))
        }
    }
}

/// Remove a Qwen runtime + its staging/backup. Refuses while an
/// install is in flight (cancel first): deleting files out from
/// under the installer worker must not recreate them afterward.
pub fn delete_qwen_runtime(data_dir: &Path, pack: &QwenPack) -> std::io::Result<()> {
    if let Ok(map) = downloads().lock()
        && let Some((snap, _)) = map.get(pack.id)
        && matches!(snap.phase, DlPhase::Downloading | DlPhase::Installing)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "installation in progress (cancel it first)",
        ));
    }
    for dir in [
        qwen_runtime_dir(data_dir, pack),
        crate::qwen_runtime::staging_dir(data_dir, pack.subdir),
        crate::qwen_runtime::backup_dir(data_dir, pack.subdir),
    ] {
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
    }
    let _ = std::fs::remove_file(crate::qwen_runtime::failed_log(data_dir, pack.subdir));
    Ok(())
}

/// Start a Qwen runtime provision on a worker thread. No-op if running.
/// Shares the download state map with model downloads (Retry after
/// failure re-enters here; Cancel sets the flag the installer polls).
pub fn start_qwen_runtime_download(data_dir: PathBuf, id: &'static str) {
    {
        let map = downloads().lock().unwrap();
        if let Some((snap, _)) = map.get(id)
            && matches!(snap.phase, DlPhase::Downloading | DlPhase::Installing)
        {
            return;
        }
    }
    let Some(pack) = QWEN_PACKS.iter().find(|p| p.id == id) else {
        return;
    };
    // Fresh attempt supersedes any persisted failure diagnostics.
    let _ = std::fs::remove_file(crate::qwen_runtime::failed_log(&data_dir, pack.subdir));
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
            if let Ok(mut map) = downloads().lock()
                && let Some((snap, _)) = map.get_mut(id)
            {
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
        });
        let result = download_qwen_runtime(&data_dir, pack, &cancel_worker, &cb);
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

/// Cancel an in-flight Qwen runtime provision. Distinct from
/// failure: the worker reports `"cancelled"` and the UI offers
/// Retry (not Remove-only).
pub fn cancel_qwen_runtime_download(id: &str) {
    cancel_download(id);
}

#[cfg(test)]
mod qwen_runtime_tests {
    use super::*;

    #[test]
    fn qwen_packs_are_consistent() {
        assert_eq!(QWEN_PACKS.len(), 2);
        for p in QWEN_PACKS {
            assert!(!p.pip_specs().is_empty(), "{}", p.id);
            assert!(!p.import_checks().is_empty(), "{}", p.id);
            assert!(p.size_bytes > 0, "{}", p.id);
            assert!(p.description.contains("3.12"), "{}", p.id);
        }
        assert_eq!(QWEN_PACKS[0].backend, QwenRuntime::Transformers);
        assert_eq!(QWEN_PACKS[1].backend, QwenRuntime::Vllm);
    }

    #[test]
    fn qwen_models_live_under_qwen_dir() {
        let entry = find_model("qwen3-asr-1.7b").unwrap();
        let dir = model_dir(Path::new("/data"), entry);
        assert_eq!(
            dir,
            Path::new("/data/models/qwen/qwen3-asr-1.7b"),
            "{dir:?}"
        );
        assert!(model_languages("qwen3-asr-0.6b").contains(&"en"));
        assert_eq!(model_languages("qwen3-asr-1.7b").len(), 30);
    }

    #[test]
    fn qwen_marker_pins_specs_and_python() {
        let pack = &QWEN_PACKS[0];
        let mark = crate::qwen_runtime::marker_text(pack.subdir, pack.pip_specs());
        assert!(mark.contains("qwen-runtime-v2"));
        assert!(mark.contains("3.12"));
        assert!(mark.contains(pack.pip_specs()[0]));
        assert!(crate::qwen_runtime::marker_valid(
            &mark,
            pack.subdir,
            pack.pip_specs()
        ));
        // Old system-python marker never validates.
        assert!(!crate::qwen_runtime::marker_valid(
            &format!("{}|{}\n", pack.id, pack.pip_specs().join(" ")),
            pack.subdir,
            pack.pip_specs()
        ));
    }

    #[test]
    fn stale_partial_runtime_is_not_installed() {
        let data = std::env::temp_dir().join(format!("qwen-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let pack = &QWEN_PACKS[0];
        let dir = qwen_runtime_dir(&data, pack);
        std::fs::create_dir_all(dir.join("venv").join("bin")).unwrap();
        // venv shell without marker/python: incomplete.
        assert!(!qwen_installed(&data, pack));
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn removal_refused_while_installing() {
        let data = std::env::temp_dir().join(format!("qwen-busy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let pack = &QWEN_PACKS[0];
        // Simulate in-flight state.
        downloads().lock().unwrap().insert(
            pack.id.to_string(),
            (
                DlSnapshot {
                    received: 0,
                    total: Some(pack.size_bytes),
                    phase: DlPhase::Installing,
                },
                Arc::new(AtomicBool::new(false)),
            ),
        );
        let err = delete_qwen_runtime(&data, pack).unwrap_err();
        assert!(err.to_string().contains("cancel"));
        downloads().lock().unwrap().remove(pack.id);
        let _ = std::fs::remove_dir_all(&data);
    }

    /// Live provisioning through the real installer: full managed
    /// install of the transformers pack into disposable storage
    /// (network + ~9 GB free). Ignored by default; run explicitly.
    /// Proves the venv works after staging cleanup (the shipped
    /// staging-linked-venv regression) and that a retry heals the
    /// broken layout left by it.
    #[test]
    #[ignore]
    fn live_provision_transformers() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let data = Path::new(&home)
            .join(".cache")
            .join(format!("qwen-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let pack = QWEN_PACKS
            .iter()
            .find(|p| p.id == "qwen-transformers")
            .unwrap();
        // Plant the shipped broken layout first: marker + venv whose
        // python points at (deleted) staging. Retry must heal it.
        let dir = qwen_runtime_dir(&data, pack);
        std::fs::create_dir_all(dir.join("venv").join("bin")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            data.join("bin")
                .join(".staging-qwen-qwen-transformers")
                .join("python"),
            dir.join("venv").join("bin").join("python"),
        )
        .unwrap();
        std::fs::write(dir.join(".installed"), "qwen-runtime-v2|stale\n").unwrap();
        assert!(
            !qwen_installed(&data, pack),
            "staging-linked venv must not count as installed"
        );
        let cancel = AtomicBool::new(false);
        let cb: DownloadCallback = Arc::new(|ev| match ev {
            DownloadEvent::Progress { received, total } => {
                eprintln!("[live] progress {received}/{total:?}");
            }
            DownloadEvent::Installing => eprintln!("[live] installing"),
            DownloadEvent::Done => eprintln!("[live] done"),
        });
        download_qwen_runtime(&data, pack, &cancel, &cb).unwrap();
        assert!(qwen_installed(&data, pack));
        // Staging is gone and the venv still works: the regression.
        assert!(
            !crate::qwen_runtime::staging_dir(&data, pack.subdir).exists(),
            "staging must be cleaned"
        );
        let out = std::process::Command::new(qwen_venv_python(&data, pack))
            .arg("--version")
            .env_remove("PYTHONHOME")
            .env_remove("PYTHONPATH")
            .env_remove("VIRTUAL_ENV")
            .output()
            .unwrap();
        assert!(out.status.success(), "venv python broken after cleanup");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(text.contains("3.12"), "{text}");
        let _ = std::fs::remove_dir_all(&data);
    }
}
