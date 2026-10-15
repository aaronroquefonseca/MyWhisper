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
    pub sha256: &'static str,
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
        expected_bytes: Some(77_691_713),
        sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
        download_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-tiny.bin",
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
        expected_bytes: Some(147_951_465),
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
        download_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.bin",
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
        expected_bytes: Some(487_601_967),
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
        download_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-small.bin",
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
        expected_bytes: Some(1_533_763_059),
        sha256: "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
        download_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-medium.bin",
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
        expected_bytes: Some(3_095_033_483),
        sha256: "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2",
        download_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-large-v3.bin",
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
        expected_bytes: Some(1_624_555_275),
        sha256: "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
        download_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-large-v3-turbo.bin",
        extract_dir: None,
        required_files: &[],
        runtime: None,
        recommended: false,
    },
];

/// Cactus Whistle (own engine, CLI-driven): 16.9 MB, 7 languages
/// with auto-detect, up to 30 s per pass. Downloads are pinned to the
/// upstream commit and SHA-256; existing manually placed models remain usable.
pub const WHISTLE_LANGS: &[&str] = &["en", "de", "fr", "es", "it", "nl", "pl"];

pub const WHISTLE_MODELS: &[ModelEntry] = &[ModelEntry {
    id: "whistle-v1",
    kind: ModelKind::Whistle,
    name: "Whistle v1",
    description: "16.9 MB STT, 7 languages, own CPU engine",
    size_label: "17MB",
    size_mb: 17,
    expected_bytes: Some(16_919_407),
    sha256: "b6e02f048568ac5d01a2042556c658061e699acbc0aa2a1439f52f3d461dffeb",
    download_url: "https://huggingface.co/Cactus-Compute/whistle/resolve/b358ddadd89b7a713b5aa131f23032d3cca1b251/whistle.cact",
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
        expected_bytes: Some(487_170_055),
        sha256: "5793d0fd397c5778d2cf2126994d58e9d56b1be7c04d13c7a15bb1b4eafb16bf",
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
        expected_bytes: Some(501_350_460),
        sha256: "99f63605b3a85a54c250c0869670a687b7d6598a47bf2421515e1f839a76e150",
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
        expected_bytes: Some(463_945_051),
        sha256: "78e2b79fcf7271553a74402a76b771b09ea40117a39566a79f52235b23db6358",
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
        expected_bytes: Some(475_271_763),
        sha256: "c6bf5e0df765f9d5b43bc9e0536d4b4b3e7d40bdf5ecf13e45f134c51c05ae3a",
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
        expected_bytes: Some(1_699_791_751),
        sha256: "bd582588d50685a795dcd2807ab77e11361b8312d96c53884682def45ab4206d",
        download_url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-cohere-transcribe-14-lang-int8-2026-04-01.tar.bz2",
        extract_dir: Some("sherpa-onnx-cohere-transcribe-14-lang-int8-2026-04-01"),
        required_files: COHERE_FILES,
        runtime: Some(ParakeetRuntime::Offline),
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
        sha256: "f9191f30178cc9122ce2f023bf9fefafc822028307b0efa4caff645ba3fe8d0a",
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
        "whistle-v1" => WHISTLE_LANGS,
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
    use super::*;
    #[test]
    fn cancelled_cohere_download_does_not_replace_existing_files() {
        let directory = tempfile::tempdir().unwrap();
        let entry = find_model("cohere-transcribe-03-2026").unwrap();
        let existing = model_dir(directory.path(), entry);
        std::fs::create_dir_all(&existing).unwrap();
        std::fs::write(existing.join("tokens.txt"), "preserved user model").unwrap();
        let callback: DownloadCallback =
            Arc::new(|_| panic!("cancelled download must not make progress"));
        assert!(
            download_model(directory.path(), entry, &AtomicBool::new(true), &callback)
                .unwrap_err()
                .contains("cancel")
        );
        assert_eq!(
            std::fs::read_to_string(existing.join("tokens.txt")).unwrap(),
            "preserved user model"
        );
    }

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

/// Capability matrix: (live streaming, dictionary bias).
/// Whisper prompts natively; sherpa transducers take hotwords files;
/// whistle takes keyword files; orukeet/cohere expose neither path.
pub fn capabilities(entry: &ModelEntry) -> (bool, bool) {
    match entry.kind {
        ModelKind::Whisper => (false, true),
        ModelKind::Whistle => (true, true),
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
    };
    models_dir(data_dir).join(sub).join(entry.id)
}

pub(crate) fn model_spec(data: &Path, entry: &ModelEntry) -> crate::installed_asset::Spec {
    let raw = match entry.kind {
        ModelKind::Whisper => Some(format!("ggml-{}.bin", whisper_file_stem(entry.id))),
        ModelKind::Whistle => Some("whistle.cact".into()),
        ModelKind::Parakeet => None,
    };
    let required = raw
        .as_ref()
        .map(|file| vec![file.clone()])
        .unwrap_or_else(|| {
            entry
                .required_files
                .iter()
                .map(|file| (*file).into())
                .collect()
        });
    crate::installed_asset::Spec {
        data: data.into(),
        root: model_dir(data, entry),
        key: format!("model-{}", entry.id),
        source: entry.sha256.into(),
        required,
        executables: Vec::new(),
        raw,
    }
}

/// Session-path presence check for inference admission: every required
/// model file exists and is non-empty. Inherited by #23 for status UI;
/// single-sourced here, never duplicated.
pub(crate) fn model_present(data: &Path, entry: &ModelEntry) -> bool {
    let spec = model_spec(data, entry);
    spec.required.iter().all(|file| {
        std::fs::metadata(spec.root.join(file))
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
    })
}

/// UI status is an off-thread integrity result, never a size/presence guess.
pub(crate) fn model_status(data_dir: &Path, entry: &ModelEntry) -> crate::installed_asset::Status {
    crate::installed_asset::status(model_spec(data_dir, entry))
}
pub fn is_installed(data_dir: &Path, entry: &ModelEntry) -> bool {
    matches!(
        model_status(data_dir, entry),
        crate::installed_asset::Status::Ready | crate::installed_asset::Status::ReadyLocal
    )
}

pub fn whisper_file_stem(id: &str) -> &str {
    match id {
        "large" => "large-v3",
        "turbo" => "large-v3-turbo",
        other => other,
    }
}

pub fn delete_model(data_dir: &Path, entry: &ModelEntry) -> std::io::Result<()> {
    crate::artifact::delete(
        data_dir,
        &format!("model-{}", entry.id),
        &model_dir(data_dir, entry),
    )
}

/// Invalidate every queued/active installer, then remove only downloaded assets.
/// The control directory and its epochs survive so old work cannot repopulate it.
pub fn clear_download_cache(data_dir: &Path) -> std::io::Result<()> {
    for (_, cancel) in downloads()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
    {
        cancel.store(true, Ordering::Release);
    }
    crate::artifact::clear_cache(data_dir)
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
    prepare_model(data_dir, entry, cancel, on_event, false, None)
}

fn prepare_model(
    data_dir: &Path,
    entry: &ModelEntry,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
    verification_only: bool,
    request: Option<&crate::artifact::Authority>,
) -> Result<(), String> {
    let captured = crate::artifact::observe_authority(data_dir, &format!("model-{}", entry.id))?;
    let stage = crate::artifact::Stage::new_requested(
        data_dir,
        &format!("model-{}", entry.id),
        model_dir(data_dir, entry),
        request.unwrap_or(&captured),
        cancel,
    )?;
    let dir = stage.path().to_owned();
    let tmp = dir.join("download.part");
    let verified = crate::artifact::download(
        crate::artifact::Source {
            url: entry.download_url,
            sha256: entry.sha256,
            limit: 5 * 1024 * 1024 * 1024,
        },
        &tmp,
        || stage.check(cancel),
        |received, total| on_event(DownloadEvent::Progress { received, total }),
    )?;
    if entry
        .expected_bytes
        .is_some_and(|bytes| bytes != verified.bytes())
    {
        return Err("model size mismatch".into());
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
            crate::artifact::unpack_tar(&tmp, &dir, || stage.check(cancel))?;
            let _ = std::fs::remove_file(&tmp);
            // Flatten one level when the archive nests everything
            // under its extract dir (as all registry archives do).
            if let Some(top) = entry.extract_dir {
                let nested = dir.join(top);
                if nested.is_dir() {
                    flatten_dir(&nested, &dir, || stage.check(cancel))
                        .map_err(|e| format!("flatten: {e}"))?;
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
    crate::installed_asset::seal(&stage, &verified, || stage.check(cancel))?;
    if verification_only {
        crate::installed_asset::adopt_reference(stage, model_spec(data_dir, entry), cancel)?;
    } else {
        crate::installed_asset::install(stage, model_spec(data_dir, entry), cancel)?;
    }
    on_event(DownloadEvent::Done);
    Ok(())
}

fn flatten_dir(
    src: &Path,
    dest: &Path,
    check: impl Fn() -> Result<(), String>,
) -> Result<(), String> {
    for entry in std::fs::read_dir(src).map_err(|e| format!("read extract dir: {e}"))? {
        check()?;
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

type DownloadStates =
    std::sync::Mutex<std::collections::HashMap<String, (DlSnapshot, Arc<AtomicBool>)>>;
static DOWNLOADS: std::sync::OnceLock<DownloadStates> = std::sync::OnceLock::new();

fn downloads() -> &'static DownloadStates {
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
    let Some(entry) = find_model(id) else {
        return;
    };
    let request = crate::artifact::observe_authority(&data_dir, &format!("model-{}", entry.id));
    start_artifact_download(id, entry.expected_bytes, move |cancel, cb| {
        prepare_model(&data_dir, entry, cancel, cb, false, Some(&request?))
    });
}

/// Admission and ownership are atomic. Long network IO does not consume the
/// short-job pool, and completion cannot overwrite a later attempt's state.
fn start_artifact_download(
    id: &'static str,
    total: Option<u64>,
    install: impl FnOnce(&AtomicBool, &DownloadCallback) -> Result<(), String> + Send + 'static,
) {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut map = downloads().lock().unwrap_or_else(|p| p.into_inner());
        if map.get(id).is_some_and(|(snap, _)| {
            matches!(snap.phase, DlPhase::Downloading | DlPhase::Installing)
        }) {
            return;
        }
        let active = map
            .values()
            .filter(|(snap, _)| matches!(snap.phase, DlPhase::Downloading | DlPhase::Installing))
            .count();
        let phase = if active >= 2 {
            DlPhase::Failed(
                "two downloads are already active; finish or cancel one before retrying".into(),
            )
        } else {
            DlPhase::Downloading
        };
        let rejected = matches!(phase, DlPhase::Failed(_));
        map.insert(
            id.to_string(),
            (
                DlSnapshot {
                    received: 0,
                    total,
                    phase,
                },
                cancel.clone(),
            ),
        );
        if rejected {
            return;
        }
    }
    let worker_cancel = cancel.clone();
    let result = crate::runtime::spawn_session("artifact-install", move || {
        let callback_cancel = worker_cancel.clone();
        let cb: DownloadCallback = Arc::new(move |event| {
            let mut map = downloads().lock().unwrap_or_else(|p| p.into_inner());
            if let Some((snap, current)) = map.get_mut(id)
                && Arc::ptr_eq(current, &callback_cancel)
            {
                match event {
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
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if worker_cancel.load(Ordering::Acquire) {
                return Err("cancelled".into());
            }
            install(&worker_cancel, &cb)
        }))
        .unwrap_or_else(|_| Err("installer failed unexpectedly".into()));
        let mut map = downloads().lock().unwrap_or_else(|p| p.into_inner());
        if map
            .get(id)
            .is_some_and(|(_, current)| Arc::ptr_eq(current, &worker_cancel))
        {
            match result {
                Ok(()) => {
                    map.remove(id);
                }
                Err(error) => {
                    map.get_mut(id).unwrap().0.phase = DlPhase::Failed(error);
                }
            }
        }
    });
    if let Err(error) = result {
        let mut map = downloads().lock().unwrap_or_else(|p| p.into_inner());
        if let Some((snapshot, current)) = map.get_mut(id)
            && Arc::ptr_eq(current, &cancel)
        {
            snapshot.phase = DlPhase::Failed(error);
        }
    }
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
    }

    #[test]
    fn registry_is_complete_and_consistent() {
        assert_eq!(WHISPER_MODELS.len(), 6);
        assert_eq!(PARAKEET_MODELS.len(), 6);
        assert_eq!(WHISTLE_MODELS.len(), 1);
        assert_eq!(GPU_PACKS.len(), 3);
        assert_eq!(SHERPA_PACKS.len(), 2);
        fn pinned(sha256: &str, id: &str) {
            assert_eq!(sha256.len(), 64, "{id}");
            assert!(sha256.bytes().all(|byte| byte.is_ascii_hexdigit()), "{id}");
        }
        let mut recommended = 0;
        for m in WHISPER_MODELS
            .iter()
            .chain(PARAKEET_MODELS.iter())
            .chain(WHISTLE_MODELS.iter())
        {
            assert!(m.download_url.starts_with("https://"), "{}", m.id);
            assert!(m.size_mb > 0, "{}", m.id);
            pinned(m.sha256, m.id);
            if m.recommended {
                recommended += 1;
            }
            if m.kind == ModelKind::Parakeet {
                assert!(!m.required_files.is_empty(), "{}", m.id);
                assert!(m.extract_dir.is_some(), "{}", m.id);
            }
        }
        // Exactly one recommended default per family.
        assert_eq!(recommended, 3);
        for pack in GPU_PACKS {
            pinned(pack.sha256, pack.id);
            assert!(pack.size_bytes > 0, "{}", pack.id);
        }
        for pack in SHERPA_PACKS {
            pinned(pack.sha256, pack.id);
            assert!(pack.size_bytes > 0, "{}", pack.id);
            assert!(!pack.binaries.is_empty(), "{}", pack.id);
        }
    }

    #[test]
    fn every_advertised_asset_has_an_authenticated_spec() {
        let data = tempfile::tempdir().unwrap();
        let mut keys = std::collections::HashSet::new();
        for entry in WHISPER_MODELS
            .iter()
            .chain(PARAKEET_MODELS.iter())
            .chain(WHISTLE_MODELS.iter())
        {
            let spec = model_spec(data.path(), entry);
            assert!(!spec.key.is_empty(), "{}", entry.id);
            assert_eq!(spec.source.len(), 64, "{}", entry.id);
            assert!(!spec.required.is_empty(), "{}", entry.id);
            assert!(spec.root.starts_with(data.path()), "{}", entry.id);
            assert!(keys.insert(spec.key.clone()), "duplicate key {}", spec.key);
        }
        for pack in GPU_PACKS {
            let spec = gpu_spec(data.path(), pack);
            assert_eq!(spec.source, pack.sha256);
            assert!(!spec.required.is_empty(), "{}", pack.id);
            assert!(keys.insert(spec.key.clone()), "duplicate key {}", spec.key);
        }
        for pack in SHERPA_PACKS {
            let spec = sherpa_spec(data.path(), pack);
            assert_eq!(spec.source, pack.sha256);
            assert!(!spec.required.is_empty(), "{}", pack.id);
            assert!(keys.insert(spec.key.clone()), "duplicate key {}", spec.key);
        }
        match needle_spec(data.path()) {
            Ok(spec) => {
                assert_eq!(spec.source.len(), 64);
                assert!(!spec.required.is_empty());
                assert!(keys.insert(spec.key.clone()));
            }
            Err(message) => panic!("needle has no build for this arch: {message}"),
        }
    }

    #[test]
    fn repeated_cancelled_downloads_preserve_existing_files() {
        for id in ["tiny", "cohere-transcribe-03-2026"] {
            let directory = tempfile::tempdir().unwrap();
            let entry = find_model(id).unwrap();
            let existing = model_dir(directory.path(), entry);
            std::fs::create_dir_all(&existing).unwrap();
            std::fs::write(existing.join("tokens.txt"), "preserved user model").unwrap();
            let callback: DownloadCallback =
                Arc::new(|_| panic!("cancelled download must not make progress"));
            for _ in 0..3 {
                assert!(
                    download_model(directory.path(), entry, &AtomicBool::new(true), &callback)
                        .unwrap_err()
                        .contains("cancel"),
                    "{id}"
                );
                assert_eq!(
                    std::fs::read_to_string(existing.join("tokens.txt")).unwrap(),
                    "preserved user model",
                    "{id}"
                );
            }
        }
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
        let temporary = tempfile::tempdir().unwrap();
        let data = temporary.path();
        let entry = find_model("tiny").unwrap();
        let cancel = AtomicBool::new(false);
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let ev = events.clone();
        let cb: DownloadCallback = std::sync::Arc::new(move |e| {
            ev.lock().unwrap().push(format!("{e:?}"));
        });
        download_model(data, entry, &cancel, &cb).expect("tiny download");
        assert!(is_installed(data, entry));
        let log = events.lock().unwrap();
        assert!(
            log.iter().any(|e| e.contains("Progress")),
            "no progress events"
        );
        assert!(log.last().unwrap().contains("Done"));
    }

    /// Full GPU pack round-trip (CPU pack, ~1.3MB). Ignored by
    /// default: needs network. Proves URL, sha256, zip extract,
    /// chmod and install layout.
    #[test]
    #[ignore]
    fn downloads_cpu_pack() {
        let temporary = tempfile::tempdir().unwrap();
        let data = temporary.path();
        let pack = GPU_PACKS.iter().find(|p| p.id == "whisper-cpu").unwrap();
        let cancel = AtomicBool::new(false);
        let cb: DownloadCallback = std::sync::Arc::new(|_| {});
        download_gpu_pack(data, pack, &cancel, &cb).expect("cpu pack");
        assert!(gpu_installed(data, pack));
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
        description: "NVIDIA GPUs (Pascal 6.1+); driver plus CUDA 12 runtime/cuBLAS libraries",
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

/// The runtime specification includes its complete authenticated tree.
pub(crate) fn gpu_spec(data_dir: &Path, pack: &GpuPack) -> crate::installed_asset::Spec {
    crate::installed_asset::Spec {
        data: data_dir.into(),
        root: gpu_bin_dir(data_dir, pack),
        key: format!("gpu-{}", pack.id),
        source: pack.sha256.into(),
        required: vec!["whisper-server".into()],
        executables: vec!["whisper-server".into()],
        raw: None,
    }
}
pub(crate) fn gpu_status(data_dir: &Path, pack: &GpuPack) -> crate::installed_asset::Status {
    crate::installed_asset::status(gpu_spec(data_dir, pack))
}
pub fn gpu_installed(data_dir: &Path, pack: &GpuPack) -> bool {
    matches!(
        gpu_status(data_dir, pack),
        crate::installed_asset::Status::Ready | crate::installed_asset::Status::ReadyLocal
    )
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
    prepare_gpu_pack(data_dir, pack, cancel, on_event, false, None)
}

fn prepare_gpu_pack(
    data_dir: &Path,
    pack: &GpuPack,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
    verification_only: bool,
    request: Option<&crate::artifact::Authority>,
) -> Result<(), String> {
    let captured = crate::artifact::observe_authority(data_dir, &format!("gpu-{}", pack.id))?;
    let stage = crate::artifact::Stage::new_requested(
        data_dir,
        &format!("gpu-{}", pack.id),
        gpu_bin_dir(data_dir, pack),
        request.unwrap_or(&captured),
        cancel,
    )?;
    let dir = stage.path().to_owned();
    let tmp = dir.join("download.part");
    let url = gpu_url(pack.asset);
    let verified = crate::artifact::download(
        crate::artifact::Source {
            url: &url,
            sha256: pack.sha256,
            limit: 5 * 1024 * 1024 * 1024,
        },
        &tmp,
        || stage.check(cancel),
        |received, total| on_event(DownloadEvent::Progress { received, total }),
    )?;

    on_event(DownloadEvent::Installing);
    crate::artifact::unpack_zip(&tmp, &dir, || stage.check(cancel))?;
    let _ = std::fs::remove_file(&tmp);
    stage.check(cancel)?;
    // The archives ship the binary under its release filename
    // (`whisper-server-linux-x64-cuda`, …): normalize to
    // `whisper-server` so runners have one stable path. Only regular
    // files are candidates: unpack_zip rejects symlinks, and neither
    // rename nor chmod may operate through one, so an archive-controlled
    // link can never redirect installation or touch unowned files.
    if !std::fs::symlink_metadata(dir.join("whisper-server")).is_ok_and(|meta| meta.is_file()) {
        let mut candidates = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("read bin dir: {e}"))? {
            stage.check(cancel)?;
            let entry = entry.map_err(|e| format!("read bin dir: {e}"))?;
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with("whisper-server")
            {
                continue;
            }
            if entry
                .file_type()
                .map_err(|e| format!("stat binary: {e}"))?
                .is_file()
            {
                candidates.push(entry.path());
            }
        }
        if candidates.len() == 1 {
            std::fs::rename(&candidates[0], dir.join("whisper-server"))
                .map_err(|e| format!("normalize binary: {e}"))?;
        }
    }
    // Server binary must be executable after unzip. Never chmod through a
    // symlink: only a regular staged file may gain owner-execute bits.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("whisper-server");
        if std::fs::symlink_metadata(&bin).is_ok_and(|meta| meta.is_file()) {
            let mut perms = std::fs::metadata(&bin)
                .map_err(|e| format!("stat binary: {e}"))?
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin, perms).map_err(|e| format!("chmod: {e}"))?;
        }
    }
    if !std::fs::symlink_metadata(dir.join("whisper-server")).is_ok_and(|meta| meta.is_file()) {
        return Err("whisper-server missing after extract".to_string());
    }
    crate::installed_asset::seal(&stage, &verified, || stage.check(cancel))?;
    if verification_only {
        crate::installed_asset::adopt_reference(stage, gpu_spec(data_dir, pack), cancel)?;
    } else {
        crate::installed_asset::install(stage, gpu_spec(data_dir, pack), cancel)?;
    }
    on_event(DownloadEvent::Done);
    Ok(())
}

pub fn delete_gpu_pack(data_dir: &Path, pack: &GpuPack) -> std::io::Result<()> {
    crate::artifact::delete(
        data_dir,
        &format!("gpu-{}", pack.id),
        &gpu_bin_dir(data_dir, pack),
    )
}

/// Start a GPU pack download on a worker thread. No-op if running.
/// Shares the download state map with model downloads.
pub fn start_gpu_download(data_dir: PathBuf, id: &'static str) {
    let Some(pack) = GPU_PACKS.iter().find(|p| p.id == id) else {
        return;
    };
    let request = crate::artifact::observe_authority(&data_dir, &format!("gpu-{}", pack.id));
    start_artifact_download(id, Some(pack.size_bytes), move |cancel, cb| {
        prepare_gpu_pack(&data_dir, pack, cancel, cb, false, Some(&request?))
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

/// Required executables supplement complete authenticated tree verification.
pub(crate) fn sherpa_spec(data_dir: &Path, pack: &SherpaPack) -> crate::installed_asset::Spec {
    crate::installed_asset::Spec {
        data: data_dir.into(),
        root: sherpa_bin_dir(data_dir, pack),
        key: format!("sherpa-{}", pack.id),
        source: pack.sha256.into(),
        required: pack.binaries.iter().map(|file| (*file).into()).collect(),
        executables: pack
            .binaries
            .iter()
            .filter(|file| !file.ends_with(".so"))
            .map(|file| (*file).into())
            .collect(),
        raw: None,
    }
}
pub(crate) fn sherpa_status(data_dir: &Path, pack: &SherpaPack) -> crate::installed_asset::Status {
    crate::installed_asset::status(sherpa_spec(data_dir, pack))
}
pub fn sherpa_installed(data_dir: &Path, pack: &SherpaPack) -> bool {
    matches!(
        sherpa_status(data_dir, pack),
        crate::installed_asset::Status::Ready | crate::installed_asset::Status::ReadyLocal
    )
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
    prepare_sherpa_pack(data_dir, pack, cancel, on_event, false, None)
}

fn prepare_sherpa_pack(
    data_dir: &Path,
    pack: &SherpaPack,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
    verification_only: bool,
    request: Option<&crate::artifact::Authority>,
) -> Result<(), String> {
    let captured = crate::artifact::observe_authority(data_dir, &format!("sherpa-{}", pack.id))?;
    let stage = crate::artifact::Stage::new_requested(
        data_dir,
        &format!("sherpa-{}", pack.id),
        sherpa_bin_dir(data_dir, pack),
        request.unwrap_or(&captured),
        cancel,
    )?;
    let dir = stage.path().to_owned();
    let tmp = dir.join("download.part");
    let url = sherpa_url(pack.asset);
    let verified = crate::artifact::download(
        crate::artifact::Source {
            url: &url,
            sha256: pack.sha256,
            limit: 5 * 1024 * 1024 * 1024,
        },
        &tmp,
        || stage.check(cancel),
        |received, total| on_event(DownloadEvent::Progress { received, total }),
    )?;

    on_event(DownloadEvent::Installing);
    crate::artifact::unpack_tar(&tmp, &dir, || stage.check(cancel))?;
    let _ = std::fs::remove_file(&tmp);
    // Flatten the single top dir when present.
    if let Ok(mut entries) = std::fs::read_dir(&dir) {
        let tops: Vec<_> = entries
            .by_ref()
            .flatten()
            .filter(|e| e.path().is_dir())
            .collect();
        if tops.len() == 1 {
            flatten_dir(&tops[0].path(), &dir, || stage.check(cancel))
                .map_err(|e| format!("flatten: {e}"))?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for b in pack.binaries {
            if b.ends_with(".so") {
                continue;
            }
            // Never chmod through a symlink: only a regular staged file
            // may gain owner-execute bits.
            let bin = dir.join(b);
            if !std::fs::symlink_metadata(&bin).is_ok_and(|meta| meta.is_file()) {
                continue;
            }
            if let Ok(meta) = std::fs::metadata(&bin) {
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
    crate::installed_asset::seal(&stage, &verified, || stage.check(cancel))?;
    if verification_only {
        crate::installed_asset::adopt_reference(stage, sherpa_spec(data_dir, pack), cancel)?;
    } else {
        crate::installed_asset::install(stage, sherpa_spec(data_dir, pack), cancel)?;
    }
    on_event(DownloadEvent::Done);
    Ok(())
}

pub fn delete_sherpa_pack(data_dir: &Path, pack: &SherpaPack) -> std::io::Result<()> {
    crate::artifact::delete(
        data_dir,
        &format!("sherpa-{}", pack.id),
        &sherpa_bin_dir(data_dir, pack),
    )
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
    format!(
        "https://huggingface.co/Cactus-Compute/needle3/resolve/2ae11323dc000f5e70c49f7403efa6af12ba9e67/{folder}/needle"
    )
}

pub fn needle_bin_dir(data_dir: &Path) -> PathBuf {
    models_dir(data_dir)
        .parent()
        .unwrap_or(data_dir)
        .join("bin")
        .join("needle")
}

/// The engine is admitted using its pinned digest or explicit local trust.
pub(crate) fn needle_spec(data_dir: &Path) -> Result<crate::installed_asset::Spec, String> {
    let asset = needle_asset()?;
    Ok(crate::installed_asset::Spec {
        data: data_dir.into(),
        root: needle_bin_dir(data_dir),
        key: "needle".into(),
        source: asset.sha256.into(),
        required: vec!["needle".into()],
        executables: vec!["needle".into()],
        raw: Some("needle".into()),
    })
}
pub(crate) fn needle_status(data_dir: &Path) -> crate::installed_asset::Status {
    match needle_spec(data_dir) {
        Ok(spec) => crate::installed_asset::status(spec),
        Err(message) => crate::installed_asset::Status::Invalid(message),
    }
}
pub fn needle_installed(data_dir: &Path) -> bool {
    matches!(
        needle_status(data_dir),
        crate::installed_asset::Status::Ready | crate::installed_asset::Status::ReadyLocal
    )
}

/// Download + sha256-verify a needle engine binary. Blocking; run on
/// a worker thread. Single file, no extraction; chmod +x on install.
pub fn download_needle_pack(
    data_dir: &Path,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
) -> Result<(), String> {
    prepare_needle_pack(data_dir, cancel, on_event, None)
}

fn prepare_needle_pack(
    data_dir: &Path,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
    request: Option<&crate::artifact::Authority>,
) -> Result<(), String> {
    let asset = needle_asset()?;
    let captured = crate::artifact::observe_authority(data_dir, "needle")?;
    let stage = crate::artifact::Stage::new_requested(
        data_dir,
        "needle",
        needle_bin_dir(data_dir),
        request.unwrap_or(&captured),
        cancel,
    )?;
    let dir = stage.path().to_owned();
    let tmp = dir.join("download.part");
    let url = needle_url(asset.folder);
    let verified = crate::artifact::download(
        crate::artifact::Source {
            url: &url,
            sha256: asset.sha256,
            limit: 5 * 1024 * 1024 * 1024,
        },
        &tmp,
        || stage.check(cancel),
        |received, total| on_event(DownloadEvent::Progress { received, total }),
    )?;

    on_event(DownloadEvent::Installing);
    let dest = dir.join("needle");
    std::fs::rename(&tmp, &dest).map_err(|e| format!("install: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod: {e}"))?;
    }
    crate::installed_asset::seal(&stage, &verified, || stage.check(cancel))?;
    crate::installed_asset::install(
        stage,
        crate::installed_asset::Spec {
            data: data_dir.into(),
            root: needle_bin_dir(data_dir),
            key: "needle".into(),
            source: asset.sha256.into(),
            required: vec!["needle".into()],
            executables: vec!["needle".into()],
            raw: Some("needle".into()),
        },
        cancel,
    )?;
    on_event(DownloadEvent::Done);
    Ok(())
}

pub fn delete_needle_pack(data_dir: &Path) -> std::io::Result<()> {
    crate::artifact::delete(data_dir, "needle", &needle_bin_dir(data_dir))
}

/// Start a needle engine download on the task runtime. No-op if
/// running or when this arch has no build.
pub fn start_needle_download(data_dir: PathBuf) {
    let request = crate::artifact::observe_authority(&data_dir, "needle");
    start_artifact_download(
        NEEDLE_ENGINE_ID,
        needle_asset().ok().map(|asset| asset.size_bytes),
        move |cancel, cb| prepare_needle_pack(&data_dir, cancel, cb, Some(&request?)),
    );
}

/// Start a sherpa pack download on a worker thread. No-op if running.
pub fn start_sherpa_download(data_dir: PathBuf, id: &'static str) {
    let Some(pack) = SHERPA_PACKS.iter().find(|p| p.id == id) else {
        return;
    };
    let request = crate::artifact::observe_authority(&data_dir, &format!("sherpa-{}", pack.id));
    start_artifact_download(id, Some(pack.size_bytes), move |cancel, cb| {
        prepare_sherpa_pack(&data_dir, pack, cancel, cb, false, Some(&request?))
    });
}
