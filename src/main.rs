use gtk4::Application;
use gtk4::prelude::*;

use mywhisper::overlay::VoiceOverlay;

const APP_ID: &str = "com.mywhisper.Pill";

/// Minimal host for the pill component. All visual options are CLI
/// flags (useful for autostart entries); the pill itself exposes the
/// same setters as a Rust API for the future pipeline/settings UI.
///
///   --mode tape|history      --bars 7|11|15|21   --height 8..32
///   --theme white|grey|red  --ornament none|mark|mic|dot
///   --idle still|silhouette|calm   --mic real|sim
///   --sim lively|quiet|off      (sim auto-burst cadence)
///   --say TEXT (show the assistant panel with TEXT)   --hide
fn main() -> gtk4::glib::ExitCode {
    let app = Application::builder().application_id(APP_ID).build();

    app.connect_activate(|app| {
        eprintln!("[poc] activate");

        // Single overlay surface for the whole session.
        let overlay = VoiceOverlay::new(app);
        apply_cli(&overlay);
        // The pill's own button closures hold clones (a deliberate
        // app-lifetime cycle), so the surface outlives this closure.
        // The local itself can drop here.
    });

    app.run_with_args::<&str>(&[])
}

fn value_of(args: &[String], key: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == key).map(|w| w[1].clone())
}

fn apply_cli(overlay: &VoiceOverlay) {
    use mywhisper::viz::{IdleStyle, Ornament, VizMode, VizTheme};
    let args: Vec<String> = std::env::args().collect();

    match value_of(&args, "--mode").as_deref() {
        Some("history") => overlay.set_viz_mode(VizMode::History),
        Some("tape") | None => overlay.set_viz_mode(VizMode::Tape),
        Some(other) => eprintln!("[poc] WARNING: unknown --mode {other:?}"),
    }
    match value_of(&args, "--bars").as_deref() {
        Some("7") => overlay.set_viz_bars(7),
        Some("11") | None => overlay.set_viz_bars(11),
        Some("15") => overlay.set_viz_bars(15),
        Some("21") => overlay.set_viz_bars(21),
        Some(other) => eprintln!("[poc] WARNING: unknown --bars {other:?}"),
    }
    if let Some(h) = value_of(&args, "--height") {
        match h.parse::<f64>() {
            Ok(px) => overlay.set_viz_height(px),
            Err(_) => eprintln!("[poc] WARNING: bad --height {h:?}"),
        }
    } else {
        overlay.set_viz_height(18.0);
    }
    match value_of(&args, "--theme").as_deref() {
        Some("grey") => overlay.set_viz_theme(VizTheme::Grey),
        Some("red") => overlay.set_viz_theme(VizTheme::Red),
        Some("white") | None => overlay.set_viz_theme(VizTheme::White),
        Some(other) => eprintln!("[poc] WARNING: unknown --theme {other:?}"),
    }
    match value_of(&args, "--ornament").as_deref() {
        Some("mark") => overlay.set_viz_ornament(Ornament::Mark),
        Some("mic") => overlay.set_viz_ornament(Ornament::Mic),
        Some("dot") => overlay.set_viz_ornament(Ornament::Dot),
        Some("none") | None => overlay.set_viz_ornament(Ornament::None),
        Some(other) => eprintln!("[poc] WARNING: unknown --ornament {other:?}"),
    }
    match value_of(&args, "--idle").as_deref() {
        Some("silhouette") => overlay.set_viz_idle(IdleStyle::Silhouette),
        Some("calm") => overlay.set_viz_idle(IdleStyle::Calm),
        Some("still") | None => overlay.set_viz_idle(IdleStyle::Still),
        Some(other) => eprintln!("[poc] WARNING: unknown --idle {other:?}"),
    }
    match value_of(&args, "--mic").as_deref() {
        Some("sim") => {
            overlay.set_viz_mic(false);
        }
        Some("real") | None => {
            overlay.set_viz_mic(true);
        }
        Some(other) => eprintln!("[poc] WARNING: unknown --mic {other:?}"),
    }
    match value_of(&args, "--sim").as_deref() {
        Some("lively") => overlay.set_viz_sim(mywhisper::viz::SimCadence::Lively),
        Some("quiet") => overlay.set_viz_sim(mywhisper::viz::SimCadence::Quiet),
        Some("off") => overlay.set_viz_sim(mywhisper::viz::SimCadence::Off),
        None => {}
        Some(other) => eprintln!("[poc] WARNING: unknown --sim {other:?}"),
    }

    if args.iter().any(|a| a == "--hide") {
        overlay.hide();
    } else if let Some(text) = value_of(&args, "--say") {
        overlay.show_assistant();
        overlay.set_assistant_text(&text);
    } else {
        overlay.show_listening();
    }
}
