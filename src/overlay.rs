use crate::viz::{IdleStyle, Ornament, SimCadence, Visualizer, VizMode, VizTheme};
use gtk4::prelude::*;
use gtk4::{Application, Box as GtkBox, Button, Label, Orientation, Window};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Overlay lifecycle states. Only `Listening` and `Assistant` have
/// meaningful UI in this proof-of-concept; the rest exist so the
/// future state machine has a place to land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Processing/Error are future states required by the POC spec.
pub enum OverlayState {
    Hidden,
    Listening,
    Processing,
    Assistant,
    Error,
}

impl std::fmt::Display for OverlayState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            OverlayState::Hidden => "Hidden",
            OverlayState::Listening => "Listening",
            OverlayState::Processing => "Processing",
            OverlayState::Assistant => "Assistant",
            OverlayState::Error => "Error",
        };
        write!(f, "{s}")
    }
}

impl OverlayState {
    /// Compact numeric code for the cross-thread state mirror
    /// (the real `Cell<OverlayState>` is GTK-thread-only).
    pub fn code(self) -> u8 {
        match self {
            OverlayState::Hidden => 0,
            OverlayState::Listening => 1,
            OverlayState::Processing => 2,
            OverlayState::Assistant => 3,
            OverlayState::Error => 4,
        }
    }

    // Cross-thread state query for future non-GTK threads (hotkeys).
    #[allow(dead_code)]
    pub fn name_of_code(code: u8) -> &'static str {
        match code {
            0 => "Hidden",
            1 => "Listening",
            2 => "Processing",
            3 => "Assistant",
            4 => "Error",
            _ => "Unknown",
        }
    }
}

const NAMESPACE: &str = "mywhisper-pill";
const BOTTOM_MARGIN: i32 = 24;
const SIDE_MARGIN: i32 = 24;
/// Expand/collapse/pop animation length; hide fade is shorter.
const ANIM_MS: u64 = 220;
const HIDE_MS: u64 = 120;

const CSS: &str = r#"
window.mywhisper-pill {
    background: transparent;
}
.voice-pill {
    background: rgba(28, 28, 34, 0.92);
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 16px;
    padding: 10px 14px;
}
.voice-title {
    font-weight: bold;
    font-size: 14px;
    color: #ffffff;
}
.voice-body {
    color: rgba(255, 255, 255, 0.85);
}
.voice-dim {
    color: rgba(255, 255, 255, 0.85);
}
.rec-dot {
    color: #ff5f57;
    font-size: 16px;
}
.voice-pill button {
    background: rgba(255, 255, 255, 0.10);
    color: #ffffff;
    border-radius: 10px;
    padding: 6px 12px;
}
.voice-pill button:hover {
    background: rgba(255, 255, 255, 0.18);
}
"#;

struct Inner {
    window: Window,
    root: GtkBox,
    compact: GtkBox,
    expanded: GtkBox,
    body: Label,
    viz: Visualizer,
    state: Cell<OverlayState>,
    /// First-words reconfigure done for this utterance.
    live_configured: Cell<bool>,
    /// Lock-free mirror of `state` for threads that may not touch GTK
    /// (e.g. a future hotkey listener).
    mirror: Arc<AtomicU8>,
    /// Animation generation: bumped on every transition so a stale
    /// frame loop exits instead of fighting the new one.
    anim_gen: Cell<u64>,
    /// Last known natural sizes per surface, so collapse can animate
    /// toward the compact size while it is still hidden.
    last_compact: Cell<(i32, i32)>,
    last_expanded: Cell<(i32, i32)>,
    layer_shell: bool,
}

/// Handle to the single layer-shell overlay window.
///
/// Cloneable (`Rc`-backed) so both the overlay's own buttons and the
/// settings-window controls can drive the same surface without ever
/// spawning a second one.
#[derive(Clone)]
pub struct VoiceOverlay {
    inner: Rc<Inner>,
}

impl VoiceOverlay {
    pub fn new(app: &Application) -> Self {
        let supported = gtk4_layer_shell::is_supported();
        eprintln!("[poc] layer-shell supported: {supported}");

        let window = Window::new();
        window.set_application(Some(app));
        window.set_decorated(false);
        window.add_css_class("mywhisper-pill");

        if supported {
            window.init_layer_shell();
            window.set_layer(Layer::Overlay);
            // Anchor bottom edge only -> bottom-center placement.
            window.set_anchor(Edge::Top, false);
            window.set_anchor(Edge::Left, false);
            window.set_anchor(Edge::Right, false);
            window.set_anchor(Edge::Bottom, true);
            window.set_margin(Edge::Bottom, BOTTOM_MARGIN);
            window.set_margin(Edge::Left, SIDE_MARGIN);
            window.set_margin(Edge::Right, SIDE_MARGIN);
            // Critical for the focus test: never take keyboard focus.
            window.set_keyboard_mode(KeyboardMode::None);
            // Never reserve desktop/workspace space.
            window.set_exclusive_zone(0);
            window.set_namespace(Some(NAMESPACE));
            eprintln!(
                "[poc] layer-shell configured: layer=Overlay bottom-anchored margin={BOTTOM_MARGIN} keyboard=None exclusive=0 ns={NAMESPACE}"
            );
        } else {
            eprintln!(
                "[poc] WARNING: layer-shell not supported; using undecorated fallback window"
            );
            window.set_default_size(280, 64);
        }

        load_css();

        // Root container carries the pill styling.
        let root = GtkBox::new(Orientation::Vertical, 0);
        root.add_css_class("voice-pill");
        window.set_child(Some(&root));

        // ---- Compact / Listening ----
        // Bars only: no buttons, no status dot. State changes come from
        // the voice pipeline (or CLI flags in this host). Bottom-pinned
        // so collapse animations shrink around content that stays glued
        // to the anchored edge.
        let compact = GtkBox::new(Orientation::Horizontal, 0);
        compact.set_valign(gtk4::Align::End);
        let viz = Visualizer::new();
        compact.append(viz.widget());
        root.append(&compact);

        // ---- Expanded / Assistant ----
        // Bottom-pinned as well: during expand the action row stays put
        // while new space opens above it.
        let expanded = GtkBox::new(Orientation::Vertical, 10);
        expanded.set_valign(gtk4::Align::End);
        let title = Label::new(Some("Assistant"));
        title.set_xalign(0.0);
        title.add_css_class("voice-title");
        let body = Label::new(Some("This is a test of the expanded assistant."));
        body.set_xalign(0.0);
        body.set_wrap(true);
        body.set_max_width_chars(40);
        body.add_css_class("voice-body");
        let row = GtkBox::new(Orientation::Horizontal, 8);
        row.set_halign(gtk4::Align::Fill);
        let do_btn = Button::with_label("Do something");
        let collapse_btn = Button::with_label("Collapse");
        row.append(&do_btn);
        row.append(&collapse_btn);
        expanded.append(&title);
        expanded.append(&body);
        expanded.append(&row);
        expanded.set_visible(false);
        root.append(&expanded);

        let overlay = Self {
            inner: Rc::new(Inner {
                window,
                root,
                compact,
                expanded,
                body,
                viz,
                state: Cell::new(OverlayState::Hidden),
                live_configured: Cell::new(false),
                mirror: Arc::new(AtomicU8::new(OverlayState::Hidden.code())),
                anim_gen: Cell::new(0),
                last_compact: Cell::new((0, 0)),
                last_expanded: Cell::new((0, 0)),
                layer_shell: supported,
            }),
        };

        // Log any pointer press landing on the overlay surface.
        let press_log = gtk4::GestureClick::new();
        press_log.connect_pressed(|_, n, x, y| {
            eprintln!("[poc] overlay pointer press: n={n} x={x:.0} y={y:.0}");
        });
        overlay.inner.window.add_controller(press_log);

        // Same-surface state transitions (no new windows).
        {
            let o = overlay.clone();
            collapse_btn.connect_clicked(move |_| {
                eprintln!("[poc] overlay button event: Collapse clicked");
                o.set_state(OverlayState::Listening);
            });
        }
        do_btn.connect_clicked(|_| {
            eprintln!("[poc] overlay button event: Do-something clicked");
        });

        overlay
    }

    // Introspection for the future settings UI.
    #[allow(dead_code)]
    pub fn is_layer_shell(&self) -> bool {
        self.inner.layer_shell
    }
    #[allow(dead_code)] // Introspection hook for future state-machine wiring.
    pub fn state(&self) -> OverlayState {
        self.inner.state.get()
    }

    /// Lock-free state copy for threads that may not touch GTK
    /// (e.g. a future hotkey listener answering state queries).
    #[allow(dead_code)]
    pub fn state_mirror(&self) -> Arc<AtomicU8> {
        self.inner.mirror.clone()
    }

    /// Show the compact Listening pill.
    pub fn show_listening(&self) {
        self.clear_live_text();
        self.set_state(OverlayState::Listening);
    }

    /// Live transcription preview (streaming models, option-gated).
    /// The surface never grows taller after first configure on this
    /// stack, so the first words re-present it once at tall size
    /// (one flicker-free reconfigure: same content, new size).
    /// Width follows text live through content sizing.
    /// Listening-only: late partials racing release are dropped —
    /// otherwise text lands in the Processing pill next to the
    /// spinner (and the hide+present reconfigure fires mid-shrink).
    pub fn set_live_text(&self, text: &str) {
        if self.inner.state.get() != OverlayState::Listening {
            return;
        }
        let words: Vec<&str> = text.split_whitespace().collect();
        let tail = if words.len() > 12 {
            words[words.len() - 12..].join(" ")
        } else {
            words.join(" ")
        };
        let first = !self.inner.live_configured.get();
        self.inner.viz.set_live_text(Some(tail));
        if first {
            self.inner.live_configured.set(true);
            // Fresh configure at tall size: hide + present re-runs
            // first-configure with the text row included.
            self.inner.window.set_visible(false);
            self.inner.window.present();
        }
    }

    /// Clear live words (release/hide retire the flag too).
    fn clear_live_text(&self) {
        self.inner.viz.set_live_text(None);
        self.inner.live_configured.set(false);
    }

    /// Inference is running: contract toward the centre into the
    /// spinner (no mic feedback in this state).
    pub fn show_processing(&self) {
        self.set_state(OverlayState::Processing);
    }

    /// Show the expanded Assistant panel.
    pub fn show_assistant(&self) {
        self.set_state(OverlayState::Assistant);
    }

    /// Hide the overlay entirely.
    pub fn hide(&self) {
        self.set_state(OverlayState::Hidden);
    }

    /// Switch the pill visualizer animation.
    pub fn set_viz_mode(&self, mode: VizMode) {
        self.inner.viz.set_mode(mode);
    }

    /// Force a simulated speech burst (demo/test hook).
    #[allow(dead_code)]
    pub fn viz_burst(&self) {
        self.inner.viz.burst();
    }

    pub fn set_viz_bars(&self, n: usize) {
        self.inner.viz.set_bar_count(n);
        self.refit();
    }

    /// Live pill height (max bar excursion, px).
    pub fn set_viz_height(&self, px: f64) {
        self.inner.viz.set_lane_max(px);
        self.refit();
    }

    pub fn set_viz_theme(&self, theme: VizTheme) {
        self.inner.viz.set_theme(theme);
    }

    pub fn set_viz_ornament(&self, ornament: Ornament) {
        self.inner.viz.set_ornament(ornament);
        self.refit();
    }

    pub fn set_viz_idle(&self, idle: IdleStyle) {
        self.inner.viz.set_idle(idle);
    }

    pub fn set_viz_sim(&self, cadence: SimCadence) {
        self.inner.viz.set_sim_cadence(cadence);
    }

    /// Switch pill level input. Returns false when real input is
    /// unavailable (stays on sim).
    pub fn set_viz_mic(&self, real: bool) -> bool {
        self.inner.viz.set_mic_source(real)
    }

    /// Pin the layer surface to a screen corner/edge. Bottom anchor is
    /// constant; only the horizontal anchoring moves. No-op on the
    /// undecorated fallback window.
    pub fn set_position(&self, pos: crate::settings::FloatingPosition) {
        use crate::settings::FloatingPosition as P;
        if !self.inner.layer_shell {
            return;
        }
        let (left, right) = match pos {
            P::BottomCenter => (false, false),
            P::BottomLeft => (true, false),
            P::BottomRight => (false, true),
        };
        self.inner.window.set_anchor(Edge::Left, left);
        self.inner.window.set_anchor(Edge::Right, right);
        self.inner.window.queue_resize();
        eprintln!("[poc] overlay position -> {pos:?}");
    }

    /// Apply a full pill preference snapshot (service watcher path).
    /// Runs on the GTK thread.
    pub fn apply_prefs(&self, p: &crate::settings::PillPrefs) {
        self.set_position(p.position);
        self.set_viz_mode(p.viz_mode);
        self.set_viz_bars(p.bars);
        self.set_viz_height(p.lane_max);
        self.set_viz_theme(p.theme);
        self.set_viz_ornament(p.ornament);
        self.set_viz_idle(p.idle);
    }

    /// Re-measure content and resize the toplevel. Needed after any
    /// layout-affecting viz change (bar count, ornament), same helper
    /// as state transitions.
    pub fn refit(&self) {
        if self.inner.state.get() != OverlayState::Hidden {
            self.fit_window_to_content();
            self.inner.window.queue_resize();
        }
    }

    /// Set the assistant panel body text. When the panel is currently
    /// shown, the window animates to the new natural size -- this is
    /// the variable-size proof: any content height/width is honored.
    pub fn set_assistant_text(&self, text: &str) {
        self.inner.body.set_text(text);
        eprintln!("[poc] assistant text set ({} chars)", text.len());
        if self.inner.state.get() == OverlayState::Assistant && self.inner.window.is_visible() {
            let (tw, th) = self.measure_content();
            self.inner.last_expanded.set((tw, th));
            self.animate_resize(tw, th, 160, None, None);
        }
    }

    /// Measure the root container's natural size for the currently
    /// visible child (a hidden child contributes nothing).
    fn measure_content(&self) -> (i32, i32) {
        let root = &self.inner.root;
        let (_, nat_w, _, _) = root.measure(Orientation::Horizontal, -1);
        let (_, nat_h, _, _) = root.measure(Orientation::Vertical, nat_w);
        (nat_w, nat_h)
    }

    /// Shrink-or-grow the toplevel to the visible content's natural size.
    ///
    /// GTK toplevels grow for larger content but never auto-shrink, so a
    /// collapsed pill would keep its expanded allocation (dead input area).
    /// Measuring the root container (which only requisitions its visible
    /// child + CSS padding) and applying it as the default size makes both
    /// directions deterministic. Records the size per surface for the
    /// collapse animation (which targets the still-hidden compact box).
    fn fit_window_to_content(&self) {
        let (nat_w, nat_h) = self.measure_content();
        eprintln!("[poc] fit window to content: {nat_w}x{nat_h}");
        self.record_size(self.inner.state.get(), nat_w, nat_h);
        self.inner.window.set_default_size(nat_w, nat_h);
    }

    fn record_size(&self, state: OverlayState, w: i32, h: i32) {
        match state {
            OverlayState::Listening | OverlayState::Processing | OverlayState::Error => {
                self.inner.last_compact.set((w, h))
            }
            OverlayState::Assistant => self.inner.last_expanded.set((w, h)),
            OverlayState::Hidden => {}
        }
    }

    fn ease_out_cubic(t: f64) -> f64 {
        1.0 - (1.0 - t).powi(3)
    }

    /// Frame-driven resize (+ optional fade) of the layer surface.
    ///
    /// This is the single animation primitive behind expand, collapse,
    /// pop-in, fade-out and content-driven resizes (`say`). A generation
    /// counter makes overlapping transitions safe: starting a new one
    /// silently retires the previous frame loop.
    fn animate_resize(
        &self,
        tw: i32,
        th: i32,
        dur_ms: u64,
        fade: Option<(f64, f64)>,
        on_done: Option<Rc<dyn Fn()>>,
    ) {
        let generation = self.inner.anim_gen.get().wrapping_add(1);
        self.inner.anim_gen.set(generation);
        let me = self.clone();
        let win = self.inner.window.clone();
        let w0 = win.width();
        let h0 = win.height();
        // A hidden window has no allocation: pop in from 75% + fade.
        let (sw, sh) = if w0 < 2 || h0 < 2 {
            ((tw * 3 / 4).max(1), (th * 3 / 4).max(1))
        } else {
            (w0, h0)
        };
        let (fo, to) = fade.unwrap_or((win.opacity(), win.opacity()));
        win.set_opacity(fo);
        let t0 = Instant::now();
        let dur = dur_ms as f64;
        eprintln!("[poc] anim {sw}x{sh} -> {tw}x{th} {dur_ms}ms fade {fo:.2}->{to:.2}");
        gtk4::glib::timeout_add_local(Duration::from_millis(16), move || {
            if me.inner.anim_gen.get() != generation {
                return gtk4::glib::ControlFlow::Break; // superseded
            }
            let t = (t0.elapsed().as_millis() as f64 / dur).min(1.0);
            let e = Self::ease_out_cubic(t);
            let w = (sw as f64 + (tw - sw) as f64 * e).round() as i32;
            let h = (sh as f64 + (th - sh) as f64 * e).round() as i32;
            win.set_default_size(w.max(1), h.max(1));
            win.set_opacity(fo + (to - fo) * e);
            if t >= 1.0 {
                win.set_default_size(tw, th);
                win.set_opacity(to);
                if let Some(f) = on_done.as_ref() {
                    f();
                }
                me.log_geom("anim done");
                return gtk4::glib::ControlFlow::Break;
            }
            gtk4::glib::ControlFlow::Continue
        });
    }

    fn log_geom(&self, tag: &'static str) {
        let window = self.inner.window.clone();
        let compact = self.inner.compact.clone();
        let expanded = self.inner.expanded.clone();
        let state = self.inner.state.get();
        gtk4::glib::idle_add_local_once(move || {
            eprintln!(
                "[poc] geom {tag} ({state}): window={}x{} compact={}x{} vis={} expanded={}x{} vis={}",
                window.width(),
                window.height(),
                compact.width(),
                compact.height(),
                compact.is_visible(),
                expanded.width(),
                expanded.height(),
                expanded.is_visible(),
            );
        });
    }

    fn show_compact_boxes(&self) {
        self.inner.compact.set_visible(true);
        self.inner.expanded.set_visible(false);
    }

    fn show_expanded_boxes(&self) {
        self.inner.compact.set_visible(false);
        self.inner.expanded.set_visible(true);
    }

    /// Instant compact show (first paint, or collapse with no size
    /// history). All animated paths funnel through here at the end.
    fn show_compact_instant(&self, next: OverlayState) {
        self.show_compact_boxes();
        self.fit_window_to_content();
        self.inner.window.present();
        eprintln!("[poc] overlay shown (compact {next})");
        self.log_geom("shown");
    }

    /// Animated compact show: pop-in/fade when coming from hidden.
    fn show_compact_animated(&self, prev: OverlayState, next: OverlayState) {
        let was_hidden = prev == OverlayState::Hidden;
        self.show_compact_boxes();
        let (tw, th) = self.measure_content();
        self.record_size(next, tw, th);
        self.inner.window.present();
        let fade = was_hidden.then_some((0.0, 1.0));
        self.animate_resize(tw, th, ANIM_MS, fade, None);
        eprintln!("[poc] overlay shown (compact {next})");
    }

    /// Animated expand: new content is shown immediately and the
    /// surface grows around it, bottom edge pinned.
    fn show_expanded_animated(&self, prev: OverlayState) {
        let was_hidden = prev == OverlayState::Hidden;
        self.show_expanded_boxes();
        let (tw, th) = self.measure_content();
        self.record_size(OverlayState::Assistant, tw, th);
        self.inner.window.present();
        let fade = was_hidden.then_some((0.0, 1.0));
        self.animate_resize(tw, th, ANIM_MS, fade, None);
        eprintln!("[poc] overlay shown (expanded Assistant)");
    }

    /// Central state transition on the *same* layer-shell window, so
    /// geometry stays bottom-anchored and growth heads upward.
    pub fn set_state(&self, next: OverlayState) {
        let prev = self.inner.state.get();
        if prev == next {
            return;
        }
        // Retire any running animation before starting a new one.
        self.inner
            .anim_gen
            .set(self.inner.anim_gen.get().wrapping_add(1));
        self.inner.state.set(next);
        self.inner.mirror.store(next.code(), Ordering::SeqCst);
        // Capture follows visibility exactly: open on any shown
        // state, drop on hide. No idle-open mic, ever.
        self.inner.viz.set_mic_active(next != OverlayState::Hidden);
        // Live words belong to Listening only: release (Processing)
        // and hide retire them with the state.
        if next != OverlayState::Listening {
            self.clear_live_text();
        }
        eprintln!("[poc] overlay state: {prev} -> {next}");

        match next {
            OverlayState::Hidden => {
                let win = self.inner.window.clone();
                let (w, h) = (win.width().max(1), win.height().max(1));
                self.animate_resize(
                    w,
                    h,
                    HIDE_MS,
                    Some((1.0, 0.0)),
                    Some(Rc::new(move || {
                        win.set_visible(false);
                        win.set_opacity(1.0);
                        eprintln!("[poc] overlay hidden");
                    })),
                );
            }
            OverlayState::Listening | OverlayState::Processing | OverlayState::Error => {
                // Spinner replaces the mic bars while inferring; the
                // size change drives the contract animation below.
                self.inner
                    .viz
                    .set_spinning(next == OverlayState::Processing);
                if next == OverlayState::Error {
                    eprintln!(
                        "[poc] state {next} has no dedicated UI in POC; showing compact pill"
                    );
                }
                if prev == OverlayState::Assistant {
                    // Animated collapse: swap to the small content FIRST
                    // (bottom-pinned), then shrink the surface. Shrinking
                    // with the big content still visible fights GTK's
                    // minimum-size negotiation (rewrap pushes back), so
                    // the swap must lead.
                    let (tw, th) = self.inner.last_compact.get();
                    if tw < 2 || th < 2 {
                        self.show_compact_instant(next);
                    } else {
                        self.show_compact_boxes();
                        self.inner.window.queue_resize();
                        let me = self.clone();
                        self.animate_resize(
                            tw,
                            th,
                            ANIM_MS,
                            None,
                            Some(Rc::new(move || {
                                me.fit_window_to_content();
                                me.inner.window.queue_resize();
                                eprintln!("[poc] overlay collapsed (compact {next})");
                                me.log_geom("collapsed");
                            })),
                        );
                    }
                } else {
                    self.show_compact_animated(prev, next);
                }
            }
            OverlayState::Assistant => {
                self.show_expanded_animated(prev);
            }
        }
    }
}

fn load_css() {
    let provider = gtk4::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gtk4::gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    } else {
        eprintln!("[poc] WARNING: no default display; CSS provider not installed");
    }
}
