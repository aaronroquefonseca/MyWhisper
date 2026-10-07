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

/// One utterance being captured.
#[derive(Debug, Default)]
pub struct Recorder {
    samples: Vec<i16>,
    rate: u32,
    consumed: usize,
    active: bool,
}

impl Recorder {
    pub fn start(&mut self, rate: u32) {
        self.samples.clear();
        self.rate = rate;
        self.consumed = 0;
        self.active = true;
    }

    pub fn push(&mut self, samples: &[i16]) {
        if self.active {
            self.samples.extend_from_slice(samples);
        }
    }

    /// Samples appended since the last drain (streaming pumps).
    pub fn drain_new(&mut self) -> Vec<i16> {
        let out = self.samples[self.consumed..].to_vec();
        self.consumed = self.samples.len();
        out
    }

    /// Whole utterance at its capture rate; deactivates.
    pub fn finish(&mut self) -> (Vec<i16>, u32) {
        self.active = false;
        (std::mem::take(&mut self.samples), self.rate)
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

/// Feed one slice of s16 mono at `rate` Hz into the armed recorder
/// (no-op when disarmed). Mic threads call this, never block: a
/// poisoned mutex just drops the slice.
pub fn tap_samples(samples: &[i16], rate: u32) {
    let guard = tap_slot().lock();
    let Ok(slot) = guard else { return };
    let Some(rec) = slot.as_ref() else { return };
    if let Ok(mut r) = rec.lock() {
        if r.rate == 0 {
            r.rate = rate;
        }
        r.push(samples);
    }
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
