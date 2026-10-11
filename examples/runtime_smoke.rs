//! Real upstream download/inference gate using synthetic 16 kHz mono PCM.
//! Every store, server, model and XDG directory is owned by a disposable root.
//! Usage: runtime_smoke MODEL_ID cpu|cuda|vulkan SPEECH.raw EXPECTED_WORD
use mywhisper::{audio, settings, stt, transcribe};
use std::sync::{Arc, atomic::AtomicBool};
use std::time::{Duration, Instant};

struct Servers(std::path::PathBuf);
impl Drop for Servers {
    fn drop(&mut self) {
        transcribe::drop_servers(&self.0);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.get(1).map(String::as_str) == Some("--sherpa-contract") {
        return sherpa_contract();
    }
    if args.len() != 5 {
        return Err("expected MODEL_ID BACKEND SPEECH.raw EXPECTED_WORD".into());
    }
    let model = stt::find_model(&args[1]).ok_or("unknown model")?;
    let speech = std::fs::read(&args[3])?;
    if speech.len() > 2 * 16000 * 30 || speech.len() % 2 != 0 {
        return Err("fixture must be s16le mono, at most thirty seconds".into());
    }
    let pcm = speech
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| i16::from_le_bytes(*bytes))
        .collect::<Vec<_>>();
    let samples = pcm
        .iter()
        .map(|sample| f32::from(*sample) / 32768.0)
        .collect::<Vec<_>>();
    let disposable = tempfile::Builder::new()
        .prefix("mywhisper-runtime-audit-")
        .tempdir()?;
    let data = disposable.path().join("data/mywhisper");
    std::fs::create_dir(disposable.path().join("home"))?;
    // SAFETY: this synthetic probe sets isolation before creating any threads.
    unsafe {
        for (name, directory) in [
            ("HOME", "home"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
        ] {
            std::env::set_var(name, disposable.path().join(directory));
        }
        std::env::set_var("MYWHISPER_DB_KEY", "00".repeat(32));
    }
    let started = Instant::now();
    let progress = Arc::new(std::sync::Mutex::new(Instant::now()));
    let callback: stt::DownloadCallback = Arc::new(move |event| {
        let mut last = progress.lock().unwrap();
        match event {
            stt::DownloadEvent::Progress { received, total }
                if last.elapsed() >= Duration::from_secs(2) =>
            {
                eprintln!(
                    "verified download progress: {received}/{} bytes",
                    total.unwrap_or(0)
                );
                *last = Instant::now();
            }
            stt::DownloadEvent::Installing => eprintln!("installing verified artifact"),
            stt::DownloadEvent::Done => eprintln!("atomic installation completed"),
            _ => {}
        }
    });
    let cancelled = AtomicBool::new(false);
    stt::download_model(&data, model, &cancelled, &callback)?;
    assert!(stt::is_installed(&data, model));
    let backend = match args[2].as_str() {
        "cpu" => settings::GpuBackendSetting::Cpu,
        "cuda" => settings::GpuBackendSetting::Cuda,
        "vulkan" => settings::GpuBackendSetting::Vulkan,
        _ => return Err("backend must be cpu, cuda, or vulkan".into()),
    };
    let provider = match model.kind {
        stt::ModelKind::Whisper => settings::LocalSttProvider::OpenAi,
        stt::ModelKind::Whistle => settings::LocalSttProvider::Whistle,
        stt::ModelKind::Parakeet if model.id.starts_with("orukeet") => {
            settings::LocalSttProvider::Oruk
        }
        stt::ModelKind::Parakeet if model.id.starts_with("cohere") => {
            settings::LocalSttProvider::Cohere
        }
        stt::ModelKind::Parakeet => settings::LocalSttProvider::Nvidia,
    };
    let sherpa = if backend == settings::GpuBackendSetting::Cuda {
        settings::SherpaBackendSetting::Cuda
    } else {
        settings::SherpaBackendSetting::Cpu
    };
    match model.kind {
        stt::ModelKind::Whisper => {
            let pack_id = match backend {
                settings::GpuBackendSetting::Cpu => "whisper-cpu",
                settings::GpuBackendSetting::Cuda => "whisper-cuda",
                settings::GpuBackendSetting::Vulkan => "whisper-vulkan",
            };
            let pack = stt::GPU_PACKS
                .iter()
                .find(|pack| pack.id == pack_id)
                .unwrap();
            stt::download_gpu_pack(&data, pack, &cancelled, &callback)?;
            assert!(stt::gpu_installed(&data, pack));
            let binary = stt::gpu_bin_dir(&data, pack).join("whisper-server");
            let diagnosis = std::process::Command::new("timeout")
                .args(["5", "ldd"])
                .arg(&binary)
                .output()?;
            for line in String::from_utf8_lossy(&diagnosis.stdout)
                .lines()
                .filter(|line| line.contains("not found"))
            {
                eprintln!("Runtime dependency requirement: {}", line.trim());
            }
        }
        stt::ModelKind::Whistle => {
            stt::download_needle_pack(&data, &cancelled, &callback)?;
            assert!(stt::needle_installed(&data));
        }
        stt::ModelKind::Parakeet => {
            if backend == settings::GpuBackendSetting::Vulkan {
                return Err("sherpa has no Vulkan execution provider".into());
            }
            let pack_id = if sherpa == settings::SherpaBackendSetting::Cuda {
                "sherpa-cuda"
            } else {
                "sherpa-cpu"
            };
            let pack = stt::SHERPA_PACKS
                .iter()
                .find(|pack| pack.id == pack_id)
                .unwrap();
            stt::download_sherpa_pack(&data, pack, &cancelled, &callback)?;
            assert!(stt::sherpa_installed(&data, pack));
        }
    }
    let socket = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = socket.local_addr()?.port();
    drop(socket);
    let settings = settings::SttSettings {
        language: if provider == settings::LocalSttProvider::Oruk {
            "uk".into()
        } else {
            "en".into()
        },
        whisper_model: model.id.into(),
        parakeet_model: model.id.into(),
        local_provider: provider,
        gpu_backend: backend,
        sherpa_backend: sherpa,
        server_port: port.to_string(),
        ..Default::default()
    };
    let plan = transcribe::plan_for(&settings, &data)?;
    let net = transcribe::server_net(&settings);
    let _servers = Servers(data.clone());
    transcribe::ensure_for_plan(&plan, &data, backend, sherpa, &net)?;
    if backend == settings::GpuBackendSetting::Cuda {
        #[derive(serde::Deserialize)]
        struct ServerRecord {
            pid: u32,
        }
        let owners: std::collections::HashMap<String, ServerRecord> =
            serde_json::from_slice(&std::fs::read(data.join("servers.json"))?)?;
        let statistics = std::process::Command::new("timeout")
            .args([
                "3",
                "nvidia-smi",
                "--query-compute-apps=pid,used_gpu_memory",
                "--format=csv,noheader,nounits",
            ])
            .output()?;
        assert!(statistics.status.success(), "GPU ownership query failed");
        assert!(
            String::from_utf8_lossy(&statistics.stdout)
                .lines()
                .any(|line| {
                    let values = line.split(',').map(str::trim).collect::<Vec<_>>();
                    values.len() == 2
                        && values[0]
                            .parse::<u32>()
                            .is_ok_and(|pid| owners.values().any(|owner| owner.pid == pid))
                        && values[1].parse::<u64>().is_ok_and(|memory| memory > 0)
                }),
            "selected CUDA server must hold its own GPU allocation"
        );
    }

    let text = match plan {
        transcribe::Plan::WhisperHttp { .. } => {
            transcribe::whisper_transcribe(&audio::wav_bytes_16k(&pcm), None, port)?
        }
        transcribe::Plan::SherpaOffline { .. } => {
            transcribe::sherpa_offline_transcribe(&samples, port)?
        }
        transcribe::Plan::SherpaOnline { .. } => {
            let mut session = transcribe::OnlineSession::open_with(port, Duration::from_secs(30))?;
            for chunk in samples.chunks(1600) {
                session.send_chunk(chunk)?;
            }
            session.finish()?
        }
        transcribe::Plan::WhistleCli {
            engine,
            model,
            lang,
        } => {
            let file_text = transcribe::whistle_transcribe(&engine, &model, &samples, &[], &lang)?;
            assert!(
                file_text.to_lowercase().contains(&args[4].to_lowercase()),
                "file decode did not recognize the synthetic expected word"
            );
            let mut stream = transcribe::WhistleStream::open(&engine, &model, None, &lang)?;
            for chunk in pcm.chunks(1600) {
                stream.feed(chunk, 16000)?;
            }
            stream.finish()?
        }
        _ => return Err("local integration probe resolved a cloud plan".into()),
    };
    assert!(
        text.to_lowercase().contains(&args[4].to_lowercase()),
        "decode did not recognize the synthetic expected word; returned {} characters",
        text.chars().count()
    );
    println!(
        "Real local runtime passed: model={} backend={} synthetic speech recognized; elapsed={:.1}s; disposable resources removed on exit",
        model.id,
        args[2],
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Verify the actual authenticated distributed binary, not only source flags.
fn sherpa_contract() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::Builder::new()
        .prefix("mywhisper-contract-audit-")
        .tempdir()?;
    let pack = stt::SHERPA_PACKS
        .iter()
        .find(|pack| pack.id == "sherpa-cpu")
        .unwrap();
    let callback: stt::DownloadCallback = Arc::new(|_| {});
    stt::download_sherpa_pack(root.path(), pack, &AtomicBool::new(false), &callback)?;
    let binary =
        stt::sherpa_bin_dir(root.path(), pack).join("bin/sherpa-onnx-offline-websocket-server");
    let output = std::process::Command::new("timeout")
        .arg("10")
        .arg(binary)
        .arg("--help")
        .output()?;
    assert!(output.stdout.len() + output.stderr.len() < 256 * 1024);
    let help = String::from_utf8([output.stdout, output.stderr].concat())?;
    assert!(
        output.status.success() && help.contains("--tokens") && help.contains("--encoder"),
        "authenticated server help contract failed"
    );
    let flags = [
        "cohere-transcribe-encoder",
        "cohere-transcribe-decoder",
        "cohere-transcribe-language",
    ]
    .map(|flag| (flag, help.contains(flag)));
    assert!(
        flags.iter().all(|(_, supported)| *supported),
        "advertised Cohere contract is missing"
    );
    println!(
        "{}",
        serde_json::json!({"pack":pack.id,"sha256":pack.sha256,"help_status":output.status.code(),"cohere_flags":flags})
    );
    Ok(())
}
