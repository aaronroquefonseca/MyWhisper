//! Transcription engines: local servers, cloud APIs, utterance flow.
//!
//! Audio never touches disk: the mic tap ([`crate::audio`]) fills a
//! RAM [`Recorder`](crate::audio::Recorder), engines receive bytes
//! over localhost HTTP/WebSocket or HTTPS, and only the final text
//! is stored (history) and pasted.
//!
//! Routing:
//!
//! - Local / OpenAI (whisper) → `whisper-server` HTTP `POST /inference`
//!   after release. whisper.cpp's server has no streaming API, so
//!   realtime partials are impossible here by construction.
//! - Local / NVIDIA, `ParakeetRuntime::Online` (nemotron streaming)
//!   → `sherpa-onnx-online-websocket-server`, true realtime: chunks
//!   stream during the hotkey hold, decoding continues after release
//!   if the engine lags, paste happens when the final arrives.
//! - Local / NVIDIA offline-or-unmarked (parakeet TDT) and Local /
//!   Oruk (orukeet) → `sherpa-onnx-offline-websocket-server`, whole
//!   utterance after release.
//! - Local / Cohere → unsupported: `offline-websocket-server`
//!   v1.13.8 exposes no `cohere-transcribe-*` flags (library-only
//!   model). Clear error, revisit when the server grows flags.
//! - Cloud OpenAI / Groq / Mistral → OpenAI-compatible
//!   `POST {base}/audio/transcriptions` after release. (Mistral's
//!   endpoint mirrors the OpenAI shape for its transcription model;
//!   if upstream drifts, the server error surfaces in the log.)
//! - Cloud Deepgram → `POST {base}/v1/listen` (pre-recorded REST).
//! - Cloud Gemini → `generateContent` with inline WAV (REST).
//! Language: whisper-server gets `-l` at spawn; OpenAI-compatible
//! endpoints get the `language` field, Deepgram a `&language=`
//! query, Gemini a prompt hint ("auto" omits everywhere). The
//! sherpa-side models in the registry are English-only, so the
//! option doesn't apply there (nothing to pass: the servers expose
//! no language flag in v1.13.8).
//!
//! Wire references: sherpa online/offline python websocket clients
//! (binary float32-LE chunks; online ends with text `"Done"` and
//! answers JSON partials until text `"Done!"`; offline sends
//! `[rate u32-LE][frames u32-LE][float32…]` chunked at 10 KiB and
//! answers one JSON text), whisper.cpp `examples/server` README
//! (`multipart /inference` + `response_format=json`, `/load`).

use std::collections::HashMap;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex, OnceLock,
};
use std::time::{Duration, Instant};

use crate::audio::{self, wav_bytes_16k, Recorder, Resampler, ENGINE_RATE, MIN_SAMPLES_16K};
use crate::settings::{GpuBackendSetting, LocalSttProvider, SttMode, SttSettings};
use crate::stt;

// ---------------------------------------------------------------- ports

const WHISPER_PORT: u16 = 8080;
const SHERPA_ONLINE_PORT: u16 = 6006;
const SHERPA_OFFLINE_PORT: u16 = 6007;
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(20);
const INFERENCE_TIMEOUT: Duration = Duration::from_secs(180);

// ---------------------------------------------------------------- plans

/// Fully resolved transcription target for one utterance.
#[derive(Debug, Clone)]
pub enum Plan {
    WhisperHttp {
        model_bin: PathBuf,
        lang: String,
    },
    SherpaOnline {
        model_dir: PathBuf,
    },
    SherpaOffline {
        model_dir: PathBuf,
    },
    CloudOpenAiCompat {
        base: String,
        key: String,
        model: String,
        lang: String,
    },
    CloudDeepgram {
        base: String,
        key: String,
        model: String,
        lang: String,
    },
    CloudGemini {
        key: String,
        model: String,
        lang: String,
    },
}

impl Plan {
    pub fn streaming(&self) -> bool {
        matches!(self, Plan::SherpaOnline { .. })
    }

    pub fn describe(&self) -> String {
        match self {
            Plan::WhisperHttp { model_bin, .. } => {
                format!("whisper-server ({})", model_bin.display())
            }
            Plan::SherpaOnline { model_dir } => {
                format!("sherpa-online ({})", model_dir.display())
            }
            Plan::SherpaOffline { model_dir } => {
                format!("sherpa-offline ({})", model_dir.display())
            }
            Plan::CloudOpenAiCompat { base, model, .. } => {
                format!("cloud {base} ({model})")
            }
            Plan::CloudDeepgram { model, .. } => format!("deepgram ({model})"),
            Plan::CloudGemini { model, .. } => format!("gemini ({model})"),
        }
    }
}

fn missing(what: &str) -> String {
    format!("{what} (download it in Settings → Speech to Text / Language Models)")
}

/// Resolve the engine for these settings, verifying binaries and
/// model files exist. Errors are user-actionable strings for the log
/// (and future pill surface).
pub fn plan_for(s: &SttSettings, data_dir: &Path) -> Result<Plan, String> {
    match s.mode {
        SttMode::SelfHosted => {
            let url = s.self_hosted_url.trim().trim_end_matches('/').to_string();
            if url.is_empty() {
                return Err("self-hosted URL is empty".to_string());
            }
            Ok(Plan::CloudOpenAiCompat {
                base: url,
                key: String::new(),
                model: s.self_hosted_model.clone(),
                lang: s.language.clone(),
            })
        }
        SttMode::Cloud => cloud_plan(s),
        SttMode::Local => local_plan(s, data_dir),
    }
}

fn cloud_plan(s: &SttSettings) -> Result<Plan, String> {
    if s.cloud_api_key.trim().is_empty() {
        return Err("cloud API key is empty (Settings → Speech to Text)".to_string());
    }
    let key = s.cloud_api_key.trim().to_string();
    let model = if s.cloud_model.trim().is_empty() {
        default_cloud_model(&s.cloud_provider)
    } else {
        s.cloud_model.trim().to_string()
    };
    match s.cloud_provider.as_str() {
        "Deepgram" => Ok(Plan::CloudDeepgram {
            base: "https://api.deepgram.com".to_string(),
            key,
            model,
            lang: s.language.clone(),
        }),
        "Gemini" => Ok(Plan::CloudGemini {
            key,
            model,
            lang: s.language.clone(),
        }),
        "OpenAI" => Ok(Plan::CloudOpenAiCompat {
            base: "https://api.openai.com/v1".to_string(),
            key,
            model,
            lang: s.language.clone(),
        }),
        "Groq" => Ok(Plan::CloudOpenAiCompat {
            base: "https://api.groq.com/openai/v1".to_string(),
            key,
            model,
            lang: s.language.clone(),
        }),
        "Mistral" => Ok(Plan::CloudOpenAiCompat {
            base: "https://api.mistral.ai/v1".to_string(),
            key,
            model,
            lang: s.language.clone(),
        }),
        other => Err(format!("cloud provider {other:?} is not supported yet")),
    }
}

fn default_cloud_model(provider: &str) -> String {
    match provider {
        "Deepgram" => "nova-3".to_string(),
        "Gemini" => "gemini-2.0-flash".to_string(),
        "Mistral" => "voxtral-mini-latest".to_string(),
        "Groq" => "whisper-large-v3-turbo".to_string(),
        _ => "whisper-1".to_string(),
    }
}

fn local_plan(s: &SttSettings, data_dir: &Path) -> Result<Plan, String> {
    match s.local_provider {
        LocalSttProvider::OpenAi => {
            let entry = stt::find_model(&s.whisper_model)
                .ok_or_else(|| format!("unknown whisper model {:?}", s.whisper_model))?;
            let dir = stt::model_dir(data_dir, entry);
            let bin = dir.join(format!("ggml-{}.bin", stt::whisper_file_stem(entry.id)));
            if !bin.exists() {
                return Err(missing(&format!("whisper model {}", entry.id)));
            }
            Ok(Plan::WhisperHttp {
                model_bin: bin,
                lang: s.language.clone(),
            })
        }
        LocalSttProvider::Nvidia => {
            let entry = stt::find_model(&s.parakeet_model)
                .ok_or_else(|| format!("unknown parakeet model {:?}", s.parakeet_model))?;
            let dir = stt::model_dir(data_dir, entry);
            require_transducer_files(&dir, entry.id)?;
            match entry.runtime {
                Some(stt::ParakeetRuntime::Online) => Ok(Plan::SherpaOnline { model_dir: dir }),
                _ => Ok(Plan::SherpaOffline { model_dir: dir }),
            }
        }
        LocalSttProvider::Oruk => {
            let entry = stt::find_model("orukeet-v0.1.0").ok_or_else(|| missing("orukeet"))?;
            let dir = stt::model_dir(data_dir, entry);
            require_transducer_files(&dir, entry.id)?;
            Ok(Plan::SherpaOffline { model_dir: dir })
        }
        LocalSttProvider::Cohere => Err(
            "Cohere Transcribe has no websocket-server flags in sherpa-onnx v1.13.8 \
             (library-only model); pick another local provider for now"
                .to_string(),
        ),
    }
}

fn require_transducer_files(dir: &Path, id: &str) -> Result<(), String> {
    for f in [
        "encoder.int8.onnx",
        "decoder.int8.onnx",
        "joiner.int8.onnx",
        "tokens.txt",
    ] {
        if !dir.join(f).exists() {
            return Err(missing(&format!("parakeet model {id} ({f})")));
        }
    }
    Ok(())
}

// ------------------------------------------------------- server lifetime

struct Supervised {
    key: String,
    child: Child,
    /// whisper-server only: model currently loaded (for /load).
    loaded_model: Option<PathBuf>,
}

static SERVERS: OnceLock<Mutex<HashMap<String, Supervised>>> = OnceLock::new();

fn servers() -> &'static Mutex<HashMap<String, Supervised>> {
    SERVERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn port_open(port: u16) -> bool {
    TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_millis(300),
    )
    .is_ok()
}

fn threads_arg() -> String {
    std::thread::available_parallelism()
        .map(|n| n.get().min(8).to_string())
        .unwrap_or_else(|_| "4".to_string())
}

/// Spawn (or reuse) a supervised localhost server keyed by `name`.
/// A dead child is reaped and respawned (its exit status is logged);
/// a different `key` kills the old one first (model/language
/// switches). `probe` runs only on fresh spawns: TCP-accept does not
/// imply the model finished loading (sherpa listens early).
fn ensure_server(
    name: &str,
    key: String,
    port: u16,
    spawn: impl FnOnce() -> Result<Child, String>,
    probe: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let mut map = servers().lock().map_err(|e| format!("server lock: {e}"))?;
    if let Some(s) = map.get_mut(name) {
        let alive = s.child.try_wait().map(|s| s.is_none()).unwrap_or(false);
        if alive && s.key == key {
            return Ok(());
        }
        match s.child.try_wait() {
            Ok(Some(status)) => {
                eprintln!("[stt] {name} died ({status}); respawning")
            }
            _ => eprintln!("[stt] {name} key changed; replacing"),
        }
        let _ = s.child.kill();
        let _ = s.child.wait();
        map.remove(name);
    }
    if port_open(port) {
        // Ours died but something answers: assume a user-run server.
        // (Model mismatch risk is logged, not fatal.)
        eprintln!("[stt] port {port} already answers; reusing foreign {name}");
        return Ok(());
    }
    let child = spawn()?;
    map.insert(
        name.to_string(),
        Supervised {
            key,
            child,
            loaded_model: None,
        },
    );
    drop(map); // the wait loop re-locks per iteration (non-reentrant)
               // Wait for listen, but fail fast when the child dies first (e.g.
               // cuda binary with no CUDA libs exits 127 in ms — no 20 s mystery).
    let start = Instant::now();
    loop {
        if port_open(port) {
            break;
        }
        let exited = {
            let mut map = servers().lock().map_err(|e| format!("server lock: {e}"))?;
            match map.get_mut(name) {
                Some(s) => s.child.try_wait().ok().flatten(),
                None => {
                    return Err(format!("{name} vanished during startup"));
                }
            }
        };
        if let Some(status) = exited {
            return Err(format!(
                "{name} exited during startup ({status}); \
                 see /tmp/mywhisper-{name}.log (missing shared libs?)"
            ));
        }
        if start.elapsed() >= SERVER_START_TIMEOUT {
            return Err(format!("server on port {port} did not answer in 20 s"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    probe()
}

/// Server stdout/stderr go here, not null: the next crash leaves a
/// trace instead of a mystery reset.
fn server_logs(name: &str) -> (Stdio, Stdio) {
    use std::fs::OpenOptions;
    let path = format!("/tmp/mywhisper-{name}.log");
    let open = || {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()
    };
    match (open(), open()) {
        (Some(o), Some(e)) => (Stdio::from(o), Stdio::from(e)),
        _ => (Stdio::null(), Stdio::null()),
    }
}

fn whisper_binary(data_dir: &Path, backend: GpuBackendSetting) -> Result<PathBuf, String> {
    // Strict: the dropdown is the single selector. No silent fallback
    // to another backend (that mismatch was reported as "cuda selected
    // but cpu used / nothing works").
    let want = match backend {
        GpuBackendSetting::Cuda => "whisper-cuda",
        GpuBackendSetting::Vulkan => "whisper-vulkan",
        GpuBackendSetting::Cpu => "whisper-cpu",
    };
    let pack = stt::GPU_PACKS
        .iter()
        .find(|p| p.id == want)
        .ok_or_else(|| format!("unknown whisper pack {want}"))?;
    let bin = stt::gpu_bin_dir(data_dir, pack).join("whisper-server");
    if bin.exists() {
        Ok(bin)
    } else {
        Err(missing(&format!("{want} runtime pack")))
    }
}

fn sherpa_binary(data_dir: &Path, which: &str) -> Result<PathBuf, String> {
    let pack = &stt::SHERPA_PACKS[0];
    let rel = pack
        .binaries
        .iter()
        .find(|b| b.ends_with(which))
        .ok_or_else(|| format!("sherpa pack lacks {which}"))?;
    let bin = stt::sherpa_bin_dir(data_dir, pack).join(rel);
    if bin.exists() {
        Ok(bin)
    } else {
        Err(missing("sherpa-cpu runtime pack"))
    }
}

fn ensure_whisper(
    data_dir: &Path,
    backend: GpuBackendSetting,
    model: &Path,
    lang: &str,
) -> Result<(), String> {
    let bin = whisper_binary(data_dir, backend)?;
    let key = format!("{}|{}|{lang}", bin.display(), model.display());
    let model_arg = model.to_string_lossy().to_string();
    let lang_arg = lang.to_string();
    ensure_server(
        "whisper",
        key,
        WHISPER_PORT,
        || {
            let (out, err) = server_logs("whisper-server");
            Command::new(&bin)
                .args([
                    "--model",
                    &model_arg,
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &WHISPER_PORT.to_string(),
                    "--language",
                    &lang_arg,
                    "--threads",
                    &threads_arg(),
                ])
                .stdin(Stdio::null())
                .stdout(out)
                .stderr(err)
                .spawn()
                .map_err(|e| format!("spawn whisper-server: {e}"))
        },
        || Ok(()), // model loads before listen: TCP-ready ≈ serving
    )?;
    // /load when the running server holds another model.
    let map = servers().lock().map_err(|e| format!("server lock: {e}"))?;
    let swap = map
        .get(&"whisper".to_string())
        .map(|s| s.loaded_model.as_deref() != Some(model))
        .unwrap_or(true);
    if swap {
        drop(map);
        whisper_load(model)?;
        let mut map = servers().lock().map_err(|e| format!("server lock: {e}"))?;
        if let Some(s) = map.get_mut("whisper") {
            s.loaded_model = Some(model.to_path_buf());
        }
    }
    Ok(())
}

/// Multipart/related body builder (no extra deps): fields + one file.
fn multipart_body(
    boundary: &str,
    fields: &[(&str, &str)],
    file_field: Option<(&str, &str, &str, &[u8])>,
) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    if let Some((field, filename, ctype, bytes)) = file_field {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(format!("Content-Type: {ctype}\r\n\r\n").as_bytes());
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(INFERENCE_TIMEOUT)
        .build()
        .map_err(|e| format!("http client: {e}"))
}

fn whisper_load(model: &Path) -> Result<(), String> {
    let boundary = "mywhisperload";
    let body = multipart_body(boundary, &[("model", &model.to_string_lossy())], None);
    let res = http_client()?
        .post(format!("http://127.0.0.1:{WHISPER_PORT}/load"))
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .map_err(|e| format!("whisper /load: {e}"))?;
    if res.status().is_success() {
        Ok(())
    } else {
        Err(format!("whisper /load: http {}", res.status()))
    }
}

fn ensure_sherpa_offline(data_dir: &Path, model_dir: &Path) -> Result<(), String> {
    let bin = sherpa_binary(data_dir, "sherpa-onnx-offline-websocket-server")?;
    let key = format!("{}|{}", bin.display(), model_dir.display());
    let (bin_s, dir_s) = (bin.clone(), model_dir.to_path_buf());
    ensure_server(
        "sherpa-offline",
        key,
        SHERPA_OFFLINE_PORT,
        || {
            let f = |n: &str| dir_s.join(n).to_string_lossy().to_string();
            let (out, err) = server_logs("sherpa-offline-server");
            Command::new(&bin_s)
                .args([
                    format!("--port={SHERPA_OFFLINE_PORT}"),
                    format!("--tokens={}", f("tokens.txt")),
                    format!("--encoder={}", f("encoder.int8.onnx")),
                    format!("--decoder={}", f("decoder.int8.onnx")),
                    format!("--joiner={}", f("joiner.int8.onnx")),
                    format!("--num-threads={}", threads_arg()),
                    "--log-file=/tmp/mywhisper-sherpa-offline.log".to_string(),
                ])
                .stdin(Stdio::null())
                .stdout(out)
                .stderr(err)
                .spawn()
                .map_err(|e| format!("spawn sherpa offline server: {e}"))
        },
        probe_sherpa_offline,
    )
}

fn ensure_sherpa_online(data_dir: &Path, model_dir: &Path) -> Result<(), String> {
    let bin = sherpa_binary(data_dir, "sherpa-onnx-online-websocket-server")?;
    let key = format!("{}|{}", bin.display(), model_dir.display());
    let (bin_s, dir_s) = (bin.clone(), model_dir.to_path_buf());
    ensure_server(
        "sherpa-online",
        key,
        SHERPA_ONLINE_PORT,
        || {
            let f = |n: &str| dir_s.join(n).to_string_lossy().to_string();
            let (out, err) = server_logs("sherpa-online-server");
            Command::new(&bin_s)
                .args([
                    format!("--port={SHERPA_ONLINE_PORT}"),
                    format!("--tokens={}", f("tokens.txt")),
                    format!("--encoder={}", f("encoder.int8.onnx")),
                    format!("--decoder={}", f("decoder.int8.onnx")),
                    format!("--joiner={}", f("joiner.int8.onnx")),
                    format!("--num-threads={}", threads_arg()),
                    "--log-file=/tmp/mywhisper-sherpa-online.log".to_string(),
                ])
                .stdin(Stdio::null())
                .stdout(out)
                .stderr(err)
                .spawn()
                .map_err(|e| format!("spawn sherpa online server: {e}"))
        },
        probe_sherpa_online,
    )
}

// ------------------------------------------------------------- ws client

type Ws = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>;

fn ws_connect(port: u16, read_timeout: Duration) -> Result<Ws, String> {
    let (ws, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}"))
        .map_err(|e| format!("ws connect :{port}: {e}"))?;
    if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
        let _ = s.set_read_timeout(Some(read_timeout));
        let _ = s.set_write_timeout(Some(Duration::from_secs(30)));
    }
    Ok(ws)
}

fn ws_send_binary(ws: &mut Ws, bytes: &[u8]) -> Result<(), String> {
    ws.send(tungstenite::Message::binary(bytes.to_vec()))
        .map_err(|e| format!("ws send: {e}"))?;
    ws.flush().map_err(|e| format!("ws flush: {e}"))
}

fn f32le(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 4);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Pull "text" out of a sherpa JSON reply (online partials and the
/// offline single reply share the field; everything else ignored).
fn sherpa_text(msg: &str) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_str(msg).map_err(|e| format!("sherpa json: {e}"))?;
    v.get("text")
        .and_then(|t| t.as_str())
        .map(|t| t.to_string())
        .ok_or_else(|| format!("sherpa reply has no text: {msg}"))
}

/// Tiny-decode readiness probes: 0.5 s of silence must come back
/// within 60 s. Catches "port open, model still loading" (sherpa
/// listens before the recognizer is ready).
fn probe_sherpa_offline() -> Result<(), String> {
    let silence = vec![0.0f32; 8000];
    eprintln!("[stt] sherpa-offline probe: 0.5s silence");
    let text = sherpa_offline_transcribe_inner(&silence, Duration::from_secs(60))?;
    eprintln!("[stt] sherpa-offline probe ok (reply: {text:?})");
    Ok(())
}

fn probe_sherpa_online() -> Result<(), String> {
    eprintln!("[stt] sherpa-online probe: 0.5s silence");
    let mut s = OnlineSession::open_with(SHERPA_ONLINE_PORT, Duration::from_secs(60))?;
    s.send_chunk(&vec![0.0f32; 8000])?;
    let text = s.finish()?;
    eprintln!("[stt] sherpa-online probe ok (reply: {text:?})");
    Ok(())
}

/// Online (streaming) session: chunks go out during the hold, the
/// final is read after `finish` sends `"Done"`.
pub struct OnlineSession {
    ws: Option<Ws>,
    last_text: String,
}

impl OnlineSession {
    pub fn open() -> Result<Self, String> {
        Self::open_with(SHERPA_ONLINE_PORT, INFERENCE_TIMEOUT)
    }

    pub fn open_with(port: u16, read_timeout: Duration) -> Result<Self, String> {
        Ok(Self {
            ws: Some(ws_connect(port, read_timeout)?),
            last_text: String::new(),
        })
    }

    /// One realtime chunk (16 kHz float32). Non-blocking w.r.t.
    /// decoding: the server answers whenever it has partials.
    pub fn send_chunk(&mut self, samples: &[f32]) -> Result<(), String> {
        let ws = self.ws.as_mut().ok_or("online session closed")?;
        ws_send_binary(ws, &f32le(samples))?;
        // Drain pending partials without blocking: anything the
        // server already emitted updates the live text.
        loop {
            match ws.read() {
                Ok(tungstenite::Message::Text(t)) => {
                    let s: &str = &t;
                    if s == "Done!" {
                        break;
                    }
                    match sherpa_text(s) {
                        Ok(text) => {
                            if text != self.last_text {
                                self.last_text = text.clone();
                                eprintln!("[stt] partial: {text}");
                            }
                        }
                        Err(e) => eprintln!("[stt] {e}"),
                    }
                    continue;
                }
                Ok(_) => continue,
                Err(tungstenite::Error::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    break;
                }
                Err(_) => break,
            }
        }
        Ok(())
    }

    /// End the utterance, collect the final text. The engine may lag
    /// behind realtime; this blocks (worker thread) until it lands.
    pub fn finish(mut self) -> Result<String, String> {
        let mut ws = self.ws.take().ok_or("online session closed")?;
        ws.send(tungstenite::Message::text("Done"))
            .map_err(|e| format!("ws done: {e}"))?;
        let mut last = std::mem::take(&mut self.last_text);
        loop {
            match ws.read().map_err(|e| format!("ws read: {e}"))? {
                tungstenite::Message::Text(t) => {
                    let s: &str = &t;
                    if s == "Done!" {
                        break;
                    }
                    if let Ok(text) = sherpa_text(s) {
                        if !text.is_empty() {
                            last = text;
                        }
                    }
                }
                tungstenite::Message::Close(_) => break,
                _ => {}
            }
        }
        let _ = ws.close(None);
        Ok(last.trim().to_string())
    }
}

/// Offline (whole-utterance) transcription over one WS connection.
pub fn sherpa_offline_transcribe(samples_16k: &[f32]) -> Result<String, String> {
    sherpa_offline_transcribe_inner(samples_16k, INFERENCE_TIMEOUT)
}

fn sherpa_offline_transcribe_inner(
    samples_16k: &[f32],
    read_timeout: Duration,
) -> Result<String, String> {
    let mut ws = ws_connect(SHERPA_OFFLINE_PORT, read_timeout)?;
    // [rate u32-LE][BYTE count u32-LE][float32…], ≤10 KiB per message.
    // The count is BYTES (samples*4): the server heap-sizes its buffer
    // from it and overflows on excess, so this must be exact.
    let mut buf = Vec::with_capacity(8 + samples_16k.len() * 4);
    buf.extend_from_slice(&ENGINE_RATE.to_le_bytes());
    buf.extend_from_slice(&((samples_16k.len() * 4) as u32).to_le_bytes());
    buf.extend_from_slice(&f32le(samples_16k));
    for chunk in buf.chunks(10_240) {
        ws_send_binary(&mut ws, chunk)?;
    }
    let reply = loop {
        match ws.read().map_err(|e| format!("ws read: {e}"))? {
            tungstenite::Message::Text(t) => break t.to_string(),
            tungstenite::Message::Close(_) => {
                return Err("sherpa offline closed without answer".to_string());
            }
            _ => {}
        }
    };
    let _ = ws.send(tungstenite::Message::text("Done"));
    let _ = ws.close(None);
    sherpa_text(&reply).map(|t| t.trim().to_string())
}

// ---------------------------------------------------------- whisper http

fn whisper_json_text(v: &serde_json::Value) -> String {
    if let Some(t) = v.get("text").and_then(|t| t.as_str()) {
        return t.trim().to_string();
    }
    // Fallback: concatenate per-segment texts.
    v.get("segments")
        .and_then(|s| s.as_array())
        .map(|segs| {
            segs.iter()
                .filter_map(|s| s.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

pub fn whisper_transcribe(wav: &[u8]) -> Result<String, String> {
    let boundary = "mywhisperwav";
    let body = multipart_body(
        boundary,
        &[("temperature", "0.0"), ("response_format", "json")],
        Some(("file", "audio.wav", "audio/wav", wav)),
    );
    let res = http_client()?
        .post(format!("http://127.0.0.1:{WHISPER_PORT}/inference"))
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .map_err(|e| format!("whisper /inference: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("whisper /inference: http {}", res.status()));
    }
    let text = res.text().map_err(|e| format!("whisper body: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("whisper json: {e}"))?;
    Ok(whisper_json_text(&v))
}

// ----------------------------------------------------------------- cloud

/// API language selector: "auto" (and empty) means omit and let
/// the provider detect; anything else is sent as-is (ISO codes).
fn lang_param(lang: &str) -> Option<&str> {
    if lang == "auto" || lang.is_empty() {
        None
    } else {
        Some(lang)
    }
}

fn openai_compat_transcribe(
    base: &str,
    key: &str,
    model: &str,
    lang: &str,
    wav: &[u8],
) -> Result<String, String> {
    let base = base.trim_end_matches('/');
    let url = if base.ends_with("transcriptions") {
        base.to_string()
    } else {
        format!("{base}/v1/audio/transcriptions")
    };
    // Some OpenAI-compatible servers live under /v1 already.
    let url = url.replace("/v1/v1/", "/v1/");
    let boundary = "mywhispercloud";
    // "auto" = omit: the API auto-detects when no language is sent.
    let mut fields = vec![("model", model)];
    if let Some(l) = lang_param(lang) {
        fields.push(("language", l));
    }
    let body = multipart_body(
        boundary,
        &fields,
        Some(("file", "audio.wav", "audio/wav", wav)),
    );
    let mut req = http_client()?
        .post(&url)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body);
    if !key.is_empty() {
        req = req.bearer_auth(key);
    }
    let res = req.send().map_err(|e| format!("cloud transcribe: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("cloud transcribe: http {}", res.status()));
    }
    let text = res.text().map_err(|e| format!("cloud body: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("cloud json: {e}"))?;
    v.get("text")
        .and_then(|t| t.as_str())
        .map(|t| t.trim().to_string())
        .ok_or_else(|| "cloud reply has no text".to_string())
}

fn deepgram_transcribe(
    base: &str,
    key: &str,
    model: &str,
    lang: &str,
    wav: &[u8],
) -> Result<String, String> {
    let mut url = format!(
        "{}/v1/listen?model={}&smart_format=true",
        base.trim_end_matches('/'),
        model
    );
    if let Some(l) = lang_param(lang) {
        url.push_str(&format!("&language={l}"));
    }
    let res = http_client()?
        .post(&url)
        .header("Authorization", format!("Token {key}"))
        .header("Content-Type", "audio/wav")
        .body(wav.to_vec())
        .send()
        .map_err(|e| format!("deepgram: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("deepgram: http {}", res.status()));
    }
    let text = res.text().map_err(|e| format!("deepgram body: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("deepgram json: {e}"))?;
    v.pointer("/results/channels/0/alternatives/0/transcript")
        .and_then(|t| t.as_str())
        .map(|t| t.trim().to_string())
        .ok_or_else(|| "deepgram reply has no transcript".to_string())
}

fn gemini_transcribe(key: &str, model: &str, lang: &str, wav: &[u8]) -> Result<String, String> {
    use base64::Engine as _;
    let url =
        format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent");
    let prompt = if let Some(l) = lang_param(lang) {
        format!("Transcribe this {l} audio exactly. Return only the transcription, no commentary.")
    } else {
        "Transcribe this audio exactly. Return only the transcription, no commentary.".to_string()
    };
    let body = serde_json::json!({
        "contents": [{
            "parts": [
                {"text": prompt},
                {"inline_data": {
                    "mime_type": "audio/wav",
                    "data": base64::engine::general_purpose::STANDARD.encode(wav),
                }},
            ],
        }],
        "generationConfig": {"temperature": 0.0},
    });
    let res = http_client()?
        .post(&url)
        .header("x-goog-api-key", key)
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(&body).map_err(|e| format!("gemini body: {e}"))?)
        .send()
        .map_err(|e| format!("gemini: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("gemini: http {}", res.status()));
    }
    let text = res.text().map_err(|e| format!("gemini body: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("gemini json: {e}"))?;
    let text = v
        .pointer("/candidates/0/content/parts")
        .and_then(|p| p.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    Ok(text.trim().to_string())
}

// ----------------------------------------------------------- orchestration

/// Live utterance: recorder tap + optional streaming pump.
struct LiveUtterance {
    recorder: Arc<Mutex<Recorder>>,
    stop: Arc<AtomicBool>,
    /// Streaming engines: pump thread exit + final text channel.
    result_rx: Option<mpsc::Receiver<Result<String, String>>>,
    plan: Plan,
    started: Instant,
    /// Worker-side engine warmup needs these after release.
    data_dir: PathBuf,
    backend: GpuBackendSetting,
    /// VAD snapshot at press: trim + silence-skip for file-based
    /// engines. Streaming pumps send everything (server-side
    /// endpointing owns that path).
    vad_enabled: bool,
    vad_gate: f32,
}

static LIVE: OnceLock<Mutex<Option<LiveUtterance>>> = OnceLock::new();
static BUSY: AtomicBool = AtomicBool::new(false);

fn live() -> &'static Mutex<Option<LiveUtterance>> {
    LIVE.get_or_init(|| Mutex::new(None))
}

/// Hotkey press: resolve the plan (fast file checks only) and arm
/// the RAM recorder SYNCHRONOUSLY, so the capture window always
/// brackets the hold. Server warmup happens in the background: the
/// release worker blocks on it before transcribing, so a cold first
/// press pastes late but never loses audio.
pub fn begin_utterance(stt: crate::settings::SttSettings, data_dir: &Path) {
    end_stale();
    let plan = match plan_for(&stt, data_dir) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[stt] press: cannot start: {e}");
            return;
        }
    };
    let data_dir = data_dir.to_path_buf();
    let backend = stt.gpu_backend;
    // Background warmup; the release worker re-ensures anyway.
    // Pool-submitted: a saturated system skips warmup instead of
    // dying (the worker will still try when the utterance ends).
    {
        let plan = plan.clone();
        let data_dir = data_dir.clone();
        if let Err(e) = crate::runtime::submit("stt-warmup", move || {
            if let Err(e) = ensure_servers(&plan, &data_dir, backend) {
                eprintln!("[stt] warmup: {e}");
            }
        }) {
            eprintln!("[stt] press: {e}");
        }
    }
    eprintln!("[stt] press: armed ({})", plan.describe());
    let recorder = Arc::new(Mutex::new(Recorder::default()));
    recorder.lock().unwrap().start(0); // rate learned from first tap
    audio::set_recorder(Some(recorder.clone()));
    let stop = Arc::new(AtomicBool::new(false));
    let result_rx = if plan.streaming() {
        match OnlineSession::open() {
            Ok(session) => {
                let (tx, rx) = mpsc::channel();
                let rec = recorder.clone();
                let stop_pump = stop.clone();
                if let Err(e) = crate::runtime::submit("stt-pump", move || {
                    pump_online(session, rec, stop_pump, tx);
                }) {
                    eprintln!("[stt] press: {e}");
                    audio::set_recorder(None);
                    return;
                }
                Some(rx)
            }
            Err(e) => {
                eprintln!("[stt] press: online session failed: {e}");
                audio::set_recorder(None);
                return;
            }
        }
    } else {
        None
    };
    *live().lock().unwrap() = Some(LiveUtterance {
        recorder,
        stop,
        result_rx,
        plan,
        started: Instant::now(),
        data_dir,
        backend,
        vad_enabled: stt.vad_enabled,
        vad_gate: stt.vad_threshold,
    });
}

/// Streaming pump body: realtime ~200 ms cadence while held, tail
/// drain + `Done`/final on stop. Runs on its own thread, reports
/// through `tx` exactly once.
fn pump_online(
    mut session: OnlineSession,
    rec: Arc<Mutex<Recorder>>,
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<Result<String, String>>,
) {
    let mut resampler: Option<(u32, Resampler)> = None;
    // Rate is learned from the tap; recreate the resampler if the
    // device changed mid-stream.
    let mut feed = |chunk: Vec<i16>| -> Result<(), String> {
        if chunk.is_empty() {
            return Ok(());
        }
        let rate = rec.lock().map(|r| r.rate()).unwrap_or(0);
        if resampler.as_ref().map(|(r, _)| *r) != Some(rate) {
            resampler = Some((rate, Resampler::new(rate.max(1))));
        }
        let f32c = resampler.as_mut().unwrap().1.process(&chunk);
        if f32c.is_empty() {
            return Ok(());
        }
        session.send_chunk(&f32c)
    };
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
        let chunk = rec.lock().map(|mut r| r.drain_new()).unwrap_or_default();
        if let Err(e) = feed(chunk) {
            let _ = tx.send(Err(e));
            return;
        }
    }
    let tail = rec.lock().map(|mut r| r.drain_new()).unwrap_or_default();
    if let Err(e) = feed(tail) {
        let _ = tx.send(Err(e));
        return;
    }
    let _ = tx.send(session.finish());
}

/// Drop an orphaned live session (new press while one is open).
fn end_stale() {
    if live().lock().unwrap().is_some() {
        eprintln!("[stt] previous utterance never ended; closing it");
        end_utterance_inner(true, None);
    }
}

/// Hotkey release: stop capture, transcribe in a worker (engines may
/// lag behind realtime), then paste + history. Never blocks the GTK
/// thread: returns immediately. `on_done` fires on the worker when the
/// pill may hide (success, empty, or failure alike).
pub fn end_utterance(on_done: impl FnOnce() + Send + 'static) {
    end_utterance_inner(false, Some(Box::new(on_done)));
}

fn end_utterance_inner(stale: bool, on_done: Option<Box<dyn FnOnce() + Send>>) {
    let sess = live().lock().unwrap().take();
    audio::set_recorder(None);
    let Some(sess) = sess else {
        if let Some(done) = on_done {
            done();
        }
        return;
    };
    sess.stop.store(true, Ordering::Relaxed);
    if stale {
        return; // replaced, not transcribed
    }
    if BUSY.swap(true, Ordering::SeqCst) {
        eprintln!("[stt] worker busy; dropping overlapping utterance");
        if let Some(done) = on_done {
            done();
        }
        return;
    }
    let on_done = Arc::new(Mutex::new(on_done));
    let od = on_done.clone();
    if let Err(e) = crate::runtime::submit("stt-worker", move || {
        let out = finish_session(sess);
        BUSY.store(false, Ordering::SeqCst);
        match out {
            Ok(Some((text, secs))) => deliver(text, secs),
            Ok(None) => {}
            Err(e) => eprintln!("[stt] transcription failed: {e}"),
        }
        if let Ok(mut g) = od.lock() {
            if let Some(done) = g.take() {
                done();
            }
        }
    }) {
        // Saturated: release the guard and hide the pill now; the
        // utterance is dropped, but the app survives to dictate again.
        BUSY.store(false, Ordering::SeqCst);
        eprintln!("[stt] release: {e}");
        if let Ok(mut g) = on_done.lock() {
            if let Some(done) = g.take() {
                done();
            }
        }
    }
}

/// Blocking finalization for one utterance. `Ok(None)` = nothing to
/// paste (too short / empty result).
fn finish_session(sess: LiveUtterance) -> Result<Option<(String, f64)>, String> {
    let secs = sess.started.elapsed().as_secs_f64();
    // Engine warmup finishes here (cold first press waits, later
    // ones sail through an already-running server).
    if let Err(e) = ensure_servers(&sess.plan, &sess.data_dir, sess.backend) {
        return Err(format!("release: engine not ready: {e}"));
    }
    eprintln!("[stt] release: engine ready, transcribing ({secs:.1}s window)");
    if let Some(rx) = sess.result_rx {
        // Streaming: the pump owns the tail; wait for the final.
        let text = rx.recv().map_err(|_| "online pump died".to_string())??;
        return Ok(empty_to_none(text).map(|t| (t, secs)));
    }
    let (pcm, rate) = sess.recorder.lock().unwrap().finish();
    if rate == 0 || pcm.is_empty() {
        eprintln!("[stt] release: no audio captured (mic unavailable or sim source)");
        return Ok(None);
    }
    eprintln!("[stt] release: {} samples @ {} Hz", pcm.len(), rate);
    let mut f32s = Resampler::resample_all(rate, &pcm);
    if sess.vad_enabled {
        match audio::apply_vad(&f32s, sess.vad_gate) {
            audio::VadResult::Silence => {
                eprintln!("[stt] release: no speech above VAD gate, skipping");
                return Ok(None);
            }
            audio::VadResult::Speech(trimmed) => {
                eprintln!(
                    "[stt] release: vad trimmed {:.2}s -> {:.2}s",
                    f32s.len() as f64 / 16_000.0,
                    trimmed.len() as f64 / 16_000.0
                );
                f32s = trimmed;
            }
        }
    }
    if f32s.len() < MIN_SAMPLES_16K {
        eprintln!("[stt] release: utterance too short, skipping");
        return Ok(None);
    }
    let t0 = Instant::now();
    let out = transcribe_blocking(&sess.plan, &f32s).map(|r| empty_to_none(r).map(|t| (t, secs)));
    eprintln!(
        "[stt] release: inference took {:.1}s",
        t0.elapsed().as_secs_f64()
    );
    out
}

fn empty_to_none(t: String) -> Option<String> {
    let t = t.trim().to_string();
    if t.is_empty() {
        eprintln!("[stt] empty transcription, nothing pasted");
        None
    } else {
        Some(t)
    }
}

fn transcribe_blocking(plan: &Plan, f32s: &[f32]) -> Result<String, String> {
    match plan {
        Plan::WhisperHttp { .. } => {
            let wav = wav_bytes_16k(&audio::f32_to_i16(f32s));
            whisper_transcribe(&wav)
        }
        Plan::SherpaOffline { .. } => sherpa_offline_transcribe(f32s),
        Plan::SherpaOnline { .. } => {
            // Release without a pump (shouldn't happen): one-shot.
            let mut s = OnlineSession::open()?;
            for chunk in f32s.chunks(8000) {
                s.send_chunk(chunk)?;
            }
            s.finish()
        }
        Plan::CloudOpenAiCompat {
            base,
            key,
            model,
            lang,
        } => {
            let wav = wav_bytes_16k(&audio::f32_to_i16(f32s));
            openai_compat_transcribe(base, key, model, lang, &wav)
        }
        Plan::CloudDeepgram {
            base,
            key,
            model,
            lang,
        } => {
            let wav = wav_bytes_16k(&audio::f32_to_i16(f32s));
            deepgram_transcribe(base, key, model, lang, &wav)
        }
        Plan::CloudGemini { key, model, lang } => {
            let wav = wav_bytes_16k(&audio::f32_to_i16(f32s));
            gemini_transcribe(key, model, lang, &wav)
        }
    }
}

/// Public warmup entry (service startup): bring this plan's server
/// up without an utterance attached.
pub fn ensure_for_plan(
    plan: &Plan,
    data_dir: &Path,
    gpu_backend: GpuBackendSetting,
) -> Result<(), String> {
    ensure_servers(plan, data_dir, gpu_backend)
}

fn ensure_servers(
    plan: &Plan,
    data_dir: &Path,
    gpu_backend: GpuBackendSetting,
) -> Result<(), String> {
    match plan {
        Plan::WhisperHttp { model_bin, lang } => {
            ensure_whisper(data_dir, gpu_backend, model_bin, lang)
        }
        Plan::SherpaOnline { model_dir } => ensure_sherpa_online(data_dir, model_dir),
        Plan::SherpaOffline { model_dir } => ensure_sherpa_offline(data_dir, model_dir),
        Plan::CloudOpenAiCompat { .. } | Plan::CloudDeepgram { .. } | Plan::CloudGemini { .. } => {
            Ok(())
        }
    }
}

/// Store in history (text only, never audio) and paste via the
/// configured clipboard pipeline.
fn deliver(text: String, secs: f64) {
    eprintln!("[stt] final: {text}");
    let paths = match crate::paths::AppPaths::create() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[stt] paths: {e}");
            return;
        }
    };
    let prefs = match crate::store::Store::open(&paths.db_file()) {
        Ok(store) => {
            let settings = store.load_settings();
            let words = text.split_whitespace().count() as u32;
            let node = crate::device::DeviceIdentity::load_or_create(&paths.device_key_file())
                .map(|i| i.node_id)
                .unwrap_or_else(|_| "unknown".to_string());
            if let Err(e) = store.add_history(&text, words, secs.round() as i64, &node) {
                eprintln!("[stt] history: {e}");
            }
            settings.preferences
        }
        Err(e) => {
            eprintln!("[stt] store: {e}");
            return;
        }
    };
    crate::clipboard::run_pipeline(&text, &prefs);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_shape() {
        let body = multipart_body(
            "b",
            &[("temperature", "0.0")],
            Some(("file", "audio.wav", "audio/wav", &[1, 2, 3])),
        );
        let s = String::from_utf8(body).unwrap();
        assert!(s.starts_with("--b\r\n"));
        assert!(s.contains("name=\"temperature\"\r\n\r\n0.0\r\n"));
        assert!(s.contains("filename=\"audio.wav\""));
        assert!(s.contains("Content-Type: audio/wav"));
        assert!(s.ends_with("--b--\r\n"));
    }

    #[test]
    fn sherpa_text_field() {
        assert_eq!(
            sherpa_text(r#"{"text":"hola","segment":2}"#).unwrap(),
            "hola"
        );
        assert!(sherpa_text(r#"{"nope":1}"#).is_err());
    }

    #[test]
    fn whisper_text_or_segments() {
        let v: serde_json::Value = serde_json::from_str(r#"{"text":" hi "}"#).unwrap();
        assert_eq!(whisper_json_text(&v), "hi");
        let v: serde_json::Value =
            serde_json::from_str(r#"{"segments":[{"text":"foo "},{"text":"bar"}]}"#).unwrap();
        assert_eq!(whisper_json_text(&v), "foo bar");
    }

    #[test]
    fn cloud_url_join() {
        // covered indirectly: openai_compat_transcribe builds the URL;
        // assert join logic via a scratch base with trailing /v1.
        let base = "http://localhost:8000/v1/";
        let url = format!("{}/v1/audio/transcriptions", base.trim_end_matches('/'))
            .replace("/v1/v1/", "/v1/");
        assert_eq!(url, "http://localhost:8000/v1/audio/transcriptions");
    }

    #[test]
    fn lang_param_auto_omits() {
        assert_eq!(lang_param("auto"), None);
        assert_eq!(lang_param(""), None);
        assert_eq!(lang_param("es"), Some("es"));
    }

    #[test]
    fn deepgram_pointer() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"results":{"channels":[{"alternatives":[{"transcript":"hey"}]}]}}"#,
        )
        .unwrap();
        assert_eq!(
            v.pointer("/results/channels/0/alternatives/0/transcript")
                .and_then(|t| t.as_str()),
            Some("hey")
        );
    }

    #[test]
    fn plan_cohere_unsupported() {
        let mut s = SttSettings::default();
        s.local_provider = LocalSttProvider::Cohere;
        let err = plan_for(&s, Path::new("/nonexistent")).unwrap_err();
        assert!(err.contains("Cohere"), "{err}");
    }

    #[test]
    fn plan_routing_needs_files() {
        // Whisper tiny not under /nonexistent → actionable missing error.
        let mut s = SttSettings::default();
        s.local_provider = LocalSttProvider::OpenAi;
        s.whisper_model = "tiny".to_string();
        let err = plan_for(&s, Path::new("/nonexistent")).unwrap_err();
        assert!(err.contains("whisper model tiny"), "{err}");
        // NVIDIA without model files → same treatment.
        s.local_provider = LocalSttProvider::Nvidia;
        s.parakeet_model = "parakeet-tdt-0.6b-v3".to_string();
        let err = plan_for(&s, Path::new("/nonexistent")).unwrap_err();
        assert!(err.contains("parakeet-tdt-0.6b-v3"), "{err}");
        // Unknown model id → unknown error, not a panic.
        s.whisper_model = "nope".to_string();
        s.local_provider = LocalSttProvider::OpenAi;
        assert!(plan_for(&s, Path::new("/nonexistent")).is_err());
    }

    #[test]
    fn plan_cloud_needs_key() {
        let mut s = SttSettings::default();
        s.mode = SttMode::Cloud;
        let err = plan_for(&s, Path::new("/nonexistent")).unwrap_err();
        assert!(err.contains("API key"), "{err}");
    }

    /// Live engine check (needs sherpa-cpu pack + orukeet on disk):
    /// spawn + probe + 1 s silence must return within timeout.
    /// Ignored by default; run explicitly when the machine has models.
    #[test]
    #[ignore]
    fn live_oruk_offline_roundtrip() {
        let home = std::env::var("HOME").unwrap();
        let data_dir = PathBuf::from(format!("{home}/.local/share/mywhisper"));
        let entry = stt::find_model("orukeet-v0.1.0").unwrap();
        let dir = stt::model_dir(&data_dir, entry);
        if !dir.join("encoder.int8.onnx").exists() {
            eprintln!("SKIP: orukeet not on disk");
            return;
        }
        ensure_sherpa_offline(&data_dir, &dir).expect("spawn+probe");
        let text = sherpa_offline_transcribe(&vec![0.0f32; 16_000]).expect("decode");
        assert!(text.trim().is_empty(), "silence gave {text:?}");
    }
}
