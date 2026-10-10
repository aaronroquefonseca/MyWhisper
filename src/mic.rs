//! Microphone input level for the pill visualizer.
//!
//! [`MicInput`] is the [`LevelSource`] the widget actually drives:
//! real capture with explicit startup/health and owned cleanup. The
//! deterministic [`SimMic`] is an explicit demo mode (`viz mic real|sim`);
//! unavailable real sources stay silent and expose a recovery error.

use crate::audio::tap_samples;
use crate::settings::MicSelection;
use crate::viz::{LevelSource, SimCadence, SimMic};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::time::Instant;

/// Capture through `parec` (PulseAudio/PipeWire CLI): exact source
/// targeting by server name, including Bluetooth -- which the raw
/// ALSA list cannot address. One reader thread per stream feeds the
/// same RMS atomic the cpal path uses, so `drain()` is shared.
pub struct ParecMic {
    level_bits: Arc<AtomicU32>,
    health: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
    smoothed: f32,
    child: Option<crate::child::OwnedChild>,
    reader: Option<crate::runtime::SessionTask>,
}

impl ParecMic {
    pub fn build(source: &str) -> Result<Self, String> {
        let mut command = Command::new("parec");
        command.args([
            "-d",
            source,
            "--format=s16le",
            "--rate=44100",
            "--channels=1",
            "--latency-msec=10",
        ]);
        Self::open_command(&mut command, Arc::new(AtomicBool::new(false)))
    }

    fn open_command(command: &mut Command, stop: Arc<AtomicBool>) -> Result<Self, String> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = crate::child::OwnedChild::spawn(command)?;
        let mut stdout = child.0.stdout.take().ok_or("parec stdout unavailable")?;
        crate::child::nonblocking(&stdout).map_err(|e| e.to_string())?;
        let level_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let health = Arc::new(AtomicU8::new(0));
        let (bits, status, stopping) = (level_bits.clone(), health.clone(), stop.clone());
        let reader = crate::runtime::spawn_session("parec-reader", move || {
            let mut buf = [0u8; 1024];
            let mut carry = None;
            while !stopping.load(Ordering::Acquire) {
                let count = match stdout.read(&mut buf) {
                    Ok(0) => break,
                    Ok(count) => count,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                let mut pcm = [0i16; 513];
                let mut samples = 0;
                let mut offset = 0;
                if let Some(first) = carry.take() {
                    pcm[samples] = i16::from_le_bytes([first, buf[0]]);
                    samples += 1;
                    offset = 1;
                }
                for bytes in buf[offset..count].as_chunks::<2>().0 {
                    pcm[samples] = i16::from_le_bytes([bytes[0], bytes[1]]);
                    samples += 1;
                }
                if (count - offset) % 2 != 0 {
                    carry = Some(buf[count - 1]);
                }
                if samples > 0 {
                    let sum: f64 = pcm[..samples]
                        .iter()
                        .map(|sample| (f64::from(*sample) / 32768.0).powi(2))
                        .sum();
                    let rms = (sum / samples as f64).sqrt() as f32;
                    bits.store((rms * 8.0).clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
                    if stopping.load(Ordering::Acquire) {
                        break;
                    }
                    tap_samples(&pcm[..samples], 44_100);
                    status.store(1, Ordering::Release);
                }
            }
            status.store(2, Ordering::Release);
        })?;
        let mic = Self {
            level_bits,
            health,
            stop,
            smoothed: 0.0,
            child: Some(child),
            reader: Some(reader),
        };
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if mic.stop.load(Ordering::Acquire) {
                return Err("microphone startup cancelled".into());
            }
            match mic.health.load(Ordering::Acquire) {
                1 => return Ok(mic),
                2 => return Err("microphone capture ended before becoming ready".into()),
                _ => {}
            }
            if mic
                .child
                .as_ref()
                .unwrap()
                .try_status()
                .map_err(|e| e.to_string())?
                .is_some()
            {
                return Err("microphone process exited during startup".into());
            }
            if Instant::now() >= deadline {
                return Err("microphone did not supply audio before its readiness deadline".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn healthy(&self) -> bool {
        self.health.load(Ordering::Acquire) == 1
            && self
                .child
                .as_ref()
                .is_some_and(|child| child.try_status().ok() == Some(None))
    }
    fn drain(&mut self) -> f32 {
        if !self.healthy() {
            return 0.0;
        }
        let fresh = f32::from_bits(self.level_bits.load(Ordering::Relaxed));
        self.smoothed = fresh.max(self.smoothed * 0.88);
        self.smoothed
    }
}
impl Drop for ParecMic {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.child.take(); // kill/reap before joining the nonblocking reader
        if let Some(reader) = self.reader.take() {
            reader.join();
        }
    }
}

/// Mic preferences snapshot, read fresh from the database on every
/// dictation edge so UI changes apply without restarts or watchers.
#[derive(Debug, Clone)]
pub struct MicConfig {
    pub selection: MicSelection,
    pub device_id: String,
    pub warm_secs: u32,
}

impl Default for MicConfig {
    fn default() -> Self {
        Self {
            selection: MicSelection::System,
            device_id: String::new(),
            warm_secs: 0,
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
    let mut command = Command::new("pactl");
    command.args(["list", "sources"]);
    if let Ok(out) = crate::child::run(
        &mut command,
        &[],
        std::time::Duration::from_secs(2),
        1024 * 1024,
    ) {
        let text = String::from_utf8_lossy(&out);
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
    health: Arc<AtomicBool>,
    failure: Arc<AtomicU8>,
    level_bits: Arc<AtomicU32>,
    smoothed: f32,
    // Kept for future `viz devices` introspection; also logged at open.
    #[allow(dead_code)]
    device_name: String,
    _stream: cpal::Stream,
    _drain: CaptureDrain,
}

const CALLBACK_CHUNK: usize = 512;
const CALLBACK_QUEUE: usize = 32;
struct CaptureChunk {
    recorder: Arc<std::sync::Mutex<crate::audio::Recorder>>,
    samples: [i16; CALLBACK_CHUNK],
    len: usize,
    rate: u32,
    stamp: Instant,
}

struct CaptureDrain {
    stop: Arc<AtomicBool>,
    task: Option<crate::runtime::SessionTask>,
}
fn drain_capture(mut consumer: rtrb::Consumer<CaptureChunk>, stopping: &AtomicBool) {
    loop {
        match consumer.pop() {
            Ok(chunk) => {
                if let Ok(mut recorder) = chunk.recorder.lock() {
                    crate::audio::push_capture(
                        &mut recorder,
                        &chunk.samples[..chunk.len],
                        chunk.rate,
                        chunk.stamp,
                    );
                }
            }
            Err(_) if stopping.load(Ordering::Acquire) || consumer.is_abandoned() => break,
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(2)),
        }
    }
}
impl CaptureDrain {
    fn new() -> Result<(rtrb::Producer<CaptureChunk>, Self), String> {
        let (producer, consumer) = rtrb::RingBuffer::<CaptureChunk>::new(CALLBACK_QUEUE);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let task = crate::runtime::spawn_session("cpal-capture-drain", move || {
            drain_capture(consumer, &stopping);
        })?;
        Ok((
            producer,
            Self {
                stop,
                task: Some(task),
            },
        ))
    }
}
impl Drop for CaptureDrain {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // The bounded session reaper owns completion; a GUI teardown never
        // waits for a recorder mutex held by inference on another thread.
        self.task.take();
    }
}
impl Drop for CpalMic {
    fn drop(&mut self) {
        self.health.store(false, Ordering::Release);
    }
}

struct MonoFrames {
    channels: usize,
    sum: f32,
    count: usize,
}
impl MonoFrames {
    fn reset(&mut self) {
        self.sum = 0.0;
        self.count = 0;
    }
    fn feed<T: Copy>(
        &mut self,
        data: &[T],
        convert: impl Fn(T) -> f32,
        mut emit: impl FnMut(i16) -> Result<(), ()>,
    ) -> Result<(), ()> {
        if self.channels == 0 || self.channels > 32 || data.len() > 256 * 1024 {
            return Err(());
        }
        for sample in data {
            let sample = convert(*sample);
            if !sample.is_finite() {
                return Err(());
            }
            self.sum += sample.clamp(-1.0, 1.0);
            self.count += 1;
            if self.count == self.channels {
                emit((self.sum / self.channels as f32 * 32767.0) as i16)?;
                self.reset();
            }
        }
        Ok(())
    }
}

struct CpalFeed {
    frames: MonoFrames,
    producer: rtrb::Producer<CaptureChunk>,
    owner: Option<Arc<std::sync::Mutex<crate::audio::Recorder>>>,
    pending: [i16; CALLBACK_CHUNK],
    pending_len: usize,
    pending_stamp: Instant,
    health: Arc<AtomicBool>,
    failure: Arc<AtomicU8>,
    ready: Arc<AtomicBool>,
    level_bits: Arc<AtomicU32>,
    rate: u32,
}
fn queue_capture(
    producer: &mut rtrb::Producer<CaptureChunk>,
    owner: &Option<Arc<std::sync::Mutex<crate::audio::Recorder>>>,
    samples: [i16; CALLBACK_CHUNK],
    len: usize,
    rate: u32,
    stamp: Instant,
) -> Result<(), ()> {
    if let Some(recorder) = owner {
        producer
            .push(CaptureChunk {
                recorder: recorder.clone(),
                samples,
                len,
                rate,
                stamp,
            })
            .map_err(|_| ())?;
    }
    Ok(())
}
impl Drop for CpalFeed {
    fn drop(&mut self) {
        if self.pending_len > 0
            && queue_capture(
                &mut self.producer,
                &self.owner,
                self.pending,
                self.pending_len,
                self.rate,
                self.pending_stamp,
            )
            .is_err()
        {
            self.failure.store(4, Ordering::Release);
            self.health.store(false, Ordering::Release);
        }
    }
}
impl CpalFeed {
    fn input<T>(&mut self, data: &[T], info: &cpal::InputCallbackInfo)
    where
        T: cpal::Sample,
        f32: cpal::FromSample<T>,
    {
        let timing = info.timestamp();
        let stamp = timing
            .callback
            .checked_duration_since(timing.capture)
            .and_then(|delay| Instant::now().checked_sub(delay));
        if let Some(stamp) = stamp {
            self.input_at(data, stamp);
        } else {
            self.failure.store(2, Ordering::Release);
            self.health.store(false, Ordering::Release);
        }
    }

    fn input_at<T>(&mut self, data: &[T], stamp: Instant)
    where
        T: cpal::Sample,
        f32: cpal::FromSample<T>,
    {
        if !self.health.load(Ordering::Acquire) {
            return;
        }
        let Ok(owner) = crate::audio::callback_recorder() else {
            self.failure.store(3, Ordering::Release);
            self.health.store(false, Ordering::Release);
            return;
        };
        self.input_target(data, stamp, owner);
    }

    fn input_target<T>(
        &mut self,
        data: &[T],
        stamp: Instant,
        owner: Option<Arc<std::sync::Mutex<crate::audio::Recorder>>>,
    ) where
        T: cpal::Sample,
        f32: cpal::FromSample<T>,
    {
        if !match (&self.owner, &owner) {
            (Some(old), Some(new)) => Arc::ptr_eq(old, new),
            (None, None) => true,
            _ => false,
        } {
            // Retire the old owner on the drain thread even for an empty
            // batch; dropping its last reference may free a large recorder.
            if queue_capture(
                &mut self.producer,
                &self.owner,
                self.pending,
                self.pending_len,
                self.rate,
                self.pending_stamp,
            )
            .is_err()
            {
                self.failure.store(4, Ordering::Release);
                self.health.store(false, Ordering::Release);
                return;
            }
            self.pending_len = 0;
            self.frames.reset();
        }
        self.owner = owner;
        push_rms(data, &self.level_bits);
        let samples = &mut self.pending;
        let len = &mut self.pending_len;
        self.pending_stamp = stamp;
        let owner = &self.owner;
        let producer = &mut self.producer;
        let failure = &self.failure;
        let rate = self.rate;
        let result = self.frames.feed(
            data,
            |sample| sample.to_sample::<f32>(),
            |sample| {
                if owner.is_none() {
                    return Ok(());
                }
                samples[*len] = sample;
                *len += 1;
                if *len == CALLBACK_CHUNK {
                    queue_capture(producer, owner, *samples, *len, rate, stamp).map_err(|_| {
                        failure.store(4, Ordering::Release);
                    })?;
                    *len = 0;
                }
                Ok(())
            },
        );
        if result.is_err() {
            // Queue pressure or invalid input aborts capture explicitly. Never
            // block the audio callback or silently deliver missing speech.
            let _ = self
                .failure
                .compare_exchange(0, 5, Ordering::AcqRel, Ordering::Acquire);
            self.health.store(false, Ordering::Release);
        } else if !data.is_empty() {
            self.ready.store(true, Ordering::Release);
        }
    }
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
        Self::build_cancelled(Arc::new(AtomicBool::new(false)))
    }

    fn build_cancelled(cancel: Arc<AtomicBool>) -> Result<Self, String> {
        let device = cpal::default_host()
            .default_input_device()
            .ok_or_else(|| "no default input device".to_string())?;
        Self::build_on(&device, cancel)
    }

    fn build_on(device: &cpal::Device, cancel: Arc<AtomicBool>) -> Result<Self, String> {
        let cfg = device
            .default_input_config()
            .map_err(|e| format!("default input config: {e}"))?;
        Self::build_config(device, cfg, cancel)
    }

    fn build_config(
        device: &cpal::Device,
        cfg: cpal::SupportedStreamConfig,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "?".to_string());
        let level_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let health = Arc::new(AtomicBool::new(true));
        let failure = Arc::new(AtomicU8::new(0));
        let rate = cfg.sample_rate();
        let channels = cfg.channels() as usize;
        if channels == 0 || channels > 32 || rate == 0 || rate > 384_000 {
            return Err("microphone channel count/sample rate exceeds capture limits".into());
        }
        let (producer, drain) = CaptureDrain::new()?;
        let ready = Arc::new(AtomicBool::new(false));
        let mut feed = CpalFeed {
            frames: MonoFrames {
                channels,
                sum: 0.0,
                count: 0,
            },
            producer,
            owner: None,
            pending: [0; CALLBACK_CHUNK],
            pending_len: 0,
            pending_stamp: Instant::now(),
            health: health.clone(),
            failure: failure.clone(),
            ready: ready.clone(),
            level_bits: level_bits.clone(),
            rate,
        };
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => {
                let status = health.clone();
                device.build_input_stream(
                    cfg.into(),
                    move |data: &[f32], info| feed.input(data, info),
                    move |_| {
                        status.store(false, Ordering::Release);
                    },
                    None,
                )
            }
            cpal::SampleFormat::I16 => {
                let status = health.clone();
                device.build_input_stream(
                    cfg.into(),
                    move |data: &[i16], info| feed.input(data, info),
                    move |_| {
                        status.store(false, Ordering::Release);
                    },
                    None,
                )
            }
            cpal::SampleFormat::U16 => {
                let status = health.clone();
                device.build_input_stream(
                    cfg.into(),
                    move |data: &[u16], info| feed.input(data, info),
                    move |_| {
                        status.store(false, Ordering::Release);
                    },
                    None,
                )
            }
            f => return Err(format!("unsupported sample format: {f:?}")),
        }
        .map_err(|e| format!("build input stream: {e}"))?;
        stream.play().map_err(|e| format!("play stream: {e}"))?;

        eprintln!("[poc] real mic active: {name} ({cfg:?})");
        let mic = Self {
            health,
            failure,
            level_bits,
            smoothed: 0.0,
            device_name: name,
            _stream: stream,
            _drain: drain,
        };
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        while !ready.load(Ordering::Acquire) {
            if cancel.load(Ordering::Acquire) {
                return Err("microphone startup cancelled".into());
            }
            if !mic.health.load(Ordering::Acquire) {
                return Err("microphone capture failed before becoming ready".into());
            }
            if Instant::now() >= deadline {
                return Err("microphone did not supply audio before its readiness deadline".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Ok(mic)
    }

    // Introspection hook for a future `viz devices` command.
    #[allow(dead_code)]
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    fn capture_error(&self) -> Option<&'static str> {
        if self.health.load(Ordering::Acquire) {
            return None;
        }
        Some(match self.failure.load(Ordering::Acquire) {
            2 => {
                "Microphone supplied invalid capture timestamps. Check the input device and retry."
            }
            3 => "Microphone capture ownership could not be read. Retry dictation.",
            4 => {
                "Microphone capture exceeded its processing budget. Retry with the selected input device."
            }
            5 => {
                "Microphone supplied invalid samples or an oversized audio callback. Check the input device and retry."
            }
            _ => "Microphone capture stopped. Check the selected input device and retry.",
        })
    }

    fn drain(&mut self) -> f32 {
        let fresh = f32::from_bits(self.level_bits.load(Ordering::Relaxed));
        // Peak-hold with decay so syllables read as discrete hits.
        self.smoothed = fresh.max(self.smoothed * 0.88);
        self.smoothed
    }
}

enum Capture {
    Parec(ParecMic),
    Cpal(CpalMic),
}

/// Real capture state and explicit demo simulation.
///
/// The capture stream is demand-gated: it exists only while dictation
/// is active (`set_active(true)` on pill show, `false` on hide), so
/// the OS mic indicator is on exactly while we listen -- never
/// idling open in the background.
pub struct MicInput {
    real_cpal: Option<CpalMic>,
    real_parec: Option<ParecMic>,
    sim: SimMic,
    use_real: bool,
    wanted: bool,
    /// Warm hold: when Some, the stream stays open until this deadline
    /// after dictation ends (set from prefs at every edge). Checked in
    /// `sample()`, which already ticks continuously.
    warm_until: Option<Instant>,
    warm_secs: u32,
    opening: Option<std::sync::mpsc::Receiver<Result<(MicConfig, Capture), String>>>,
    opening_cancel: Option<Arc<AtomicBool>>,
    opening_task: Option<crate::runtime::SessionTask>,
    error: Option<String>,
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
            warm_secs: 0,
            opening: None,
            opening_cancel: None,
            opening_task: None,
            error: None,
        }
    }

    /// Open the capture stream (pill show) or schedule its close (pill
    /// hide). With a warm hold configured, the stream stays open for
    /// that many seconds after use so slow-starting mics don't clip
    /// the next utterance's onset; 0 closes immediately.
    pub fn set_active(&mut self, active: bool) {
        self.wanted = active;
        if active {
            self.warm_until = None;
            if self.use_real
                && self.real_cpal.is_none()
                && self.real_parec.is_none()
                && self.opening.is_none()
            {
                self.open_selected();
            }
        } else {
            self.cancel_open();
            let warm = self.warm_secs;
            if warm > 0 && (self.real_cpal.is_some() || self.real_parec.is_some()) {
                self.warm_until =
                    Some(Instant::now() + std::time::Duration::from_secs(warm as u64));
                eprintln!("[mic] warm hold {warm}s");
            } else {
                self.close_capture();
                self.warm_until = None;
            }
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
        self.close_capture();
        eprintln!("[poc] mic source -> sim");
    }

    fn cancel_open(&mut self) {
        if let Some(cancel) = self.opening_cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        self.opening.take();
        self.opening_task.take();
    }

    fn close_capture(&mut self) {
        self.cancel_open();
        if self.real_cpal.is_some() || self.real_parec.is_some() {
            eprintln!("[mic] capture closed");
        }
        self.real_cpal = None;
        self.real_parec = None;
    }

    fn open_selected(&mut self) {
        self.error = None;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        match crate::runtime::spawn_session("mic-start", move || {
            let cfg = resolve_config();
            let result = open_capture(&cfg, worker_stop);
            let _ = tx.send(result.map(|capture| (cfg, capture)));
        }) {
            Ok(task) => {
                self.opening = Some(rx);
                self.opening_cancel = Some(stop);
                self.opening_task = Some(task);
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn poll_open(&mut self) {
        let Some(receiver) = self.opening.as_ref() else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("microphone worker failed".into())
            }
        };
        self.opening.take();
        self.opening_cancel.take();
        self.opening_task.take();
        match result {
            Ok((config, capture)) if self.wanted => {
                self.warm_secs = config.warm_secs;
                match capture {
                    Capture::Parec(mic) => self.real_parec = Some(mic),
                    Capture::Cpal(mic) => self.real_cpal = Some(mic),
                }
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!("[mic] capture unavailable: {error}");
                self.error = Some(error);
            }
        }
    }

    pub fn capture_error(&self) -> Option<&str> {
        if !self.use_real || !self.wanted || self.opening.is_some() {
            return None;
        }
        if crate::audio::capture_failed() {
            return Some(
                "Capture exceeded its budget or changed sample rate. Check the selected microphone and retry.",
            );
        }
        if self.real_parec.as_ref().is_some_and(|mic| !mic.healthy()) {
            return Some(
                "Microphone capture stopped or exceeded its processing budget. Check the selected input device and retry.",
            );
        }
        if let Some(error) = self.real_cpal.as_ref().and_then(CpalMic::capture_error) {
            return Some(error);
        }
        self.error.as_deref()
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

fn open_capture(cfg: &MicConfig, stop: Arc<AtomicBool>) -> Result<Capture, String> {
    if stop.load(Ordering::Acquire) {
        return Err("microphone startup cancelled".into());
    }
    let target = match cfg.selection {
        MicSelection::System => "@DEFAULT_SOURCE@".to_owned(),
        MicSelection::Specific if !cfg.device_id.is_empty() => cfg.device_id.clone(),
        MicSelection::BuiltIn => {
            let sources = input_sources();
            let descriptions = sources
                .iter()
                .map(|source| source.description.as_str())
                .collect::<Vec<_>>();
            pick_builtin(&descriptions)
                .and_then(|description| {
                    sources
                        .iter()
                        .find(|source| source.description == description)
                })
                .map(|source| source.name.clone())
                .ok_or("configured built-in microphone is unavailable")?
        }
        _ => return Err("configured microphone is unavailable".into()),
    };
    let mut command = Command::new("parec");
    command.args([
        "-d",
        &target,
        "--format=s16le",
        "--rate=44100",
        "--channels=1",
        "--latency-msec=10",
    ]);
    match ParecMic::open_command(&mut command, stop.clone()) {
        Ok(mic) => Ok(Capture::Parec(mic)),
        Err(error) if !matches!(cfg.selection, MicSelection::System) => Err(error),
        Err(error) => {
            if stop.load(Ordering::Acquire) {
                return Err("microphone startup cancelled".into());
            }
            eprintln!("[mic] sound-server capture failed: {error}; trying the default CPAL device");
            CpalMic::build_cancelled(stop).map(Capture::Cpal)
        }
    }
}

impl Drop for MicInput {
    fn drop(&mut self) {
        self.close_capture();
    }
}

impl Default for MicInput {
    fn default() -> Self {
        Self::new()
    }
}

impl LevelSource for MicInput {
    fn sample(&mut self, _dt: f32) -> f32 {
        self.poll_open();
        if !self.wanted {
            // Warm hold countdown runs here (this ticks continuously).
            if let Some(deadline) = self.warm_until
                && Instant::now() >= deadline
            {
                eprintln!("[mic] warm hold expired, capture closed");
                self.real_cpal = None;
                self.real_parec = None;
                self.warm_until = None;
            }
            return 0.0;
        }
        if self.use_real {
            if let Some(real) = self.real_parec.as_mut() {
                return real.drain();
            }
            if let Some(real) = self.real_cpal.as_mut() {
                return if real.health.load(Ordering::Acquire) {
                    real.drain()
                } else {
                    0.0
                };
            }
            // Simulation is explicit demo mode only. A missing real capture
            // must stay silent and be reported by the engine's capture state.
            0.0
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
}

#[cfg(test)]
fn downmix<T: Copy>(input: &[T], channels: usize, to_sample: impl Fn(T) -> f32) -> Vec<i16> {
    let mut output = Vec::new();
    let mut frames = MonoFrames {
        channels,
        sum: 0.0,
        count: 0,
    };
    let _ = frames.feed(input, to_sample, |sample| {
        output.push(sample);
        Ok(())
    });
    output
}

#[cfg(test)]
mod format_regressions {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
        static ALLOCATION_COUNT: Cell<usize> = const { Cell::new(0) };
    }
    struct CountingAllocator;
    fn count_allocation() {
        if TRACK_ALLOCATIONS.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCATION_COUNT.try_with(|count| count.set(count.get() + 1));
        }
    }
    // Tracking is enabled on this test's thread only, after all fixtures are
    // allocated. Other parallel tests and the runtime allocator are unaffected.
    unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
            count_allocation();
            unsafe { std::alloc::GlobalAlloc::alloc(&std::alloc::System, layout) }
        }
        unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout) {
            unsafe { std::alloc::GlobalAlloc::dealloc(&std::alloc::System, pointer, layout) }
        }
        unsafe fn realloc(
            &self,
            pointer: *mut u8,
            layout: std::alloc::Layout,
            size: usize,
        ) -> *mut u8 {
            count_allocation();
            unsafe { std::alloc::GlobalAlloc::realloc(&std::alloc::System, pointer, layout, size) }
        }
    }
    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    fn native_cpal_file_source_gate() {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let Some(root) = std::env::var_os("MYWHISPER_CPAL_NATIVE_GATE") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        assert_eq!(
            Some(root.as_os_str()),
            std::env::var_os("MYWHISPER_AUDIT_ROOT").as_deref()
        );
        assert!(root.join(".disposable").is_file());
        assert_eq!(root.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(
            std::env::var_os("ALSA_CONFIG_PATH").unwrap(),
            root.join("alsa.conf")
        );
        let channels: u16 = std::env::var("MYWHISPER_CPAL_CHANNELS")
            .unwrap()
            .parse()
            .unwrap();
        assert!([1, 2, 6].contains(&channels));
        let format = std::env::var("MYWHISPER_CPAL_FORMAT").unwrap();
        let (sample_format, sample) = match format.as_str() {
            "f32" => (cpal::SampleFormat::F32, 0.5f32.to_le_bytes().to_vec()),
            "i16" => (cpal::SampleFormat::I16, 16384i16.to_le_bytes().to_vec()),
            "u16" => (cpal::SampleFormat::U16, 49152u16.to_le_bytes().to_vec()),
            _ => panic!("unexpected native sample format"),
        };
        let mut bytes = Vec::new();
        for _ in 0..480 * channels as usize {
            bytes.extend_from_slice(&sample);
        }
        let mut fifo = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(root.join("synthetic.fifo"))
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        struct Writer {
            stop: Arc<AtomicBool>,
            task: Option<std::thread::JoinHandle<()>>,
        }
        impl Drop for Writer {
            fn drop(&mut self) {
                self.stop.store(true, Ordering::Release);
                if let Some(task) = self.task.take() {
                    task.join().unwrap();
                }
            }
        }
        let writer = Writer {
            stop,
            task: Some(std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(100));
                while !stopping.load(Ordering::Acquire) {
                    let mut offset = 0;
                    while offset < bytes.len() && !stopping.load(Ordering::Acquire) {
                        match fifo.write(&bytes[offset..]) {
                            Ok(0) => panic!("synthetic source stopped accepting samples"),
                            Ok(count) => offset += count,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(std::time::Duration::from_millis(1));
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                            Err(error) => panic!("synthetic source failed: {error}"),
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            })),
        };
        let device = cpal::default_host().default_input_device().unwrap();
        let cfg = cpal::SupportedStreamConfig::new(
            channels,
            48_000,
            cpal::SupportedBufferSize::Unknown,
            sample_format,
        );
        let mic = CpalMic::build_config(&device, cfg, Arc::new(AtomicBool::new(false))).unwrap();
        let recorder = Arc::new(std::sync::Mutex::new(crate::audio::Recorder::default()));
        recorder.lock().unwrap().start(0);
        struct Tap(Arc<std::sync::Mutex<crate::audio::Recorder>>);
        impl Drop for Tap {
            fn drop(&mut self) {
                crate::audio::clear_recorder(&self.0);
            }
        }
        crate::audio::set_recorder(Some(recorder.clone()));
        let tap = Tap(recorder.clone());
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        while recorder.lock().unwrap().len() < 2048 {
            assert!(
                mic.health.load(Ordering::Acquire),
                "native capture became unhealthy (reason {})",
                mic.failure.load(Ordering::Acquire)
            );
            assert!(Instant::now() < deadline, "native capture made no progress");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        drop(tap);
        let close = Instant::now();
        drop(mic);
        assert!(close.elapsed() < std::time::Duration::from_secs(1));
        drop(writer);
        let recorder = recorder.lock().unwrap();
        assert_eq!(recorder.rate(), 48_000);
        assert!(!recorder.overflowed());
        assert!(
            recorder.snapshot().iter().all(|sample| *sample == 16383),
            "native channel conversion changed sample values"
        );
        eprintln!(
            "Native CPAL {format}/{channels} channels: synthetic source captured, downmixed and closed"
        );
    }

    #[test]
    fn capture_shutdown_drains_queued_audio_into_its_original_recorder() {
        let (mut producer, consumer) = rtrb::RingBuffer::new(CALLBACK_QUEUE);
        let recorder = Arc::new(std::sync::Mutex::new(crate::audio::Recorder::default()));
        recorder.lock().unwrap().start(44_100);
        assert!(
            producer
                .push(CaptureChunk {
                    recorder: recorder.clone(),
                    samples: [7; CALLBACK_CHUNK],
                    len: 3,
                    rate: 44_100,
                    stamp: Instant::now()
                })
                .is_ok()
        );
        drop(producer);
        drain_capture(consumer, &AtomicBool::new(true));
        assert_eq!(recorder.lock().unwrap().snapshot(), [7, 7, 7]);
    }

    #[test]
    fn callback_queue_never_allocates_blocks_or_redirects_old_slices() {
        let (producer, mut consumer) = rtrb::RingBuffer::new(CALLBACK_QUEUE);
        let health = Arc::new(AtomicBool::new(true));
        let ready = Arc::new(AtomicBool::new(false));
        let old = Arc::new(std::sync::Mutex::new(crate::audio::Recorder::default()));
        let new = Arc::new(std::sync::Mutex::new(crate::audio::Recorder::default()));
        old.lock().unwrap().start(44_100);
        new.lock().unwrap().start(44_100);
        let mut feed = CpalFeed {
            frames: MonoFrames {
                channels: 2,
                sum: 0.0,
                count: 0,
            },
            producer,
            owner: None,
            pending: [0; CALLBACK_CHUNK],
            pending_len: 0,
            pending_stamp: Instant::now(),
            health: health.clone(),
            failure: Arc::new(AtomicU8::new(0)),
            ready: ready.clone(),
            level_bits: Arc::new(AtomicU32::new(0)),
            rate: 44_100,
        };
        // Holding the inference-side mutex must not block the callback.
        let hold = old.lock().unwrap();
        let stamp = Instant::now();
        ALLOCATION_COUNT.with(|count| count.set(0));
        TRACK_ALLOCATIONS.with(|track| track.set(true));
        feed.input_target(&[0.5f32; 1024], stamp, Some(old.clone()));
        feed.input_target(&[16384i16; 1024], stamp, Some(new.clone()));
        feed.input_target(&[49152u16; 1024], stamp, Some(new.clone()));
        TRACK_ALLOCATIONS.with(|track| track.set(false));
        assert!(stamp.elapsed() < std::time::Duration::from_millis(100));
        assert_eq!(ALLOCATION_COUNT.with(Cell::get), 0);
        assert!(health.load(Ordering::Acquire));
        assert!(ready.load(Ordering::Acquire));
        drop(hold);
        let chunk = consumer.pop().unwrap();
        assert!(Arc::ptr_eq(&chunk.recorder, &old));
        crate::audio::push_capture(
            &mut chunk.recorder.lock().unwrap(),
            &chunk.samples[..chunk.len],
            chunk.rate,
            chunk.stamp,
        );
        let retired = consumer.pop().unwrap();
        assert!(Arc::ptr_eq(&retired.recorder, &old));
        assert_eq!(
            retired.len, 0,
            "old ownership is retired on the drain thread"
        );
        for _ in 0..2 {
            let chunk = consumer.pop().unwrap();
            assert!(Arc::ptr_eq(&chunk.recorder, &new));
            crate::audio::push_capture(
                &mut chunk.recorder.lock().unwrap(),
                &chunk.samples[..chunk.len],
                chunk.rate,
                chunk.stamp,
            );
        }
        assert_eq!(old.lock().unwrap().len(), 512);
        assert_eq!(new.lock().unwrap().len(), 1024);
        assert!(
            old.lock()
                .unwrap()
                .snapshot()
                .iter()
                .all(|sample| *sample == 16383)
        );
        for _ in 0..CALLBACK_QUEUE {
            feed.input_target(&[1i16; 1024], stamp, Some(new.clone()));
        }
        let start = Instant::now();
        feed.input_target(&[1i16; 1024], stamp, Some(new.clone()));
        assert!(start.elapsed() < std::time::Duration::from_millis(100));
        assert!(
            !health.load(Ordering::Acquire),
            "saturated capture must fail explicitly"
        );
        assert_eq!(consumer.slots(), CALLBACK_QUEUE);
    }

    #[test]
    fn tiny_callbacks_coalesce_and_pending_batches_survive_owner_change_and_drop() {
        let (producer, mut consumer) = rtrb::RingBuffer::new(CALLBACK_QUEUE);
        let old = Arc::new(std::sync::Mutex::new(crate::audio::Recorder::default()));
        let new = Arc::new(std::sync::Mutex::new(crate::audio::Recorder::default()));
        let mut feed = CpalFeed {
            frames: MonoFrames {
                channels: 1,
                sum: 0.0,
                count: 0,
            },
            producer,
            owner: None,
            pending: [0; CALLBACK_CHUNK],
            pending_len: 0,
            pending_stamp: Instant::now(),
            health: Arc::new(AtomicBool::new(true)),
            failure: Arc::new(AtomicU8::new(0)),
            ready: Arc::new(AtomicBool::new(false)),
            level_bits: Arc::new(AtomicU32::new(0)),
            rate: 44_100,
        };
        let stamp = Instant::now();
        for _ in 0..513 {
            feed.input_target(&[16384i16], stamp, Some(old.clone()));
        }
        assert_eq!(
            consumer.slots(),
            1,
            "one-sample callbacks must form full batches"
        );
        feed.input_target(&[8192i16], stamp, Some(new.clone()));
        drop(feed);
        assert_eq!(consumer.slots(), 3);
        let full = consumer.pop().unwrap();
        let old_tail = consumer.pop().unwrap();
        let new_tail = consumer.pop().unwrap();
        assert!(Arc::ptr_eq(&full.recorder, &old));
        assert_eq!(full.len, 512);
        assert!(full.samples.iter().all(|sample| *sample == 16383));
        assert!(Arc::ptr_eq(&old_tail.recorder, &old));
        assert_eq!(old_tail.len, 1);
        assert_eq!(old_tail.samples[0], 16383);
        assert!(Arc::ptr_eq(&new_tail.recorder, &new));
        assert_eq!(new_tail.len, 1);
        assert_eq!(new_tail.samples[0], 8191);
        assert!(consumer.is_abandoned());
    }

    #[test]
    fn partial_channel_frames_and_invalid_samples_are_handled_explicitly() {
        let mut frames = MonoFrames {
            channels: 2,
            sum: 0.0,
            count: 0,
        };
        let mut output = Vec::new();
        for data in [&[0.5][..], &[-0.5, 1.0][..], &[1.0][..]] {
            frames
                .feed(
                    data,
                    |sample| sample,
                    |sample| {
                        output.push(sample);
                        Ok(())
                    },
                )
                .unwrap();
        }
        assert_eq!(output, [0, 32767]);
        assert!(
            frames
                .feed(&[f32::NAN], |sample| sample, |_| Ok(()))
                .is_err()
        );
        frames.channels = 33;
        assert!(
            frames
                .feed(&[0.0; 33], |sample| sample, |_| Ok(()))
                .is_err()
        );
    }
    #[test]
    fn interleaved_frames_downmix_without_changing_duration_or_order() {
        assert_eq!(
            super::downmix(&[0.5f32, -0.5, 1.0, 1.0], 2, |x| x),
            [0, 32767]
        );
        assert_eq!(super::downmix(&[0.25f32; 24], 6, |x| x), [8191; 4]);
        assert_eq!(
            super::downmix(&[16384i16; 4], 1, |x| x as f32 / 32768.0),
            [16383; 4]
        );
        assert!(super::downmix(&[1i16], 0, |x| x as f32).is_empty());
        native_cpal_file_source_gate();
    }
}

#[cfg(test)]
mod lifecycle_regressions {
    use super::*;
    use std::time::Duration;

    #[test]
    fn exited_or_silent_capture_process_cannot_report_readiness() {
        let mut exited = Command::new("python3");
        exited.args(["-c", "raise SystemExit(1)"]);
        let started = Instant::now();
        assert!(ParecMic::open_command(&mut exited, Arc::new(AtomicBool::new(false))).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        let mut silent = Command::new("python3");
        silent.args(["-c", "import time; time.sleep(30)"]);
        let started = Instant::now();
        assert!(ParecMic::open_command(&mut silent, Arc::new(AtomicBool::new(false))).is_err());
        assert!(started.elapsed() < Duration::from_millis(2500));
    }

    #[test]
    fn startup_cancel_joins_reader_and_reaps_owned_process_without_waiting_for_audio() {
        let data = tempfile::tempdir().unwrap();
        let marker = data.path().join("pid");
        let path = marker.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || {
            let mut command = Command::new("python3");
            command.args(["-c", "import os,pathlib,sys,time; path=pathlib.Path(sys.argv[1]); staging=path.with_suffix('.part'); staging.write_text(str(os.getpid())); staging.replace(path); time.sleep(30)"]);
            command.arg(path);
            ParecMic::open_command(&mut command, worker_stop)
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !marker.exists() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let pid: i32 = std::fs::read_to_string(marker).unwrap().parse().unwrap();
        let started = Instant::now();
        stop.store(true, Ordering::Release);
        assert!(worker.join().unwrap().is_err());
        assert!(started.elapsed() < Duration::from_millis(500));
        // SAFETY: signal zero observes liveness; it does not send a signal.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[test]
    fn partial_sample_reads_and_device_exit_have_distinct_health_from_silence() {
        let mut command = Command::new("python3");
        command.args(["-c", "import os,time; os.write(1,b'\\0'); time.sleep(.02); os.write(1,b'\\0'*1023); time.sleep(.2)"]);
        let mut mic =
            ParecMic::open_command(&mut command, Arc::new(AtomicBool::new(false))).unwrap();
        assert!(mic.healthy());
        assert_eq!(mic.drain(), 0.0, "real silence is still a healthy capture");
        let deadline = Instant::now() + Duration::from_secs(2);
        while mic.healthy() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(mic.drain(), 0.0);
        let mut input = MicInput::new();
        input.wanted = true;
        input.real_parec = Some(mic);
        assert!(
            input.capture_error().is_some(),
            "exited capture cannot silently become a simulated microphone"
        );
    }

    #[test]
    fn unavailable_real_capture_has_no_simulated_feedback() {
        let mut input = MicInput::new();
        input.wanted = true;
        input.error = Some("unavailable".into());
        input.burst();
        for _ in 0..100 {
            assert_eq!(input.sample(0.033), 0.0);
        }
        assert_eq!(input.capture_error(), Some("unavailable"));
        input.use_sim();
        input.burst();
        assert!(input.sample(0.033) > 0.0);
        assert!(input.capture_error().is_none());
    }
}
