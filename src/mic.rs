//! Microphone input level for the pill visualizer.
//!
//! [`MicInput`] is the [`LevelSource`] the widget actually drives:
//! a real capture stream (cpal) when one can be opened, otherwise the
//! deterministic [`SimMic`]. Switchable at runtime (`viz mic real|sim`)
//! so experiments can compare live input against the repeatable fake.

use crate::viz::{LevelSource, SimCadence, SimMic};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

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
    pub fn build() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "no default input device".to_string())?;
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "?".to_string());
        let cfg = device
            .default_input_config()
            .map_err(|e| format!("default input config: {e}"))?;

        let level_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => {
                let bits = level_bits.clone();
                device.build_input_stream(
                    cfg.clone().into(),
                    move |data: &[f32], _| push_rms(data, &bits),
                    |e| eprintln!("[poc] mic stream error: {e}"),
                    None,
                )
            }
            cpal::SampleFormat::I16 => {
                let bits = level_bits.clone();
                device.build_input_stream(
                    cfg.clone().into(),
                    move |data: &[i16], _| push_rms(data, &bits),
                    |e| eprintln!("[poc] mic stream error: {e}"),
                    None,
                )
            }
            cpal::SampleFormat::U16 => {
                let bits = level_bits.clone();
                device.build_input_stream(
                    cfg.clone().into(),
                    move |data: &[u16], _| push_rms(data, &bits),
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
/// is active (`set_active(true)` on pill show, `false` on hide), so
/// the OS mic indicator is on exactly while we listen -- never
/// idling open in the background.
pub struct MicInput {
    real: Option<CpalMic>,
    sim: SimMic,
    use_real: bool,
    wanted: bool,
}

impl MicInput {
    pub fn new() -> Self {
        Self {
            real: None,
            sim: SimMic::new(),
            use_real: true,
            wanted: false,
        }
    }

    /// Open (or drop) the capture stream. Called on pill show/hide.
    pub fn set_active(&mut self, active: bool) {
        self.wanted = active;
        if active {
            if self.use_real && self.real.is_none() {
                match CpalMic::build() {
                    Ok(real) => self.real = Some(real),
                    Err(e) => eprintln!("[mic] real input unavailable ({e}); sim it is"),
                }
            }
        } else {
            if self.real.is_some() {
                eprintln!("[mic] capture closed");
            }
            self.real = None;
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
        self.real = None;
        eprintln!("[poc] mic source -> sim");
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
            return 0.0;
        }
        if self.use_real {
            match self.real.as_mut() {
                Some(real) => real.drain(),
                // Build failed (or stream died with the drop): sim
                // keeps the pill alive without touching hardware.
                None => self.sim.sample(_dt),
            }
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

    /// Capture must exist only while dictation is active: silent when
    /// idle, live when wanted -- without touching hardware in sim mode.
    #[test]
    fn mic_gated_on_dictation_active() {
        let mut mic = MicInput::new();
        mic.use_sim();
        mic.set_active(false);
        assert_eq!(mic.sample(0.033), 0.0);
        assert!(mic.real.is_none());
        mic.set_active(true);
        assert!(mic.real.is_none(), "sim mode must not open hardware");
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
