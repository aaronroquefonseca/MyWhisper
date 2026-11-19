//! Local speech-to-text: model registry, downloads, storage.
//!
//! Same model versions OpenWhispr ships (same upstreams, same files),
//! managed locally under `<data>/models/`:
//!
//! * whisper: single `ggml-*.bin` from `ggerganov/whisper.cpp` (HF)
//! * parakeet: `.tar.bz2` archives from k2-fsa sherpa-onnx releases
//!   (or the pinned Orukeet HF commit)
//! * bark: loose files of commit-pinned HF snapshots
//!
//! Nothing is preinstalled: models and runtime packs download on
//! demand with progress and cancel through one installer (`Asset`):
//! every installed file is pinned to exact size + SHA-256, staged, and
//! published by one rename once all of them verified. The server
//! processes are the next layer, not this module.

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
    /// The archive itself, or the base each file is fetched from as
    /// `{download_url}/{name}`. Hugging Face URLs pin a commit.
    pub download_url: &'static str,
    /// Every installed file (what the engine loads, plus license and
    /// attribution files), each pinned to exact size + SHA-256.
    pub files: &'static [PinnedFile],
    /// Pinned archive at `download_url` the files are extracted from.
    pub archive: Option<Archive>,
    pub runtime: Option<ParakeetRuntime>,
    pub recommended: bool,
}

/// One file of a pinned install: exact size and SHA-256 (the Hugging
/// Face LFS oid or GitHub release digest where upstream publishes one,
/// hashed locally from the pinned upload otherwise).
#[derive(Debug, Clone, Copy)]
pub struct PinnedFile {
    pub name: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

/// A pinned release archive. Only members `member` maps to a pinned
/// file name are written; everything else in the archive is skipped.
#[derive(Debug, Clone, Copy)]
pub struct Archive {
    pub file: PinnedFile,
    pub member: fn(&str) -> Option<&str>,
}

/// Member path below the archive's single top dir.
fn below_top(path: &str) -> Option<&str> {
    path.trim_start_matches("./")
        .split_once('/')
        .map(|(_, rest)| rest)
}

const fn pin(name: &'static str, bytes: u64, sha256: &'static str) -> PinnedFile {
    PinnedFile {
        name,
        bytes,
        sha256,
    }
}

const fn tar_bz2(name: &'static str, bytes: u64, sha256: &'static str) -> Archive {
    Archive {
        file: pin(name, bytes, sha256),
        member: below_top,
    }
}

/// whisper.cpp ggml models at a fixed commit (LFS oids as pins).
const WHISPER_BASE_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1";

pub const WHISPER_MODELS: &[ModelEntry] = &[
    ModelEntry {
        id: "tiny",
        kind: ModelKind::Whisper,
        name: "Tiny",
        description: "Fastest, lower quality",
        size_label: "75MB",
        size_mb: 75,
        download_url: WHISPER_BASE_URL,
        files: &[pin(
            "ggml-tiny.bin",
            77_691_713,
            "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
        )],
        archive: None,
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
        download_url: WHISPER_BASE_URL,
        files: &[pin(
            "ggml-base.bin",
            147_951_465,
            "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
        )],
        archive: None,
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
        download_url: WHISPER_BASE_URL,
        files: &[pin(
            "ggml-small.bin",
            487_601_967,
            "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
        )],
        archive: None,
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
        download_url: WHISPER_BASE_URL,
        files: &[pin(
            "ggml-medium.bin",
            1_533_763_059,
            "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
        )],
        archive: None,
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
        download_url: WHISPER_BASE_URL,
        files: &[pin(
            "ggml-large-v3.bin",
            3_095_033_483,
            "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2",
        )],
        archive: None,
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
        download_url: WHISPER_BASE_URL,
        files: &[pin(
            "ggml-large-v3-turbo.bin",
            1_624_555_275,
            "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
        )],
        archive: None,
        runtime: None,
        recommended: false,
    },
];

/// Cactus Whistle (own engine, CLI-driven): 16.9 MB, 7 languages
/// with auto-detect, up to 30 s per pass.
pub const WHISTLE_LANGS: &[&str] = &["en", "de", "fr", "es", "it", "nl", "pl"];

pub const WHISTLE_MODELS: &[ModelEntry] = &[ModelEntry {
    id: "whistle-v1",
    kind: ModelKind::Whistle,
    name: "Whistle v1",
    description: "16.9 MB STT, 7 languages, own CPU engine",
    size_label: "17MB",
    size_mb: 17,
    download_url: "https://huggingface.co/Cactus-Compute/whistle/resolve/88cb00eeb8928ddbcd410b491584f938ba030b07",
    files: &[pin(
        "whistle.cact",
        16_919_407,
        "b6e02f048568ac5d01a2042556c658061e699acbc0aa2a1439f52f3d461dffeb",
    )],
    archive: None,
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
        download_url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2",
        files: &[
            pin(
                "encoder.int8.onnx",
                652_282_294,
                "1afbe69c1ffdabee94755f42e4daf1d93549131a3da11356bda84fcef0f2c479",
            ),
            pin(
                "decoder.int8.onnx",
                11_845_274,
                "0222a86147299d4dee90edf95b6d13827edb42ad8b7c9cf0b2582f030937c0b5",
            ),
            pin(
                "joiner.int8.onnx",
                6_355_277,
                "cbd02f3696e9944970111fb439d9c3fd365a80374565f13f2b9c7007d253fc98",
            ),
            pin(
                "tokens.txt",
                93_939,
                "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
            ),
            // Build upstream replaced in place on 2026-11-07 (HF mirror
            // csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8@2bda32ec).
            pin(
                "encoder.int8.onnx",
                652_184_281,
                "acfc2b4456377e15d04f0243af540b7fe7c992f8d898d751cf134c3a55fd2247",
            ),
            pin(
                "decoder.int8.onnx",
                11_845_275,
                "179e50c43d1a9de79c8a24149a2f9bac6eb5981823f2a2ed88d655b24248db4e",
            ),
            pin(
                "joiner.int8.onnx",
                6_355_277,
                "3164c13fc2821009440d20fcb5fdc78bff28b4db2f8d0f0b329101719c0948b3",
            ),
        ],
        archive: Some(tar_bz2(
            "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2",
            487_211_443,
            "1495e91b59d7fc53ddca91f3431758ec99f8fa253fdb0e1e851be0f72b14b0f9",
        )),
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
        download_url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming.tar.bz2",
        files: &[
            pin(
                "encoder.int8.onnx",
                654_040_552,
                "6716910b7a0833997fec7a410494c995d70124001a0e9b66d6370d6aced577e0",
            ),
            pin(
                "decoder.int8.onnx",
                7_257_753,
                "a5e223392c90e75f8144cdb5eb95af7625db389e39edef2bd1a9c872b3298fe6",
            ),
            pin(
                "joiner.int8.onnx",
                1_735_860,
                "869f43f7d24595c55581ad3bf249a935fb8a71389fbdaa7504b9f46f93140f8a",
            ),
            pin(
                "tokens.txt",
                8_952,
                "dc0b4584ab2e4ddbf888425c076c61b736e7356a015250db7d307e6f1a8188ff",
            ),
        ],
        archive: Some(tar_bz2(
            "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming.tar.bz2",
            501_350_460,
            "99f63605b3a85a54c250c0869670a687b7d6598a47bf2421515e1f839a76e150",
        )),
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
        download_url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25.tar.bz2",
        files: &[
            pin(
                "encoder.int8.onnx",
                652_916_849,
                "7d932213491ad355c6e5576705dc3494731a52af87d7a1b954559340147909d8",
            ),
            pin(
                "decoder.int8.onnx",
                7_257_753,
                "0be9702c2f427a2b6bb241d298e0d3836a558de1f5b9fd3018f1cce6e2b3fa98",
            ),
            pin(
                "joiner.int8.onnx",
                1_735_862,
                "a35eac38a22ebceb04d230ed7afe0d68f446ba6914a036b97f14fece95967e23",
            ),
            pin(
                "tokens.txt",
                8_952,
                "dc0b4584ab2e4ddbf888425c076c61b736e7356a015250db7d307e6f1a8188ff",
            ),
        ],
        archive: Some(tar_bz2(
            "sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25.tar.bz2",
            463_945_051,
            "78e2b79fcf7271553a74402a76b771b09ea40117a39566a79f52235b23db6358",
        )),
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
        download_url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-560ms-int8-2026-06-11.tar.bz2",
        files: &[
            pin(
                "encoder.int8.onnx",
                657_601_403,
                "012e9321373af99021415e0b0eb3ec827b4be3153be6f30d9b448fe65e896e68",
            ),
            pin(
                "decoder.int8.onnx",
                14_978_075,
                "19f9c98fc6d0a2c33a65a43b36fdb2e914c26c0aa9764be3aebc502a1e982fb0",
            ),
            pin(
                "joiner.int8.onnx",
                9_504_438,
                "4101c7c679a0bc30483794b27a059e34e79232aa2068d78d51231a22c8b0d7ce",
            ),
            pin(
                "tokens.txt",
                131_440,
                "729cc103155bafa785f9cd45746cd41cabe97eab7182fc04d594129587958f8a",
            ),
        ],
        archive: Some(tar_bz2(
            "sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-560ms-int8-2026-06-11.tar.bz2",
            475_271_763,
            "c6bf5e0df765f9d5b43bc9e0536d4b4b3e7d40bdf5ecf13e45f134c51c05ae3a",
        )),
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
        download_url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-cohere-transcribe-14-lang-int8-2026-04-01.tar.bz2",
        files: &[
            pin(
                "encoder.int8.onnx",
                3_090_822,
                "cf704f8cfa90e3f0a76f9ffc05998bdf00ba9ae983192c14a85a3a5eb008b367",
            ),
            pin(
                "encoder.int8.onnx.data",
                2_731_503_072,
                "bcf1b7148c8518ae52df1ad2d2fc2b4e89261ea23e6c874eef1d9f55bcbaa4a3",
            ),
            pin(
                "decoder.int8.onnx",
                153_250_705,
                "8372ca6c8ff4db8b916ca3592f5c757a715e691b9edec751ba19b29fc854baf9",
            ),
            pin(
                "tokens.txt",
                207_437,
                "013ede043ae2480e3a9205cc34550d9686100cc682bacc90f702facdfbb93035",
            ),
        ],
        archive: Some(tar_bz2(
            "sherpa-onnx-cohere-transcribe-14-lang-int8-2026-04-01.tar.bz2",
            1_699_791_751,
            "bd582588d50685a795dcd2807ab77e11361b8312d96c53884682def45ab4206d",
        )),
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
        download_url: "https://huggingface.co/oruk/orukeet/resolve/55a984d46f68323301837194ce647c702f55facc/onnx/sherpa-onnx-orukeet-v0.1.0-int8.tar.bz2",
        files: &[
            pin(
                "encoder.int8.onnx",
                653_182_378,
                "7b55f2a504a20a8e462899f5befd45f4a1784948d76ed0127902d9cf39405487",
            ),
            pin(
                "decoder.int8.onnx",
                11_845_332,
                "c185c2afb4c77c94bb1314807ecb3dc1623057a3dc540b83e10301af9bf4cfca",
            ),
            pin(
                "joiner.int8.onnx",
                6_355_335,
                "1a7e90abf7172d926dd7e2edac2a5d5035c24dfb641a15e131d57b6a5f63cdd3",
            ),
            pin(
                "tokens.txt",
                93_939,
                "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
            ),
            pin(
                "bpe.vocab",
                117_408,
                "41d5e71b3591642eff088151efd7acd4e750124cc0054c8ba9fa3245187a4804",
            ),
            pin(
                "LICENSE-WEIGHTS",
                20_137,
                "23ee78c8bae49cf08ea2f0c84945c66b987ebe4520881fb51b3dad4fb43d07c2",
            ),
            pin(
                "NOTICE.md",
                5_271,
                "440361d963edd9621e744f251332b47f2c4de2e2594ecfe42b215e3f6223fa44",
            ),
        ],
        archive: Some(tar_bz2(
            "sherpa-onnx-orukeet-v0.1.0-int8.tar.bz2",
            486_807_585,
            "f9191f30178cc9122ce2f023bf9fefafc822028307b0efa4caff645ba3fe8d0a",
        )),
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
/// Provenance of one Bark pack. Its pinned files (`ModelEntry::files`)
/// include the attribution files (NOTICE, LICENSE, the converter
/// README with its modification note), installed beside the weights.
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

const BARK_CLEAN_FILES: &[PinnedFile] = &[
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
];
const BARK_VERBATIM_FILES: &[PinnedFile] = &[
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
];
const BARK_PLUS_FILES: &[PinnedFile] = &[
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
];

pub const BARK_PACKS: &[BarkPack] = &[
    BarkPack {
        id: "bark-3.0-clean",
        repo: "velddev/Underdog-Bark-3.0-Clean-sherpa-onnx",
        revision: "bdcbdca05592e01e075a7a0179fe57ef0ccb50d5",
        source_repo: "ConwayResearch/Underdog-Bark-3.0-Clean",
        source_revision: "6d34ed8639b2b9a33e764acf06748519bbb950cf",
        leading_silence_ms: 0,
    },
    BarkPack {
        id: "bark-3.0",
        repo: "velddev/Underdog-Bark-3.0-sherpa-onnx",
        revision: "f438275bc431c58010eadb982dd0f8693ff4364f",
        source_repo: "ConwayResearch/Underdog-Bark-3.0",
        source_revision: "85db2ad18ab41e534a222ee253c29191d77ed919",
        leading_silence_ms: 250,
    },
    BarkPack {
        id: "bark-3.0-plus",
        repo: "velddev/Underdog-Bark-3.0-Plus-sherpa-onnx",
        revision: "5f95a80a9da9d392a38df8e9a6609e25be643d07",
        source_repo: "ConwayResearch/Underdog-Bark-3.0-Plus",
        source_revision: "d4fab2f8b95681e5ad3cfb4e8c5f4e0c009b3deb",
        leading_silence_ms: 250,
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
        download_url: "https://huggingface.co/velddev/Underdog-Bark-3.0-Clean-sherpa-onnx/resolve/bdcbdca05592e01e075a7a0179fe57ef0ccb50d5",
        files: BARK_CLEAN_FILES,
        archive: None,
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
        download_url: "https://huggingface.co/velddev/Underdog-Bark-3.0-sherpa-onnx/resolve/f438275bc431c58010eadb982dd0f8693ff4364f",
        files: BARK_VERBATIM_FILES,
        archive: None,
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
        download_url: "https://huggingface.co/velddev/Underdog-Bark-3.0-Plus-sherpa-onnx/resolve/5f95a80a9da9d392a38df8e9a6609e25be643d07",
        files: BARK_PLUS_FILES,
        archive: None,
        runtime: None,
        recommended: false,
    },
];

/// Pinned upstream Qwen3-ASR model snapshots.
pub const QWEN_MODELS: &[ModelEntry] = &[
    ModelEntry {
        id: "qwen3-asr-1.7b",
        kind: ModelKind::Qwen,
        name: "Qwen3-ASR 1.7B",
        description: "LLM-based multilingual ASR, 30 languages + dialects",
        size_label: "4.7GB",
        size_mb: 4700,
        download_url: "https://huggingface.co/Qwen/Qwen3-ASR-1.7B/resolve/7278e1e70fe206f11671096ffdd38061171dd6e5",
        files: &[
            pin(
                "config.json",
                6_194,
                "2e74a751548b8ad7d7526d29365ad8144c345d8b412b1152d25dc6698452712f",
            ),
            pin(
                "generation_config.json",
                142,
                "1da527824d81e07118facff437e03f2e24a23311e3bdeb2368973fe77e5f275c",
            ),
            pin(
                "preprocessor_config.json",
                330,
                "45e120a4eda2c20c5d7f2ea9354e63536bf35e27aa573fb7cdf78017b378770d",
            ),
            pin(
                "tokenizer_config.json",
                12_487,
                "4942d005604266809309cabc9f4e9cb89ce855d59b14681fdc0e1cc62ea26c4c",
            ),
            pin(
                "vocab.json",
                2_776_833,
                "ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910",
            ),
            pin(
                "merges.txt",
                1_671_853,
                "8831e4f1a044471340f7c0a83d7bd71306a5b867e95fd870f74d0c5308a904d5",
            ),
            pin(
                "chat_template.json",
                1_161,
                "75a8cfca24f00de72d796fbfed6858fc9614ef3dabd8696684cc3bc03a9c58ff",
            ),
            pin(
                "model.safetensors.index.json",
                64_821,
                "f994739fe38e5210b9e3e8ce6c6307315e2ceac3cb630e7b7414d69dce520f60",
            ),
            pin(
                "model-00001-of-00002.safetensors",
                4_220_320_824,
                "a4cd1f1a04d90b757dc7f7dd26254e69a013b19e80efe590a83c6a3bde8608d6",
            ),
            pin(
                "model-00002-of-00002.safetensors",
                478_200_688,
                "6e0b9d9e09e2e0238e7ef3cc8a484ab387e91b90f1900bedf88bc92d7929ccfc",
            ),
        ],
        archive: None,
        runtime: None,
        recommended: true,
    },
    ModelEntry {
        id: "qwen3-asr-0.6b",
        kind: ModelKind::Qwen,
        name: "Qwen3-ASR 0.6B",
        description: "Lightweight LLM-based multilingual ASR, 30 languages + dialects",
        size_label: "1.9GB",
        size_mb: 1900,
        download_url: "https://huggingface.co/Qwen/Qwen3-ASR-0.6B/resolve/5eb144179a02acc5e5ba31e748d22b0cf3e303b0",
        files: &[
            pin(
                "config.json",
                6_193,
                "76d3ae4601ce939830b2517f4a6cadb86cc51316c3900af6b020b051c21a478c",
            ),
            pin(
                "generation_config.json",
                142,
                "1da527824d81e07118facff437e03f2e24a23311e3bdeb2368973fe77e5f275c",
            ),
            pin(
                "preprocessor_config.json",
                330,
                "45e120a4eda2c20c5d7f2ea9354e63536bf35e27aa573fb7cdf78017b378770d",
            ),
            pin(
                "tokenizer_config.json",
                12_487,
                "4942d005604266809309cabc9f4e9cb89ce855d59b14681fdc0e1cc62ea26c4c",
            ),
            pin(
                "vocab.json",
                2_776_833,
                "ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910",
            ),
            pin(
                "merges.txt",
                1_671_853,
                "8831e4f1a044471340f7c0a83d7bd71306a5b867e95fd870f74d0c5308a904d5",
            ),
            pin(
                "chat_template.json",
                1_161,
                "75a8cfca24f00de72d796fbfed6858fc9614ef3dabd8696684cc3bc03a9c58ff",
            ),
            pin(
                "model.safetensors",
                1_876_091_704,
                "79d6cbd4c98c7bbffe9db2edac07f56cd6637d0d5944b27f6c2b8353840323ea",
            ),
        ],
        archive: None,
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
const QWEN_30: &[&str] = &[
    "zh", "en", "yue", "ar", "de", "fr", "es", "pt", "id", "it", "ko", "ru", "th", "vi", "ja",
    "tr", "hi", "ms", "nl", "sv", "da", "fi", "pl", "cs", "fil", "fa", "el", "hu", "mk", "ro",
];

pub fn model_languages(model_id: &str) -> &'static [&'static str] {
    match model_id {
        "qwen3-asr-1.7b" | "qwen3-asr-0.6b" => QWEN_30,
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
            .chain(super::BARK_MODELS.iter())
            .chain(super::QWEN_MODELS.iter())
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
        .chain(BARK_MODELS.iter())
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
        ModelKind::Bark => "bark",
        ModelKind::Qwen => "qwen",
    };
    models_dir(data_dir).join(sub).join(entry.id)
}

/// Every pinned file present, verified by the installer (see
/// `Asset::installed`). A stat per file, so the GTK thread may call it.
pub fn is_installed(data_dir: &Path, entry: &ModelEntry) -> bool {
    model_asset(data_dir, entry).installed()
}

pub fn whisper_file_stem(id: &str) -> &str {
    match id {
        "large" => "large-v3",
        "turbo" => "large-v3-turbo",
        other => other,
    }
}

/// Remove an installed model and any staged files of an unfinished
/// install. Refused while a download or verification owns them.
pub fn delete_model(data_dir: &Path, entry: &ModelEntry) -> std::io::Result<()> {
    model_asset(data_dir, entry).delete()
}

// ---------------------------------------------------------------- install

#[derive(Debug, Clone)]
pub enum DownloadEvent {
    Progress { received: u64, total: Option<u64> },
    Installing,
    Done,
}

pub type DownloadCallback = Arc<dyn Fn(DownloadEvent) + Send + Sync + 'static>;

/// Download + install one registry model. Blocking; run on a worker
/// thread. See `Asset::install`.
pub fn download_model(
    data_dir: &Path,
    entry: &ModelEntry,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
) -> Result<(), String> {
    model_asset(data_dir, entry).install(READ_TIMEOUT, cancel, on_event)
}

/// HTTP bounds: connect, and the idle gap allowed per body read
/// (reqwest blocking applies `timeout` to the headers and to each
/// `read`, not the whole body). A stalled transfer fails, and a
/// pending cancel resolves, within one read timeout.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Marker the installer writes once every pinned file hashed clean.
const MARKER: &str = ".verified";

/// One installable (model or runtime pack): where it lives, where its
/// pinned files come from, and how they are checked.
struct Asset {
    id: &'static str,
    data_dir: PathBuf,
    dir: PathBuf,
    url: String,
    archive: Option<Archive>,
    files: &'static [PinnedFile],
    /// Runtime binaries and libraries: installed 0755.
    executable: bool,
    /// Old installers wrote no marker; see `installed`.
    legacy: bool,
    /// Sherpa can use a vocabulary supplied by an earlier install.
    legacy_vocab: bool,
}

fn model_asset(data_dir: &Path, entry: &ModelEntry) -> Asset {
    Asset {
        id: entry.id,
        data_dir: data_dir.to_path_buf(),
        dir: model_dir(data_dir, entry),
        url: entry.download_url.to_string(),
        archive: entry.archive,
        files: entry.files,
        executable: false,
        // Bark shipped with its marker.
        legacy: entry.kind != ModelKind::Bark,
        legacy_vocab: entry.kind == ModelKind::Parakeet,
    }
}

fn asset_lock_dir(data_dir: &Path) -> std::io::Result<PathBuf> {
    // Clear Cache and Reset All remove models/ and bin/. Keep the lock
    // files outside both so every process continues to open the same inodes.
    let locks = data_dir.join(".asset-locks");
    crate::private_fs::directory(&locks)?;
    Ok(locks)
}

#[derive(Debug)]
pub(crate) struct AssetLock(std::fs::File);

impl Drop for AssetLock {
    fn drop(&mut self) {
        // A concurrent fork can hold a copy of the descriptor until
        // exec. Unlock now so that copy cannot delay the next operation.
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

pub(crate) fn try_lock_download_cache(data_dir: &Path) -> std::io::Result<AssetLock> {
    crate::private_fs::try_lock(&asset_lock_dir(data_dir)?.join("cache.lock")).map(AssetLock)
}

fn lock_asset_change(data_dir: &Path, id: &str) -> std::io::Result<(AssetLock, AssetLock)> {
    let locks = asset_lock_dir(data_dir)?;
    let cache = AssetLock(crate::private_fs::try_lock_shared(
        &locks.join("cache.lock"),
    )?);
    let asset = AssetLock(crate::private_fs::try_lock(
        &locks.join(format!("{id}.lock")),
    )?);
    Ok((asset, cache))
}

/// First pin per name. A name listed twice is a file upstream replaced
/// in place; either build verifies.
fn unique(files: &[PinnedFile]) -> impl Iterator<Item = &PinnedFile> {
    files
        .iter()
        .enumerate()
        .filter(|(i, f)| !files[..*i].iter().any(|g| g.name == f.name))
        .map(|(_, f)| f)
}

/// `name size mtime_ns ctime_ns` per pinned file; `None` when one is
/// missing or has no pinned size. Any write moves ctime (even one that
/// restores mtime), so a stale marker sends the install back through
/// the installer, which re-hashes the files on its worker thread and
/// keeps good ones.
fn fingerprint(dir: &Path, files: &[PinnedFile]) -> Option<String> {
    let mut out = String::new();
    for pin in unique(files) {
        let meta = std::fs::metadata(dir.join(pin.name)).ok()?;
        let bytes = meta.len();
        if !meta.is_file() || !files.iter().any(|f| f.name == pin.name && f.bytes == bytes) {
            return None;
        }
        let mtime = meta
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        // ctime can't be set by userspace; renaming the parent dir keeps it.
        #[cfg(unix)]
        let ctime = {
            use std::os::unix::fs::MetadataExt;
            (meta.ctime(), meta.ctime_nsec())
        };
        #[cfg(not(unix))]
        let ctime = (0i64, 0i64);
        out.push_str(&format!(
            "{} {bytes} {mtime} {}.{}\n",
            pin.name, ctime.0, ctime.1
        ));
    }
    Some(out)
}

/// Record `dir`'s current files as verified (installer, and tests that
/// fake an install).
pub(crate) fn write_marker(dir: &Path, files: &[PinnedFile]) -> Result<(), String> {
    let marker = fingerprint(dir, files).ok_or("marker: install incomplete")?;
    std::fs::write(dir.join(MARKER), marker).map_err(|e| format!("marker: {e}"))
}

impl Asset {
    /// The marker matches every pinned file's current size, mtime and
    /// ctime. Hashes are checked before the marker is written, so a
    /// stat per file is enough here (called on every UI refresh).
    /// An install from before markers existed has none: it counts at
    /// its pinned sizes until `spawn_legacy_verification` hashes it.
    fn installed(&self) -> bool {
        match std::fs::read_to_string(self.dir.join(MARKER)) {
            Ok(marker) => fingerprint(&self.dir, self.files).is_some_and(|f| f == marker),
            Err(e) => {
                self.legacy
                    && e.kind() == std::io::ErrorKind::NotFound
                    && fingerprint(&self.dir, self.files).is_some()
            }
        }
    }

    fn staging(&self) -> PathBuf {
        staging_dir(&self.dir)
    }

    fn lock_for_change(&self) -> std::io::Result<(AssetLock, AssetLock)> {
        lock_asset_change(&self.data_dir, self.id)
    }

    fn recover_interrupted_parts(&self) -> std::io::Result<()> {
        let staging = self.staging();
        let has_staging = || staging.symlink_metadata().is_ok_and(|meta| meta.is_dir());
        if !has_staging() {
            return Ok(());
        }
        let _locks = self.lock_for_change()?;
        if !has_staging() {
            return Ok(());
        }
        let mut partials: Vec<_> = unique(self.files)
            .map(|pin| staging.join(format!("{}.part", pin.name)))
            .collect();
        if self.archive.is_some() {
            partials.push(staging.join(".archive.part"));
        }
        remove_interrupted_parts(&staging, &partials)
    }

    fn delete(&self) -> std::io::Result<()> {
        // A stale Delete button must not remove files a running job owns.
        if busy(self.id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "download in progress",
            ));
        }
        let _locks = self.lock_for_change()?;
        for dir in [self.staging(), self.dir.clone()] {
            if dir.exists() {
                std::fs::remove_dir_all(&dir)?;
            }
        }
        Ok(())
    }

    /// Install: pinned files stream into a staging dir (each from
    /// `{url}/{name}`, or extracted from the pinned archive at `url`),
    /// must match their exact size and SHA-256, and the staging dir is
    /// renamed onto `dir` only once all of them verified. `.part`
    /// files are removed on every error; verified files stay staged
    /// and are re-hashed before reuse. Blocking; run on a worker thread.
    fn install(
        &self,
        read_timeout: std::time::Duration,
        cancel: &AtomicBool,
        on_event: &DownloadCallback,
    ) -> Result<(), String> {
        let cancelled = || cancel.load(Ordering::SeqCst);
        let cancel_err = || "cancelled".to_string();
        if cancelled() {
            return Err(cancel_err());
        }
        let _locks = self
            .lock_for_change()
            .map_err(|e| format!("asset lock: {e}"))?;
        if self.dir.join(MARKER).is_file() && self.installed() {
            on_event(DownloadEvent::Done);
            return Ok(());
        }
        let staging = self.staging();
        std::fs::create_dir_all(&staging).map_err(|e| format!("mkdir: {e}"))?;
        if !staging.symlink_metadata().is_ok_and(|m| m.is_dir()) {
            return Err("staging is not a directory".to_string());
        }
        let paths: Vec<_> = unique(self.files).map(|p| staging.join(p.name)).collect();
        prune_staging(&staging, &paths).map_err(|e| format!("clean staging: {e}"))?;
        let total = match &self.archive {
            Some(a) => a.file.bytes,
            None => unique(self.files).map(|f| f.bytes).sum(),
        };
        let mut received = 0u64;
        let mut progress = |n: u64| {
            received += n;
            on_event(DownloadEvent::Progress {
                received,
                total: Some(total),
            });
        };
        let mut missing = Vec::new();
        for pin in unique(self.files) {
            if cancelled() {
                return Err(cancel_err());
            }
            let path = staging.join(pin.name);
            if !verified(&path, pin.name, self.files, cancel) {
                // A cancel mid-hash is not a mismatch: keep the staged file.
                if cancelled() {
                    return Err(cancel_err());
                }
                remove_if_present(&path)
                    .map_err(|e| format!("discard staged {}: {e}", pin.name))?;
                if !self.reuse_live_file(pin.name, &path, cancel)? {
                    if cancelled() {
                        return Err(cancel_err());
                    }
                    remove_if_present(&path)
                        .map_err(|e| format!("discard staged {}: {e}", pin.name))?;
                    missing.push(pin);
                    continue;
                }
            }
            if self.archive.is_none() {
                progress(pin.bytes);
            }
        }
        if !missing.is_empty() {
            let client = reqwest::blocking::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .timeout(read_timeout)
                .build()
                .map_err(|e| format!("http client: {e}"))?;
            let fetched = match &self.archive {
                None => missing.iter().try_for_each(|pin| {
                    let part = staging.join(format!("{}.part", pin.name));
                    let url = format!("{}/{}", self.url, pin.name);
                    fetch_pinned(
                        &client,
                        &url,
                        &part,
                        pin.name,
                        std::slice::from_ref(*pin),
                        cancel,
                        &mut progress,
                    )
                    .and_then(|()| {
                        std::fs::rename(&part, staging.join(pin.name))
                            .map_err(|e| format!("stage {}: {e}", pin.name))
                    })
                    .inspect_err(|_| {
                        let _ = std::fs::remove_file(&part);
                    })
                }),
                Some(archive) => {
                    let part = staging.join(".archive.part");
                    let pinned = std::slice::from_ref(&archive.file);
                    let name = archive.file.name;
                    let result = fetch_pinned(
                        &client,
                        &self.url,
                        &part,
                        name,
                        pinned,
                        cancel,
                        &mut progress,
                    )
                    .and_then(|()| {
                        on_event(DownloadEvent::Installing);
                        extract_pinned(&part, archive, self.files, &missing, &staging, cancel)
                    });
                    let cleanup = remove_if_present(&part);
                    result.and(cleanup.map_err(|e| format!("discard archive: {e}")))
                }
            };
            fetched.map_err(|e| if cancelled() { cancel_err() } else { e })?;
        }
        on_event(DownloadEvent::Installing);
        #[cfg(unix)]
        if self.executable {
            use std::os::unix::fs::PermissionsExt;
            for pin in unique(self.files) {
                std::fs::set_permissions(
                    staging.join(pin.name),
                    std::fs::Permissions::from_mode(0o755),
                )
                .map_err(|e| format!("chmod {}: {e}", pin.name))?;
            }
        }
        // Keep the optional local vocabulary used for Sherpa hotwords.
        // A pinned vocabulary always comes from the verified install.
        if self.legacy_vocab && !self.files.iter().any(|pin| pin.name == "bpe.vocab") {
            let live = self.dir.join("bpe.vocab");
            match live.metadata() {
                Ok(meta) if meta.is_file() => {
                    std::fs::copy(&live, staging.join("bpe.vocab"))
                        .map_err(|e| format!("preserve bpe.vocab: {e}"))?;
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("inspect bpe.vocab: {e}")),
            }
        }
        // The dir rename keeps the files' times, so the marker stays valid.
        write_marker(&staging, self.files)?;
        sync_install_tree(&staging, cancel).map_err(|e| {
            if cancelled() {
                cancel_err()
            } else {
                format!("sync staging: {e}")
            }
        })?;
        // A first install can create the model/runtime parent directories.
        // Persist their entries before publishing into the immediate parent.
        for ancestor in staging
            .ancestors()
            .skip(2)
            .take_while(|dir| dir.starts_with(&self.data_dir))
        {
            open_install_dir(ancestor)
                .and_then(|dir| dir.sync_all())
                .map_err(|e| format!("sync install ancestors: {e}"))?;
        }
        if cancelled() {
            return Err(cancel_err());
        }
        publish_install(&staging, &self.dir, std::fs::File::sync_all)
            .map_err(|e| format!("install: {e}"))?;
        on_event(DownloadEvent::Done);
        Ok(())
    }

    /// Copy a candidate into staging so a failed or cancelled repair
    /// leaves the live file and its metadata intact.
    fn reuse_live_file(
        &self,
        name: &str,
        staged: &Path,
        cancel: &AtomicBool,
    ) -> Result<bool, String> {
        let live = self.dir.join(name);
        if cancel.load(Ordering::SeqCst)
            || !live.metadata().is_ok_and(|m| {
                m.is_file()
                    && self
                        .files
                        .iter()
                        .any(|p| p.name == name && p.bytes == m.len())
            })
        {
            return Ok(false);
        }
        std::fs::create_dir_all(staged.parent().unwrap())
            .and_then(|()| std::fs::copy(&live, staged))
            .map_err(|e| format!("stage {name} from live install: {e}"))?;
        Ok(verified(staged, name, self.files, cancel))
    }

    /// Hash an install that has no marker yet in place. On mismatch,
    /// record that it needs repair without moving any files.
    fn adopt(&self, cancel: &AtomicBool, on_event: &DownloadCallback) -> Result<(), String> {
        let _locks = self
            .lock_for_change()
            .map_err(|e| format!("asset lock: {e}"))?;
        on_event(DownloadEvent::Installing);
        let before = fingerprint(&self.dir, self.files).ok_or("install incomplete")?;
        for pin in unique(self.files) {
            if verified(&self.dir.join(pin.name), pin.name, self.files, cancel) {
                continue;
            }
            if cancel.load(Ordering::SeqCst) {
                return Err("cancelled".to_string());
            }
            std::fs::write(self.dir.join(MARKER), "verification failed\n")
                .map_err(|e| format!("marker: {e}"))?;
            return Err(format!(
                "{} failed verification; download it again",
                pin.name
            ));
        }
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".to_string());
        }
        // Hashes only vouch for the files as they were when hashed.
        if fingerprint(&self.dir, self.files).as_ref() != Some(&before) {
            return Err("files changed during verification".to_string());
        }
        write_marker(&self.dir, self.files)
    }
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Keep pinned file paths and their parent directories. A retry may
/// find partial downloads or a marker left by an interrupted run.
fn prune_staging(dir: &Path, pinned: &[PathBuf]) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ty = entry.file_type()?;
        if ty.is_dir() {
            if pinned.iter().any(|p| p != &path && p.starts_with(&path)) {
                prune_staging(&path, pinned)?;
            } else {
                std::fs::remove_dir_all(&path)?;
            }
        } else if !ty.is_file() || !pinned.contains(&path) {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Startup cleanup only removes regular files at known partial paths.
/// Completed stage members remain available for verification on retry.
fn remove_interrupted_parts(dir: &Path, partials: &[PathBuf]) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() && partials.iter().any(|part| part.starts_with(&path)) {
            remove_interrupted_parts(&path, partials)?;
        } else if kind.is_file() && partials.contains(&path) {
            remove_if_present(&path)?;
        }
    }
    Ok(())
}

fn open_install_dir(path: &Path) -> std::io::Result<std::fs::File> {
    let path = if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    };
    std::fs::File::open(path)
}

fn sync_install_tree(dir: &Path, cancel: &AtomicBool) -> std::io::Result<()> {
    for item in std::fs::read_dir(dir)? {
        if cancel.load(Ordering::SeqCst) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "cancelled",
            ));
        }
        let item = item?;
        if item.file_type()?.is_dir() {
            sync_install_tree(&item.path(), cancel)?;
        } else {
            crate::private_fs::open_regular(&item.path())?.sync_all()?;
        }
    }
    open_install_dir(dir)?.sync_all()
}

fn exchange_install(left: &Path, right: &Path) -> std::io::Result<()> {
    let left = std::ffi::CString::new(left.as_os_str().as_encoded_bytes())?;
    let right = std::ffi::CString::new(right.as_os_str().as_encoded_bytes())?;
    // SAFETY: Both pointers refer to live, NUL-terminated paths. Exchange
    // commits both renames together or leaves both directories untouched.
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            left.as_ptr(),
            libc::AT_FDCWD,
            right.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn publish_install(
    staging: &Path,
    dest: &Path,
    sync_parent: impl FnOnce(&std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let parent = open_install_dir(
        dest.parent()
            .ok_or_else(|| std::io::Error::other("install directory has no parent"))?,
    )?;
    let replacing = dest.try_exists()?;
    if replacing {
        exchange_install(staging, dest)?;
    } else {
        std::fs::rename(staging, dest)?;
    }
    if let Err(error) = sync_parent(&parent) {
        // Until the new name is durable, keep the old tree intact.
        let rollback = if replacing {
            exchange_install(staging, dest)
        } else {
            std::fs::rename(dest, staging)
        };
        rollback
            .and_then(|()| parent.sync_all())
            .map_err(|rollback| {
                std::io::Error::other(format!(
                    "sync install: {error}; restore prior state: {rollback}"
                ))
            })?;
        return Err(error);
    }
    // The previous install is now at staging. Cleanup is retryable.
    if replacing && let Err(error) = std::fs::remove_dir_all(staging) {
        tracing::warn!(%error, path = %staging.display(), "could not remove replaced install");
    }
    Ok(())
}

/// Leftover staged files (failed or cancelled install) that no running
/// download owns; Settings offers to discard them.
pub fn has_stale_staging(data_dir: &Path, entry: &ModelEntry) -> bool {
    model_asset(data_dir, entry).staging().exists() && !busy(entry.id)
}

/// Verified files of an unfinished install, beside the final dir.
fn staging_dir(dest: &Path) -> PathBuf {
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

/// Whether `path` is exactly one of `name`'s pins (size, then SHA-256).
/// A cancel mid-hash answers `false`; the caller checks the flag.
fn verified(path: &Path, name: &str, files: &[PinnedFile], cancel: &AtomicBool) -> bool {
    use sha2::Digest;
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let pins: Vec<_> = files
        .iter()
        .filter(|f| f.name == name && f.bytes == meta.len())
        .collect();
    if !meta.is_file() || pins.is_empty() {
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
    let digest = sha256_hex(hasher);
    pins.iter().any(|p| p.sha256 == digest)
}

/// Stream one pinned file to `part`, failing on cancel or any HTTP
/// error status, then as `copy_pinned`.
fn fetch_pinned(
    client: &reqwest::blocking::Client,
    url: &str,
    part: &Path,
    name: &str,
    files: &[PinnedFile],
    cancel: &AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<(), String> {
    let mut resp = client
        .get(url)
        .send()
        .map_err(|e| format!("request {name}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("http status for {name}: {}", resp.status()));
    }
    copy_pinned(&mut resp, part, name, files, cancel, on_bytes)
}

/// Copy `src` to `part`, failing on cancel, a byte past the pinned
/// size, a short body or a SHA-256 that matches no pin of `name`.
fn copy_pinned(
    src: &mut dyn Read,
    part: &Path,
    name: &str,
    files: &[PinnedFile],
    cancel: &AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<(), String> {
    use sha2::Digest;
    let pins: Vec<_> = files.iter().filter(|f| f.name == name).collect();
    let limit = pins.iter().map(|p| p.bytes).max().unwrap_or(0);
    if let Some(parent) = part.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {name}: {e}"))?;
    }
    let mut out = std::fs::File::create(part).map_err(|e| format!("create {name}: {e}"))?;
    let mut hasher = sha2::Sha256::new();
    let mut written = 0u64;
    let mut buf = [0u8; 65536];
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".to_string());
        }
        let n = src
            .read(&mut buf)
            .map_err(|e| format!("read {name}: {e}"))?;
        if n == 0 {
            break;
        }
        written += n as u64;
        if written > limit {
            return Err(format!("{name}: more than the pinned {limit} bytes"));
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])
            .map_err(|e| format!("write {name}: {e}"))?;
        on_bytes(n as u64);
    }
    out.sync_all().map_err(|e| format!("sync {name}: {e}"))?;
    let sized: Vec<_> = pins.iter().filter(|p| p.bytes == written).collect();
    if sized.is_empty() {
        return Err(format!("{name}: got {written} bytes, pinned {limit}"));
    }
    let digest = sha256_hex(hasher);
    if !sized.iter().any(|p| p.sha256 == digest) {
        return Err(format!(
            "{name}: sha256 mismatch (got {digest}, pinned {})",
            sized[0].sha256
        ));
    }
    Ok(())
}

/// Write the pinned members of a verified archive into `staging`, each
/// checked like a download. Links, other entry types and unpinned
/// members are skipped, so no archive path is ever written as given.
fn extract_pinned(
    path: &Path,
    archive: &Archive,
    files: &[PinnedFile],
    missing: &[&PinnedFile],
    staging: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut done: Vec<&str> = Vec::new();
    let mut take = |member: &str, src: &mut dyn Read| -> Result<(), String> {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".to_string());
        }
        let Some(pin) =
            (archive.member)(member).and_then(|name| unique(files).find(|f| f.name == name))
        else {
            return Ok(());
        };
        if done.contains(&pin.name) {
            return Err(format!("archive repeats {}", pin.name));
        }
        done.push(pin.name);
        if !missing.iter().any(|p| p.name == pin.name) {
            return Ok(());
        }
        let part = staging.join(format!("{}.part", pin.name));
        copy_pinned(src, &part, pin.name, files, cancel, &mut |_| {})
            .and_then(|()| {
                std::fs::rename(&part, staging.join(pin.name))
                    .map_err(|e| format!("stage {}: {e}", pin.name))
            })
            .inspect_err(|_| {
                let _ = std::fs::remove_file(&part);
            })?;
        Ok(())
    };
    let file = std::fs::File::open(path).map_err(|e| format!("open archive: {e}"))?;
    if archive.file.name.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("open zip: {e}"))?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|e| format!("zip entry: {e}"))?;
            if entry.is_file() {
                let member = entry
                    .name()
                    .map_err(|e| format!("zip entry: {e}"))?
                    .to_string();
                take(&member, &mut entry)?;
            }
        }
    } else {
        let mut tar = tar::Archive::new(bzip2::read::BzDecoder::new(file));
        for entry in tar.entries().map_err(|e| format!("extract: {e}"))? {
            let mut entry = entry.map_err(|e| format!("extract: {e}"))?;
            if entry.header().entry_type().is_file() {
                let member = entry
                    .path()
                    .map_err(|e| format!("extract: {e}"))?
                    .to_string_lossy()
                    .into_owned();
                take(&member, &mut entry)?;
            } else if cancel.load(Ordering::SeqCst) {
                return Err("cancelled".to_string());
            }
        }
    }
    let lacking: Vec<_> = unique(files)
        .map(|f| f.name)
        .filter(|n| !done.contains(n))
        .collect();
    if lacking.is_empty() {
        Ok(())
    } else {
        Err(format!("archive lacks {lacking:?}"))
    }
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

/// A download or verification of `id` is running.
fn busy(id: &str) -> bool {
    download_state(id)
        .is_some_and(|d| matches!(d.phase, DlPhase::Downloading | DlPhase::Installing))
}

type Job = Box<dyn FnOnce(&AtomicBool, &DownloadCallback) -> Result<(), String> + Send>;

/// Register a job for `id` (check and insert under one lock); `None`
/// when one is already running.
fn claim(id: &'static str, total: Option<u64>) -> Option<Arc<AtomicBool>> {
    let mut map = downloads().lock().ok()?;
    if map
        .get(id)
        .is_some_and(|(s, _)| matches!(s.phase, DlPhase::Downloading | DlPhase::Installing))
    {
        return None;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let snap = DlSnapshot {
        received: 0,
        total,
        phase: DlPhase::Downloading,
    };
    map.insert(id.to_string(), (snap, cancel.clone()));
    Some(cancel)
}

/// Run a claimed job, mirroring its events into the state map: removed
/// on success, `Failed` on error.
fn run_claimed(id: &'static str, cancel: &AtomicBool, job: Job) {
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
    let result = job(cancel, &cb);
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
}

/// Start a job for `id` on its own thread. No-op if one is running.
fn start_job(id: &'static str, total: Option<u64>, job: Job) {
    let Some(cancel) = claim(id, total) else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name(format!("dl-{id}"))
        .spawn(move || run_claimed(id, &cancel, job));
    if let Err(e) = spawned {
        run_claimed(
            id,
            &AtomicBool::new(false),
            Box::new(move |_, _| Err(format!("thread: {e}"))),
        );
    }
}

/// Start a download on a worker thread. No-op if one is running.
pub fn start_download(data_dir: PathBuf, id: &'static str) {
    let Some(entry) = find_model(id) else {
        return;
    };
    start_job(
        id,
        None,
        Box::new(move |cancel, cb| download_model(&data_dir, entry, cancel, cb)),
    );
}

/// Cancel an in-flight download.
pub fn cancel_download(id: &str) {
    if let Ok(map) = downloads().lock()
        && let Some((_, cancel)) = map.get(id)
    {
        cancel.store(true, Ordering::SeqCst);
    }
}

/// Every installable with its asset view (models, then runtime packs).
fn all_assets(data_dir: &Path) -> Vec<Asset> {
    let models = WHISPER_MODELS
        .iter()
        .chain(PARAKEET_MODELS)
        .chain(WHISTLE_MODELS)
        .chain(BARK_MODELS)
        .chain(QWEN_MODELS)
        .map(|m| model_asset(data_dir, m));
    let gpu = GPU_PACKS.iter().map(|p| gpu_asset(data_dir, p));
    let sherpa = SHERPA_PACKS.iter().map(|p| sherpa_asset(data_dir, p));
    models
        .chain(gpu)
        .chain(sherpa)
        .chain(needle_asset(data_dir).ok())
        .collect()
}

/// Clean interrupted download parts and verify pre-marker installs on
/// a worker thread. A retry reuses completed stage files that still verify.
pub fn spawn_legacy_verification(
    data_dir: PathBuf,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("verify-installs".to_string())
        .spawn(move || {
            for asset in all_assets(&data_dir) {
                if let Err(error) = asset.recover_interrupted_parts()
                    && error.kind() != std::io::ErrorKind::WouldBlock
                {
                    tracing::warn!(%error, asset = asset.id, "could not clean interrupted download");
                }
                if !asset.legacy || asset.dir.join(MARKER).exists() || !asset.installed() {
                    continue;
                }
                if let Some(cancel) = claim(asset.id, None) {
                    let id = asset.id;
                    run_claimed(id, &cancel, Box::new(move |c, cb| asset.adopt(c, cb)));
                }
            }
        })
}

#[cfg(test)]
/// Sparse files at the pinned sizes plus a marker: a verified install
/// of `id` (model or runtime pack) for tests.
pub(crate) fn fake_install(data_dir: &Path, id: &str) -> PathBuf {
    fake_install_with_script(data_dir, id, None)
}

#[cfg(test)]
pub(crate) fn fake_install_with_script(
    data_dir: &Path,
    id: &str,
    executable: Option<(&str, &str)>,
) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let asset = all_assets(data_dir)
        .into_iter()
        .find(|a| a.id == id)
        .expect(id);
    for pin in unique(asset.files) {
        let path = asset.dir.join(pin.name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        if let Some((name, script)) = executable.filter(|(name, _)| *name == pin.name) {
            let mut contents = script.as_bytes().to_vec();
            assert!(contents.len() <= pin.bytes as usize, "{name}");
            contents.resize(pin.bytes as usize, b'\n');
            std::fs::write(&path, &contents).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        } else {
            std::fs::File::create(path)
                .unwrap()
                .set_len(pin.bytes)
                .unwrap();
        }
    }
    write_marker(&asset.dir, asset.files).unwrap();
    asset.dir
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the sherpa transducer servers load (`--encoder` etc.).
    const TRANSDUCER: &[&str] = &[
        "encoder.int8.onnx",
        "decoder.int8.onnx",
        "joiner.int8.onnx",
        "tokens.txt",
    ];

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
                assert!(m.archive.is_some(), "{}", m.id);
            }
        }
        // Exactly one recommended default per family.
        assert_eq!(recommended, 2);
        // Bark family: flat HF snapshots (no archive), exactly one
        // recommended default (Clean, the dictation fit).
        let mut bark_recommended = 0;
        for m in BARK_MODELS.iter() {
            assert!(m.size_mb > 0, "{}", m.id);
            assert!(m.archive.is_none(), "{}", m.id);
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
            let names: Vec<&str> = m.files.iter().map(|f| f.name).collect();
            for f in TRANSDUCER.iter().chain(&["NOTICE", "LICENSE", "README.md"]) {
                assert!(names.contains(f), "{} misses {f}", m.id);
            }
            for f in m.files {
                assert!(f.bytes > 0, "{} {}", m.id, f.name);
                assert!(hex(f.sha256, 64), "{} {}", m.id, f.name);
            }
        }
        assert!(bark_pack("parakeet-tdt-0.6b-v3").is_none());
    }

    #[test]
    fn bark_install_needs_every_pinned_file_at_its_size() {
        let data = tempfile::tempdir().unwrap();
        let entry = find_model("bark-3.0").unwrap();
        let dir = model_dir(data.path(), entry);
        std::fs::create_dir_all(&dir).unwrap();
        for pin in entry.files {
            std::fs::File::create(dir.join(pin.name))
                .unwrap()
                .set_len(pin.bytes)
                .unwrap();
        }
        assert!(!is_installed(data.path(), entry), "no verified marker");
        write_marker(&dir, entry.files).unwrap();
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
        write_marker(&dir, entry.files).unwrap();
        std::fs::write(dir.join("tokens.txt"), b"").unwrap();
        assert!(!is_installed(data.path(), entry), "empty tokens.txt");
        std::fs::remove_file(dir.join("tokens.txt")).unwrap();
        assert!(!is_installed(data.path(), entry), "missing tokens.txt");
    }

    /// Loopback HTTP fixture: `GET /<name>` answers from `routes`
    /// (unknown names: 404). A `Stall` route sends its prefix and then
    /// holds the connection open without further bytes; `Cut` closes
    /// it after the prefix instead.
    enum Route {
        Body(Vec<u8>),
        Stall(Vec<u8>, u64),
        Cut(Vec<u8>, u64),
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
                        Some((_, Route::Cut(b, len))) => (b.as_slice(), *len, false),
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

    fn pin_body(name: &'static str, body: &[u8]) -> PinnedFile {
        use sha2::Digest;
        PinnedFile {
            name,
            bytes: body.len() as u64,
            sha256: Box::leak(sha256_hex(sha2::Sha256::new_with_prefix(body)).into_boxed_str()),
        }
    }

    fn fixture_pack(files: Vec<PinnedFile>) -> &'static [PinnedFile] {
        Box::leak(files.into_boxed_slice())
    }

    /// Loose-file asset (Bark, whisper, whistle, needle layout).
    fn loose(dest: &Path, base: &str, files: &'static [PinnedFile]) -> Asset {
        Asset {
            id: "fixture",
            data_dir: dest.parent().unwrap().to_path_buf(),
            dir: dest.to_path_buf(),
            url: base.to_string(),
            archive: None,
            files,
            executable: false,
            legacy: false,
            legacy_vocab: false,
        }
    }

    fn install(
        dest: &Path,
        pack: &'static [PinnedFile],
        base: &str,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        let cb: DownloadCallback = Arc::new(|_| {});
        loose(dest, base, pack).install(std::time::Duration::from_secs(5), cancel, &cb)
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
    fn install_sync_failure_restores_previous_directory_and_allows_retry() {
        for replacing in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let dest = root.path().join("live");
            let staging = staging_dir(&dest);
            std::fs::create_dir(&staging).unwrap();
            std::fs::write(staging.join("model"), "new").unwrap();
            if replacing {
                std::fs::create_dir(&dest).unwrap();
                std::fs::write(dest.join("model"), "old").unwrap();
            }
            let error = publish_install(&staging, &dest, |_| {
                assert_eq!(std::fs::read_to_string(dest.join("model")).unwrap(), "new");
                Err(std::io::Error::other("injected parent sync failure"))
            })
            .unwrap_err();
            assert_eq!(error.to_string(), "injected parent sync failure");
            if replacing {
                assert_eq!(std::fs::read_to_string(dest.join("model")).unwrap(), "old");
            } else {
                assert!(!dest.exists());
            }
            assert_eq!(
                std::fs::read_to_string(staging.join("model")).unwrap(),
                "new"
            );
            publish_install(&staging, &dest, std::fs::File::sync_all).unwrap();
            assert_eq!(std::fs::read_to_string(dest.join("model")).unwrap(), "new");
            assert!(!staging.exists());
        }
    }

    #[test]
    fn delete_rejects_an_install_that_owns_the_staging_directory() {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().to_path_buf();
        let dest = root.path().join("fixture");
        let pack = fixture_pack(vec![pin_body("weights.bin", b"weights")]);
        let asset = loose(&dest, "", pack);
        let same_asset = loose(&dest, "", pack);
        std::fs::create_dir_all(asset.staging()).unwrap();
        std::fs::write(asset.staging().join("weights.bin"), b"weights").unwrap();
        let cb: DownloadCallback = Arc::new(move |event| {
            if matches!(event, DownloadEvent::Installing) {
                assert_eq!(
                    same_asset.delete().unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
                assert_eq!(
                    try_lock_download_cache(&data_dir).unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
            }
        });
        asset
            .install(
                std::time::Duration::from_secs(1),
                &AtomicBool::new(false),
                &cb,
            )
            .unwrap();
        assert!(asset.installed());
        let clear_cache = try_lock_download_cache(root.path()).unwrap();
        assert_eq!(
            asset.delete().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(
            asset.lock_for_change().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        let inherited_handle = clear_cache.0.try_clone().unwrap();
        drop(clear_cache);
        asset.delete().unwrap();
        drop(inherited_handle);
        assert!(!asset.installed());
    }

    #[test]
    fn bark_installer_publishes_only_verified_packs() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("bark").join("bark-fixture");
        let staging = staging_dir(&dest);
        let pack = fixture_pack(vec![
            pin_body("encoder.int8.onnx", b"weights"),
            pin_body("tokens.txt", b"a 0\n"),
        ]);
        let cancel = AtomicBool::new(false);

        // Same size, wrong bytes.
        let base = serve(vec![
            ("encoder.int8.onnx", Route::Body(b"weights".to_vec())),
            ("tokens.txt", Route::Body(b"b 0\n".to_vec())),
        ]);
        let err = install(&dest, pack, &base, &cancel).unwrap_err();
        assert!(err.contains("sha256 mismatch"), "{err}");
        assert!(!dest.exists());
        assert!(leftover_parts(&staging).is_empty());

        // Short, empty and oversized bodies.
        for body in [&b"a "[..], b"", b"a 0\nextra"] {
            let base = serve(vec![
                ("encoder.int8.onnx", Route::Body(b"weights".to_vec())),
                ("tokens.txt", Route::Body(body.to_vec())),
            ]);
            let err = install(&dest, pack, &base, &cancel).unwrap_err();
            assert!(err.contains("bytes"), "{err}");
            assert!(!dest.exists());
            assert!(leftover_parts(&staging).is_empty());
        }

        // 404 is an error, not an empty file.
        let base = serve(vec![(
            "encoder.int8.onnx",
            Route::Body(b"weights".to_vec()),
        )]);
        let err = install(&dest, pack, &base, &cancel).unwrap_err();
        assert!(err.contains("404"), "{err}");
        assert!(!dest.exists());

        // The encoder verified above stays staged and is reused (the
        // server no longer has it); a corrupted staged file is refetched.
        std::fs::write(staging.join("tokens.txt"), b"bad\n").unwrap();
        let base = serve(vec![("tokens.txt", Route::Body(b"a 0\n".to_vec()))]);
        install(&dest, pack, &base, &cancel).unwrap();
        assert_eq!(
            std::fs::read(dest.join("encoder.int8.onnx")).unwrap(),
            b"weights"
        );
        assert_eq!(std::fs::read(dest.join("tokens.txt")).unwrap(), b"a 0\n");
        assert!(!staging.exists());
        assert!(leftover_parts(&dest).is_empty());
        assert!(loose(&dest, "", pack).installed());

        // A flipped byte (same size, original mtime restored) still
        // invalidates the marker through ctime; reinstall re-hashes in
        // place, keeps the encoder and refetches tokens.
        use std::os::unix::fs::FileExt;
        let mtime = std::fs::metadata(dest.join("tokens.txt"))
            .unwrap()
            .modified()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let tokens = std::fs::OpenOptions::new()
            .write(true)
            .open(dest.join("tokens.txt"))
            .unwrap();
        tokens.write_all_at(b"z", 0).unwrap();
        tokens.set_modified(mtime).unwrap();
        assert!(!loose(&dest, "", pack).installed());
        let base = serve(vec![("tokens.txt", Route::Body(b"a 0\n".to_vec()))]);
        install(&dest, pack, &base, &cancel).unwrap();
        assert_eq!(std::fs::read(dest.join("tokens.txt")).unwrap(), b"a 0\n");
        assert!(loose(&dest, "", pack).installed());
    }

    #[test]
    fn bark_cancel_after_a_cached_file_does_not_publish() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("bark").join("bark-fixture");
        let staging = staging_dir(&dest);
        let pack = fixture_pack(vec![
            pin_body("encoder.int8.onnx", b"weights"),
            pin_body("tokens.txt", b"a 0\n"),
        ]);
        // Both files already staged and valid: no request is needed.
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("encoder.int8.onnx"), b"weights").unwrap();
        std::fs::write(staging.join("tokens.txt"), b"a 0\n").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        // Cancel lands right after the first cached file verifies.
        let cb: DownloadCallback = Arc::new(move |_| flag.store(true, Ordering::SeqCst));
        let err = loose(&dest, "http://127.0.0.1:9", pack)
            .install(std::time::Duration::from_secs(1), &cancel, &cb)
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
        let err = loose(&dest, "http://127.0.0.1:9", pack)
            .install(std::time::Duration::from_secs(1), &cancel, &cb)
            .unwrap_err();
        assert_eq!(err, "cancelled");
        assert!(!dest.exists());
    }

    #[test]
    fn bark_cancel_resolves_during_a_stalled_body() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("bark").join("bark-fixture");
        let pack = fixture_pack(vec![pin_body("encoder.int8.onnx", &[7u8; 4096])]);
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
        let err = loose(&dest, &base, pack)
            .install(std::time::Duration::from_secs(1), &cancel, &cb)
            .unwrap_err();
        assert_eq!(err, "cancelled");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert!(!dest.exists());
        assert!(leftover_parts(&staging_dir(&dest)).is_empty());
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
            for pin in entry.files {
                let path = model_dir(data.path(), entry).join(pin.name);
                assert!(verified(&path, pin.name, entry.files, &cancel));
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

    /// Real round trip of the small pinned assets (needle, Whistle,
    /// sherpa CPU pack; ~45MB). Ignored by default: needs network.
    /// Proves their URLs, archive members, sizes and hashes upstream.
    #[test]
    #[ignore]
    fn downloads_small_assets() {
        let data = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let cb: DownloadCallback = Arc::new(|_| {});
        download_needle_pack(data.path(), &cancel, &cb).expect("needle");
        assert!(needle_installed(data.path()));
        let whistle = find_model("whistle-v1").unwrap();
        download_model(data.path(), whistle, &cancel, &cb).expect("whistle");
        assert!(is_installed(data.path(), whistle));
        let pack = SHERPA_PACKS.iter().find(|p| p.id == "sherpa-cpu").unwrap();
        download_sherpa_pack(data.path(), pack, &cancel, &cb).expect("sherpa-cpu");
        assert!(sherpa_installed(data.path(), pack));
        let sherpa = sherpa_asset(data.path(), pack);
        assert_eq!(
            std::fs::read_dir(sherpa.dir.join("lib")).unwrap().count(),
            1
        );
        std::fs::remove_file(sherpa.dir.join(MARKER)).unwrap();
        assert!(
            sherpa.installed(),
            "legacy pack with only runtime dependencies"
        );
        sherpa.adopt(&cancel, &cb).unwrap();
        assert!(sherpa.installed());
    }

    #[test]
    fn whisper_stems_map_to_filenames() {
        assert_eq!(whisper_file_stem("base"), "base");
        assert_eq!(whisper_file_stem("large"), "large-v3");
        assert_eq!(whisper_file_stem("turbo"), "large-v3-turbo");
    }

    /// Every advertised model and runtime pack pins each installed file
    /// (and its archive) to a size and SHA-256, and fetches from a
    /// fixed revision wherever the host has one (GitHub release tags
    /// are names, not commits: those archives are pinned by hash).
    #[test]
    fn every_asset_pins_exact_files() {
        let hex = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit());
        let data = tempfile::tempdir().unwrap();
        let assets = all_assets(data.path());
        assert_eq!(
            assets.len(),
            WHISPER_MODELS.len()
                + PARAKEET_MODELS.len()
                + WHISTLE_MODELS.len()
                + BARK_MODELS.len()
                + QWEN_MODELS.len()
                + GPU_PACKS.len()
                + SHERPA_PACKS.len()
                + 1
        );
        for a in &assets {
            assert!(!a.files.is_empty(), "{}", a.id);
            for f in a.files.iter().chain(a.archive.as_ref().map(|x| &x.file)) {
                assert!(f.bytes > 0 && hex(f.sha256, 64), "{} {}", a.id, f.name);
                assert!(
                    !f.name.contains("..") && !f.name.starts_with('/'),
                    "{}",
                    f.name
                );
            }
            if a.url.starts_with("https://huggingface.co/") {
                let rev = a.url.split("/resolve/").nth(1).unwrap();
                assert!(hex(&rev[..40], 40), "{} fetches a moving ref", a.id);
            } else {
                assert!(a.url.starts_with("https://github.com/"), "{}", a.id);
                assert!(a.url.ends_with(a.archive.unwrap().file.name), "{}", a.id);
            }
            // Old installers wrote no marker; Bark's always did.
            assert_eq!(a.legacy, !a.id.starts_with("bark-"), "{}", a.id);
        }
        // What each engine loads is among its pinned files.
        let names = |files: &[PinnedFile]| files.iter().map(|f| f.name).collect::<Vec<_>>();
        for m in PARAKEET_MODELS.iter().chain(BARK_MODELS) {
            let want: &[&str] = if m.id == "cohere-transcribe-03-2026" {
                &[
                    "encoder.int8.onnx",
                    "encoder.int8.onnx.data",
                    "decoder.int8.onnx",
                    "tokens.txt",
                ]
            } else {
                TRANSDUCER
            };
            for f in want {
                assert!(names(m.files).contains(f), "{} misses {f}", m.id);
            }
        }
        for m in WHISPER_MODELS {
            let bin = format!("ggml-{}.bin", whisper_file_stem(m.id));
            assert_eq!(names(m.files), [bin.as_str()]);
        }
        for p in SHERPA_PACKS {
            for b in p.binaries {
                assert!(names(p.files).contains(b), "{} misses {b}", p.id);
            }
            assert!(
                names(p.files).contains(&"lib/libonnxruntime.so"),
                "{}",
                p.id
            );
        }
        for p in GPU_PACKS {
            assert_eq!(names(p.files), ["whisper-server"]);
        }
        // Orukeet keeps its CC-BY-SA notices and the BPE vocabulary
        // that enables hotwords.
        let oruk = find_model("orukeet-v0.1.0").unwrap();
        for f in ["bpe.vocab", "LICENSE-WEIGHTS", "NOTICE.md"] {
            assert!(names(oruk.files).contains(&f), "{f}");
        }
    }

    fn tar_bz2_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        let mut tar = tar::Builder::new(enc);
        for (name, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            // Raw name bytes: the builder itself refuses `..` paths.
            header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
            header.set_cksum();
            tar.append(&header, *body).unwrap();
        }
        // A link member must never be followed or written.
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        tar.append_link(&mut link, "top/tokens.txt.link", "/etc/passwd")
            .unwrap();
        tar.into_inner().unwrap().finish().unwrap()
    }

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, body) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(body).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    /// Archive asset (parakeet, cohere, orukeet, sherpa/whisper packs).
    fn archived(
        dest: &Path,
        base: &str,
        archive: Archive,
        files: &'static [PinnedFile],
        executable: bool,
    ) -> Asset {
        Asset {
            url: format!("{base}/{}", archive.file.name),
            archive: Some(archive),
            executable,
            legacy_vocab: !executable,
            ..loose(dest, base, files)
        }
    }

    fn noop() -> DownloadCallback {
        Arc::new(|_| {})
    }

    #[test]
    fn archive_installs_publish_only_verified_pinned_members() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("parakeet").join("fixture");
        let staging = staging_dir(&dest);
        let files = fixture_pack(vec![
            pin_body("encoder.int8.onnx", b"weights"),
            pin_body("tokens.txt", b"a 0\n"),
        ]);
        let good = tar_bz2_of(&[
            ("top/encoder.int8.onnx", b"weights"),
            ("top/tokens.txt", b"a 0\n"),
            ("top/test_wavs/0.wav", b"RIFF"),
            ("top/../../escape.txt", b"x"),
        ]);
        let archive_of = |body: &[u8]| Archive {
            file: pin_body("model.tar.bz2", body),
            member: below_top,
        };
        let cancel = AtomicBool::new(false);
        let timeout = std::time::Duration::from_secs(5);
        let run = |asset: &Asset| asset.install(timeout, &cancel, &noop());

        // The archive itself is pinned: same size, other bytes.
        let mut forged = good.clone();
        let last = forged.len() - 1;
        forged[last] ^= 1;
        let base = serve(vec![("model.tar.bz2", Route::Body(forged))]);
        let err = run(&archived(&dest, &base, archive_of(&good), files, false)).unwrap_err();
        assert!(err.contains("sha256 mismatch"), "{err}");
        assert!(!dest.exists());
        assert!(!staging.join(".archive.part").exists());

        // A body cut short mid-transfer leaves no partial file.
        let base = serve(vec![(
            "model.tar.bz2",
            Route::Cut(good[..good.len() / 2].to_vec(), good.len() as u64),
        )]);
        let err = run(&archived(&dest, &base, archive_of(&good), files, false)).unwrap_err();
        assert!(err.contains("model.tar.bz2"), "{err}");
        assert!(!dest.exists());
        assert!(std::fs::read_dir(&staging).unwrap().next().is_none());

        // Archive verifies, but a member is not the pinned file.
        let wrong = tar_bz2_of(&[
            ("top/encoder.int8.onnx", b"weightz"),
            ("top/tokens.txt", b"a 0\n"),
        ]);
        let base = serve(vec![("model.tar.bz2", Route::Body(wrong.clone()))]);
        let err = run(&archived(&dest, &base, archive_of(&wrong), files, false)).unwrap_err();
        assert!(err.contains("encoder.int8.onnx: sha256 mismatch"), "{err}");
        assert!(!dest.exists());
        assert!(leftover_parts(&staging).is_empty());

        // A pinned member missing from the archive.
        let short = tar_bz2_of(&[("top/encoder.int8.onnx", b"weights")]);
        let base = serve(vec![("model.tar.bz2", Route::Body(short.clone()))]);
        let err = run(&archived(&dest, &base, archive_of(&short), files, false)).unwrap_err();
        assert!(err.contains("archive lacks"), "{err}");
        assert!(!dest.exists());

        // Good archive: only the pinned files are published.
        let base = serve(vec![("model.tar.bz2", Route::Body(good.clone()))]);
        let asset = archived(&dest, &base, archive_of(&good), files, false);
        run(&asset).unwrap();
        assert!(asset.installed());
        assert_eq!(std::fs::read(dest.join("tokens.txt")).unwrap(), b"a 0\n");
        assert!(!dest.join("test_wavs").exists());
        assert!(!dest.join("tokens.txt.link").exists());
        assert!(!root.path().join("escape.txt").exists());
        assert!(!staging.exists());

        // A bad staged file with the same name must not hide a good live
        // one. Failed repair must leave both live files and the marker.
        let offline = archived(&dest, "http://127.0.0.1:9", archive_of(&good), files, false);
        std::fs::write(dest.join("tokens.txt"), b"b 0\n").unwrap();
        std::fs::write(dest.join("bpe.vocab"), b"legacy vocab").unwrap();
        assert!(!asset.installed());
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("encoder.int8.onnx"), b"weightz").unwrap();
        let marker = std::fs::read(dest.join(MARKER)).unwrap();
        assert!(run(&offline).is_err());
        assert_eq!(std::fs::read(dest.join("tokens.txt")).unwrap(), b"b 0\n");
        assert_eq!(std::fs::read(dest.join(MARKER)).unwrap(), marker);
        let staged_encoder = staging.join("encoder.int8.onnx");
        assert_eq!(std::fs::read(&staged_encoder).unwrap(), b"weights");

        // Repair extracts only the missing member and publishes the
        // exact staged encoder that already verified.
        use std::os::unix::fs::MetadataExt;
        let encoder_inode = staged_encoder.metadata().unwrap().ino();
        run(&asset).unwrap();
        assert_eq!(
            dest.join("encoder.int8.onnx").metadata().unwrap().ino(),
            encoder_inode
        );
        assert_eq!(std::fs::read(dest.join("tokens.txt")).unwrap(), b"a 0\n");
        assert_eq!(
            std::fs::read(dest.join("bpe.vocab")).unwrap(),
            b"legacy vocab"
        );
        std::fs::remove_file(dest.join(MARKER)).unwrap();
        assert!(
            !offline.installed(),
            "bark-style asset: no marker, no install"
        );
        // Interrupted extraction left valid members and a partial archive.
        // Cached members let the retry skip the download entirely.
        std::fs::create_dir(&staging).unwrap();
        std::fs::copy(dest.join("encoder.int8.onnx"), &staged_encoder).unwrap();
        std::fs::copy(dest.join("tokens.txt"), staging.join("tokens.txt")).unwrap();
        std::fs::write(staging.join(".archive.part"), b"incomplete").unwrap();
        std::fs::write(staging.join("tokens.txt.part"), b"incomplete").unwrap();
        std::fs::create_dir(staging.join("unexpected")).unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let cancel_on_publish = flag.clone();
        let cb: DownloadCallback = Arc::new(move |event| {
            if matches!(event, DownloadEvent::Installing) {
                cancel_on_publish.store(true, Ordering::SeqCst);
            }
        });
        assert_eq!(
            offline.install(timeout, &flag, &cb).unwrap_err(),
            "cancelled"
        );
        assert_eq!(
            dest.join("encoder.int8.onnx").metadata().unwrap().ino(),
            encoder_inode
        );
        run(&offline).unwrap();
        assert!(offline.installed());
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 4);
        assert!(!staging.exists());
    }

    #[test]
    fn precancelled_tiny_reinstall_keeps_verified_model() {
        let data = tempfile::tempdir().unwrap();
        let entry = find_model("tiny").unwrap();
        let dir = fake_install(data.path(), entry.id);
        let marker = std::fs::read(dir.join(MARKER)).unwrap();
        let err = download_model(data.path(), entry, &AtomicBool::new(true), &noop()).unwrap_err();
        assert_eq!(err, "cancelled");
        assert!(is_installed(data.path(), entry));
        assert_eq!(std::fs::read(dir.join(MARKER)).unwrap(), marker);
        assert!(!has_stale_staging(data.path(), entry));
    }

    #[test]
    fn cancel_during_extraction_does_not_install() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("bin").join("fixture");
        let files = fixture_pack(vec![pin_body("whisper-server", b"\x7fELF")]);
        let body = zip_of(&[("whisper-server-linux-x64-cpu", b"\x7fELF")]);
        let archive = Archive {
            file: pin_body("server.zip", &body),
            member: whisper_server,
        };
        let base = serve(vec![("server.zip", Route::Body(body))]);
        let asset = archived(&dest, &base, archive, files, true);
        // Cancel lands once the archive verified, as extraction starts.
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let cb: DownloadCallback = Arc::new(move |ev| {
            if matches!(ev, DownloadEvent::Installing) {
                flag.store(true, Ordering::SeqCst);
            }
        });
        let err = asset
            .install(std::time::Duration::from_secs(5), &cancel, &cb)
            .unwrap_err();
        assert_eq!(err, "cancelled");
        assert!(!dest.exists());
        assert!(!asset.installed());
        assert!(leftover_parts(&staging_dir(&dest)).is_empty());

        // Uncancelled: the release binary installs executable under
        // the stable name.
        asset
            .install(
                std::time::Duration::from_secs(5),
                &AtomicBool::new(false),
                &noop(),
            )
            .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dest.join("whisper-server"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        assert!(asset.installed());
    }

    #[test]
    fn loose_downloads_clean_up_cut_bodies() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("whisper").join("fixture");
        let files = fixture_pack(vec![pin_body("ggml-fixture.bin", &[3u8; 4096])]);
        let base = serve(vec![(
            "ggml-fixture.bin",
            Route::Cut(vec![3u8; 1024], 4096),
        )]);
        let err = install(&dest, files, &base, &AtomicBool::new(false)).unwrap_err();
        assert!(err.contains("ggml-fixture.bin"), "{err}");
        assert!(!dest.exists());
        assert!(leftover_parts(&staging_dir(&dest)).is_empty());
    }

    #[test]
    fn unmarked_installs_count_at_pinned_sizes_until_verified() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("whisper").join("fixture");
        let files = fixture_pack(vec![
            pin_body("encoder.int8.onnx", b"weights"),
            pin_body("tokens.txt", b"a 0\n"),
            // Second accepted build of a file upstream replaced in place.
            pin_body("encoder.int8.onnx", b"weights-v2"),
        ]);
        let legacy = Asset {
            legacy: true,
            ..loose(&dest, "http://127.0.0.1:9", files)
        };
        let cancel = AtomicBool::new(false);
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("encoder.int8.onnx"), b"weights-v2").unwrap();
        // Truncated by an interrupted old install: not installed.
        std::fs::write(dest.join("tokens.txt"), b"a ").unwrap();
        assert!(!legacy.installed());
        std::fs::write(dest.join("tokens.txt"), b"a 0\n").unwrap();
        assert!(legacy.installed(), "pinned sizes, no marker yet");
        legacy.adopt(&cancel, &noop()).unwrap();
        assert!(legacy.installed());
        assert!(dest.join(MARKER).exists());

        // Same size, wrong bytes: counts until hashed, then reads as
        // not installed while keeping the original files.
        std::fs::remove_file(dest.join(MARKER)).unwrap();
        std::fs::write(dest.join("tokens.txt"), b"b 0\n").unwrap();
        assert!(legacy.installed());
        let err = legacy.adopt(&cancel, &noop()).unwrap_err();
        assert!(err.contains("tokens.txt failed verification"), "{err}");
        assert!(!legacy.installed());
        assert_eq!(
            std::fs::read(dest.join("encoder.int8.onnx")).unwrap(),
            b"weights-v2"
        );
        assert!(!staging_dir(&dest).exists());
        // A download then refetches only the bad file.
        let base = serve(vec![("tokens.txt", Route::Body(b"a 0\n".to_vec()))]);
        install(&dest, files, &base, &cancel).unwrap();
        assert!(legacy.installed());
        assert!(dest.join(MARKER).exists());
    }

    #[test]
    fn startup_cleans_only_unlocked_installer_parts() {
        let data = tempfile::tempdir().unwrap();
        let model = model_asset(data.path(), find_model("tiny").unwrap());
        let model_stage = model.staging();
        std::fs::create_dir_all(model_stage.parent().unwrap()).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let untouched = elsewhere.path().join("ggml-tiny.bin.part");
        std::fs::write(&untouched, b"outside staging").unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), &model_stage).unwrap();

        let pack = SHERPA_PACKS.iter().find(|p| p.id == "sherpa-cpu").unwrap();
        let runtime = sherpa_asset(data.path(), pack);
        let stage = runtime.staging();
        std::fs::create_dir_all(stage.join("lib")).unwrap();
        let library = stage.join("lib/libonnxruntime.so");
        let library_part = stage.join("lib/libonnxruntime.so.part");
        let archive_part = stage.join(".archive.part");
        let unknown_part = stage.join("custom.part");
        for path in [&library, &library_part, &archive_part, &unknown_part] {
            std::fs::write(path, b"fixture").unwrap();
        }

        let start = || {
            spawn_legacy_verification(data.path().to_path_buf())
                .unwrap()
                .join()
                .unwrap();
        };
        let live_install = runtime.lock_for_change().unwrap();
        start();
        assert!(library_part.is_file() && archive_part.is_file());
        drop(live_install);
        start();
        assert!(!library_part.exists() && !archive_part.exists());
        assert!(library.is_file() && unknown_part.is_file());
        assert_eq!(std::fs::read(&untouched).unwrap(), b"outside staging");
    }

    #[test]
    fn legacy_verification_runs_off_thread_and_flags_bad_installs() {
        let data = tempfile::tempdir().unwrap();
        let entry = find_model("whistle-v1").unwrap();
        // Right size, wrong bytes, no marker: an old install.
        let dir = model_dir(data.path(), entry);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::File::create(dir.join("whistle.cact"))
            .unwrap()
            .set_len(entry.files[0].bytes)
            .unwrap();
        let sidecar = dir.join("whistle.json");
        std::fs::write(&sidecar, b"legacy sidecar").unwrap();
        assert!(is_installed(data.path(), entry));
        spawn_legacy_verification(data.path().to_path_buf())
            .unwrap()
            .join()
            .unwrap();
        assert!(!is_installed(data.path(), entry));
        assert_eq!(std::fs::read(&sidecar).unwrap(), b"legacy sidecar");
        assert!(dir.join("whistle.cact").is_file());
        assert!(!has_stale_staging(data.path(), entry));
        match download_state(entry.id).map(|d| d.phase) {
            Some(DlPhase::Failed(e)) => assert!(e.contains("failed verification"), "{e}"),
            other => panic!("unexpected {other:?}"),
        }
        delete_model(data.path(), entry).unwrap();
        assert!(!has_stale_staging(data.path(), entry));
        downloads().lock().unwrap().remove(entry.id);
    }

    #[test]
    fn delete_waits_for_running_jobs() {
        let data = tempfile::tempdir().unwrap();
        let dir = fake_install(data.path(), "sherpa-cpu");
        let pack = &SHERPA_PACKS[1];
        let cancel = claim(pack.id, None).unwrap();
        assert!(claim(pack.id, None).is_none(), "one job per id");
        assert!(delete_sherpa_pack(data.path(), pack).is_err());
        assert!(dir.exists());
        run_claimed(pack.id, &cancel, Box::new(|_, _| Ok(())));
        delete_sherpa_pack(data.path(), pack).unwrap();
        assert!(!dir.exists());
    }
}

// ---------------------------------------------------------------- GPU packs

/// whisper-server binary packs from the pinned OpenWhispr/whisper.cpp
/// release. CPU is tiny (~1MB); CUDA (~219MB) and Vulkan (~24MB) are
/// large downloads, hence on-demand. Same tag + digests OpenWhispr
/// pins, Linux x64 only (our platform). The release tag is mutable;
/// the archive and the binary inside it are pinned by SHA-256.
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
    /// Release asset; its one binary installs as `whisper-server`.
    pub archive: Archive,
    pub files: &'static [PinnedFile],
    /// Install subdir under `<data>/bin/`, also the binary dir.
    pub subdir: &'static str,
}

/// The zips ship the binary under its release name
/// (`whisper-server-linux-x64-cuda`, …); runners use one stable path.
fn whisper_server(member: &str) -> Option<&str> {
    member
        .starts_with("whisper-server")
        .then_some("whisper-server")
}

const fn zip(name: &'static str, bytes: u64, sha256: &'static str) -> Archive {
    Archive {
        file: pin(name, bytes, sha256),
        member: whisper_server,
    }
}

pub const GPU_PACKS: &[GpuPack] = &[
    GpuPack {
        id: "whisper-cpu",
        backend: GpuBackend::Cpu,
        name: "CPU server",
        description: "Portable whisper-server, works everywhere",
        size_label: "1.3MB",
        archive: zip(
            "whisper-server-linux-x64-cpu.zip",
            1_297_401,
            "d10561f59eabb6e3273d721bc31e17092aa30ec543d9f3931e8ae04c58be6dd4",
        ),
        files: &[pin(
            "whisper-server",
            3_219_648,
            "47f6d37417cd9acde262f6d4bbebb09834f9cdb7f3f05fa71cd94c235eadb970",
        )],
        subdir: "whisper-cpu",
    },
    GpuPack {
        id: "whisper-cuda",
        backend: GpuBackend::Cuda,
        name: "CUDA server",
        description: "NVIDIA GPUs (Pascal 6.1+, needs proprietary driver)",
        size_label: "219MB",
        archive: zip(
            "whisper-server-linux-x64-cuda.zip",
            218_743_566,
            "9282d706a3c1853e10f563be5fab3d7e9e544142cc73fe9e6470aaeeaf0a5454",
        ),
        files: &[pin(
            "whisper-server",
            239_825_688,
            "1980c73b9072b578848c7db3c0fdedaa6816ef2041a2451e636f4277a740f5d0",
        )],
        subdir: "whisper-cuda",
    },
    GpuPack {
        id: "whisper-vulkan",
        backend: GpuBackend::Vulkan,
        name: "Vulkan server",
        description: "AMD/Intel/discrete GPUs via Vulkan",
        size_label: "24MB",
        archive: zip(
            "whisper-server-linux-x64-vulkan.zip",
            24_013_074,
            "7bcd8b226cae9b0384613ce315fd8afab533fbdc5c3731343a09de8693eca203",
        ),
        files: &[pin(
            "whisper-server",
            77_248_920,
            "7e684ce2a976854f03438915ece0564a5d69ed0ec09dc97a4c5db54e180b9cf3",
        )],
        subdir: "whisper-vulkan",
    },
];

fn gpu_url(asset: &str) -> String {
    format!("https://github.com/OpenWhispr/whisper.cpp/releases/download/{WHISPER_CPP_TAG}/{asset}")
}

pub fn gpu_bin_dir(data_dir: &Path, pack: &GpuPack) -> PathBuf {
    data_dir.join("bin").join(pack.subdir)
}

fn gpu_asset(data_dir: &Path, pack: &GpuPack) -> Asset {
    Asset {
        id: pack.id,
        data_dir: data_dir.to_path_buf(),
        dir: gpu_bin_dir(data_dir, pack),
        url: gpu_url(pack.archive.file.name),
        archive: Some(pack.archive),
        files: pack.files,
        executable: true,
        legacy: true,
        legacy_vocab: false,
    }
}

/// Installed = the verified whisper-server binary (see `is_installed`).
pub fn gpu_installed(data_dir: &Path, pack: &GpuPack) -> bool {
    gpu_asset(data_dir, pack).installed()
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

/// Download + verify + install a GPU pack. Blocking; run on a worker
/// thread.
pub fn download_gpu_pack(
    data_dir: &Path,
    pack: &GpuPack,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
) -> Result<(), String> {
    gpu_asset(data_dir, pack).install(READ_TIMEOUT, cancel, on_event)
}

pub fn delete_gpu_pack(data_dir: &Path, pack: &GpuPack) -> std::io::Result<()> {
    gpu_asset(data_dir, pack).delete()
}

/// Start a GPU pack download on a worker thread. No-op if running.
/// Shares the download state map with model downloads.
pub fn start_gpu_download(data_dir: PathBuf, id: &'static str) {
    let Some(pack) = GPU_PACKS.iter().find(|p| p.id == id) else {
        return;
    };
    start_job(
        id,
        Some(pack.archive.file.bytes),
        Box::new(move |cancel, cb| download_gpu_pack(&data_dir, pack, cancel, cb)),
    );
}

/// Cancel an in-flight GPU download.
pub fn cancel_gpu_download(id: &str) {
    cancel_download(id);
}

// ---------------------------------------------------------------- sherpa runtimes

/// sherpa-onnx server runtimes for parakeet/orukeet/cohere models
/// (whisper models use the whisper-server packs above instead).
/// Same v1.13.8 upstream OpenWhispr fetches. The servers and their
/// runtime libraries are pinned.
pub const SHERPA_TAG: &str = "v1.13.8";

#[derive(Debug, Clone)]
pub struct SherpaPack {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub size_label: &'static str,
    pub archive: Archive,
    pub files: &'static [PinnedFile],
    pub subdir: &'static str,
    /// The server binaries (relative to subdir), among `files`.
    pub binaries: &'static [&'static str],
}

const SHERPA_SERVERS: &[&str] = &[
    "bin/sherpa-onnx-offline-websocket-server",
    "bin/sherpa-onnx-online-websocket-server",
];

pub const SHERPA_PACKS: &[SherpaPack] = &[
    SherpaPack {
        id: "sherpa-cuda",
        name: "Sherpa CUDA server",
        description: "Offline + online websocket servers, CUDA 13 (needs system CUDA libs)",
        size_label: "255MB",
        archive: tar_bz2(
            "sherpa-onnx-v1.13.8-cuda-13.x-cudnn-9.x-onnxruntime1.28.2-linux-x64-gpu.tar.bz2",
            254_858_007,
            "7eddaee14a42d143ec995ed68e9ec3623dd6ca9d22076139d4c0879c3121c819",
        ),
        files: &[
            pin(
                "bin/sherpa-onnx-offline-websocket-server",
                3_060_184,
                "9808cf3ad8caa92bee71b000e8080bf57c6117175151dc1791868c7003f05a3f",
            ),
            pin(
                "bin/sherpa-onnx-online-websocket-server",
                2_729_584,
                "3bd79f958e15a29524b6be4d6f56b905c1b8dc90a1fbbd2e22c3a8e144838822",
            ),
            pin(
                "lib/libonnxruntime.so",
                27_691_993,
                "d93d9b6a7610b0084f6962eb05da1b56667c81d28031ec68ac45375460a398ff",
            ),
            pin(
                "lib/libonnxruntime_providers_cuda.so",
                279_766_824,
                "ea130388ae67b40e1bacd1917a7aaf9254a8278a3ce7e08bbb0dcd0d3b004794",
            ),
            pin(
                "lib/libonnxruntime_providers_shared.so",
                14_632,
                "c6a12593396095f5670160e284c35d1700b7708cf3037b7042e2a5200ccae772",
            ),
            pin(
                "lib/libonnxruntime_providers_tensorrt.so",
                863_240,
                "da7d0520c763e2b8bc57caec275e5394aa02280cc0f0695cc9a710e6ed25dd41",
            ),
            pin(
                "lib/libsherpa-onnx-c-api.so",
                5_162_520,
                "b6aee82c3e6d96ccc7888ec71641a0bf3003c8b2cd6bdae218d2d09448fe0b60",
            ),
            pin(
                "lib/libsherpa-onnx-cxx-api.so",
                177_112,
                "b2d98a3f46007ce829b4534513caf275317afa7ace8b58bbcf083da10add2bcd",
            ),
        ],
        subdir: "sherpa-cuda",
        binaries: SHERPA_SERVERS,
    },
    SherpaPack {
        id: "sherpa-cpu",
        name: "Sherpa CPU server",
        description: "Offline + online websocket servers, onnxruntime included",
        size_label: "25MB",
        archive: tar_bz2(
            "sherpa-onnx-v1.13.8-linux-x64-shared-no-tts.tar.bz2",
            24_802_494,
            "d0f96c8b65c6cd0974fada22737e337de81bc8cd2abbec2e39caf358b1eec5fc",
        ),
        files: &[
            pin(
                "bin/sherpa-onnx-offline-websocket-server",
                3_071_832,
                "a119ec9d7b156164ef02f097734f736932763ee3f8c1c8f975ea558b51b33dae",
            ),
            pin(
                "bin/sherpa-onnx-online-websocket-server",
                2_770_096,
                "1e04fb013fe3a56667b124f123e8a93ede84360b263ed60f7189367b4a0261dd",
            ),
            pin(
                "lib/libonnxruntime.so",
                27_026_609,
                "4b3607aebd1784b26b6f9b20e4bd974c7ab8287043e4d095cb7d2cb40b5e566e",
            ),
            // ONNX Runtime is the only bundled library the CPU servers load.
        ],
        subdir: "sherpa-cpu",
        binaries: SHERPA_SERVERS,
    },
];

fn sherpa_url(asset: &str) -> String {
    format!("https://github.com/k2-fsa/sherpa-onnx/releases/download/{SHERPA_TAG}/{asset}")
}

pub fn sherpa_bin_dir(data_dir: &Path, pack: &SherpaPack) -> PathBuf {
    data_dir.join("bin").join(pack.subdir)
}

fn sherpa_asset(data_dir: &Path, pack: &SherpaPack) -> Asset {
    Asset {
        id: pack.id,
        data_dir: data_dir.to_path_buf(),
        dir: sherpa_bin_dir(data_dir, pack),
        url: sherpa_url(pack.archive.file.name),
        archive: Some(pack.archive),
        files: pack.files,
        executable: true,
        legacy: true,
        legacy_vocab: false,
    }
}

/// Installed = servers and libraries verified (see `is_installed`).
pub fn sherpa_installed(data_dir: &Path, pack: &SherpaPack) -> bool {
    sherpa_asset(data_dir, pack).installed()
}

/// Download + verify + install a sherpa pack. Blocking; run on a
/// worker thread.
pub fn download_sherpa_pack(
    data_dir: &Path,
    pack: &SherpaPack,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
) -> Result<(), String> {
    sherpa_asset(data_dir, pack).install(READ_TIMEOUT, cancel, on_event)
}

pub fn delete_sherpa_pack(data_dir: &Path, pack: &SherpaPack) -> std::io::Result<()> {
    sherpa_asset(data_dir, pack).delete()
}

/// Start a sherpa pack download on a worker thread. No-op if running.
pub fn start_sherpa_download(data_dir: PathBuf, id: &'static str) {
    let Some(pack) = SHERPA_PACKS.iter().find(|p| p.id == id) else {
        return;
    };
    start_job(
        id,
        Some(pack.archive.file.bytes),
        Box::new(move |cancel, cb| download_sherpa_pack(&data_dir, pack, cancel, cb)),
    );
}

// ---------------------------------------------------------------- needle engine
//
// Whistle's `needle` binary: one file per arch, no archive. The first
// pin is the download from the last needle3 commit shipping the CLI.
// The second accepts the previous build on disk. Both hashes are LFS oids.

pub const NEEDLE_ENGINE_ID: &str = "needle-engine";

const NEEDLE_REVISION: &str = "d6c7d6a2e9655505e0f8e23339356dd1705900eb";

const NEEDLE_X86_64: &[PinnedFile] = &[
    pin(
        "needle",
        1_550_176,
        "77cd35ee75c53df8532b397e11ed7cf94a45d89a9f14f50881fd9c68ba54a2d0",
    ),
    pin(
        "needle",
        1_541_680,
        "f38dc4b0345d66b4e385734ad0f12af43ac6e2cfa1752d0795af5c137200c8e4",
    ),
];
const NEEDLE_ARM64: &[PinnedFile] = &[
    pin(
        "needle",
        1_439_720,
        "a7dd07f740763507edbca760dc7e7065b6da6b6bd6c3a05288ca974f14ce0fcb",
    ),
    pin(
        "needle",
        1_431_816,
        "6fc25a97def475e1c7d1933a4a219d4e37076be77974e76dd331d9e0cd97f3cb",
    ),
];

/// Release folder and pin of this arch's build.
fn needle_build() -> Result<(&'static str, &'static [PinnedFile]), String> {
    match std::env::consts::ARCH {
        "x86_64" => Ok(("linux-x86_64", NEEDLE_X86_64)),
        "aarch64" => Ok(("linux-arm64", NEEDLE_ARM64)),
        other => Err(format!("needle engine has no build for {other}")),
    }
}

pub fn needle_bin_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("bin").join("needle")
}

fn needle_asset(data_dir: &Path) -> Result<Asset, String> {
    let (folder, files) = needle_build()?;
    Ok(Asset {
        id: NEEDLE_ENGINE_ID,
        data_dir: data_dir.to_path_buf(),
        dir: needle_bin_dir(data_dir),
        url: format!(
            "https://huggingface.co/Cactus-Compute/needle3/resolve/{NEEDLE_REVISION}/{folder}"
        ),
        archive: None,
        files,
        executable: true,
        legacy: true,
        legacy_vocab: false,
    })
}

/// Installed = the verified engine binary (see `is_installed`).
pub fn needle_installed(data_dir: &Path) -> bool {
    needle_asset(data_dir).is_ok_and(|a| a.installed())
}

/// Download + verify + install the needle engine binary. Blocking;
/// run on a worker thread.
pub fn download_needle_pack(
    data_dir: &Path,
    cancel: &AtomicBool,
    on_event: &DownloadCallback,
) -> Result<(), String> {
    needle_asset(data_dir)?.install(READ_TIMEOUT, cancel, on_event)
}

pub fn delete_needle_pack(data_dir: &Path) -> std::io::Result<()> {
    needle_asset(data_dir).map_or(Ok(()), |a| a.delete())
}

/// Start a needle engine download on a worker thread. No-op if
/// running; fails visibly when this arch has no build.
pub fn start_needle_download(data_dir: PathBuf) {
    let total = needle_build().ok().map(|(_, f)| f[0].bytes);
    start_job(
        NEEDLE_ENGINE_ID,
        total,
        Box::new(move |cancel, cb| download_needle_pack(&data_dir, cancel, cb)),
    );
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

fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries.flatten().fold(0u64, |sum, entry| {
        let size = entry.file_type().map_or(0, |kind| {
            if kind.is_dir() {
                dir_bytes(&entry.path())
            } else if kind.is_file() {
                entry.metadata().map_or(0, |m| m.len())
            } else {
                0
            }
        });
        sum.saturating_add(size)
    })
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
    let _locks = lock_asset_change(data_dir, pack.id).map_err(|e| format!("asset lock: {e}"))?;
    use crate::qwen_runtime as qr;
    qr::supported_platform()?;
    qr::check_locked_compat()?;
    if pack.backend == QwenRuntime::Vllm {
        qr::check_gpu_for_vllm()?;
    }
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled".to_string());
    }
    let _ = std::fs::remove_file(qr::failed_log(data_dir, pack.subdir));
    let staging = qr::staging_dir(data_dir, pack.subdir);
    let runtime = qr::runtime_dir(data_dir, pack.subdir);
    let backup = qr::backup_dir(data_dir, pack.subdir);
    // A killed installer can leave the old runtime in backup and an
    // unverified replacement at its original path.
    if backup.exists() && !qwen_installed(data_dir, pack) {
        if runtime.exists() {
            std::fs::remove_dir_all(&runtime).map_err(|e| format!("clear partial runtime: {e}"))?;
        }
        std::fs::rename(&backup, &runtime).map_err(|e| format!("restore previous runtime: {e}"))?;
    }
    // Stale staging from an interrupted (SIGKILL) run: owned, drop it.
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("mkdir staging: {e}"))?;
    let log_path = staging.join("install.log");
    let _ = std::fs::write(&log_path, format!("qwen {} install\n", pack.id));
    let stage = |s: qr::Stage| {
        eprintln!("[qwen] {}: {}", pack.id, s.label());
        on_event(DownloadEvent::Installing);
    };
    let mut replacing = false;
    let result: Result<(), String> = (|| {
        stage(qr::Stage::FetchUv);
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".to_string());
        }
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
        replacing = true;
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
        if !qr::venv_python_self_contained(&runtime) {
            return Err("venv interpreter escapes the runtime dir (see install.log)".to_string());
        }
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".to_string());
        }
        stage(qr::Stage::Activate);
        crate::private_fs::replace(
            &runtime.join(".python-version"),
            qr::QWEN_PYTHON_VERSION.as_bytes(),
        )
        .map_err(|e| format!("version marker: {e}"))?;
        crate::private_fs::replace(
            &runtime.join(".installed"),
            qr::marker_text(pack.subdir, pack.pip_specs()).as_bytes(),
        )
        .map_err(|e| format!("install marker: {e}"))?;
        let _ = std::fs::copy(&log_path, qr::install_log(&runtime));
        let _ = std::fs::remove_dir_all(&backup);
        let _ = std::fs::remove_dir_all(&staging);
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
            let rollback = if replacing {
                std::fs::remove_dir_all(&runtime)
                    .or_else(|error| {
                        if error.kind() == std::io::ErrorKind::NotFound {
                            Ok(())
                        } else {
                            Err(error)
                        }
                    })
                    .and_then(|()| {
                        if backup.exists() {
                            std::fs::rename(&backup, &runtime)
                        } else {
                            Ok(())
                        }
                    })
            } else {
                Ok(())
            };
            // Persist diagnostics BEFORE staging is cleaned: the
            // concise error references this file, which survives
            // restarts (the in-memory Failed state does not).
            let failed = qr::failed_log(data_dir, pack.subdir);
            if !qr::is_cancel_error(&e) {
                let _ = qr::copy_log_tail(&log_path, &failed, 262_144);
            }
            let _ = std::fs::remove_dir_all(&staging);
            if let Err(error) = rollback {
                return Err(format!("{e}; could not restore previous runtime: {error}"));
            }
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
    let _locks = lock_asset_change(data_dir, pack.id)?;
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
    let Some(pack) = QWEN_PACKS.iter().find(|p| p.id == id) else {
        return;
    };
    start_job(
        id,
        Some(pack.size_bytes),
        Box::new(move |cancel, on_event| download_qwen_runtime(&data_dir, pack, cancel, on_event)),
    );
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
    fn qwen_bootstrap_cancel_preserves_working_runtime() {
        use crate::qwen_runtime as qr;
        let pack = &QWEN_PACKS[0];
        for interrupted in [false, true] {
            let data = tempfile::tempdir().unwrap();
            let runtime = qwen_runtime_dir(data.path(), pack);
            let python = qwen_venv_python(data.path(), pack);
            std::fs::create_dir_all(python.parent().unwrap()).unwrap();
            std::fs::write(&python, b"working interpreter").unwrap();
            std::fs::write(
                runtime.join(".installed"),
                qr::marker_text(pack.subdir, pack.pip_specs()),
            )
            .unwrap();
            assert!(qwen_installed(data.path(), pack));
            let backup = qr::backup_dir(data.path(), pack.subdir);
            if interrupted {
                std::fs::rename(&runtime, &backup).unwrap();
                std::fs::create_dir(&runtime).unwrap();
            }
            let cancel = Arc::new(AtomicBool::new(false));
            let on_event: DownloadCallback = {
                let cancel = Arc::clone(&cancel);
                Arc::new(move |_| cancel.store(true, Ordering::SeqCst))
            };
            let result = download_qwen_runtime(data.path(), pack, &cancel, &on_event);
            assert_eq!(result, Err("cancelled".to_string()));
            assert!(qwen_installed(data.path(), pack));
            assert_eq!(std::fs::read(python).unwrap(), b"working interpreter");
            assert!(!backup.exists());
        }
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
