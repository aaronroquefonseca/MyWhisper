//! UI sounds: synthesized dictation cues, no audio assets.
//!
//! Recipe ported from OpenWhispr's `src/utils/dictationCues.js`
//! (MIT licensed, (c) 2024 OpenWhispr Team): two sine notes with a
//! fast attack and exponential decay, scheduled back to back.
//!
//! * start: C5 (523.25 Hz) -> E5 (659.25 Hz), rising
//! * stop:  D5 (587.33 Hz) -> A4 (440.00 Hz), falling
//! * 90 ms per note, 25 ms gap, 15 ms attack, peak gain 0.2
//!
//! Samples render into one buffer per cue (sample-accurate timing)
//! and play fire-and-forget on a short thread. Failures only log.

use std::time::Duration;

const SAMPLE_RATE: u32 = 44100;
const NOTE_SECS: f32 = 0.09;
const NOTE_GAP_SECS: f32 = 0.025;
const ATTACK_SECS: f32 = 0.015;
const MAX_GAIN: f32 = 0.2;
const MIN_GAIN: f32 = 0.0001;

const START_NOTES: [f32; 2] = [523.25, 659.25];
const STOP_NOTES: [f32; 2] = [587.33, 440.0];

#[derive(Debug, Clone, Copy)]
pub enum UiSound {
    Start,
    Stop,
}

/// Master switch for cue playback, refreshed from the
/// `dictation_sounds` preference by the service watcher. Plays are
/// skipped silently while off (no log spam on every keypress).
static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, std::sync::atomic::Ordering::SeqCst);
}

/// Play asynchronously. Never blocks the caller; failures only log
/// (a missing audio device must not break dictation). Pooled: a
/// saturated system skips the cue instead of panicking on spawn.
pub fn play(sound: UiSound) {
    if !ENABLED.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    if let Err(e) = crate::runtime::submit("cue", move || {
        if let Err(e) = play_once(sound) {
            eprintln!("[sound] {e:?}");
        }
    }) {
        eprintln!("[sound] {e}");
    }
}

/// Render one note with linear attack + exponential decay, appended
/// into `out` at the current cursor.
fn render_note(out: &mut Vec<f32>, freq: f32) {
    let attack_n = (ATTACK_SECS * SAMPLE_RATE as f32) as usize;
    let total_n = (NOTE_SECS * SAMPLE_RATE as f32) as usize;
    let decay_n = total_n.saturating_sub(attack_n);
    for i in 0..attack_n {
        let t = i as f32 / SAMPLE_RATE as f32;
        let env = MIN_GAIN + (MAX_GAIN - MIN_GAIN) * (i as f32 / attack_n as f32);
        out.push((2.0 * std::f32::consts::PI * freq * t).sin() * env);
    }
    for i in 0..decay_n {
        let t = (attack_n + i) as f32 / SAMPLE_RATE as f32;
        // Exponential decay from peak to floor over the remainder.
        let k = i as f32 / decay_n.max(1) as f32;
        let env = MAX_GAIN * (MIN_GAIN / MAX_GAIN).powf(k);
        out.push((2.0 * std::f32::consts::PI * freq * t).sin() * env);
    }
}

fn render_cue(notes: &[f32]) -> Vec<f32> {
    let gap_n = (NOTE_GAP_SECS * SAMPLE_RATE as f32) as usize;
    // Exact capacity: notes + gaps between them.
    let note_n = (NOTE_SECS * SAMPLE_RATE as f32) as usize;
    let mut out = Vec::with_capacity(notes.len() * note_n + gap_n * notes.len());
    for (i, freq) in notes.iter().enumerate() {
        if i > 0 {
            out.extend(std::iter::repeat(0.0).take(gap_n));
        }
        render_note(&mut out, *freq);
    }
    out
}

fn play_once(sound: UiSound) -> Result<(), String> {
    let samples = render_cue(match sound {
        UiSound::Start => &START_NOTES,
        UiSound::Stop => &STOP_NOTES,
    });
    let sink = rodio::DeviceSinkBuilder::open_default_sink()
        .map_err(|e| format!("no output device: {e}"))?;
    let duration = Duration::from_secs_f32(samples.len() as f32 / SAMPLE_RATE as f32);
    let source = rodio::buffer::SamplesBuffer::new(
        std::num::NonZero::new(1).unwrap(),
        std::num::NonZero::new(SAMPLE_RATE).unwrap(),
        samples,
    );
    let player = rodio::Player::connect_new(sink.mixer());
    player.append(source);
    // The player stops the sound on drop; hold it for the duration.
    std::thread::sleep(duration + Duration::from_millis(50));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cues_render_expected_length_and_level() {
        for notes in [&START_NOTES, &STOP_NOTES] {
            let cue = render_cue(notes);
            let note_n = (NOTE_SECS * SAMPLE_RATE as f32) as usize;
            let gap_n = (NOTE_GAP_SECS * SAMPLE_RATE as f32) as usize;
            assert_eq!(cue.len(), 2 * note_n + gap_n);
            let peak: f32 = cue.iter().fold(0.0, |m, s| m.max(s.abs()));
            assert!(peak <= MAX_GAIN + 1e-6, "peak {peak} exceeds designed gain");
            assert!(peak > MAX_GAIN * 0.9, "peak {peak} far below design");
            // Gap between notes is true silence.
            assert!(cue[note_n..note_n + gap_n].iter().all(|s| *s == 0.0));
        }
    }
}
