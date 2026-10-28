//! Microphone input level for the pill visualizer.
//!
//! [`MicInput`] is the [`LevelSource`] the widget actually drives:
//! a real capture stream (cpal) when one can be opened, otherwise the
//! deterministic [`SimMic`]. Switchable at runtime (`viz mic real|sim`)
//! so experiments can compare live input against the repeatable fake.

use crate::audio::tap_samples;
use crate::settings::MicSelection;
use crate::viz::{LevelSource, SimCadence, SimMic};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

/// Capture through `parec` (PulseAudio/PipeWire CLI): exact source
/// targeting by server name, including Bluetooth -- which the raw
/// ALSA list cannot address. One reader thread per stream feeds the
/// same RMS atomic the cpal path uses, so `drain()` is shared.
pub struct ParecMic {
    level_bits: Arc<AtomicU32>,
    smoothed: f32,
    #[allow(dead_code)]
    device_name: String,
    _child: std::process::Child,
}

impl ParecMic {
    pub fn build(source: &str) -> Result<Self, String> {
        let mut child = Command::new("parec")
            .args([
                "-d",
                source,
                "--format=s16le",
                "--rate=44100",
                "--channels=1",
                "--latency-msec=10",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn parec: {e} (is pulseaudio-utils installed?)"))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| "parec stdout unavailable".to_string())?;
        let level_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let bits = level_bits.clone();
        // Dedicated reader (long-lived): Builder-based, so a saturated
        // system fails the mic open instead of panicking the service.
        crate::runtime::spawn_dedicated("parec-reader", move || {
            // s16le mono in ~12ms slices: RMS per slice, same x8
            // staging as cpal. Small slices keep pill latency low;
            // larger ones batch updates into visible steps. Every
            // full slice is also offered to the capture tap (RAM
            // dictation buffer; no-op when disarmed).
            let mut buf = [0u8; 1024];
            loop {
                let mut got = 0;
                while got < buf.len() {
                    match stdout.read(&mut buf[got..]) {
                        Ok(0) => return, // EOF: stream ended
                        Ok(n) => got += n,
                        Err(_) => return,
                    }
                }
                let mut pcm = [0i16; 512];
                let mut sum = 0f64;
                for (i, chunk) in buf.as_chunks::<2>().0.iter().enumerate() {
                    let v = i16::from_le_bytes([chunk[0], chunk[1]]);
                    pcm[i] = v;
                    let f = v as f64 / 32768.0;
                    sum += f * f;
                }
                let rms = (sum / 512.0).sqrt() as f32;
                bits.store((rms * 8.0).clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
                tap_samples(&pcm, 44_100);
            }
        })?;
        eprintln!("[mic] parec capture active: {source}");
        Ok(Self {
            level_bits,
            smoothed: 0.0,
            device_name: source.to_string(),
            _child: child,
        })
    }

    fn drain(&mut self) -> f32 {
        let fresh = f32::from_bits(self.level_bits.load(Ordering::Relaxed));
        self.smoothed = fresh.max(self.smoothed * 0.88);
        self.smoothed
    }
}

impl Drop for ParecMic {
    fn drop(&mut self) {
        // Dropping a Child does NOT terminate it: kill explicitly so
        // utterances don't orphan parec processes holding the mic.
        // The reader thread then sees EOF and exits on its own.
        let _ = self._child.kill();
        let _ = self._child.wait();
    }
}

/// Mic preferences snapshot, read fresh from the database on every
/// dictation edge so UI changes apply without restarts or watchers.
#[derive(Debug, Clone)]
pub struct MicConfig {
    pub selection: MicSelection,
    pub device_id: String,
    pub warm_secs: u32,
    pub release_delay_ms: u64,
}

impl Default for MicConfig {
    fn default() -> Self {
        Self {
            selection: MicSelection::System,
            device_id: String::new(),
            warm_secs: 0,
            release_delay_ms: 0,
        }
    }
}

/// Current mic preferences (system default on any failure).
pub fn resolve_config() -> MicConfig {
    let Ok(paths) = crate::paths::AppPaths::create() else {
        return MicConfig::default();
    };
    let Ok(store) = crate::store::Store::open(&paths.db_file()) else {
        return MicConfig::default();
    };
    let prefs = store.load_settings().preferences;
    MicConfig {
        selection: prefs.mic_selection,
        device_id: prefs.mic_device_id,
        warm_secs: prefs.mic_warm_hold_secs,
        release_delay_ms: prefs.mic_release_delay_ms,
    }
}

/// A usable capture source from the sound server: PipeWire name +
/// human description. Monitor streams are excluded.
#[derive(Debug, Clone)]
pub struct AudioSource {
    pub name: String,
    pub description: String,
}

/// Real sources from the sound server (`pactl`), not the raw ALSA
/// list (which is full of plugins, servers and null sinks, and
/// misses Bluetooth). Falls back to cpal names when pactl is absent.
pub fn input_sources() -> Vec<AudioSource> {
    if let Ok(out) = std::process::Command::new("pactl")
        .args(["list", "sources"])
        .output()
        && out.status.success()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        let mut sources = Vec::new();
        let mut name: Option<String> = None;
        for line in text.lines() {
            let line = line.trim();
            if let Some(n) = line.strip_prefix("Name: ") {
                name = Some(n.to_string());
            } else if let Some(d) = line.strip_prefix("Description: ")
                && let Some(n) = name.take()
            {
                let is_monitor = n.ends_with(".monitor") || d.starts_with("Monitor of ");
                if !is_monitor {
                    sources.push(AudioSource {
                        name: n,
                        description: d.to_string(),
                    });
                }
            }
        }
        if !sources.is_empty() {
            return sources;
        }
    }
    cpal::default_host()
        .input_devices()
        .map(|devs| {
            devs.filter_map(|d| {
                d.description().ok().map(|desc| {
                    let n = desc.name().to_string();
                    AudioSource {
                        name: n.clone(),
                        description: n,
                    }
                })
            })
            .collect()
        })
        .unwrap_or_default()
}

/// Legacy ALSA-name list (kept for the fallback path only).
#[allow(dead_code)]
pub fn input_device_names() -> Vec<String> {
    input_sources().into_iter().map(|s| s.name).collect()
}

/// Built-in heuristic: first device that looks like onboard audio
/// and not like USB/Bluetooth. Deterministic, no probing.
pub fn pick_builtin<'a>(names: &[&'a str]) -> Option<&'a str> {
    const ONBOARD: &[&str] = &[
        "built-in", "builtin", "internal", "hda", "acp", "sof-", "pch", "legacy", "analog",
    ];
    const EXTERNAL: &[&str] = &["usb", "bluetooth", "bluez", "hdmi", "displayport"];
    names.iter().find_map(|n| {
        let lower = n.to_ascii_lowercase();
        let onboard = ONBOARD.iter().any(|k| lower.contains(k));
        let external = EXTERNAL.iter().any(|k| lower.contains(k));
        (onboard && !external).then_some(*n)
    })
}

/// Real input stream. The cpal data callback (audio thread) writes RMS
/// levels into `level_bits`; the GTK thread drains them in `sample()`.
pub struct CpalMic {
    level_bits: Arc<AtomicU32>,
    smoothed: f32,
    // Kept for future `viz devices` introspection; also logged at open.
    #[allow(dead_code)]
    device_name: String,
    _stream: cpal::Stream,
}

fn push_rms<T>(data: &[T], out: &Arc<AtomicU32>)
where
    T: cpal::Sample,
    f32: cpal::FromSample<T>,
{
    if data.is_empty() {
        return;
    }
    let sum: f64 = data
        .iter()
        .map(|s| {
            let v = s.to_sample::<f32>() as f64;
            v * v
        })
        .sum();
    let rms = (sum / data.len() as f64).sqrt() as f32;
    // Same gain staging as the render curve so conversational speech
    // (~0.02-0.15 RMS) spans the threshold and lane nicely.
    out.store((rms * 8.0).clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
}

impl CpalMic {
    /// Open the default input device (fallback when parec is
    /// unavailable; the server routes the default source).
    pub fn build() -> Result<Self, String> {
        let device = cpal::default_host()
            .default_input_device()
            .ok_or_else(|| "no default input device".to_string())?;
        Self::build_on(&device)
    }

    fn build_on(device: &cpal::Device) -> Result<Self, String> {
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "?".to_string());
        let cfg = device
            .default_input_config()
            .map_err(|e| format!("default input config: {e}"))?;

        let level_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let rate = cfg.sample_rate();
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => {
                let bits = level_bits.clone();
                device.build_input_stream(
                    cfg.into(),
                    move |data: &[f32], _| {
                        push_rms(data, &bits);
                        // f32 [-1,1] -> s16 for the capture tap.
                        let pcm: Vec<i16> = data
                            .iter()
                            .map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
                            .collect();
                        tap_samples(&pcm, rate);
                    },
                    |e| eprintln!("[poc] mic stream error: {e}"),
                    None,
                )
            }
            cpal::SampleFormat::I16 => {
                let bits = level_bits.clone();
                device.build_input_stream(
                    cfg.into(),
                    move |data: &[i16], _| {
                        push_rms(data, &bits);
                        tap_samples(data, rate);
                    },
                    |e| eprintln!("[poc] mic stream error: {e}"),
                    None,
                )
            }
            cpal::SampleFormat::U16 => {
                let bits = level_bits.clone();
                device.build_input_stream(
                    cfg.into(),
                    move |data: &[u16], _| {
                        push_rms(data, &bits);
                        let pcm: Vec<i16> =
                            data.iter().map(|s| (*s as i32 - 32768) as i16).collect();
                        tap_samples(&pcm, rate);
                    },
                    |e| eprintln!("[poc] mic stream error: {e}"),
                    None,
                )
            }
            f => return Err(format!("unsupported sample format: {f:?}")),
        }
        .map_err(|e| format!("build input stream: {e}"))?;
        stream.play().map_err(|e| format!("play stream: {e}"))?;

        eprintln!("[poc] real mic active: {name} ({cfg:?})");
        Ok(Self {
            level_bits,
            smoothed: 0.0,
            device_name: name,
            _stream: stream,
        })
    }

    // Introspection hook for a future `viz devices` command.
    #[allow(dead_code)]
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    fn drain(&mut self) -> f32 {
        let fresh = f32::from_bits(self.level_bits.load(Ordering::Relaxed));
        // Peak-hold with decay so syllables read as discrete hits.
        self.smoothed = fresh.max(self.smoothed * 0.88);
        self.smoothed
    }
}

/// Level source with automatic sim fallback.
///
/// The capture stream is demand-gated: it exists only while dictation
/// is active (`set_active(true)` on Listening, `false` on release to
/// Processing/hide), so the OS mic indicator is on exactly while we
/// listen -- never idling open in the background and never held
/// through inference.
pub struct MicInput {
    real_cpal: Option<CpalMic>,
    real_parec: Option<ParecMic>,
    sim: SimMic,
    use_real: bool,
    wanted: bool,
    /// Warm hold: when Some, the stream stays open until this deadline
    /// after dictation ends (set from prefs at every edge). Checked in
    /// `sample()`, which already ticks continuously. Also covers the
    /// release-tail settle grace (see `SETTLE_GRACE_MS`), so the close
    /// below never amputates in-flight slices.
    warm_until: Option<Instant>,
}

/// Settle grace: minimum stream hold after dictation ends so the
/// release-tail thread (`RELEASE_FLUSH_MS` in transcribe.rs) can land
/// in-flight slices before the device closes. Inference itself never
/// needs the mic; this only protects the ~1 quantum tail.
const SETTLE_GRACE_MS: u64 = 500;

/// Hold after dictation ends: the longer of the warm hold and the
/// release-tail settle window. Pure for testability.
fn settle_hold_ms(cfg: &MicConfig) -> u64 {
    (cfg.warm_secs as u64 * 1000).max(cfg.release_delay_ms + SETTLE_GRACE_MS)
}

/// Merge a new hold deadline with an armed one: never extend. A second
/// deactivate (e.g. Processing -> Hidden after release already armed
/// the hold) must not slide the close past release + hold.
fn merge_hold(existing: Option<Instant>, candidate: Instant) -> Instant {
    match existing {
        Some(e) if e < candidate => e,
        _ => candidate,
    }
}

impl MicInput {
    pub fn new() -> Self {
        Self {
            real_cpal: None,
            real_parec: None,
            sim: SimMic::new(),
            use_real: true,
            wanted: false,
            warm_until: None,
        }
    }

    /// Open the capture stream (dictation start) or schedule its close
    /// (dictation end). The stream stays open for the longer of the
    /// configured warm hold and the release-tail settle grace
    /// (`release_delay_ms + SETTLE_GRACE_MS`), so slow-starting mics
    /// don't clip the next onset and the release tail still lands;
    /// inference itself never needs the mic. Callers must deactivate
    /// on release (not on hide), so Processing never holds the mic.
    pub fn set_active(&mut self, active: bool) {
        self.wanted = active;
        if active {
            self.warm_until = None;
            if self.use_real && self.real_cpal.is_none() && self.real_parec.is_none() {
                self.open_selected();
            }
        } else {
            if self.real_cpal.is_none() && self.real_parec.is_none() {
                self.warm_until = None;
                return;
            }
            let cfg = resolve_config();
            // Always >= SETTLE_GRACE_MS by construction (see
            // settle_hold_ms): the tail outlives every close.
            let hold_ms = settle_hold_ms(&cfg);
            debug_assert!(hold_ms > 0, "settle hold must cover the release tail");
            let candidate = Instant::now() + std::time::Duration::from_millis(hold_ms);
            let deadline = merge_hold(self.warm_until, candidate);
            // A second deactivate must not re-log as a fresh arm when
            // the earlier deadline won.
            if self.warm_until != Some(deadline) {
                eprintln!("[mic] hold {hold_ms}ms (max warm {}s, settle)", cfg.warm_secs);
            }
            self.warm_until = Some(deadline);
        }
    }

    /// Switch to real input. The stream itself opens on the next
    /// `set_active(true)`; returns false only when no input device
    /// exists at all (stays on sim).
    pub fn use_real(&mut self) -> bool {
        self.use_real = true;
        true
    }

    pub fn use_sim(&mut self) {
        self.use_real = false;
        // Not close_now(): a source switch may land mid-utterance,
        // where close_now's wanted-debug_assert must not fire.
        self.close_capture();
        self.warm_until = None;
        eprintln!("[poc] mic source -> sim");
    }

    /// Immediate close: the cancel path (tap disarmed right after, no
    /// tail to transcribe) must not park the device open for the hold.
    pub fn close_now(&mut self) {
        debug_assert!(!self.wanted, "close_now must not orphan a live utterance");
        self.close_capture();
        self.warm_until = None;
    }

    fn close_capture(&mut self) {
        if self.real_cpal.is_some() || self.real_parec.is_some() {
            eprintln!("[mic] capture closed");
        }
        self.real_cpal = None;
        self.real_parec = None;
    }

    /// Open the configured source: exact server targeting through
    /// parec (System uses the server default), cpal default as the
    /// fallback when parec is unavailable, sim when nothing opens.
    fn open_selected(&mut self) {
        let cfg = resolve_config();
        let sources = input_sources();
        // Resolve the wanted server-side source name, if any.
        let wanted: Option<String> = match cfg.selection {
            MicSelection::System => None,
            MicSelection::BuiltIn => {
                let descs: Vec<&str> = sources.iter().map(|s| s.description.as_str()).collect();
                pick_builtin(&descs).and_then(|desc| {
                    sources
                        .iter()
                        .find(|s| s.description == desc)
                        .map(|s| s.name.clone())
                })
            }
            MicSelection::Specific => Some(cfg.device_id.clone()),
        };
        // Specific names a source that may be unplugged; BuiltIn may
        // find nothing. In those cases fail loudly to sim (never
        // silently capture the wrong mic).
        let target = match (cfg.selection, wanted) {
            (MicSelection::System, _) => "@DEFAULT_SOURCE@".to_string(),
            (_, Some(name)) => name,
            _ => {
                eprintln!("[mic] configured source unavailable; sim it is");
                return;
            }
        };
        match ParecMic::build(&target) {
            Ok(mic) => {
                self.real_parec = Some(mic);
                return;
            }
            Err(e) => eprintln!("[mic] parec failed ({e})"),
        }
        if matches!(cfg.selection, MicSelection::System) {
            match CpalMic::build() {
                Ok(mic) => self.real_cpal = Some(mic),
                Err(e) => eprintln!("[mic] real input unavailable ({e}); sim it is"),
            }
        }
    }

    // Introspection for the future settings UI.
    #[allow(dead_code)]
    pub fn source_name(&self) -> &'static str {
        if self.use_real { "real" } else { "sim" }
    }

    pub fn set_sim_cadence(&mut self, cadence: SimCadence) {
        self.sim.apply_cadence(cadence);
    }
}

impl LevelSource for MicInput {
    fn sample(&mut self, _dt: f32) -> f32 {
        if !self.wanted {
            // Hold countdown runs here (this ticks continuously).
            if let Some(deadline) = self.warm_until
                && Instant::now() >= deadline
            {
                eprintln!("[mic] hold expired");
                self.close_capture();
                self.warm_until = None;
            }
            return 0.0;
        }
        if self.use_real {
            if let Some(real) = self.real_parec.as_mut() {
                return real.drain();
            }
            if let Some(real) = self.real_cpal.as_mut() {
                return real.drain();
            }
            // Nothing opened: sim keeps the pill alive without
            // touching hardware.
            self.sim.sample(_dt)
        } else {
            self.sim.sample(_dt)
        }
    }

    fn burst(&mut self) {
        // Meaningless for a live mic (it hears what it hears) but
        // harmless: also kicks the sim so `burst` demos the pill.
        self.sim.burst();
    }
}

impl Default for MicInput {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Built-in picker: onboard names win, USB/HDMI lose.
    #[test]
    fn builtin_picker_prefers_onboard() {
        let names = [
            "HyperX SoloCast",
            "Built-in Audio Analog Stereo",
            "HDMI Output",
        ];
        assert_eq!(pick_builtin(&names), Some("Built-in Audio Analog Stereo"));
        let names = ["HyperX SoloCast", "HDMI Output"];
        assert_eq!(pick_builtin(&names), None);
    }

    /// Capture must exist only while dictation is active: silent when
    /// idle, live when wanted -- without touching hardware in sim mode.
    #[test]
    fn mic_gated_on_dictation_active() {
        let mut mic = MicInput::new();
        mic.use_sim();
        mic.set_active(false);
        assert_eq!(mic.sample(0.033), 0.0);
        assert!(mic.real_cpal.is_none() && mic.real_parec.is_none());
        mic.set_active(true);
        assert!(
            mic.real_cpal.is_none() && mic.real_parec.is_none(),
            "sim mode must not open hardware"
        );
        // Sim produces nonzero levels once active (may need a tick).
        let mut live = false;
        for _ in 0..60 {
            if mic.sample(0.033) > 0.0 {
                live = true;
                break;
            }
        }
        assert!(live, "sim should produce levels while active");
        mic.set_active(false);
        assert_eq!(mic.sample(0.033), 0.0);
    }

    /// Release hold covers the tail settle first, the warm hold when
    /// it is longer: Processing never holds the mic, the tail still
    /// lands.
    #[test]
    fn settle_hold_covers_tail_then_warm() {
        let cfg = MicConfig::default();
        assert_eq!(settle_hold_ms(&cfg), SETTLE_GRACE_MS);
        let cfg = MicConfig {
            release_delay_ms: 1000,
            ..MicConfig::default()
        };
        assert_eq!(settle_hold_ms(&cfg), 1000 + SETTLE_GRACE_MS);
        let cfg = MicConfig {
            warm_secs: 10,
            release_delay_ms: 1000,
            ..MicConfig::default()
        };
        assert_eq!(settle_hold_ms(&cfg), 10_000);
    }

    /// A second deactivate (Processing -> Hidden) must not slide the
    /// close past release + hold.
    #[test]
    fn hold_merge_never_extends() {
        let t0 = Instant::now();
        let early = t0 + std::time::Duration::from_millis(500);
        let late = t0 + std::time::Duration::from_millis(1000);
        assert_eq!(merge_hold(Some(early), late), early);
        assert_eq!(merge_hold(Some(late), early), early);
        assert_eq!(merge_hold(None, late), late);
    }

    /// Cancel closes now: no armed hold may survive it or a sim
    /// switch (both leave no tail to land).
    #[test]
    fn close_now_and_sim_clear_hold() {
        let t0 = Instant::now() + std::time::Duration::from_secs(60);
        let mut mic = MicInput::new();
        mic.warm_until = Some(t0);
        mic.close_now();
        assert!(mic.warm_until.is_none());
        mic.warm_until = Some(t0);
        mic.use_sim();
        assert!(mic.warm_until.is_none());
    }
}
