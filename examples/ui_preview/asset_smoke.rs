//! Opt-in native asset acceptance with private files and a real pinned runtime.
use gtk4::prelude::*;
use libadwaita::prelude::*;
use mywhisper::{app::AppState, i18n::tr, stt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::{Duration, Instant};

const MANUAL: &[u8] = b"synthetic manually installed model";

fn cpu() -> &'static stt::GpuPack {
    stt::GPU_PACKS
        .iter()
        .find(|pack| pack.id == "whisper-cpu")
        .unwrap()
}
fn model(data: &Path) -> PathBuf {
    stt::model_dir(data, stt::find_model("tiny").unwrap()).join("ggml-tiny.bin")
}

pub fn prepare(state: &Rc<AppState>) {
    let data = state.db_path.parent().unwrap();
    assert!(data.starts_with(std::env::temp_dir()));
    state.settings.borrow_mut().stt.local_provider = mywhisper::settings::LocalSttProvider::OpenAi;
    // Real upstream bytes are never executed here. Remove only the disposable
    // receipt to reproduce a legacy installation needing reference comparison.
    let progress: stt::DownloadCallback = Arc::new(|_| {});
    stt::download_gpu_pack(data, cpu(), &AtomicBool::new(false), &progress).unwrap();
    std::fs::remove_file(stt::gpu_bin_dir(data, cpu()).join(".mywhisper-installed.json")).unwrap();
    let path = model(data);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, MANUAL).unwrap();
}

fn row(window: &gtk4::ApplicationWindow, id: &str) -> Option<gtk4::Widget> {
    super::smoke::descendants(window.upcast_ref())
        .into_iter()
        .find(|widget| widget.is::<gtk4::Box>() && widget.tooltip_text().as_deref() == Some(id))
}
fn action(window: &gtk4::ApplicationWindow, id: &str, tag: &str) -> Option<gtk4::Button> {
    super::smoke::descendants(&row(window, id)?)
        .into_iter()
        .find_map(|widget| {
            widget
                .downcast::<gtk4::Button>()
                .ok()
                .filter(|button| button.is_visible() && button.label().as_deref() == Some(tr(tag)))
        })
}
fn local_label(window: &gtk4::ApplicationWindow, id: &str) -> bool {
    row(window, id).is_some_and(|row| {
        super::smoke::descendants(&row).into_iter().any(|widget| {
            widget
                .downcast::<gtk4::Label>()
                .is_ok_and(|label| label.text() == tr("stt.asset.local"))
        })
    })
}

pub fn run(app: &gtk4::Application, state: &Rc<AppState>, window: &gtk4::ApplicationWindow) {
    let app = app.clone();
    let window = window.clone();
    let data = state.db_path.parent().unwrap().to_owned();
    let runtime = stt::gpu_bin_dir(&data, cpu()).join("whisper-server");
    let inode = std::fs::metadata(&runtime).unwrap().ino();
    let original = std::fs::read(&runtime).unwrap();
    let started = Instant::now();
    let mut step = 0;
    let mut ticks = 0;
    gtk4::glib::timeout_add_local(Duration::from_millis(20), move || {
        ticks += 1;
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "asset acceptance stalled at step {step}"
        );
        match step {
            0 => {
                let Some(button) = action(&window, "tiny", "stt.asset.trust") else {
                    return gtk4::glib::ControlFlow::Continue;
                };
                assert!(!stt::is_installed(&data, stt::find_model("tiny").unwrap()));
                let before = Instant::now();
                button.emit_clicked();
                assert!(before.elapsed() < Duration::from_millis(100));
            }
            1 => {
                let host = window
                    .clone()
                    .downcast::<libadwaita::ApplicationWindow>()
                    .unwrap();
                let dialog = host
                    .visible_dialog()
                    .unwrap()
                    .downcast::<libadwaita::AlertDialog>()
                    .unwrap();
                assert_eq!(
                    dialog.heading().as_deref(),
                    Some(tr("stt.asset.trusttitle"))
                );
                assert_eq!(dialog.body().as_str(), tr("stt.asset.trustbody"));
                assert_eq!(dialog.default_response().as_deref(), Some("cancel"));
                assert_eq!(dialog.close_response(), "cancel");
                super::smoke::respond(&window, "cancel");
            }
            2 => {
                assert!(
                    !model(&data)
                        .parent()
                        .unwrap()
                        .join(".mywhisper-installed.json")
                        .exists()
                );
                assert_eq!(std::fs::read(model(&data)).unwrap(), MANUAL);
                let Some(button) = action(&window, "whisper-cpu", "stt.asset.verify") else {
                    return gtk4::glib::ControlFlow::Continue;
                };
                assert_eq!(
                    button.tooltip_text().as_deref(),
                    Some(tr("stt.asset.verifyhelp"))
                );
                let before = Instant::now();
                button.emit_clicked();
                assert!(
                    before.elapsed() < Duration::from_millis(100),
                    "reference verification blocked GTK"
                );
            }
            3 => {
                if !stt::gpu_installed(&data, cpu()) {
                    if let Some(download) = stt::download_state(cpu().id)
                        && let stt::DlPhase::Failed(error) = download.phase
                    {
                        panic!("real pinned reference verification failed: {error}");
                    }
                    return gtk4::glib::ControlFlow::Continue;
                }
                assert_eq!(std::fs::metadata(&runtime).unwrap().ino(), inode);
                assert_eq!(std::fs::read(&runtime).unwrap(), original);
                assert!(!local_label(&window, "whisper-cpu"));
                assert!(ticks > 3, "verification did not exercise GTK heartbeat");
                action(&window, "tiny", "stt.asset.trust")
                    .unwrap()
                    .emit_clicked();
            }
            4 => super::smoke::respond(&window, "confirm"),
            5 => {
                if !stt::is_installed(&data, stt::find_model("tiny").unwrap())
                    || !local_label(&window, "tiny")
                {
                    return gtk4::glib::ControlFlow::Continue;
                }
                assert_eq!(std::fs::read(model(&data)).unwrap(), MANUAL);
                let receipt: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(
                        model(&data)
                            .parent()
                            .unwrap()
                            .join(".mywhisper-installed.json"),
                    )
                    .unwrap(),
                )
                .unwrap();
                assert_eq!(receipt["claim"]["locally_trusted"], true);
                // Same-length mutation must invalidate the attestation.
                let mut changed = MANUAL.to_vec();
                changed[0] = b'S';
                std::fs::write(model(&data), changed).unwrap();
            }
            6 => {
                if stt::is_installed(&data, stt::find_model("tiny").unwrap())
                    || local_label(&window, "tiny")
                    || action(&window, "tiny", "stt.asset.trust").is_none()
                {
                    return gtk4::glib::ControlFlow::Continue;
                }
                assert_eq!(std::fs::metadata(&runtime).unwrap().ino(), inode);
                assert!(stt::gpu_installed(&data, cpu()));
                eprintln!(
                    "[ui-smoke] asset acceptance passed: real pinned legacy runtime verification preserves bytes/inode, native cancel grants no trust, explicit manual trust is labeled and preserves bytes, same-length mutation loses installation authority, GTK stays responsive"
                );
                window.close();
                app.quit();
                return gtk4::glib::ControlFlow::Break;
            }
            _ => unreachable!(),
        }
        step += 1;
        gtk4::glib::ControlFlow::Continue
    });
}
