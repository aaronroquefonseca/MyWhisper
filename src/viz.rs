//! Pill visualizer: rounded vertical bars driven by mic level.
//!
//! Design notes borrow from the OpenWhispr recording pill
//! (`PillWaveform.tsx` / `waveformMath.ts`): thin monochrome bars, no
//! frantic idle motion, a scrolling level tape, and a compressive
//! level curve (`(rms*8)^0.75`) that keeps quiet speech visible.
//!
//! Everything visual is runtime-switchable through the
//! `Visualizer` setters (see the host CLI flags), so variants can be
//! A/B-compared without recompiling:
//!
//! * mode: `bloom` (default, the phone app's center bloom: silence
//!   shows dots, voice opens the bars from the middle out), `tape`
//!   (OpenWhispr-style: newest sample at the right edge) or `history`
//!   (center bar live, older samples propagate outward ~100 ms/step).
//! * idle (level below threshold): `still` (bars rest at minimum),
//!   `silhouette` (static resting profile), `calm` (faint slow sine).
//! * bars: odd counts 7/11/15/21 (default 11, per reference).
//! * theme: `white` / `grey` / `red` (all flat monochrome).
//! * ornament: `none` / `mark` (local 5-bar symbolic motif) /
//!   `mic` (cairo mic glyph) / `dot`.
//!
//! Level input goes through [`LevelSource`] so a real capture backend
//! (e.g. cpal) can replace [`SimMic`] without touching the widget.

use crate::mic::MicInput;
use gtk4::DrawingArea;
use gtk4::cairo;
use gtk4::glib;
use gtk4::prelude::*;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

const TICK_MS: u64 = 33; // ~30 fps
const HISTORY_EVERY_N_TICKS: u64 = 3; // ~100 ms per sample (~80 ms ref)
const BAR_WIDTH: f64 = 3.0;
const BAR_GAP: f64 = 3.0;
const MIN_BAR_PX: f64 = 4.0; // resting height, per reference
const DEFAULT_LANE_MAX_PX: f64 = 18.0;
const LANE_MIN_PX: f64 = 8.0;
const LANE_MAX_LIMIT_PX: f64 = 32.0;
const LANE_PADDING_PX: i32 = 8; // vertical chrome around the lane
const ORNAMENT_W: f64 = 22.0;
/// Spinner requisition: small enough that the pill visibly contracts
/// when inference starts.
const SPINNER_PX: i32 = 30;
const MARK_PX: i32 = 18;
const SPEECH_THRESHOLD: f32 = 0.12;
const QUIET_ENV: f32 = 0.03;

// Level curve from the reference: gain puts loud voicing at the top
// of the lane, exponent 0.75 keeps quiet speech visible.
const LEVEL_GAIN: f32 = 8.0;
const LEVEL_EXPONENT: f32 = 0.75;

fn to_bar_level(rms: f32) -> f32 {
    (rms.max(0.0) * LEVEL_GAIN).powf(LEVEL_EXPONENT).min(1.0)
}

/// `mic.rs` publishes `(rms * 8).clamp(0, 1)`; Bloom undoes the gain
/// to recover block RMS (clipping only above -18 dBFS, already loud).
const MIC_GAIN: f32 = 8.0;

/// Port of the phone app's `LevelNormalizer`: block RMS to 0..1 in dB
/// with an adaptive noise floor (gates room noise) and a decaying
/// ceiling (auto-gain for quiet talkers). Constants are per ~32 ms
/// block; the pill ticks every 33 ms.
#[derive(Default)]
struct LevelNormalizer {
    floor_db: Option<f32>,
    ceil_db: f32,
}

impl LevelNormalizer {
    const FLOOR_RISE_DB: f32 = 0.05;
    const CEIL_FALL_DB: f32 = 0.2;
    const MIN_FLOOR_DB: f32 = -70.0;
    const GATE_DB: f32 = 6.0;
    const MIN_SPAN_DB: f32 = 24.0;

    fn push(&mut self, rms: f32) -> f32 {
        // Digital-zero blocks say nothing about room noise.
        if rms <= 0.0 {
            return 0.0;
        }
        let db = 20.0 * rms.max(1e-6).log10();
        // Seeded from the first real block so a noisy room is learned at once.
        let prev = match self.floor_db {
            Some(floor) => floor,
            None => {
                self.ceil_db = db + Self::MIN_SPAN_DB;
                db
            }
        };
        let floor = if db < prev {
            db
        } else {
            prev + Self::FLOOR_RISE_DB
        }
        .clamp(Self::MIN_FLOOR_DB, -20.0);
        self.floor_db = Some(floor);
        self.ceil_db = db
            .max(self.ceil_db - Self::CEIL_FALL_DB)
            .max(floor + Self::MIN_SPAN_DB);
        let gate = floor + Self::GATE_DB;
        ((db - gate) / (self.ceil_db - gate)).clamp(0.0, 1.0)
    }
}

/// Port of the phone app's `BloomBars`: silence shows dots; voice
/// opens the bars from the middle out, every bar showing the current
/// level under a mirrored bell. Rates are "per 60 Hz frame", rescaled
/// by `dt`.
struct Bloom {
    heights: Vec<f32>,
    level: f32,
    spread: f32,
    clock: f32,
    since_voice: f32,
}

impl Default for Bloom {
    fn default() -> Self {
        Self {
            heights: Vec::new(),
            level: 0.0,
            spread: 0.0,
            clock: 0.0,
            since_voice: f32::INFINITY,
        }
    }
}

impl Bloom {
    /// Normalized level above this counts as voice (the normalizer gates noise).
    const VOICE_GATE: f32 = 0.08;
    /// Voice stays "on" this long after the last loud block, bridging word gaps.
    const HANGOVER_S: f32 = 0.22;
    const BELL_SIGMA: f32 = 0.32;
    const WOBBLE: f32 = 0.16;

    fn step(&mut self, input: f32, count: usize, dt: f32) {
        let input = input.clamp(0.0, 1.0);
        self.heights.resize(count, 0.0);
        self.clock += dt;
        self.since_voice = if input > Self::VOICE_GATE {
            0.0
        } else {
            self.since_voice + dt
        };
        let voice = self.since_voice < Self::HANGOVER_S;
        let rise = if input > self.level { 0.55 } else { 0.18 };
        self.level = approach(self.level, input, rise, dt);
        // ~120 ms to open, ~400 ms to close.
        let (target, rate) = if voice { (1.0, 0.22) } else { (0.0, 0.07) };
        self.spread = approach(self.spread, target, rate, dt);
        let c = count.saturating_sub(1) as f32 / 2.0;
        for i in 0..count {
            let d = if c == 0.0 {
                0.0
            } else {
                (i as f32 - c).abs() / c
            };
            let reveal = ((self.spread * 1.25 - d) / 0.25).clamp(0.0, 1.0);
            let sigma2 = 2.0 * Self::BELL_SIGMA * Self::BELL_SIGMA;
            let bell = 0.12 + 0.88 * (-(d * d) / sigma2).exp();
            // One phase set per mirrored pair, so both halves move together.
            let p = bloom_phases(i.min(count - 1 - i));
            let wobble = 1.0 - Self::WOBBLE
                + Self::WOBBLE
                    * (0.5
                        + 0.25 * (self.clock * p[2] + p[0]).sin()
                        + 0.25 * (self.clock * p[3] + p[1]).sin());
            self.heights[i] = approach(
                self.heights[i],
                reveal * self.level * bell * wobble,
                0.45,
                dt,
            );
        }
    }
}

/// Fixed pseudo-random phases/speeds per bar pair (std has no RNG).
fn bloom_phases(pair: usize) -> [f32; 4] {
    let r = |k: u32| {
        let x = (pair as u32 + 1)
            .wrapping_mul(2_654_435_761)
            .wrapping_add(k.wrapping_mul(40_503).wrapping_mul(2_246_822_519));
        (x >> 16) as f32 / 65_535.0
    };
    let tau = std::f32::consts::TAU;
    [r(1) * tau, r(2) * tau, 2.0 + r(3) * 3.0, 3.0 + r(4) * 4.0]
}

fn approach(cur: f32, target: f32, per_frame_60: f32, dt: f32) -> f32 {
    cur + (target - cur) * (1.0 - (1.0 - per_frame_60).powf(dt * 60.0))
}

// Resting silhouette from the reference (px in a 22px lane);
// resampled to the active bar count for the `silhouette` idle style.
const RESTING_SILHOUETTE: [f32; 11] = [6., 12., 5., 9., 7., 22., 18., 5., 20., 12., 17.];

/// Anything that yields a 0.0..=1.0 mic level per tick.
pub trait LevelSource {
    fn sample(&mut self, dt: f32) -> f32;
    /// Force a speech burst (demo/test hook).
    fn burst(&mut self);
    /// Auto-burst cadence; sources without one ignore it.
    /// (Kept on the trait so future sources stay switchable through
    /// one handle; MicInput routes cadence via its own method.)
    #[allow(dead_code)]
    fn set_cadence(&mut self, _cadence: SimCadence) {}
}

/// Deterministic fake mic: mostly silent (like a real idle mic) with
/// rare short speech-like bursts. Speech on demand via `burst()`.
/// Auto-burst cadence is runtime-switchable (`viz sim ...`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimCadence {
    /// Bursts every few seconds (demo default).
    Lively,
    /// A burst every ~12-25 s.
    Quiet,
    /// No auto bursts; manual `burst` only.
    Off,
}

impl SimCadence {
    /// (min gap, gap span, min length, length span) in seconds.
    fn params(self) -> (f32, f32, f32, f32) {
        match self {
            SimCadence::Lively => (2.5, 2.5, 1.2, 1.3),
            SimCadence::Quiet => (12.0, 13.0, 1.0, 0.8),
            SimCadence::Off => (f32::INFINITY, 0.0, 0.0, 0.0),
        }
    }
}

pub struct SimMic {
    t: f32,
    level: f32,
    next_burst: f32,
    burst_end: f32,
    seed: u32,
    cadence: SimCadence,
}

impl SimMic {
    pub fn new() -> Self {
        Self {
            t: 0.0,
            level: 0.0,
            next_burst: 2.0,
            burst_end: 0.0,
            seed: 0x1234_5678,
            cadence: SimCadence::Quiet,
        }
    }

    pub fn apply_cadence(&mut self, cadence: SimCadence) {
        self.cadence = cadence;
        if cadence == SimCadence::Off {
            self.next_burst = f32::INFINITY;
            self.burst_end = 0.0;
        } else {
            self.next_burst = self.t + 1.0;
        }
        eprintln!("[poc] sim cadence -> {cadence:?}");
    }

    fn rand(&mut self) -> f32 {
        self.seed = self.seed.wrapping_mul(1664525).wrapping_add(1013904223);
        ((self.seed >> 8) as f32) / 16_777_216.0
    }
}

impl LevelSource for SimMic {
    fn sample(&mut self, dt: f32) -> f32 {
        self.t += dt;
        if self.t >= self.next_burst {
            let (gap_min, gap_span, len_min, len_span) = self.cadence.params();
            self.burst_end = self.t + len_min + self.rand() * len_span;
            self.next_burst = self.burst_end + gap_min + self.rand() * gap_span;
            if len_min > 0.0 {
                eprintln!("[poc] sim auto-burst");
            }
        }
        let speaking = self.t < self.burst_end;
        let target = if speaking {
            (0.55 + 0.30 * (self.t * 9.0).sin() + 0.15 * (self.t * 23.7).sin()).clamp(0.05, 1.0)
        } else {
            0.02
        };
        let k = (dt * 14.0).min(1.0);
        self.level += (target - self.level) * k;
        (self.level + self.rand() * 0.015).clamp(0.0, 1.0)
    }

    fn burst(&mut self) {
        self.burst_end = self.t + 2.5;
        self.next_burst = self.burst_end + 2.0;
    }

    fn set_cadence(&mut self, cadence: SimCadence) {
        self.apply_cadence(cadence);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VizMode {
    History,
    Tape,
    Bloom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VizTheme {
    White,
    Grey,
    Red,
}

impl VizTheme {
    fn rgba(self, quiet: bool) -> (f64, f64, f64, f64) {
        let a = if quiet { 0.6 } else { 0.92 };
        match self {
            VizTheme::White => (1.0, 1.0, 1.0, a),
            VizTheme::Grey => (0.65, 0.68, 0.75, a),
            VizTheme::Red => (1.0, 0.37, 0.34, a),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Ornament {
    None,
    Mark,
    Mic,
    Dot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IdleStyle {
    Still,
    Silhouette,
    Calm,
}

pub const BAR_COUNT_OPTIONS: [usize; 4] = [7, 11, 15, 21];

struct VizState {
    mode: VizMode,
    theme: VizTheme,
    ornament: Ornament,
    idle: IdleStyle,
    bar_count: usize,
    /// Lane height: max bar excursion in px (live-adjustable).
    lane_max: f64,
    /// Processing spinner: replaces bars + ornament while inference
    /// runs (no mic feedback in this mode).
    spinning: bool,
    /// Live preview text (None = none shown).
    live_text: Option<String>,
    /// Morph 0 (bars) -> 1 (spinner), animated in tick so the swap
    /// reads as one motion instead of a content pop.
    proc_blend: f32,
    /// Bar heights frozen at spinner start: the morph shrinks these,
    /// it doesn't dance live mic behind the spinner.
    frozen: Vec<f32>,
    sim: SimCadence,
    phase: f32,
    env: f32,
    hist: VecDeque<f32>,
    ticks: u64,
    mic: MicInput,
    /// Our monochrome symbolic mark, white-recolored SVG rasterized once.
    mark: Option<gtk4::gdk_pixbuf::Pixbuf>,
    norm: LevelNormalizer,
    bloom: Bloom,
}

/// Rounded-bars visualizer. Cloneable handle; the draw closure and
/// the frame timer share `Rc<RefCell<...>>` state on the GTK thread.
#[derive(Clone)]
pub struct Visualizer {
    area: DrawingArea,
    state: Rc<RefCell<VizState>>,
}

impl Visualizer {
    pub fn new() -> Self {
        let area = DrawingArea::new();
        // Fill the box when it grows (live preview); identical when
        // the window matches content, as usual.
        area.set_hexpand(true);
        area.set_vexpand(true);
        let state = Rc::new(RefCell::new(VizState {
            mode: VizMode::Bloom,
            theme: VizTheme::White,
            ornament: Ornament::None,
            idle: IdleStyle::Still,
            bar_count: 11,
            lane_max: DEFAULT_LANE_MAX_PX,
            spinning: false,
            live_text: None,
            proc_blend: 0.0,
            frozen: Vec::new(),
            sim: SimCadence::Lively,
            phase: 0.0,
            env: 0.0,
            hist: VecDeque::from(vec![0.0; 32]),
            ticks: 0,
            mic: MicInput::new(),
            mark: load_mark_pixbuf(),
            norm: LevelNormalizer::default(),
            bloom: Bloom::default(),
        }));
        size_area(&area, &state.borrow());

        // Paint.
        {
            let s = state.clone();
            area.set_draw_func(move |_, cr, width, height| {
                draw(cr, width as f64, height as f64, &s.borrow());
            });
        }

        // Frame clock.
        {
            let s = state.clone();
            let a = area.clone();
            glib::timeout_add_local(Duration::from_millis(TICK_MS), move || {
                tick(&mut s.borrow_mut());
                a.queue_draw();
                glib::ControlFlow::Continue
            });
        }

        Self { area, state }
    }

    pub fn widget(&self) -> &DrawingArea {
        &self.area
    }

    pub fn set_mode(&self, mode: VizMode) {
        let mut st = self.state.borrow_mut();
        if st.mode != mode {
            st.mode = mode;
            eprintln!("[poc] viz mode -> {mode:?}");
        }
    }

    pub fn set_bar_count(&self, n: usize) {
        if !BAR_COUNT_OPTIONS.contains(&n) {
            eprintln!("[poc] viz bars must be one of {BAR_COUNT_OPTIONS:?}");
            return;
        }
        let mut st = self.state.borrow_mut();
        if st.bar_count != n {
            st.bar_count = n;
            size_area(&self.area, &st);
            eprintln!("[poc] viz bars -> {n}");
        }
    }

    /// Live lane height (max bar excursion, px). Clamped to a sane range.
    pub fn set_lane_max(&self, px: f64) {
        let px = px.clamp(LANE_MIN_PX, LANE_MAX_LIMIT_PX);
        let mut st = self.state.borrow_mut();
        if (st.lane_max - px).abs() > f64::EPSILON {
            st.lane_max = px;
            size_area(&self.area, &st);
            eprintln!("[poc] viz height -> {px}");
        }
    }

    pub fn set_theme(&self, theme: VizTheme) {
        let mut st = self.state.borrow_mut();
        if st.theme != theme {
            st.theme = theme;
            eprintln!("[poc] viz theme -> {theme:?}");
        }
    }

    pub fn set_ornament(&self, ornament: Ornament) {
        let mut st = self.state.borrow_mut();
        if st.ornament != ornament {
            st.ornament = ornament;
            size_area(&self.area, &st);
            eprintln!("[poc] viz ornament -> {ornament:?}");
        }
    }

    pub fn set_idle(&self, idle: IdleStyle) {
        let mut st = self.state.borrow_mut();
        if st.idle != idle {
            st.idle = idle;
            eprintln!("[poc] viz idle -> {idle:?}");
        }
    }

    pub fn set_sim_cadence(&self, cadence: SimCadence) {
        let mut st = self.state.borrow_mut();
        if st.sim != cadence {
            st.sim = cadence;
            st.mic.set_sim_cadence(cadence);
        }
    }

    /// Switch level input. `real=true` opens the capture stream on
    /// first use; returns false (stays on sim) if none is available.
    pub fn set_mic_source(&self, real: bool) -> bool {
        let mut st = self.state.borrow_mut();

        if real {
            st.mic.use_real()
        } else {
            st.mic.use_sim();
            true
        }
    }

    pub fn burst(&self) {
        self.state.borrow_mut().mic.burst();
        eprintln!("[poc] viz burst triggered");
    }

    /// Live preview text (None clears). Sizing follows it live;
    /// height changes materialize through the overlay's re-present.
    pub fn set_live_text(&self, text: Option<String>) {
        let mut st = self.state.borrow_mut();
        if st.live_text != text {
            st.live_text = text;
            size_area(&self.area, &st);
        }
    }

    /// Processing spinner on/off. Shrinks the requisition so the
    /// pill contracts toward its centre when inference starts.
    /// Freezes current bar heights: the morph below shrinks them.
    /// Clearing snaps back instantly: a fresh utterance must open on
    /// bars, never replay the morph backwards.
    pub fn set_spinning(&self, spinning: bool) {
        let mut st = self.state.borrow_mut();
        if st.spinning != spinning {
            st.spinning = spinning;
            if spinning {
                st.frozen = heights(&st);
            } else {
                st.proc_blend = 0.0;
            }
            size_area(&self.area, &st);
            eprintln!("[poc] viz spinning -> {spinning}");
        }
    }

    /// Dictation gate: open capture while listening, drop it on
    /// release (the mic's settle grace lands the tail; Processing
    /// never holds the device).
    pub fn set_mic_active(&self, active: bool) {
        let mut st = self.state.borrow_mut();
        if active {
            // Each utterance learns its room noise afresh, as on the phone.
            st.norm = LevelNormalizer::default();
            st.bloom = Bloom::default();
        }
        st.mic.set_active(active);
    }

    /// Immediate close for cancel: no tail to land, so skip the hold.
    pub fn close_mic_now(&self) {
        self.state.borrow_mut().mic.close_now();
    }
}

fn bars_width(count: usize) -> f64 {
    (BAR_WIDTH + BAR_GAP) * count as f64 - BAR_GAP
}

fn ornament_width(o: Ornament) -> f64 {
    match o {
        Ornament::None => 0.0,
        _ => ORNAMENT_W,
    }
}

fn area_width(st: &VizState) -> i32 {
    if st.spinning {
        return SPINNER_PX;
    }
    let gap = if st.ornament == Ornament::None {
        0.0
    } else {
        8.0
    };
    (ornament_width(st.ornament) + gap + bars_width(st.bar_count)).ceil() as i32
}

/// Preview row height: text line + breathing room.
const LIVE_TEXT_HEIGHT: i32 = 22;

/// Rough text width (7px/char at 13pt Sans): lets the area request
/// — and the content-sized surface — follow longer partials.
fn live_text_width(st: &VizState) -> i32 {
    let Some(text) = st.live_text.as_ref() else {
        return 0;
    };
    let words: Vec<&str> = text.split_whitespace().collect();
    let tail = if words.len() > 12 {
        words[words.len() - 12..].join(" ")
    } else {
        words.join(" ")
    };
    tail.chars().count() as i32 * 7 + 16
}

fn size_area(area: &DrawingArea, st: &VizState) {
    let extra = if st.live_text.is_some() {
        LIVE_TEXT_HEIGHT
    } else {
        0
    };
    area.set_size_request(
        area_width(st).max(live_text_width(st)),
        st.lane_max.ceil() as i32 + LANE_PADDING_PX + extra,
    );
    area.queue_resize();
}

/// Rasterize `assets/icons/symbolic.svg` once, recolored white
/// (`currentColor` has no context at load time and renders black).
/// Returns `None` if the SVG loader is unavailable; the ornament
/// falls back to drawn bars.
fn load_mark_pixbuf() -> Option<gtk4::gdk_pixbuf::Pixbuf> {
    let svg = include_str!("../assets/icons/symbolic.svg").replace("currentColor", "#FFFFFF");
    let loader = gtk4::gdk_pixbuf::PixbufLoader::new();
    loader.set_size(MARK_PX, MARK_PX);
    loader.write(svg.as_bytes()).ok()?;
    loader.close().ok()?;
    match loader.pixbuf() {
        Some(p) => {
            eprintln!("[poc] viz mark icon loaded: {}x{}", p.width(), p.height());
            Some(p)
        }
        None => {
            eprintln!("[poc] WARNING: viz mark icon failed to load, using drawn fallback");
            None
        }
    }
}

/// Advance simulation + envelopes one frame (GTK thread only).
fn tick(st: &mut VizState) {
    let dt = TICK_MS as f32 / 1000.0;
    let level = st.mic.sample(dt);
    st.phase += dt * 4.5;
    st.ticks += 1;

    // Morph toward the spinner target in ~200 ms (matches the
    // overlay contract animation).
    let target = if st.spinning { 1.0 } else { 0.0 };
    let step = dt * 5.0;
    st.proc_blend = if st.proc_blend < target {
        (st.proc_blend + step).min(target)
    } else if st.proc_blend > target {
        (st.proc_blend - step).max(target)
    } else {
        st.proc_blend
    };

    // Speech envelope: fast attack, slow release.
    let target = if level > SPEECH_THRESHOLD { level } else { 0.0 };
    if target > st.env {
        st.env += (target - st.env) * 0.5;
    } else {
        st.env *= 0.94;
    }

    if st.mode == VizMode::Bloom {
        let voice = st.norm.push(level / MIC_GAIN);
        let count = st.bar_count;
        st.bloom.step(voice, count, dt);
    }

    // Sample history/tape buffers at ~100 ms.
    if st.ticks.is_multiple_of(HISTORY_EVERY_N_TICKS) {
        st.hist.push_front(level);
        st.hist.truncate(32);
    }
}

/// Normalized (0..1) bar heights for the current state.
fn heights(st: &VizState) -> Vec<f32> {
    let n = st.bar_count;
    let mut out = vec![0.0f32; n];
    let speaking = st.env >= QUIET_ENV;

    if !speaking && st.mode != VizMode::Bloom {
        match st.idle {
            IdleStyle::Still => {} // rest at minimum
            IdleStyle::Silhouette => {
                for (i, h) in out.iter_mut().enumerate() {
                    // Resample the 11-step reference profile.
                    let pos =
                        i as f32 * (RESTING_SILHOUETTE.len() - 1) as f32 / (n - 1).max(1) as f32;
                    let lo = pos.floor() as usize;
                    let hi = pos.ceil() as usize;
                    let v = RESTING_SILHOUETTE[lo]
                        + (RESTING_SILHOUETTE[hi] - RESTING_SILHOUETTE[lo]) * (pos - lo as f32);
                    *h = v / 22.0;
                }
            }
            IdleStyle::Calm => {
                for (i, h) in out.iter_mut().enumerate() {
                    let wave = (st.phase * 0.35 + i as f32 * 0.55).sin().abs();
                    *h = 0.015 + 0.02 * wave;
                }
            }
        }
        return out;
    }

    match st.mode {
        VizMode::Bloom => return st.bloom.heights.clone(),
        VizMode::History => {
            let center = n / 2;
            for (i, h) in out.iter_mut().enumerate() {
                let age = i.abs_diff(center).min(31);
                *h = to_bar_level(st.hist[age]);
            }
        }
        VizMode::Tape => {
            for (i, h) in out.iter_mut().enumerate() {
                let age = (n - 1 - i).min(31); // newest at right edge
                *h = to_bar_level(st.hist[age]);
            }
        }
    }
    out
}

fn draw(cr: &cairo::Context, width: f64, height: f64, st: &VizState) {
    // Bloom already shows silence as dots, so it never dims.
    let quiet = st.env < QUIET_ENV && st.mode != VizMode::Bloom;
    let (r, g, b, a) = st.theme.rgba(quiet);
    cr.set_source_rgba(r, g, b, a);
    cr.set_line_cap(cairo::LineCap::Round);

    // Preview row on top; the scene below shifts down to match
    // size_area exactly.
    let live_h = if st.live_text.is_some() {
        if let Some(text) = st.live_text.as_ref() {
            let words: Vec<&str> = text.split_whitespace().collect();
            let tail = if words.len() > 12 {
                words[words.len() - 12..].join(" ")
            } else {
                words.join(" ")
            };
            if !tail.is_empty() {
                cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
                cr.set_font_size(13.0);
                if let Ok(ext) = cr.text_extents(&tail) {
                    let tx = (width - ext.width()) / 2.0 - ext.x_bearing();
                    cr.move_to(tx, 15.0);
                    let _ = cr.show_text(&tail);
                }
            }
        }
        LIVE_TEXT_HEIGHT as f64
    } else {
        0.0
    };
    let _ = cr.save();
    cr.translate(0.0, live_h);
    let height = height - live_h;

    // Processing morph: frozen bars squeeze toward the centre and
    // shrink while the spinner fades in over them. One motion, no pop.
    let blend = st.proc_blend;
    if blend > 0.0 {
        if blend < 1.0 {
            draw_morph(cr, width, height, st, blend);
            let _ = cr.restore();
            return;
        }
        draw_spinner(cr, width, height, st.phase, 1.0);
        let _ = cr.restore();
        return;
    }

    let mut x = 0.0;
    match st.ornament {
        Ornament::None => {}
        Ornament::Mark => {
            draw_mark(cr, x, height, st.mark.as_ref());
            x += ORNAMENT_W + 8.0;
        }
        Ornament::Mic => {
            draw_mic(cr, x, height);
            x += ORNAMENT_W + 8.0;
        }
        Ornament::Dot => {
            cr.arc(x + 5.0, height / 2.0, 4.0, 0.0, 2.0 * std::f64::consts::PI);
            let _ = cr.fill();
            x += ORNAMENT_W + 8.0;
        }
    }

    let hs = heights(st);
    let pitch = BAR_WIDTH + BAR_GAP;
    let total = pitch * hs.len() as f64 - BAR_GAP;
    let free = width - x - total;
    let x0 = x + free / 2.0; // keep bars centered in remaining space
    // Bars grow symmetrically from the vertical center.
    let mid = height / 2.0;

    // NOTE: ornament painting above may replace the cairo source
    // (pixbuf for the mark), so re-select the bar color here --
    // otherwise bars stroke with transparent texels and vanish.
    cr.set_source_rgba(r, g, b, a);
    cr.set_line_width(BAR_WIDTH);
    for (i, h) in hs.iter().enumerate() {
        let bar_h = (MIN_BAR_PX + h.clamp(0.0, 1.0) as f64 * (st.lane_max - MIN_BAR_PX)).max(2.0);
        let bx = x0 + i as f64 * pitch + BAR_WIDTH / 2.0;
        cr.move_to(bx, mid + bar_h / 2.0);
        cr.line_to(bx, mid - bar_h / 2.0);
        if cr.stroke().is_err() {
            return;
        }
    }
}

/// Mid-transition frame: frozen bars dim in place (no lateral
/// motion) while the spinner blooms from the centre. `blend` in
/// (0, 1): bars hold position, spinner scales 0.5 -> 1.
fn draw_morph(cr: &cairo::Context, width: f64, height: f64, st: &VizState, blend: f32) {
    let (r, g, b, a) = st.theme.rgba(true);
    cr.set_source_rgba(r, g, b, a * (1.0 - blend as f64));
    cr.set_line_cap(cairo::LineCap::Round);
    let n = st.frozen.len().max(1);
    let pitch = BAR_WIDTH + BAR_GAP;
    let total = pitch * n as f64 - BAR_GAP;
    let x0 = (width - total) / 2.0;
    let mid = height / 2.0;
    cr.set_line_width(BAR_WIDTH);
    for (i, h) in st.frozen.iter().enumerate() {
        // Gentle settle, not a collapse: bars keep their slots.
        let bar_h = (MIN_BAR_PX + h.clamp(0.0, 1.0) as f64 * (st.lane_max - MIN_BAR_PX))
            * (1.0 - 0.55 * blend as f64);
        let bar_h = bar_h.max(1.0);
        let bx = x0 + i as f64 * pitch + BAR_WIDTH / 2.0;
        cr.move_to(bx, mid + bar_h / 2.0);
        cr.line_to(bx, mid - bar_h / 2.0);
        if cr.stroke().is_err() {
            return;
        }
    }
    draw_spinner(cr, width, height, st.phase, blend);
}

/// Twin-arc spinner: a bright head arc with a fading trailer,
/// rotating around the pill centre. `alpha` fades it in on morph;
/// the ring also blooms from half size so it arrives instead of
/// popping.
fn draw_spinner(cr: &cairo::Context, width: f64, height: f64, phase: f32, alpha: f32) {
    use std::f64::consts::PI;
    let cx = width / 2.0;
    let cy = height / 2.0;
    let bloom = 0.5 + 0.5 * alpha as f64;
    let rad = ((width.min(height) / 2.0 - 4.0).max(4.0)) * bloom;
    let head = (phase as f64 * 2.2) % (2.0 * PI);
    // Trailer: dimmer, lagging a quarter turn.
    cr.set_line_width(2.5);
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.30 * alpha as f64);
    cr.arc(cx, cy, rad, head - 2.4, head - 0.5);
    let _ = cr.stroke();
    // Head: bright 100° arc.
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.95 * alpha as f64);
    cr.set_line_width(3.0);
    cr.arc(cx, cy, rad, head, head + 1.75);
    let _ = cr.stroke();
    // Core dot, gently pulsing toward the centre.
    let pulse = 1.6 + (phase as f64 * 2.2).sin() * 0.7;
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.9 * alpha as f64);
    cr.arc(cx, cy, pulse.max(0.8), 0.0, 2.0 * PI);
    let _ = cr.fill();
}

/// Our monochrome symbolic mark, painted from the rasterized SVG;
/// simple drawn bars if the loader was unavailable.
fn draw_mark(cr: &cairo::Context, x: f64, height: f64, mark: Option<&gtk4::gdk_pixbuf::Pixbuf>) {
    if let Some(pb) = mark {
        let px = x + (ORNAMENT_W - MARK_PX as f64) / 2.0;
        let py = (height - MARK_PX as f64) / 2.0;
        cr.set_source_pixbuf(pb, px, py);
        let _ = cr.paint();
        return;
    }
    const FRACS: [f64; 5] = [0.38, 0.66, 1.0, 0.66, 0.38];
    let w = 2.5;
    let gap = 2.0;
    let max_h = 18.0;
    let base = height - (height - max_h) / 2.0;
    cr.set_line_width(w);
    for (i, f) in FRACS.iter().enumerate() {
        let bx = x + 2.0 + i as f64 * (w + gap) + w / 2.0;
        cr.move_to(bx, base);
        cr.line_to(bx, base - max_h * f);
        let _ = cr.stroke();
    }
}

/// Minimal mic glyph: capsule + cradle + stand, monochrome.
fn draw_mic(cr: &cairo::Context, x: f64, height: f64) {
    let cx = x + 9.0;
    let top = height / 2.0 - 9.0;
    cr.set_line_width(2.0);
    // Capsule.
    cr.move_to(cx, top + 2.0);
    cr.line_to(cx, top + 7.0);
    cr.set_line_width(5.0);
    let _ = cr.stroke();
    // Cradle.
    cr.set_line_width(1.6);
    cr.arc(
        cx,
        top + 7.0,
        5.0,
        0.15 * std::f64::consts::PI,
        0.85 * std::f64::consts::PI,
    );
    let _ = cr.stroke();
    // Stand.
    cr.move_to(cx, top + 12.5);
    cr.line_to(cx, top + 16.0);
    cr.move_to(cx - 3.5, top + 16.0);
    cr.line_to(cx + 3.5, top + 16.0);
    let _ = cr.stroke();
}

impl Default for SimMic {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for Visualizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod bloom_tests {
    use super::{Bloom, LevelNormalizer};

    const DT: f32 = 0.033;

    fn rms(db: f32) -> f32 {
        10f32.powf(db / 20.0)
    }

    fn run(b: &mut Bloom, level: f32, secs: f32) {
        for _ in 0..(secs / DT) as usize {
            b.step(level, 11, DT);
        }
    }

    #[test]
    fn normalizer_gates_room_noise_and_lifts_speech() {
        let mut n = LevelNormalizer::default();
        for _ in 0..200 {
            assert_eq!(n.push(rms(-45.0)), 0.0);
        }
        let mut n = LevelNormalizer::default();
        for _ in 0..30 {
            n.push(rms(-65.0));
        }
        assert!(n.push(rms(-30.0)) > 0.9);
        assert!((0.4..0.9).contains(&n.push(rms(-40.0))));
    }

    #[test]
    fn silence_stays_flat() {
        let mut b = Bloom::default();
        run(&mut b, 0.0, 1.0);
        assert!(b.heights.iter().all(|h| *h == 0.0));
    }

    #[test]
    fn voice_is_symmetric_and_center_tallest() {
        let mut b = Bloom::default();
        run(&mut b, 0.8, 0.6);
        let h = &b.heights;
        for i in 0..5 {
            assert!((h[i] - h[10 - i]).abs() < 1e-6);
        }
        assert!(h[5] > 0.5 && h[5] > h[2] && h[2] > h[0]);
    }

    #[test]
    fn opens_from_center_then_settles() {
        let mut b = Bloom::default();
        run(&mut b, 0.8, 0.066);
        assert!(b.heights[5] > 0.0);
        assert_eq!(b.heights[0], 0.0);
        run(&mut b, 0.8, 0.6);
        run(&mut b, 0.0, 2.0);
        assert!(b.heights.iter().all(|h| *h < 0.01));
    }
}
