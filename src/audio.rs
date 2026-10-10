//! RAM-only audio capture for dictation.
//!
//! The mic reader threads (parec/cpal in [`crate::mic`]) call
//! [`tap_samples`] with every slice they already pulled for level
//! metering. While a [`Recorder`] is armed, slices accumulate in
//! RAM; nothing is ever written to disk. Transcription input is
//! always resampled to 16 kHz mono here, so engines get a uniform
//! format regardless of the capture device.

use std::sync::{Arc, Mutex, OnceLock};

/// Engine sample rate. whisper.cpp requires 16 kHz; sherpa servers
/// resample internally but get 16 kHz anyway for uniformity.
pub const ENGINE_RATE: u32 = 16_000;
/// Utterances shorter than this are dropped (key taps, mic blips).
pub const MIN_SAMPLES_16K: usize = 4_800; // 0.3 s
const MAX_CAPTURE_CHUNKS: usize = 65_536;
const MAX_CAPTURE_RATE: u32 = 384_000;

/// One utterance being captured. Every pushed slice is stamped
/// with its arrival time so release can truncate the buffer at the
/// exact release instant: everything spoken before release is kept
/// (plus the one straddling slice), nothing after is.
#[derive(Debug, Default)]
pub struct Recorder {
    samples: Vec<i16>,
    /// (end offset, arrival stamp) per pushed slice.
    ends: Vec<(usize, std::time::Instant)>,
    rate: u32,
    consumed: usize,
    active: bool,
    overflow: bool,
}

impl Recorder {
    pub fn start(&mut self, rate: u32) {
        self.samples.clear();
        self.ends.clear();
        self.rate = rate;
        self.consumed = 0;
        self.active = true;
        self.overflow = false;
    }

    pub fn push(&mut self, samples: &[i16]) {
        self.push_at(samples, std::time::Instant::now());
    }

    pub(crate) fn push_at(&mut self, samples: &[i16], stamp: std::time::Instant) {
        if self.active && !samples.is_empty() {
            // Ten minutes of bounded mono capture. Refuse overflow instead
            // of silently decoding a truncated utterance.
            let limit = (self.rate.max(ENGINE_RATE) as usize * 600).min(32 * 1024 * 1024);
            if self.rate > MAX_CAPTURE_RATE
                || self.ends.len() >= MAX_CAPTURE_CHUNKS
                || samples.len() > limit.saturating_sub(self.samples.len())
            {
                self.active = false;
                self.overflow = true;
                return;
            }
            self.samples.extend_from_slice(samples);
            self.ends.push((self.samples.len(), stamp));
        }
    }

    /// Samples appended since the last drain (streaming pumps).
    pub fn drain_new(&mut self) -> Vec<i16> {
        let out = self.samples[self.consumed..].to_vec();
        self.consumed = self.samples.len();
        out
    }

    /// Non-destructive peek (speculative thread tracks its own offset).
    pub fn snapshot(&self) -> Vec<i16> {
        self.samples.clone()
    }

    /// Whole utterance at its capture rate; deactivates.
    pub fn finish(&mut self) -> (Vec<i16>, u32) {
        self.active = false;
        self.ends.clear();
        (std::mem::take(&mut self.samples), self.rate)
    }

    /// Arrival stamp of the newest slice, if any.
    pub fn last_stamp(&self) -> Option<std::time::Instant> {
        self.ends.last().map(|(_, t)| *t)
    }

    /// Settle at release: keep everything up to the release instant
    /// plus the one straddling slice (its head is pre-release
    /// speech; its tail is milliseconds of room tone the VAD/file
    /// path trims and streaming ignores). Drops everything after.
    pub fn settle_release(&mut self, release: std::time::Instant) {
        self.active = false;
        if let Some(pos) = self.ends.iter().position(|(_, t)| *t > release) {
            let end = self.ends[pos].0;
            self.samples.truncate(end);
            self.ends.truncate(pos + 1);
        }
    }

    pub fn overflowed(&self) -> bool {
        self.overflow
    }

    pub fn samples_since(&self, consumed: usize) -> Vec<i16> {
        self.samples.get(consumed..).unwrap_or_default().to_vec()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }
}

static TAP: OnceLock<Mutex<Option<Arc<Mutex<Recorder>>>>> = OnceLock::new();

fn tap_slot() -> &'static Mutex<Option<Arc<Mutex<Recorder>>>> {
    TAP.get_or_init(|| Mutex::new(None))
}

/// Arm/disarm the process-global capture tap. Called on hotkey
/// press/release; cheap enough to call per slice.
pub fn set_recorder(rec: Option<Arc<Mutex<Recorder>>>) {
    *tap_slot().lock().unwrap() = rec;
}

/// Disarm only the tap belonging to this session.
pub fn clear_recorder(expected: &Arc<Mutex<Recorder>>) {
    let mut slot = tap_slot().lock().unwrap();
    if slot
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, expected))
    {
        *slot = None;
    }
}

/// Feed one slice of s16 mono at `rate` Hz into the armed recorder
/// (no-op when disarmed). Mic threads call this, never block: a
/// poisoned mutex just drops the slice.
pub fn tap_samples(samples: &[i16], rate: u32) {
    let guard = tap_slot().lock();
    let Ok(slot) = guard else { return };
    let Some(rec) = slot.as_ref() else { return };
    if let Ok(mut r) = rec.lock() {
        push_capture(&mut r, samples, rate, std::time::Instant::now());
    }
}

/// The callback retains the recorder owning this slice. A queued slice can
/// therefore never be redirected into a later session's recorder.
pub(crate) fn callback_recorder() -> Result<Option<Arc<Mutex<Recorder>>>, ()> {
    tap_slot()
        .try_lock()
        .map(|slot| slot.clone())
        .map_err(|_| ())
}

pub(crate) fn capture_failed() -> bool {
    callback_recorder()
        .ok()
        .flatten()
        .and_then(|recorder| {
            recorder
                .try_lock()
                .ok()
                .map(|recorder| recorder.overflowed())
        })
        .unwrap_or(false)
}

pub(crate) fn push_capture(
    recorder: &mut Recorder,
    samples: &[i16],
    rate: u32,
    stamp: std::time::Instant,
) {
    if recorder.rate == 0 {
        recorder.rate = rate;
    }
    if rate == 0 || recorder.rate != rate {
        recorder.active = false;
        recorder.overflow = true;
        return;
    }
    recorder.push_at(samples, stamp);
}

/// Incremental 16-bit mono resampler to [`ENGINE_RATE`].
/// Linear interpolation with previous-sample carry: no prefilter,
/// so ultrasonics alias down, but speech energy sits well below
/// the 8 kHz Nyquist and dictation quality is unaffected.
pub struct Resampler {
    from_rate: u32,
    /// Fractional position in source samples.
    pos: f64,
    step: f64,
    prev: f32,
    has_prev: bool,
}

impl Resampler {
    pub fn new(from_rate: u32) -> Self {
        Self {
            from_rate,
            pos: 0.0,
            step: from_rate as f64 / ENGINE_RATE as f64,
            prev: 0.0,
            has_prev: false,
        }
    }

    /// Resample a chunk, keeping fractional state for the next call.
    /// Output is f32 in [-1, 1] (sherpa wire format); use
    /// [`f32_to_i16`] for integer consumers.
    pub fn process(&mut self, input: &[i16]) -> Vec<f32> {
        if input.is_empty() {
            return Vec::new();
        }
        if self.from_rate == ENGINE_RATE {
            return input.iter().map(|s| *s as f32 / 32768.0).collect();
        }
        let cur: Vec<f32> = input.iter().map(|s| *s as f32 / 32768.0).collect();
        let n = cur.len() as i64;
        // Virtual array: index -1 is the previous chunk's last
        // sample (when known), 0..n the current chunk.
        let get = |idx: i64| -> Option<f32> {
            if idx < -1 || idx > n {
                None
            } else if idx == -1 {
                self.has_prev.then_some(self.prev)
            } else {
                cur.get(idx as usize).copied()
            }
        };
        let mut out = Vec::new();
        loop {
            let i = self.pos.floor() as i64;
            let frac = (self.pos - i as f64) as f32;
            let (Some(a), Some(b)) = (get(i), get(i + 1)) else {
                break;
            };
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        self.prev = cur[cur.len() - 1];
        self.has_prev = true;
        // Clamp so a degenerate (single-sample) chunk can't park
        // the position out of range and stall the stream.
        self.pos = (self.pos - n as f64).max(-1.0);
        out
    }

    /// Convenience: whole buffer at once.
    pub fn resample_all(from_rate: u32, input: &[i16]) -> Vec<f32> {
        Self::new(from_rate).process(input)
    }
}

pub fn f32_to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
        .collect()
}

/// Voice activity: trim leading/trailing silence and detect
/// all-silence utterances. Frame RMS over 30 ms windows at 16 kHz;
/// `gate` is the 0..1 amplitude fraction from settings (useful range
/// ~0.05..0.2 for normal mic levels; 0 disables the gate). Kept
/// speech is padded 150 ms each side so plosives survive.
pub const VAD_FRAME: usize = 480;
pub const VAD_PAD: usize = 2400;

#[derive(Debug, Clone, PartialEq)]
pub enum VadResult {
    Speech(Vec<f32>),
    Silence,
}

pub fn apply_vad(samples: &[f32], gate: f32) -> VadResult {
    if samples.is_empty() {
        return VadResult::Silence;
    }
    let gate = gate.clamp(0.0, 1.0);
    if gate <= 0.0 {
        return VadResult::Speech(samples.to_vec());
    }
    let mut first: Option<usize> = None;
    let mut last = 0usize;
    for (i, frame) in samples.chunks(VAD_FRAME).enumerate() {
        let sum: f32 = frame.iter().map(|s| s * s).sum();
        let rms = (sum / frame.len() as f32).sqrt();
        if rms >= gate {
            if first.is_none() {
                first = Some(i);
            }
            last = i;
        }
    }
    let Some(f0) = first else {
        return VadResult::Silence;
    };
    let from = (f0 * VAD_FRAME).saturating_sub(VAD_PAD);
    let to = ((last + 1) * VAD_FRAME + VAD_PAD).min(samples.len());
    VadResult::Speech(samples[from..to].to_vec())
}

/// Private audio vault (`<data>/audio`), populated only by explicit retention.
pub fn audio_vault_dir(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("audio")
}

/// Delete every vault file. Returns files removed.
pub fn clear_audio(data_dir: &std::path::Path) -> std::io::Result<usize> {
    let dir = audio_vault_dir(data_dir);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries.collect::<std::io::Result<Vec<_>>>()?.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e),
    };
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    crate::private_fs::directory(&dir)?;
    Ok(entries)
}

/// Delete vault files older than `days`. Tombstone-free zone (audio
/// rows never sync), so plain deletion is safe. Returns files removed.
pub fn purge_old_audio(data_dir: &std::path::Path, days: u32) -> usize {
    let dir = audio_vault_dir(data_dir);
    let cutoff_secs = days as u64 * 86400;
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .map(|d| d.as_secs() > cutoff_secs)
                .unwrap_or(false);
            if old && std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// Minimal WAV (16-bit PCM mono) builder, in RAM. Never touches disk:
/// the bytes go straight into HTTP multipart / debug paths.
pub fn wav_bytes_16k(samples: &[i16]) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let mut wav = Vec::with_capacity(44 + data_len);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&ENGINE_RATE.to_le_bytes());
    wav.extend_from_slice(&(ENGINE_RATE * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in samples {
        wav.extend_from_slice(&s.to_le_bytes());
    }
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_minute_synthetic_capture_has_fixed_memory_and_linear_stream_copying() {
        let mut recorder = Recorder::default();
        recorder.start(44_100);
        let total = 44_100 * 600;
        let began = std::time::Instant::now();
        let chunk = [7; 1024];
        let mut copied = 0;
        while recorder.len() < total {
            let count = (total - recorder.len()).min(chunk.len());
            let stamp = began
                + std::time::Duration::from_secs_f64((recorder.len() + count) as f64 / 44_100.0);
            recorder.push_at(&chunk[..count], stamp);
            copied += recorder.drain_new().len();
        }
        assert_eq!(copied, total, "streaming copies each sample only once");
        assert_eq!(recorder.len(), total);
        let memory = recorder.samples.capacity() * std::mem::size_of::<i16>()
            + recorder.ends.capacity() * std::mem::size_of::<(usize, std::time::Instant)>();
        assert!(
            memory <= 68 * 1024 * 1024,
            "capture allocation exceeded its bound: {memory}"
        );
        recorder.push(&[7]);
        assert!(recorder.overflowed());
        assert_eq!(recorder.len(), total);
    }

    #[test]
    fn capture_bounds_empty_slices_metadata_rate_changes_and_late_queued_audio() {
        let mut recorder = Recorder::default();
        recorder.start(16_000);
        for _ in 0..100_000 {
            recorder.push(&[]);
        }
        assert!(recorder.ends.is_empty());
        let stamp = std::time::Instant::now();
        for _ in 0..MAX_CAPTURE_CHUNKS {
            recorder.push_at(&[7], stamp);
        }
        assert_eq!(recorder.ends.len(), MAX_CAPTURE_CHUNKS);
        recorder.push_at(&[8], stamp);
        assert!(recorder.overflowed());
        assert_eq!(recorder.len(), MAX_CAPTURE_CHUNKS);
        recorder.start(16_000);
        push_capture(&mut recorder, &[1, 2, 3], 16_000, stamp);
        push_capture(&mut recorder, &[4, 5], 48_000, stamp);
        assert!(recorder.overflowed());
        assert_eq!(recorder.snapshot(), [1, 2, 3]);
        recorder.start(16_000);
        recorder.push_at(&[1, 2], stamp);
        recorder.settle_release(stamp);
        recorder.push_at(&[3, 4], stamp);
        assert_eq!(
            recorder.snapshot(),
            [1, 2],
            "queued callback audio cannot change a settled utterance"
        );
    }

    #[test]
    fn settle_release_cuts_at_release_plus_straddler() {
        let mut r = Recorder::default();
        r.start(44_100);
        r.push(&[1i16; 100]);
        std::thread::sleep(std::time::Duration::from_millis(15));
        let t_release = std::time::Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(15));
        r.push(&[2i16; 100]); // straddler: kept
        r.push(&[3i16; 100]); // post-release: dropped
        r.settle_release(t_release);
        // Pre-release slice + straddler survive, later slice gone.
        assert_eq!(r.samples.len(), 200);
        assert!(r.samples.iter().take(100).all(|s| *s == 1));
        assert!(r.samples.iter().skip(100).all(|s| *s == 2));
    }

    #[test]
    fn wav_header_shape() {
        let wav = wav_bytes_16k(&[0, 1000, -1000]);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(wav.len(), 44 + 6);
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 36 + 6);
        let rate = u32::from_le_bytes(wav[24..28].try_into().unwrap());
        assert_eq!(rate, 16_000);
    }

    #[test]
    fn resample_44k_to_16k_ratio() {
        // 1 s of 44.1 kHz must yield ~16000 samples.
        let input: Vec<i16> = (0..44_100).map(|i| (i % 100) as i16).collect();
        let out = Resampler::resample_all(44_100, &input);
        assert!((15_900..=16_100).contains(&out.len()), "len={}", out.len());
    }

    #[test]
    fn resample_chunked_matches_whole() {
        let input: Vec<i16> = (0..44_100).map(|i| ((i * 7) % 1000) as i16).collect();
        let whole = Resampler::resample_all(44_100, &input);
        let mut r = Resampler::new(44_100);
        let mut chunked = Vec::new();
        for c in input.chunks(1024) {
            chunked.extend(r.process(c));
        }
        assert!((whole.len() as i32 - chunked.len() as i32).abs() <= 2);
        for (a, b) in whole.iter().zip(chunked.iter()).take(1000) {
            assert!((a - b).abs() < 1e-4, "{a} vs {b}");
        }
    }

    #[test]
    fn resample_passthrough_16k() {
        let input = vec![0i16, 16384, -16384];
        let out = Resampler::resample_all(16_000, &input);
        assert_eq!(out.len(), 3);
        assert!((out[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn vad_trims_silence_and_detects_none() {
        // 0.5 s silence, 0.5 s tone (RMS ~0.35), 0.5 s silence.
        let mut sig = vec![0.0f32; 8000];
        for i in 0..8000 {
            sig.push((2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16_000.0).sin() * 0.5);
        }
        sig.extend(vec![0.0f32; 8000]);
        match apply_vad(&sig, 0.1) {
            VadResult::Speech(trimmed) => {
                // Keeps the burst + 150 ms pads, drops the rest.
                assert!(trimmed.len() < sig.len());
                assert!(trimmed.len() >= 8000);
                // Burst + pads + one frame slack each side.
                assert!(trimmed.len() <= 8000 + 2 * VAD_PAD + 2 * VAD_FRAME);
            }
            VadResult::Silence => panic!("speech missed"),
        }
        assert_eq!(apply_vad(&vec![0.0f32; 16_000], 0.1), VadResult::Silence);
        assert_eq!(apply_vad(&[], 0.1), VadResult::Silence);
        // Gate 0 passes everything through untouched.
        match apply_vad(&sig, 0.0) {
            VadResult::Speech(whole) => assert_eq!(whole.len(), sig.len()),
            VadResult::Silence => panic!("gate 0 must pass"),
        }
    }

    #[test]
    fn audio_vault_clear_and_purge() {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path();
        let dir = super::audio_vault_dir(base);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.wav"), [1, 2, 3]).unwrap();
        std::fs::write(dir.join("b.wav"), [4, 5]).unwrap();
        // Fresh files survive a 30-day purge, clear removes all.
        assert_eq!(super::purge_old_audio(base, 30), 0);
        assert_eq!(super::clear_audio(base).unwrap(), 2);
        assert!(dir.is_dir());
    }

    #[test]
    fn recorder_drain_semantics() {
        let mut r = Recorder::default();
        r.start(44_100);
        r.push(&[1, 2, 3]);
        assert_eq!(r.drain_new(), vec![1, 2, 3]);
        assert!(r.drain_new().is_empty());
        r.push(&[4]);
        let (all, rate) = r.finish();
        assert_eq!((all, rate), (vec![1, 2, 3, 4], 44_100));
    }
}
