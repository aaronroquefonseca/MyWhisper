//! Application settings model.
//!
//! Plain data with serde support and opinionated defaults. The UI in
//! `app_settings` initializes its widgets from this model; nothing is
//! wired yet -- persistence, hotkey capture, device probing and all
//! backends come later. Keep every future-tweakable knob here so the
//! UI never grows parallel state.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            preferences: Preferences::default(),
            hotkeys: HotkeySettings::default(),
            stt: SttSettings::default(),
            language_models: LanguageModelSettings::default(),
            privacy: PrivacySettings::default(),
            system: SystemSettings::default(),
            dictionary: Dictionary::default(),
            sync: SyncSettings::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Sync (node-based device sync, Syncthing-style trust)
// ---------------------------------------------------------------------------

/// Default TCP port for incoming sync connections.
pub const DEFAULT_SYNC_PORT: u16 = 42973;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSettings {
    /// This device's display name shown to peers.
    pub device_name: String,
    /// TCP port for incoming sync connections (0 = disabled).
    pub listen_port: u16,
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            device_name: String::new(),
            listen_port: DEFAULT_SYNC_PORT,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CopyBackend {
    WlCopy,
}

impl Default for CopyBackend {
    fn default() -> Self {
        CopyBackend::WlCopy
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PasteBackend {
    Wtype,
    Ydotool,
}

impl Default for PasteBackend {
    fn default() -> Self {
        PasteBackend::Wtype
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PasteCombo {
    CtrlShiftV,
    CtrlV,
    ShiftInsert,
}

impl Default for PasteCombo {
    fn default() -> Self {
        PasteCombo::CtrlShiftV
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SttMode {
    Local,
    Cloud,
    SelfHosted,
}

/// Local vendor tabs, mirroring OpenWhispr: Oruk and Cohere ship
/// exactly one model each; OpenAI is the whisper.cpp family, NVIDIA
/// the sherpa parakeet/nemotron family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalSttProvider {
    #[serde(alias = "Whisper")]
    OpenAi,
    #[serde(alias = "Parakeet")]
    Nvidia,
    Oruk,
    Cohere,
    Whistle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttSettings {
    pub mode: SttMode,
    pub local_provider: LocalSttProvider,
    pub whisper_model: String,
    pub parakeet_model: String,
    pub cloud_provider: String,
    pub cloud_model: String,
    pub cloud_api_key: String,
    pub self_hosted_url: String,
    pub self_hosted_model: String,
    pub language: String,
    pub vad_enabled: bool,
    pub vad_threshold: f32,
    pub live_preview: bool,
    /// Manual GPU backend choice (never auto-switched).
    pub gpu_backend: GpuBackendSetting,
}

impl Default for SttSettings {
    fn default() -> Self {
        Self {
            mode: SttMode::Local,
            local_provider: LocalSttProvider::OpenAi,
            whisper_model: "base".to_string(),
            parakeet_model: "parakeet-tdt-0.6b-v3".to_string(),
            cloud_provider: "OpenAI".to_string(),
            cloud_model: "whisper-1".to_string(),
            cloud_api_key: String::new(),
            self_hosted_url: "http://localhost:8000".to_string(),
            self_hosted_model: String::new(),
            language: "auto".to_string(),
            vad_enabled: false,
            vad_threshold: 0.1,
            live_preview: false,
            gpu_backend: GpuBackendSetting::Cpu,
        }
    }
}

// ---------------------------------------------------------------------------
// Language models (Ollama-first, local-first)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LmMode {
    Ollama,
    Local,
    Cloud,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageModelSettings {
    pub mode: LmMode,
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
            mode: LmMode::Ollama,
            ollama_url: "http://localhost:11434".to_string(),
            ollama_model: "llama3.1".to_string(),
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

// ---------------------------------------------------------------------------
// Privacy & data
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacySettings {
    /// Audio retention in days; 0 = disabled.
    pub audio_retention_days: u32,
    pub history_enabled: bool,
    /// Transcript auto-delete in days; 0 = keep forever.
    pub transcript_auto_delete_days: u32,
    pub save_discarded_transcriptions: bool,
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self {
            audio_retention_days: 30,
            history_enabled: true,
            transcript_auto_delete_days: 0,
            save_discarded_transcriptions: false,
        }
    }
}

// ---------------------------------------------------------------------------
// System
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemSettings {
    pub model_cache_path: String,
}

impl Default for SystemSettings {
    fn default() -> Self {
        Self {
            model_cache_path: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Dictionary + snippets
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snippet {
    pub trigger: String,
    pub replacement: String,
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
