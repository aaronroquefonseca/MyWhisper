//! Application settings model.
//!
//! Plain data with serde support and opinionated defaults. The UI in
//! `app_settings` initializes and persists controls through this model.
//! The background service observes the same stored preferences.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppSettings {
    pub preferences: Preferences,
    pub hotkeys: HotkeySettings,
    pub stt: SttSettings,
    pub language_models: LanguageModelSettings,
    pub privacy: PrivacySettings,
    pub system: SystemSettings,
    pub dictionary: Dictionary,
    pub sync: SyncSettings,
}

// ---------------------------------------------------------------------------
// Sync (node-based device sync, Syncthing-style trust)
// ---------------------------------------------------------------------------

/// Default TCP port for incoming sync connections.
pub const DEFAULT_SYNC_PORT: u16 = 42973;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSettings {
    /// Master sync switch (sync tab toggle): the service starts the
    /// sync engine only when on, and watches changes. systemd owns the process.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// This device's display name shown to peers.
    pub device_name: String,
    /// TCP port for incoming sync connections (0 = disabled).
    pub listen_port: u16,
    /// Advertise on mDNS so nearby devices list us. Off = invisible
    /// (we still browse and see others).
    #[serde(default = "default_true")]
    pub advertise: bool,
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            device_name: String::new(),
            listen_port: DEFAULT_SYNC_PORT,
            advertise: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Preferences (general)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    Light,
    Dark,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FloatingPosition {
    BottomRight,
    BottomCenter,
    BottomLeft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MicSelection {
    System,
    BuiltIn,
    Specific,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CopyBackend {
    #[default]
    WlCopy,
}

fn default_true() -> bool {
    true
}

fn default_spec_update() -> f32 {
    2.0
}

fn default_spec_overlap() -> f32 {
    1.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PasteBackend {
    #[default]
    Wtype,
    Ydotool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PasteCombo {
    #[default]
    CtrlShiftV,
    CtrlV,
    ShiftInsert,
}

/// Pill visualizer preferences. Defaults mirror the service runtime
/// (tape, 11 bars, 18px lane, no ornament, still idle).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PillPrefs {
    pub position: FloatingPosition,
    pub viz_mode: crate::viz::VizMode,
    pub bars: usize,
    pub lane_max: f64,
    pub theme: crate::viz::VizTheme,
    pub ornament: crate::viz::Ornament,
    pub idle: crate::viz::IdleStyle,
}

impl Default for PillPrefs {
    fn default() -> Self {
        Self {
            position: FloatingPosition::BottomCenter,
            viz_mode: crate::viz::VizMode::Tape,
            bars: 11,
            lane_max: 18.0,
            theme: crate::viz::VizTheme::White,
            ornament: crate::viz::Ornament::None,
            idle: crate::viz::IdleStyle::Still,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preferences {
    pub theme: Theme,
    pub dictation_sounds: bool,
    pub pause_media_on_dictation: bool,
    pub auto_paste: bool,
    pub keep_transcription_in_clipboard: bool,
    /// Copy backend. Only wl-copy for now; the enum grows with options.
    #[serde(default)]
    pub copy_backend: CopyBackend,
    /// Hide from clipboard history via wl-copy --sensitive. Default on.
    #[serde(default = "default_true")]
    pub hide_from_history: bool,
    /// Paste backend for auto-paste injection.
    #[serde(default)]
    pub paste_backend: PasteBackend,
    /// Key chord used to paste.
    #[serde(default)]
    pub paste_combo: PasteCombo,
    #[serde(default)]
    pub pill: PillPrefs,
    pub ui_language: String,
    pub mic_selection: MicSelection,
    pub mic_device_id: String,
    pub mic_warm_hold_secs: u32,
    /// Extra listening past release (ms, 0 = disabled): the release
    /// cut shifts later by this much. Settle/flush/truncate all key
    /// off the shifted instant.
    #[serde(default)]
    pub mic_release_delay_ms: u64,
    pub auto_learn_dictionary: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            dictation_sounds: true,
            pause_media_on_dictation: false,
            auto_paste: true,
            keep_transcription_in_clipboard: false,
            copy_backend: CopyBackend::WlCopy,
            hide_from_history: true,
            paste_backend: PasteBackend::Wtype,
            paste_combo: PasteCombo::CtrlShiftV,
            pill: PillPrefs::default(),
            ui_language: "system".to_string(),
            mic_selection: MicSelection::System,
            mic_device_id: String::new(),
            mic_warm_hold_secs: 0,
            mic_release_delay_ms: 0,
            auto_learn_dictionary: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Hotkeys (dictation only for now)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivationMode {
    /// Press once to start/stop.
    Tap,
    /// Hold to talk.
    Push,
    /// Quick tap (< ~35ms) toggles; longer press holds. Best of both.
    Smart,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyBinding {
    /// Combo string, e.g. "F13" or "Ctrl+Alt+D".
    pub combo: String,
    /// Hold-to-talk vs press-to-toggle, per binding.
    pub mode: ActivationMode,
}

fn de_bindings<'de, D>(d: D) -> Result<Vec<HotkeyBinding>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Compat {
        Full(HotkeyBinding),
        Legacy(String),
    }
    let v = Vec::<Compat>::deserialize(d)?;
    Ok(v.into_iter()
        .map(|c| match c {
            Compat::Full(b) => b,
            // Pre-per-key-mode databases stored bare strings; they
            // arrive as Push (the default).
            Compat::Legacy(combo) => HotkeyBinding {
                combo,
                mode: ActivationMode::Push,
            },
        })
        .collect())
}

fn default_dictation_hotkeys() -> Vec<HotkeyBinding> {
    vec![HotkeyBinding {
        combo: "F13".to_string(),
        mode: ActivationMode::Push,
    }]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeySettings {
    /// Dictation bindings, any-of fires. Per-binding mode means one
    /// key can hold-to-talk while another toggles.
    #[serde(
        default = "default_dictation_hotkeys",
        deserialize_with = "de_bindings"
    )]
    pub dictation_hotkeys: Vec<HotkeyBinding>,
    /// Legacy single combo (pre-multi-binding). Migrated into
    /// `dictation_hotkeys` on load, then ignored on save.
    #[serde(default)]
    pub dictation_hotkey: String,
    /// Smart-mode tap ceiling in ms: presses shorter than this
    /// toggle, longer ones hold. Default 150.
    #[serde(default = "default_smart_tap_ms")]
    pub smart_tap_ms: u64,
    /// Master hotkey switch (banner toggle): off mutes all hotkey
    /// events (presses never reach the pipeline). systemd owns the
    /// process; this only gates input.
    #[serde(default = "default_true")]
    pub hotkeys_enabled: bool,
}

fn default_smart_tap_ms() -> u64 {
    150
}

impl Default for HotkeySettings {
    fn default() -> Self {
        Self {
            dictation_hotkeys: default_dictation_hotkeys(),
            dictation_hotkey: String::new(),
            smart_tap_ms: default_smart_tap_ms(),
            hotkeys_enabled: true,
        }
    }
}

impl HotkeySettings {
    /// Effective bindings: the list is authoritative. The legacy
    /// single slot only fills an empty list (one-time migration for
    /// pre-multi-binding databases); an empty list with no legacy
    /// means F13, never a stale default.
    pub fn effective_hotkeys(&self) -> Vec<HotkeyBinding> {
        if !self.dictation_hotkeys.is_empty() {
            return self.dictation_hotkeys.clone();
        }
        if !self.dictation_hotkey.is_empty() {
            return vec![HotkeyBinding {
                combo: self.dictation_hotkey.clone(),
                mode: ActivationMode::Push,
            }];
        }
        default_dictation_hotkeys()
    }

    /// Fold the legacy slot into the list once, then clear it so it
    /// can never resurface after the user removes everything.
    /// Returns true when it changed something (caller persists).
    pub fn migrate_legacy(&mut self) -> bool {
        if self.dictation_hotkeys.is_empty() && !self.dictation_hotkey.is_empty() {
            self.dictation_hotkeys.push(HotkeyBinding {
                combo: self.dictation_hotkey.clone(),
                mode: ActivationMode::Push,
            });
            self.dictation_hotkey.clear();
            true
        } else if !self.dictation_hotkey.is_empty() {
            self.dictation_hotkey.clear();
            true
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Speech to text
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpuBackendSetting {
    Cpu,
    Cuda,
    Vulkan,
}

/// Sherpa runtime backend. CPU everywhere; CUDA where onnxruntime
/// has an EP (no Vulkan EP exists upstream).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SherpaBackendSetting {
    #[default]
    Cpu,
    Cuda,
}

/// Local vendor tabs, mirroring OpenWhispr: Oruk and Cohere ship
/// exactly one model each; OpenAI is the whisper.cpp family, NVIDIA
/// the sherpa parakeet/nemotron family, Qwen the Qwen3-ASR family
/// (LLM-based, Python sidecar server).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalSttProvider {
    #[serde(alias = "Whisper")]
    OpenAi,
    #[serde(alias = "Parakeet")]
    Nvidia,
    Oruk,
    Cohere,
    Whistle,
    Qwen,
}

/// Qwen runtime backend. Transformers runs on CPU or CUDA (offline
/// decode only); vLLM needs an NVIDIA GPU but adds live streaming.
/// Manual choice, never auto-switched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum QwenBackendSetting {
    #[default]
    Transformers,
    Vllm,
}

/// vLLM GPU footprint preset: vLLM preallocates its whole share
/// eagerly, so the 4.7 GB weights always read larger in VRAM.
/// Measured on a 24 GB card (1.7B serves in ~7.5 GB at Balanced):
/// Compact fits the 0.6B model only (1.7B refuses to boot under it —
/// the installer errors clearly instead of OOMing later).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum QwenVramSetting {
    Compact,
    #[default]
    Balanced,
    Large,
}

impl QwenVramSetting {
    /// (gpu_memory_utilization, max_model_len).
    pub fn profile(self) -> (f64, u32) {
        match self {
            // ~6 GB cap, 4k context (~2.5 min audio).
            QwenVramSetting::Compact => (0.25, 4096),
            // ~9 GB cap, 8k context (~5 min audio).
            QwenVramSetting::Balanced => (0.375, 8192),
            // ~14 GB cap, 16k context (~10 min audio).
            QwenVramSetting::Large => (0.6, 16384),
        }
    }
}

/// Pre-chain engine selector. Deprecated: the chain supersedes it,
/// kept only so legacy configs migrate instead of resetting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SttMode {
    #[default]
    Local,
    Cloud,
    SelfHosted,
}

/// One STT chain entry. Local appears at most once and references
/// the live local configuration (pills/models/backend/language);
/// remotes carry their own connection config. Order = priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SttEntryKind {
    Local,
    SelfHosted,
    Cloud,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttChainEntry {
    pub id: String,
    pub name: String,
    pub kind: SttEntryKind,
    pub enabled: bool,
    /// Cloud only: OpenAI | Groq | Mistral | Gemini | Deepgram.
    pub provider: String,
    pub url: String,
    pub model: String,
    pub api_key: String,
    /// Self-hosted only: server speaks a streaming protocol
    /// (ws:// sherpa servers). Default true; http(s) URLs always
    /// use request/response regardless.
    #[serde(default = "default_true")]
    pub streaming: bool,
}

fn default_qwen_model() -> String {
    "qwen3-asr-1.7b".to_string()
}

impl SttChainEntry {
    pub fn summary(&self) -> String {
        match self.kind {
            SttEntryKind::Local => "current local configuration".to_string(),
            SttEntryKind::SelfHosted => format!("{} · {}", self.url, self.model),
            SttEntryKind::Cloud => format!("{} · {}", self.provider, self.model),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttSettings {
    #[serde(default)]
    pub entries: Vec<SttChainEntry>,
    /// Deprecated migration source (see SttMode).
    #[serde(default)]
    pub mode: SttMode,
    pub local_provider: LocalSttProvider,
    pub whisper_model: String,
    pub parakeet_model: String,
    /// Qwen3-ASR model id (registry id, e.g. "qwen3-asr-1.7b").
    /// serde-defaulted so pre-Qwen databases keep loading.
    #[serde(default = "default_qwen_model")]
    pub qwen_model: String,
    pub cloud_provider: String,
    pub cloud_model: String,
    pub cloud_api_key: String,
    pub self_hosted_url: String,
    pub self_hosted_model: String,
    pub language: String,
    pub vad_enabled: bool,
    pub vad_threshold: f32,
    /// Speculative streaming for non-streaming engines (whisper):
    /// background passes + local agreement while held, tail decode
    /// on release. Off by default; opt-in latency trade.
    #[serde(default)]
    pub spec_enabled: bool,
    /// Seconds between speculative passes (adaptive: passes never
    /// overlap, so slow machines space out by themselves).
    #[serde(default = "default_spec_update")]
    pub spec_update_secs: f32,
    /// Confirmed-audio overlap (seconds) kept for context on every
    /// pass and the final tail.
    #[serde(default = "default_spec_overlap")]
    pub spec_overlap_secs: f32,
    /// Start servers at service boot when a local option is enabled
    /// (default on). Off = lazy: first press records instantly while
    /// the server spins up, then the buffered audio feeds in.
    #[serde(default = "default_true")]
    pub auto_start_server: bool,
    /// Keep-warm idle timeout («» = forever, else 1m/5m/15m/30m/1h):
    /// servers idle longer are stopped. Server families only.
    #[serde(default)]
    pub keep_warm: String,
    /// Server port ("" = each engine's default: whisper 8080,
    /// sherpa-online 6006, sherpa-offline 6007; set = that port for
    /// whichever server runs — only one runs at a time). Whistle
    /// has no server: network options don't apply to it.
    #[serde(default)]
    pub server_port: String,
    /// Expose servers on the LAN (0.0.0.0) instead of loopback.
    /// Default off. Whisper-only in practice: the sherpa binaries
    /// have no bind flag and always listen on 0.0.0.0 (upstream),
    /// so this can't restrict them — reported, not silently
    /// papered over.
    #[serde(default)]
    pub lan_expose: bool,
    pub live_preview: bool,
    /// Manual GPU backend choice (never auto-switched).
    pub gpu_backend: GpuBackendSetting,
    /// Manual sherpa backend choice (never auto-switched).
    #[serde(default)]
    pub sherpa_backend: SherpaBackendSetting,
    /// Manual Qwen backend choice (never auto-switched):
    /// transformers (CPU/CUDA, offline) vs vLLM (CUDA, streaming).
    #[serde(default)]
    pub qwen_backend: QwenBackendSetting,
    /// vLLM GPU footprint preset (ignored by transformers).
    #[serde(default)]
    pub qwen_vram: QwenVramSetting,
}

impl Default for SttSettings {
    fn default() -> Self {
        Self {
            mode: SttMode::Local,
            entries: vec![SttChainEntry {
                id: "stt-local".to_string(),
                name: "Local".to_string(),
                kind: SttEntryKind::Local,
                enabled: true,
                provider: String::new(),
                url: String::new(),
                model: String::new(),
                api_key: String::new(),
                streaming: true,
            }],
            local_provider: LocalSttProvider::OpenAi,
            whisper_model: "base".to_string(),
            parakeet_model: "parakeet-tdt-0.6b-v3".to_string(),
            qwen_model: default_qwen_model(),
            cloud_provider: "OpenAI".to_string(),
            cloud_model: "whisper-1".to_string(),
            cloud_api_key: String::new(),
            self_hosted_url: "http://localhost:8000".to_string(),
            self_hosted_model: String::new(),
            language: "auto".to_string(),
            vad_enabled: false,
            vad_threshold: 0.1,
            spec_enabled: false,
            spec_update_secs: 2.0,
            spec_overlap_secs: 1.0,
            auto_start_server: true,
            keep_warm: String::new(),
            server_port: String::new(),
            lan_expose: false,
            live_preview: false,
            gpu_backend: GpuBackendSetting::Cpu,
            sherpa_backend: SherpaBackendSetting::Cpu,
            qwen_backend: QwenBackendSetting::Transformers,
            qwen_vram: QwenVramSetting::Balanced,
        }
    }
}

// ---------------------------------------------------------------------------
// Language models (Ollama-first, local-first)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LlmKind {
    Ollama,
    Local,
    Cloud,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloudLlm {
    /// Renamed from Grok (serde alias keeps old configs loading).
    #[serde(alias = "Grok")]
    Groq,
    OpenRouter,
    OpenAI,
    Claude,
    Google,
}

impl CloudLlm {
    pub fn base_url(&self) -> &'static str {
        match self {
            CloudLlm::Groq => "https://api.groq.com/openai/v1",
            CloudLlm::OpenRouter => "https://openrouter.ai/api/v1",
            CloudLlm::OpenAI => "https://api.openai.com/v1",
            CloudLlm::Claude => "https://api.anthropic.com/v1",
            CloudLlm::Google => "https://generativelanguage.googleapis.com/v1beta",
        }
    }

    pub fn default_model(&self) -> &'static str {
        match self {
            CloudLlm::Groq => "llama-3.3-70b-versatile",
            CloudLlm::OpenRouter => "moonshotai/kimi-k2",
            CloudLlm::OpenAI => "gpt-6-luna",
            CloudLlm::Claude => "claude-haiku-5-5",
            CloudLlm::Google => "gemini-3.8-flash",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            CloudLlm::Groq => "Groq",
            CloudLlm::OpenRouter => "OpenRouter",
            CloudLlm::OpenAI => "OpenAI",
            CloudLlm::Claude => "Claude",
            CloudLlm::Google => "Google",
        }
    }
}

/// One configured LLM option. Ordered in the list: the future
/// pipeline tries enabled entries top-down until one answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmEntry {
    pub id: String,
    pub name: String,
    pub kind: LlmKind,
    pub enabled: bool,
    /// Ollama: server URL + model. Local: model path/name.
    pub url: String,
    pub model: String,
    /// Cloud only.
    pub provider: CloudLlm,
    pub api_key: String,
    /// Ollama only: keep-alive («» = server default, else -1, 0,
    /// 5m, 15m, 30m, 2h, 5h, 24h). Sent on every request.
    #[serde(default)]
    pub keep_alive: String,
    /// Ollama context window (tokens). None = server default;
    /// smaller reserves less memory (cleanup prompts are small).
    #[serde(default)]
    pub num_ctx: Option<u32>,
    /// Reasoning preference: None = model default (param omitted),
    /// Some(true/false) = forced. Ollama `think`, Claude extended
    /// thinking (10k budget), Gemini dynamic budget (-1) / off (0);
    /// other providers ignore it. Old stored bools read as forced.
    #[serde(default)]
    pub thinking: Option<bool>,
}

impl LlmEntry {
    pub fn ollama(url: &str, model: &str) -> Self {
        Self {
            id: String::new(),
            name: format!("Ollama ({model})"),
            kind: LlmKind::Ollama,
            enabled: true,
            url: url.to_string(),
            model: model.to_string(),
            provider: CloudLlm::OpenAI,
            api_key: String::new(),
            keep_alive: String::new(),
            num_ctx: None,
            thinking: Some(false),
        }
    }

    pub fn summary(&self) -> String {
        match self.kind {
            LlmKind::Ollama => {
                let base = format!("{} · {}", self.url, self.model);
                if self.keep_alive.trim().is_empty() {
                    base
                } else {
                    format!("{} · keep {}", base, self.keep_alive.trim())
                }
            }
            LlmKind::Local => self.model.clone(),
            LlmKind::Cloud => format!("{} · {}", self.provider.name(), self.model),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageModelSettings {
    #[serde(default)]
    pub entries: Vec<LlmEntry>,
    // Legacy single-option fields (kept for migration; the list is
    // authoritative once non-empty).
    pub ollama_url: String,
    pub ollama_model: String,
    pub local_model: String,
    pub keep_loaded: bool,
    pub cloud_provider: String,
    pub cloud_model: String,
    pub cloud_api_key: String,
    pub cleanup_enabled: bool,
    pub cleanup_prompt: String,
}

impl Default for LanguageModelSettings {
    fn default() -> Self {
        Self {
            entries: vec![LlmEntry::ollama("http://127.0.0.1:11434", "gemma4:e4b")],
            ollama_url: "http://127.0.0.1:11434".to_string(),
            ollama_model: "gemma4:e4b".to_string(),
            local_model: String::new(),
            keep_loaded: true,
            cloud_provider: "OpenAI".to_string(),
            cloud_model: "gpt-4o-mini".to_string(),
            cloud_api_key: String::new(),
            cleanup_enabled: true,
            cleanup_prompt: String::new(),
        }
    }
}

impl SttSettings {
    /// One-time seed for pre-chain configs: convert the legacy mode +
    /// single-option fields. Idempotent.
    pub fn ensure_seed(&mut self) {
        if !self.entries.is_empty() {
            return;
        }
        let mut entry = SttChainEntry {
            id: "stt-legacy".to_string(),
            name: String::new(),
            kind: SttEntryKind::Local,
            enabled: true,
            provider: String::new(),
            url: String::new(),
            model: String::new(),
            api_key: String::new(),
            streaming: true,
        };
        match self.mode {
            SttMode::Local => {
                entry.id = "stt-local".to_string();
                entry.name = "Local".to_string();
            }
            SttMode::Cloud => {
                entry.kind = SttEntryKind::Cloud;
                entry.name = format!("{} ({})", self.cloud_provider, self.cloud_model);
                entry.provider = self.cloud_provider.clone();
                entry.model = self.cloud_model.clone();
                entry.api_key = self.cloud_api_key.clone();
            }
            SttMode::SelfHosted => {
                entry.kind = SttEntryKind::SelfHosted;
                entry.name = format!("Self-hosted ({})", self.self_hosted_model);
                entry.url = self.self_hosted_url.clone();
                entry.model = self.self_hosted_model.clone();
            }
        }
        if entry.name.trim().is_empty() {
            entry.name = "Legacy option".to_string();
        }
        self.entries.push(entry);
    }

    /// Enabled entries top-down: the order the pipeline tries.
    pub fn chain(&self) -> Vec<&SttChainEntry> {
        self.entries.iter().filter(|e| e.enabled).collect()
    }
}

impl LanguageModelSettings {
    /// One-time seed for pre-list configs: convert legacy fields, or
    /// fall back to the default Ollama entry. Idempotent.
    pub fn ensure_seed(&mut self) {
        if !self.entries.is_empty() {
            return;
        }
        if !self.ollama_url.is_empty() || !self.ollama_model.is_empty() {
            let url = if self.ollama_url.is_empty() {
                "http://127.0.0.1:11434".to_string()
            } else {
                self.ollama_url.clone()
            };
            let model = if self.ollama_model.is_empty() {
                "gemma4:e4b".to_string()
            } else {
                self.ollama_model.clone()
            };
            self.entries.push(LlmEntry::ollama(&url, &model));
        } else {
            self.entries
                .push(LlmEntry::ollama("http://127.0.0.1:11434", "gemma4:e4b"));
        }
        // Legacy localhost form canonicalized to 127.0.0.1.
        for e in &mut self.entries {
            if e.url == "http://localhost:11434" {
                e.url = "http://127.0.0.1:11434".to_string();
            }
        }
    }

    /// Enabled entries top-down: the order the future pipeline tries.
    pub fn chain(&self) -> Vec<&LlmEntry> {
        self.entries.iter().filter(|e| e.enabled).collect()
    }
}

// ---------------------------------------------------------------------------
// Privacy & data
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacySettings {
    /// Audio retention in days (no Never option: clips grow fast).
    pub audio_retention_days: u32,
    /// Master switch for the audio vault. Default off. Note: the
    /// capture pipeline is RAM-only, so the vault is empty until an
    /// audio-storing feature lands; the controls below already manage
    /// it (greyed while off, purge + clear work on whatever is there).
    #[serde(default)]
    pub keep_audio_recordings: bool,
    pub history_enabled: bool,
    /// Transcript auto-delete in days; 0 = keep forever.
    pub transcript_auto_delete_days: u32,
    pub save_discarded_transcriptions: bool,
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self {
            audio_retention_days: 30,
            keep_audio_recordings: false,
            history_enabled: true,
            transcript_auto_delete_days: 0,
            save_discarded_transcriptions: false,
        }
    }
}

// ---------------------------------------------------------------------------
// System
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SystemSettings {
    /// Placeholder for future system knobs. The model cache lives at
    /// `<data>/models` (+ `<data>/bin` runtimes) and is shown, not
    /// configured: relocating it would invalidate every download,
    /// server, and pipeline path.
    pub _reserved: bool,
}

// ---------------------------------------------------------------------------
// Dictionary + snippets
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snippet {
    /// Comma-separated trigger list ("ask, aks, axe"). Identity for
    /// delete/update is the whole field; matching uses [`Snippet::triggers`].
    pub trigger: String,
    pub replacement: String,
}

impl Snippet {
    /// Individual triggers: comma-split, trimmed, empties dropped.
    /// Commas can't appear inside a trigger by construction.
    pub fn triggers(&self) -> Vec<String> {
        self.trigger
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Dictionary {
    pub words: Vec<String>,
    pub snippets: Vec<Snippet>,
}

// ---------------------------------------------------------------------------
// Runtime insights (computed, not persisted with settings)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct UsageStats {
    pub words_spoken: u64,
    pub words_per_minute: f64,
    pub dictations: u64,
    pub streak_days: u32,
}

#[derive(Debug, Clone, Default)]
pub struct HistoryEntry {
    pub uuid: String,
    pub text: String,
    pub raw_text: String,
    pub date_label: String,
    pub word_count: u32,
    pub duration_secs: i64,
    pub recorded_at: i64,
    pub discarded: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_respects_order_and_enabled() {
        let mut lm = LanguageModelSettings::default();
        assert_eq!(lm.chain().len(), 1); // seeded ollama
        lm.entries[0].enabled = false;
        assert!(lm.chain().is_empty());
        let mut second = LlmEntry::ollama("http://192.168.1.10:11434", "llama3.1");
        second.id = "lm-2".to_string();
        lm.entries.push(second);
        let chain = lm.chain();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].url, "http://192.168.1.10:11434");
    }

    #[test]
    fn ensure_seed_migrates_legacy() {
        let mut lm = LanguageModelSettings {
            entries: vec![],
            ..LanguageModelSettings::default()
        };
        lm.ollama_url = "http://localhost:11434".to_string();
        lm.ollama_model = "llama3.1".to_string();
        lm.ensure_seed();
        assert_eq!(lm.entries.len(), 1);
        // Canonicalized to 127.0.0.1.
        assert_eq!(lm.entries[0].url, "http://127.0.0.1:11434");
        // Idempotent.
        lm.ensure_seed();
        assert_eq!(lm.entries.len(), 1);
    }

    #[test]
    fn cloud_defaults_sane() {
        assert!(CloudLlm::Groq.base_url().contains("groq.com"));
        assert!(CloudLlm::OpenRouter.base_url().contains("openrouter"));
        assert!(CloudLlm::Claude.base_url().contains("anthropic"));
        assert!(CloudLlm::Google.base_url().contains("googleapis"));
        for p in [
            CloudLlm::Groq,
            CloudLlm::OpenRouter,
            CloudLlm::OpenAI,
            CloudLlm::Claude,
            CloudLlm::Google,
        ] {
            assert!(!p.default_model().is_empty());
        }
    }
}

#[cfg(test)]
mod snippet_tests {
    use super::*;

    #[test]
    fn triggers_split_commas() {
        let s = Snippet {
            trigger: " omw, on my way ,,  ".to_string(),
            replacement: "On my way!".to_string(),
        };
        assert_eq!(
            s.triggers(),
            vec!["omw".to_string(), "on my way".to_string()]
        );
        let single = Snippet {
            trigger: "brb".to_string(),
            replacement: "x".to_string(),
        };
        assert_eq!(single.triggers(), vec!["brb".to_string()]);
    }
}
