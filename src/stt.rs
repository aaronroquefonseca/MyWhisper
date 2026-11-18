//! Local speech-to-text: model registry, downloads, storage.
//!
//! Same model versions OpenWhispr ships (same upstreams, same files),
//! managed locally under `<data>/models/`:
//!
//! * whisper: single `ggml-*.bin` from `ggerganov/whisper.cpp` (HF)
//! * parakeet: `.tar.bz2` archives from k2-fsa sherpa-onnx releases
//!   (or the pinned Orukeet HF commit), extracted + verified by
//!   required files
//! * bark: loose files of commit-pinned HF snapshots, each checked
//!   for exact size + SHA-256 before the pack dir is published
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
    Bark,
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

/// Underdog Bark 3.0 family by Conway Research: English-only
/// Parakeet TDT 0.6B v3 fine-tunes, in the unofficial sherpa-onnx
/// ONNX conversions by Hugging Face user `velddev`. They keep k2-fsa's
/// NeMo transducer layout (encoder/decoder/joiner + tokens.txt), so
/// the existing sherpa offline server runs them. int8 only.
///
/// Licensing: Conway Research relicensed the current `main` of these
/// repos to CC BY-NC 4.0; the CC BY 4.0 grant covers the exact source
/// revisions below, and each conversion records being made from that
/// revision and is CC BY 4.0 at its pinned commit. Never move a pin
/// without re-checking both licenses. Bark 3.0 Pro has no ONNX build.
const BARK_FILES: &[&str] = &[
    "encoder.int8.onnx",
    "decoder.int8.onnx",
    "joiner.int8.onnx",
    "tokens.txt",
];

/// One file of a pinned Hugging Face snapshot: exact size and
/// SHA-256 (the LFS oid for weights, hashed locally for text files).
#[derive(Debug)]
pub struct PinnedFile {
    pub name: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

/// Provenance and pins of one Bark pack. Attribution files (NOTICE,
/// LICENSE, the converter README with its modification note) install
/// beside the weights.
#[derive(Debug)]
pub struct BarkPack {
    pub id: &'static str,
    /// Converter repo and the commit every file is fetched from.
    pub repo: &'static str,
    pub revision: &'static str,
    /// Conway Research repo and the CC BY 4.0 revision it was converted from.
    pub source_repo: &'static str,
    pub source_revision: &'static str,
    /// Leading silence fed before each utterance (the variant's eval
    /// condition: it keeps the first word). Clean was evaluated as-is.
    pub leading_silence_ms: u32,
    pub files: &'static [PinnedFile],
}

const BARK_LICENSE: PinnedFile = PinnedFile {
    name: "LICENSE",
    bytes: 18_657,
    sha256: "9ba9550ad48438d0836ddab3da480b3b69ffa0aac7b7878b5a0039e7ab429411",
};
const BARK_TOKENS: PinnedFile = PinnedFile {
    name: "tokens.txt",
    bytes: 93_939,
    sha256: "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
};
// 3.0 and Plus share the decoder and joiner bit for bit.
const BARK_VERBATIM_DECODER: PinnedFile = PinnedFile {
    name: "decoder.int8.onnx",
    bytes: 47_658_545,
    sha256: "89a081f0cf1500ec64e8a45f24f0bc10d1224aa334191e7f7e6f3226365b06b8",
};
const BARK_VERBATIM_JOINER: PinnedFile = PinnedFile {
    name: "joiner.int8.onnx",
    bytes: 6_123_465,
    sha256: "b8fe34824c5d2ec077489e4b4f66907cb47a971e1d34fe96915e9e5427df3c89",
};

pub const BARK_PACKS: &[BarkPack] = &[
    BarkPack {
        id: "bark-3.0-clean",
        repo: "velddev/Underdog-Bark-3.0-Clean-sherpa-onnx",
        revision: "bdcbdca05592e01e075a7a0179fe57ef0ccb50d5",
        source_repo: "ConwayResearch/Underdog-Bark-3.0-Clean",
        source_revision: "6d34ed8639b2b9a33e764acf06748519bbb950cf",
        leading_silence_ms: 0,
        files: &[
            PinnedFile {
                name: "encoder.int8.onnx",
                bytes: 630_748_321,
                sha256: "dd8b0275d775ea664f0306764f80a6d9b357ed7d7a1fc15e8269b08b14e89c47",
            },
            PinnedFile {
                name: "decoder.int8.onnx",
                bytes: 47_658_545,
                sha256: "8fb0c48ed66cbc43855912e2e1b75c50037e9d8d628eef11bcb5712b7eab7681",
            },
            PinnedFile {
                name: "joiner.int8.onnx",
                bytes: 6_123_465,
                sha256: "2d9a8a895f38a2e132c5c4edf9de4af638c121664329f7a86c473190700c6378",
            },
            BARK_TOKENS,
            PinnedFile {
                name: "NOTICE",
                bytes: 615,
                sha256: "4e627d41fa10efedd84a7dbb915711b0b73c904af1fee8bacdd4a2387bcc5a10",
            },
            BARK_LICENSE,
            PinnedFile {
                name: "README.md",
                bytes: 8_318,
                sha256: "bd3c3697d0673deac5592e9b32c1eadc8c9abc854b9b4295ab7e84282f1307b0",
            },
        ],
    },
    BarkPack {
        id: "bark-3.0",
        repo: "velddev/Underdog-Bark-3.0-sherpa-onnx",
        revision: "f438275bc431c58010eadb982dd0f8693ff4364f",
        source_repo: "ConwayResearch/Underdog-Bark-3.0",
        source_revision: "85db2ad18ab41e534a222ee253c29191d77ed919",
        leading_silence_ms: 250,
        files: &[
            PinnedFile {
                name: "encoder.int8.onnx",
                bytes: 630_748_293,
                sha256: "d7e8f38d29aa486777eb7d505f2c7f35a0d5914760d622ed328e009cf18b9429",
            },
            BARK_VERBATIM_DECODER,
            BARK_VERBATIM_JOINER,
            BARK_TOKENS,
            PinnedFile {
                name: "NOTICE",
                bytes: 603,
                sha256: "7760825e72004ccf97f7a8b63a75e2890105a7cae8ca875b70d4df78cff9abcd",
            },
            BARK_LICENSE,
            PinnedFile {
                name: "README.md",
                bytes: 8_083,
                sha256: "ed2588449f37090ee5fda4cf7ad28b4cf509c1f65da3af1d47065b1d55d3d736",
            },
        ],
    },
    BarkPack {
        id: "bark-3.0-plus",
        repo: "velddev/Underdog-Bark-3.0-Plus-sherpa-onnx",
        revision: "5f95a80a9da9d392a38df8e9a6609e25be643d07",
        source_repo: "ConwayResearch/Underdog-Bark-3.0-Plus",
        source_revision: "d4fab2f8b95681e5ad3cfb4e8c5f4e0c009b3deb",
        leading_silence_ms: 250,
        files: &[
            PinnedFile {
                name: "encoder.int8.onnx",
                bytes: 630_748_318,
                sha256: "c32c6964e13877500701f1699d2fa92a7f6bc3cd700370ba884c09901c183a16",
            },
            BARK_VERBATIM_DECODER,
            BARK_VERBATIM_JOINER,
            BARK_TOKENS,
            PinnedFile {
                name: "NOTICE",
                bytes: 613,
                sha256: "0d4bc9bcb50fc5b239e573a7ec5765534636c13b7d312866a9b423ad0b8621cf",
            },
            BARK_LICENSE,
            PinnedFile {
                name: "README.md",
                bytes: 8_141,
                sha256: "0c155d401934704f2e5db8a07b51422cbfed009354651171220a77f586e8f7e4",
            },
        ],
    },
];

/// Bark pack for a registry id; `None` for every non-Bark id.
pub fn bark_pack(id: &str) -> Option<&'static BarkPack> {
    BARK_PACKS.iter().find(|p| p.id == id)
}

pub const BARK_MODELS: &[ModelEntry] = &[
    ModelEntry {
        id: "bark-3.0-clean",
        kind: ModelKind::Bark,
        name: "Bark 3.0 Clean",
        description: "English dictation ASR, drops ums and uhs (CC-BY-4.0)",
        size_label: "685MB",
        size_mb: 685,
        expected_bytes: Some(684_624_270),
        download_url: "https://huggingface.co/velddev/Underdog-Bark-3.0-Clean-sherpa-onnx/resolve/bdcbdca05592e01e075a7a0179fe57ef0ccb50d5",
        extract_dir: None,
        required_files: BARK_FILES,
        runtime: None,
        recommended: true,
    },
    ModelEntry {
        id: "bark-3.0",
        kind: ModelKind::Bark,
        name: "Bark 3.0",
        description: "English verbatim ASR, SOTA at its size (CC-BY-4.0)",
        size_label: "685MB",
        size_mb: 685,
        expected_bytes: Some(684_624_242),
        download_url: "https://huggingface.co/velddev/Underdog-Bark-3.0-sherpa-onnx/resolve/f438275bc431c58010eadb982dd0f8693ff4364f",
        extract_dir: None,
        required_files: BARK_FILES,
        runtime: None,
        recommended: false,
    },
    ModelEntry {
        id: "bark-3.0-plus",
        kind: ModelKind::Bark,
        name: "Bark 3.0 Plus",
        description: "English verbatim ASR, larger sibling (CC-BY-4.0)",
        size_label: "685MB",
        size_mb: 685,
        expected_bytes: Some(684_624_267),
        download_url: "https://huggingface.co/velddev/Underdog-Bark-3.0-Plus-sherpa-onnx/resolve/5f95a80a9da9d392a38df8e9a6609e25be643d07",
        extract_dir: None,
        required_files: BARK_FILES,
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

/// Supported transcription languages for one registry model.
/// Whisper reads the full upstream list; sherpa models carry their
/// own list (single-language models return just that one).
pub fn model_languages(model_id: &str) -> &'static [&'static str] {
    match model_id {
        "parakeet-tdt-0.6b-v3" | "orukeet-v0.1.0" => PARAKEET_25,
        "whistle-v1" => WHISTLE_LANGS,
        "bark-3.0" | "bark-3.0-plus" | "bark-3.0-clean" => &["en"],
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
        V::Bark => BARK_MODELS.iter().collect(),
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
            .chain(super::BARK_MODELS.iter())
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
/// Bark packs ship no `bpe.vocab`, so sherpa skips hotwords: no bias.
pub fn capabilities(entry: &ModelEntry) -> (bool, bool) {
    match entry.kind {
        ModelKind::Whisper => (false, true),
        ModelKind::Whistle => (true, true),
        ModelKind::Bark => (false, false),
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
        .chain(BARK_MODELS.iter())
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
        ModelKind::Bark => "bark",
    };
    models_dir(data_dir).join(sub).join(entry.id)
}

/// Whisper: the .bin exists and (when known) matches expected bytes.
/// Parakeet: all required files exist under the model dir.
/// Bark: the installer's `.verified` marker matches every pinned
/// file's current size + mtime (see `bark_fingerprint`). Hashes are
/// checked by the installer before the pack dir appears, so a stat
/// per file is enough here (called on every UI refresh).
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
        ModelKind::Bark => bark_pack(entry.id).is_some_and(|pack| bark_pack_verified(&dir, pack)),
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
    // Bark keeps verified files of an interrupted install for reuse.
    for dir in [bark_staging_dir(&dir), dir] {
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
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
    if entry.kind == ModelKind::Bark {
        let pack = bark_pack(entry.id).ok_or_else(|| format!("no bark pin for {}", entry.id))?;
        return download_bark_pack(
            &model_dir(data_dir, entry),
            pack,
            entry.download_url,
            BARK_READ_TIMEOUT,
            cancel,
            on_event,
        );
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
        // Returned through download_bark_pack above.
        ModelKind::Bark => return Err("bark packs use download_bark_pack".to_string()),
    }
    on_event(DownloadEvent::Done);
    Ok(())
}

/// Bark HTTP bounds: connect, and the idle gap allowed per body read
/// (reqwest blocking applies `timeout` to the headers and to each
/// `read`, not the whole body). A stalled transfer fails, and a
/// pending cancel resolves, within one read timeout.
const BARK_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const BARK_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Marker the installer writes once every pinned file hashed clean.
const BARK_MARKER: &str = ".verified";

/// `name size mtime_ns` per pinned file; `None` when one is missing or
/// has the wrong size. Any rewrite of a file (even same-size) moves its
/// mtime, so a stale marker sends the pack back through the installer,
/// which re-hashes the files on its worker thread and keeps good ones.
fn bark_fingerprint(dir: &Path, pack: &BarkPack) -> Option<String> {
    let mut out = String::new();
    for pin in pack.files {
        let meta = std::fs::metadata(dir.join(pin.name)).ok()?;
        if !meta.is_file() || meta.len() != pin.bytes {
            return None;
        }
        let mtime = meta
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        out.push_str(&format!("{} {} {mtime}\n", pin.name, pin.bytes));
    }
    Some(out)
}

fn bark_pack_verified(dir: &Path, pack: &BarkPack) -> bool {
    bark_fingerprint(dir, pack)
        .is_some_and(|f| std::fs::read_to_string(dir.join(BARK_MARKER)).is_ok_and(|m| m == f))
}

/// Record `dir`'s current files as verified (installer, and tests that
/// fake an install).
pub(crate) fn write_bark_marker(dir: &Path, pack: &BarkPack) -> Result<(), String> {
    let marker = bark_fingerprint(dir, pack).ok_or("bark marker: pack incomplete")?;
    std::fs::write(dir.join(BARK_MARKER), marker).map_err(|e| format!("bark marker: {e}"))
}

/// Leftover staged Bark files (failed or cancelled install) that no
/// running download owns; Settings offers to discard them.
pub fn has_stale_staging(data_dir: &Path, entry: &ModelEntry) -> bool {
    entry.kind == ModelKind::Bark
        && bark_staging_dir(&model_dir(data_dir, entry)).exists()
        && !download_state(entry.id)
            .is_some_and(|d| matches!(d.phase, DlPhase::Downloading | DlPhase::Installing))
}

/// Verified files of an unfinished install, beside the final dir.
fn bark_staging_dir(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    dest.with_file_name(format!(".{name}.staging"))
}

fn sha256_hex(hasher: sha2::Sha256) -> String {
    use sha2::Digest;
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Whether `path` is exactly the pinned file (size, then SHA-256).
/// A cancel mid-hash answers `false`; the caller checks the flag.
fn matches_pin(path: &Path, pin: &PinnedFile, cancel: &AtomicBool) -> bool {
    use sha2::Digest;
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == pin.bytes) {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        if cancel.load(Ordering::SeqCst) {
            return false;
        }
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return false,
        }
    }
    sha256_hex(hasher) == pin.sha256
}

/// Install a Bark pack: each pinned file streams from
/// `{base_url}/{name}` into a staging dir, must match its exact size
/// and SHA-256, and the staging dir is renamed onto `dest` only once
/// all files are verified. `.part` files are removed on every error;
/// verified files stay staged and are re-hashed before reuse.
/// Blocking; run on a worker thread.
fn download_bark_pack(
    dest: &Path,
    pack: &BarkPack,
    base_url: &str,
    read_timeout: std::time::Duration,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
) -> Result<(), String> {
    let staging = bark_staging_dir(dest);
    std::fs::create_dir_all(&staging).map_err(|e| format!("mkdir: {e}"))?;
    // A pack whose marker went stale: its files go back to staging so
    // the loop below re-hashes them and refetches only what changed.
    if dest.exists() {
        for pin in pack.files {
            let (from, to) = (dest.join(pin.name), staging.join(pin.name));
            if from.is_file() && !to.exists() {
                let _ = std::fs::rename(from, to);
            }
        }
        std::fs::remove_dir_all(dest).map_err(|e| format!("replace old install: {e}"))?;
    }
    let cancelled = || cancel.load(Ordering::SeqCst);
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(BARK_CONNECT_TIMEOUT)
        .timeout(read_timeout)
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let total = Some(pack.files.iter().map(|f| f.bytes).sum());
    let mut received = 0u64;
    for pin in pack.files {
        if cancelled() {
            return Err("cancelled".to_string());
        }
        let path = staging.join(pin.name);
        if matches_pin(&path, pin, cancel) {
            received += pin.bytes;
            on_event(DownloadEvent::Progress { received, total });
            continue;
        }
        let _ = std::fs::remove_file(&path);
        let part = staging.join(format!("{}.part", pin.name));
        let url = format!("{base_url}/{}", pin.name);
        let fetched = fetch_pinned(&client, &url, &part, pin, cancel, &mut |n| {
            received += n;
            on_event(DownloadEvent::Progress { received, total });
        })
        .and_then(|()| {
            std::fs::rename(&part, &path).map_err(|e| format!("stage {}: {e}", pin.name))
        });
        if let Err(e) = fetched {
            let _ = std::fs::remove_file(&part);
            return Err(if cancelled() {
                "cancelled".to_string()
            } else {
                e
            });
        }
    }
    on_event(DownloadEvent::Installing);
    // The rename preserves mtimes, so the marker stays valid in `dest`.
    write_bark_marker(&staging, pack)?;
    if cancelled() {
        return Err("cancelled".to_string());
    }
    std::fs::rename(&staging, dest).map_err(|e| format!("install: {e}"))?;
    on_event(DownloadEvent::Done);
    Ok(())
}

/// Stream one pinned file to `part`, failing on cancel, a byte past
/// the pinned size, a short body or a SHA-256 mismatch.
fn fetch_pinned(
    client: &reqwest::blocking::Client,
    url: &str,
    part: &Path,
    pin: &PinnedFile,
    cancel: &AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<(), String> {
    use sha2::Digest;
    let name = pin.name;
    let mut resp = client
        .get(url)
        .send()
        .map_err(|e| format!("request {name}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("http status for {name}: {}", resp.status()));
    }
    let mut out = std::fs::File::create(part).map_err(|e| format!("create {name}: {e}"))?;
    let mut hasher = sha2::Sha256::new();
    let mut written = 0u64;
    let mut buf = [0u8; 65536];
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".to_string());
        }
        let n = resp
            .read(&mut buf)
            .map_err(|e| format!("read {name}: {e}"))?;
        if n == 0 {
            break;
        }
        written += n as u64;
        if written > pin.bytes {
            return Err(format!("{name}: more than the pinned {} bytes", pin.bytes));
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])
            .map_err(|e| format!("write {name}: {e}"))?;
        on_bytes(n as u64);
    }
    out.sync_all().map_err(|e| format!("sync {name}: {e}"))?;
    if written != pin.bytes {
        return Err(format!("{name}: got {written} bytes, pinned {}", pin.bytes));
    }
    let digest = sha256_hex(hasher);
    if digest != pin.sha256 {
        return Err(format!(
            "{name}: sha256 mismatch (got {digest}, pinned {})",
            pin.sha256
        ));
    }
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
        // No bpe.vocab in the packs: sherpa ignores hotwords.
        for b in BARK_MODELS {
            assert_eq!(capabilities(b), (false, false), "{}", b.id);
        }
    }

    #[test]
    fn registry_is_complete_and_consistent() {
        assert_eq!(WHISPER_MODELS.len(), 6);
        assert_eq!(PARAKEET_MODELS.len(), 6);
        assert_eq!(BARK_MODELS.len(), 3);
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
        // Bark family: flat HF snapshots (no archive), exactly one
        // recommended default (Clean, the dictation fit).
        let mut bark_recommended = 0;
        for m in BARK_MODELS.iter() {
            assert!(m.size_mb > 0, "{}", m.id);
            assert!(m.extract_dir.is_none(), "{}", m.id);
            assert_eq!(m.kind, ModelKind::Bark);
            if m.recommended {
                bark_recommended += 1;
            }
        }
        assert_eq!(bark_recommended, 1);
        assert_eq!(BARK_PACKS.len(), BARK_MODELS.len());
    }

    #[test]
    fn bark_packs_pin_exact_revisions_sizes_and_hashes() {
        let hex = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit());
        for m in BARK_MODELS {
            let pack = bark_pack(m.id).expect(m.id);
            assert!(hex(pack.revision, 40), "{}", m.id);
            assert!(hex(pack.source_revision, 40), "{}", m.id);
            assert!(pack.source_repo.starts_with("ConwayResearch/"), "{}", m.id);
            assert_eq!(
                m.download_url,
                format!(
                    "https://huggingface.co/{}/resolve/{}",
                    pack.repo, pack.revision
                )
            );
            let names: Vec<&str> = pack.files.iter().map(|f| f.name).collect();
            for f in m
                .required_files
                .iter()
                .chain(&["NOTICE", "LICENSE", "README.md"])
            {
                assert!(names.contains(f), "{} misses {f}", m.id);
            }
            for f in pack.files {
                assert!(f.bytes > 0, "{} {}", m.id, f.name);
                assert!(hex(f.sha256, 64), "{} {}", m.id, f.name);
            }
            let runtime: u64 = pack
                .files
                .iter()
                .filter(|f| m.required_files.contains(&f.name))
                .map(|f| f.bytes)
                .sum();
            assert_eq!(m.expected_bytes, Some(runtime), "{}", m.id);
        }
        assert!(bark_pack("parakeet-tdt-0.6b-v3").is_none());
    }

    #[test]
    fn bark_install_needs_every_pinned_file_at_its_size() {
        let data = tempfile::tempdir().unwrap();
        let entry = find_model("bark-3.0").unwrap();
        let dir = model_dir(data.path(), entry);
        std::fs::create_dir_all(&dir).unwrap();
        let pack = bark_pack(entry.id).unwrap();
        for pin in pack.files {
            std::fs::File::create(dir.join(pin.name))
                .unwrap()
                .set_len(pin.bytes)
                .unwrap();
        }
        assert!(!is_installed(data.path(), entry), "no verified marker");
        write_bark_marker(&dir, pack).unwrap();
        assert!(is_installed(data.path(), entry));
        // Same size, rewritten: the mtime no longer matches the marker.
        let tokens = std::fs::OpenOptions::new()
            .write(true)
            .open(dir.join("tokens.txt"))
            .unwrap();
        tokens
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
            .unwrap();
        assert!(!is_installed(data.path(), entry), "same-size rewrite");
        write_bark_marker(&dir, pack).unwrap();
        std::fs::write(dir.join("tokens.txt"), b"").unwrap();
        assert!(!is_installed(data.path(), entry), "empty tokens.txt");
        std::fs::remove_file(dir.join("tokens.txt")).unwrap();
        assert!(!is_installed(data.path(), entry), "missing tokens.txt");
    }

    /// Loopback HTTP fixture: `GET /<name>` answers from `routes`
    /// (unknown names: 404). A `Stall` route sends its prefix and then
    /// holds the connection open without further bytes.
    enum Route {
        Body(Vec<u8>),
        Stall(Vec<u8>, u64),
    }

    fn serve(routes: Vec<(&'static str, Route)>) -> String {
        use std::io::BufRead;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes = Arc::new(routes);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let routes = routes.clone();
                std::thread::spawn(move || {
                    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                            break;
                        }
                    }
                    let route = routes.iter().find(|(n, _)| format!("/{n}") == path);
                    let (body, length, stall) = match route {
                        Some((_, Route::Body(b))) => (b.as_slice(), b.len() as u64, false),
                        Some((_, Route::Stall(b, len))) => (b.as_slice(), *len, true),
                        None => {
                            let _ = stream.write_all(
                                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            );
                            return;
                        }
                    };
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(body);
                    let _ = stream.flush();
                    if stall {
                        // Hold the socket open until the client gives up.
                        let mut sink = [0u8; 1];
                        let _ = reader.read(&mut sink);
                    }
                });
            }
        });
        base
    }

    fn pin(name: &'static str, body: &[u8]) -> PinnedFile {
        use sha2::Digest;
        PinnedFile {
            name,
            bytes: body.len() as u64,
            sha256: Box::leak(sha256_hex(sha2::Sha256::new_with_prefix(body)).into_boxed_str()),
        }
    }

    fn fixture_pack(files: Vec<PinnedFile>) -> BarkPack {
        BarkPack {
            id: "bark-fixture",
            repo: "fixture/repo",
            revision: "0000000000000000000000000000000000000000",
            source_repo: "fixture/source",
            source_revision: "0000000000000000000000000000000000000000",
            leading_silence_ms: 0,
            files: Box::leak(files.into_boxed_slice()),
        }
    }

    fn install(
        dest: &Path,
        pack: &BarkPack,
        base: &str,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        let cb: DownloadCallback = Arc::new(|_| {});
        download_bark_pack(
            dest,
            pack,
            base,
            std::time::Duration::from_secs(5),
            cancel,
            &cb,
        )
    }

    fn leftover_parts(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| n.ends_with(".part"))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn bark_installer_publishes_only_verified_packs() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("bark").join("bark-fixture");
        let staging = bark_staging_dir(&dest);
        let pack = fixture_pack(vec![
            pin("encoder.int8.onnx", b"weights"),
            pin("tokens.txt", b"a 0\n"),
        ]);
        let cancel = AtomicBool::new(false);

        // Same size, wrong bytes.
        let base = serve(vec![
            ("encoder.int8.onnx", Route::Body(b"weights".to_vec())),
            ("tokens.txt", Route::Body(b"b 0\n".to_vec())),
        ]);
        let err = install(&dest, &pack, &base, &cancel).unwrap_err();
        assert!(err.contains("sha256 mismatch"), "{err}");
        assert!(!dest.exists());
        assert!(leftover_parts(&staging).is_empty());

        // Short, empty and oversized bodies.
        for body in [&b"a "[..], b"", b"a 0\nextra"] {
            let base = serve(vec![
                ("encoder.int8.onnx", Route::Body(b"weights".to_vec())),
                ("tokens.txt", Route::Body(body.to_vec())),
            ]);
            let err = install(&dest, &pack, &base, &cancel).unwrap_err();
            assert!(err.contains("bytes"), "{err}");
            assert!(!dest.exists());
            assert!(leftover_parts(&staging).is_empty());
        }

        // 404 is an error, not an empty file.
        let base = serve(vec![(
            "encoder.int8.onnx",
            Route::Body(b"weights".to_vec()),
        )]);
        let err = install(&dest, &pack, &base, &cancel).unwrap_err();
        assert!(err.contains("404"), "{err}");
        assert!(!dest.exists());

        // The encoder verified above stays staged and is reused (the
        // server no longer has it); a corrupted staged file is refetched.
        std::fs::write(staging.join("tokens.txt"), b"bad\n").unwrap();
        let base = serve(vec![("tokens.txt", Route::Body(b"a 0\n".to_vec()))]);
        install(&dest, &pack, &base, &cancel).unwrap();
        assert_eq!(
            std::fs::read(dest.join("encoder.int8.onnx")).unwrap(),
            b"weights"
        );
        assert_eq!(std::fs::read(dest.join("tokens.txt")).unwrap(), b"a 0\n");
        assert!(!staging.exists());
        assert!(leftover_parts(&dest).is_empty());
        assert!(bark_pack_verified(&dest, &pack));

        // A flipped byte (same size) invalidates the marker; reinstall
        // re-hashes in place, keeps the encoder and refetches tokens.
        use std::os::unix::fs::FileExt;
        let tokens = std::fs::OpenOptions::new()
            .write(true)
            .open(dest.join("tokens.txt"))
            .unwrap();
        tokens.write_all_at(b"z", 0).unwrap();
        tokens
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
            .unwrap();
        assert!(!bark_pack_verified(&dest, &pack));
        let base = serve(vec![("tokens.txt", Route::Body(b"a 0\n".to_vec()))]);
        install(&dest, &pack, &base, &cancel).unwrap();
        assert_eq!(std::fs::read(dest.join("tokens.txt")).unwrap(), b"a 0\n");
        assert!(bark_pack_verified(&dest, &pack));
    }

    #[test]
    fn bark_cancel_after_a_cached_file_does_not_publish() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("bark").join("bark-fixture");
        let staging = bark_staging_dir(&dest);
        let pack = fixture_pack(vec![
            pin("encoder.int8.onnx", b"weights"),
            pin("tokens.txt", b"a 0\n"),
        ]);
        // Both files already staged and valid: no request is needed.
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("encoder.int8.onnx"), b"weights").unwrap();
        std::fs::write(staging.join("tokens.txt"), b"a 0\n").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        // Cancel lands right after the first cached file verifies.
        let cb: DownloadCallback = Arc::new(move |_| flag.store(true, Ordering::SeqCst));
        let err = download_bark_pack(
            &dest,
            &pack,
            "http://127.0.0.1:9",
            std::time::Duration::from_secs(1),
            &cancel,
            &cb,
        )
        .unwrap_err();
        assert_eq!(err, "cancelled");
        assert!(!dest.exists());
        assert!(staging.join("encoder.int8.onnx").exists());

        // Cancel after every file verified still blocks the publish.
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let cb: DownloadCallback = Arc::new(move |ev| {
            if matches!(ev, DownloadEvent::Progress { received, total } if Some(received) == total)
            {
                flag.store(true, Ordering::SeqCst);
            }
        });
        let err = download_bark_pack(
            &dest,
            &pack,
            "http://127.0.0.1:9",
            std::time::Duration::from_secs(1),
            &cancel,
            &cb,
        )
        .unwrap_err();
        assert_eq!(err, "cancelled");
        assert!(!dest.exists());
    }

    #[test]
    fn bark_cancel_resolves_during_a_stalled_body() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("bark").join("bark-fixture");
        let pack = fixture_pack(vec![pin("encoder.int8.onnx", &[7u8; 4096])]);
        let base = serve(vec![(
            "encoder.int8.onnx",
            Route::Stall(vec![7u8; 1024], 4096),
        )]);
        // Cancel lands while the worker is blocked inside the read.
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let tx = std::sync::Mutex::new(tx);
        let cb: DownloadCallback = Arc::new(move |_| {
            let _ = tx.lock().unwrap().send(());
        });
        let flag = cancel.clone();
        std::thread::spawn(move || {
            let _ = rx.recv();
            std::thread::sleep(std::time::Duration::from_millis(200));
            flag.store(true, Ordering::SeqCst);
        });
        let started = std::time::Instant::now();
        let err = download_bark_pack(
            &dest,
            &pack,
            &base,
            std::time::Duration::from_secs(1),
            &cancel,
            &cb,
        )
        .unwrap_err();
        assert_eq!(err, "cancelled");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert!(!dest.exists());
        assert!(leftover_parts(&bark_staging_dir(&dest)).is_empty());
    }

    /// Real Hugging Face round trip for all three packs (~2GB).
    /// Ignored by default: needs network. Proves the pinned URLs,
    /// sizes and hashes against upstream.
    #[test]
    #[ignore]
    fn downloads_real_bark_packs() {
        let data = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let cb: DownloadCallback = Arc::new(|_| {});
        for entry in BARK_MODELS {
            download_model(data.path(), entry, &cancel, &cb).expect(entry.id);
            assert!(is_installed(data.path(), entry), "{}", entry.id);
            for pin in bark_pack(entry.id).unwrap().files {
                assert!(matches_pin(
                    &model_dir(data.path(), entry).join(pin.name),
                    pin,
                    &cancel
                ));
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
            "bark-3.0",
            "bark-3.0-plus",
            "bark-3.0-clean",
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
