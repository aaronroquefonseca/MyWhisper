//! Settings "Test" probes for configured transcription and language
//! models. UI-free: the settings page maps [`Blocker`] and [`Outcome`]
//! to translated copy.
//!
//! Probes never transcribe or generate. They follow the same routing
//! dictation uses ([`crate::transcribe::entry_plan`] for STT, the
//! entry's kind/provider for LLMs), so Test can only reach the
//! destination a real dictation would:
//! - Local STT: the runtime's prerequisite check (model files and the
//!   selected engine binary), no inference.
//! - Self-hosted HTTP: `GET` the model list beside the transcription
//!   endpoint. Self-hosted `ws(s)`: a TCP connect (reachability only).
//! - OpenAI/Groq/Mistral: authenticated model list, exact model id.
//! - OpenRouter: authenticated `/key`, then the model list.
//! - Claude/Gemini/Google: authenticated `GET models/{model}`.
//! - Deepgram: authenticated project list (model not checked).
//! - Ollama: read-only `/api/tags`, exact model name. Never loads it.
//!
//! Plain HTTP is allowed wherever dictation allows it (local and LAN
//! servers); a probe URL whose origin differs from the configured one is
//! refused before any credential is attached; cross-origin redirects are
//! never followed; transport errors are reported by class, never by text
//! (it can carry URLs).

use std::net::TcpStream;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use reqwest::Url;
use reqwest::blocking::Client;

use crate::settings::{
    CloudLlm, LlmEntry, LlmKind, LocalSttProvider, SttChainEntry, SttEntryKind, SttSettings,
};
use crate::transcribe::Plan;

/// Whole-probe budget (DNS, connect, and reply).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a configuration cannot be tested yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocker {
    NeedUrl,
    BadUrl,
    NeedModel,
    NeedKey,
    /// A `ws(s)` server with Streaming off: dictation cannot use it.
    NeedsStreaming,
    /// Dictation rejects this provider.
    Unsupported,
    /// Managed local language models are not implemented.
    LocalUnavailable,
}

/// What a finished probe found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Local model and engine files are present.
    Installed,
    /// The provider resolved the configured model (with the key, if any).
    ModelAvailable,
    /// Key accepted; the provider exposes no model check.
    KeyAccepted,
    /// Self-hosted model list answered; the model is not checked.
    ServerAnswered,
    /// TCP connect only (streaming servers have no HTTP surface).
    Reachable,
    NotInstalled,
    ModelMissing,
    KeyRejected,
    Http(u16),
    /// A redirect to another origin (never followed).
    Redirected,
    Timeout,
    Unreachable,
    Network,
    BadReply,
    Blocked(Blocker),
}

impl Outcome {
    pub fn passed(self) -> bool {
        matches!(
            self,
            Outcome::Installed
                | Outcome::ModelAvailable
                | Outcome::KeyAccepted
                | Outcome::ServerAnswered
                | Outcome::Reachable
        )
    }
}

#[derive(Debug, Clone)]
pub struct TestResult {
    pub outcome: Outcome,
    pub latency_ms: u64,
    /// The model the runtime would use (defaults applied).
    pub model: String,
}

fn timed(model: String, probe: impl FnOnce() -> Outcome) -> TestResult {
    let start = Instant::now();
    let outcome = probe();
    TestResult {
        outcome,
        latency_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        model,
    }
}

/// Server URL shape accepted by the settings forms.
pub fn valid_server_url(value: &str, websocket: bool) -> bool {
    Url::parse(value.trim()).is_ok_and(|url| {
        url.host_str().is_some()
            && (matches!(url.scheme(), "http" | "https")
                || (websocket && matches!(url.scheme(), "ws" | "wss")))
    })
}

/// Credential binding: a key belongs to this destination (vendor, or
/// server origin plus service path). The forms drop the key when the
/// destination changes; see [`key_stays`].
pub fn stt_destination(entry: &SttChainEntry) -> String {
    match entry.kind {
        SttEntryKind::Local => "local".to_string(),
        SttEntryKind::Cloud => format!("cloud:{}", entry.provider),
        SttEntryKind::SelfHosted => format!("server:{}", service(&entry.url)),
    }
}

pub fn llm_destination(entry: &LlmEntry) -> String {
    match entry.kind {
        LlmKind::Local => "local".to_string(),
        LlmKind::Cloud => format!("cloud:{}", entry.provider.name()),
        LlmKind::Ollama => format!("ollama:{}", service(&entry.url)),
    }
}

/// Origin, path (trailing `/` ignored) and query: `/first` vs
/// `/second`, or `?tenant=a` vs `?tenant=b`, are different services.
fn service(url: &str) -> String {
    match Url::parse(url.trim()) {
        Ok(u) => {
            let path = if matches!(u.scheme(), "ws" | "wss") {
                u.path()
            } else {
                u.path().trim_end_matches('/')
            };
            format!(
                "{}{path}{}",
                u.origin().ascii_serialization(),
                u.query().map(|q| format!("?{q}")).unwrap_or_default()
            )
        }
        Err(_) => url.trim().to_string(),
    }
}

/// Form-side binding of the typed or prefilled key to `dest`, run on
/// every field change. A key binds to the destination present when it
/// is entered; `false` = the destination moved since, so the caller
/// must clear the key fields (and `bound` is reset).
pub fn key_stays(bound: &mut Option<String>, dest: String, has_key: bool) -> bool {
    if !has_key {
        *bound = None;
        return true;
    }
    match bound {
        Some(d) if *d != dest => {
            *bound = None;
            false
        }
        Some(_) => true,
        None => {
            *bound = Some(dest);
            true
        }
    }
}

/// Why this STT entry cannot be tested, or `None` when it can.
pub fn stt_blocker(entry: &SttChainEntry) -> Option<Blocker> {
    match entry.kind {
        SttEntryKind::Local => None,
        SttEntryKind::SelfHosted => {
            let url = entry.url.trim();
            if url.is_empty() {
                Some(Blocker::NeedUrl)
            } else if !valid_server_url(url, true) {
                Some(Blocker::BadUrl)
            } else if crate::transcribe::is_ws_url(url) {
                // Settings checks TCP reachability for these servers.
                (!entry.streaming).then_some(Blocker::NeedsStreaming)
            } else {
                None
            }
        }
        SttEntryKind::Cloud => {
            if entry.api_key.trim().is_empty() {
                Some(Blocker::NeedKey)
            } else if crate::transcribe::entry_plan(&SttSettings::default(), entry, Path::new(""))
                .is_err()
            {
                Some(Blocker::Unsupported)
            } else {
                None
            }
        }
    }
}

/// Why this LLM entry cannot be tested, or `None` when it can.
pub fn llm_blocker(entry: &LlmEntry) -> Option<Blocker> {
    match entry.kind {
        LlmKind::Local => Some(Blocker::LocalUnavailable),
        LlmKind::Ollama => {
            let url = entry.url.trim();
            if url.is_empty() {
                Some(Blocker::NeedUrl)
            } else if !valid_server_url(url, false) {
                Some(Blocker::BadUrl)
            } else if entry.model.trim().is_empty() {
                Some(Blocker::NeedModel)
            } else {
                None
            }
        }
        LlmKind::Cloud => {
            if entry.model.trim().is_empty() {
                Some(Blocker::NeedModel)
            } else if entry.api_key.trim().is_empty() {
                Some(Blocker::NeedKey)
            } else {
                None
            }
        }
    }
}

/// What the Local row's result depends on: a change to any of these
/// invalidates a shown or running Local test.
pub fn local_config(
    s: &SttSettings,
) -> (
    LocalSttProvider,
    String,
    crate::settings::GpuBackendSetting,
    crate::settings::SherpaBackendSetting,
    Option<(
        crate::settings::QwenBackendSetting,
        crate::settings::QwenVramSetting,
    )>,
) {
    (
        s.local_provider,
        local_model_id(s),
        s.gpu_backend,
        s.sherpa_backend,
        (s.local_provider == LocalSttProvider::Qwen).then_some((s.qwen_backend, s.qwen_vram)),
    )
}

/// The local model id the Local chain entry would run.
pub fn local_model_id(s: &SttSettings) -> String {
    match s.local_provider {
        LocalSttProvider::Nvidia => s.parakeet_model.clone(),
        LocalSttProvider::OpenAi => s.whisper_model.clone(),
        LocalSttProvider::Oruk => "orukeet-v0.1.0".to_string(),
        LocalSttProvider::Cohere => "cohere-transcribe-03-2026".to_string(),
        LocalSttProvider::Whistle => "whistle-v1".to_string(),
        LocalSttProvider::Bark => s.bark_model.clone(),
        LocalSttProvider::Qwen => s.qwen_model.clone(),
    }
}

/// Test one STT chain entry through dictation's routing.
pub fn probe_stt(entry: &SttChainEntry, s: &SttSettings, data_dir: &Path) -> TestResult {
    let fallback_model = match entry.kind {
        SttEntryKind::Local => local_model_id(s),
        _ => entry.model.trim().to_string(),
    };
    if let Some(blocker) = stt_blocker(entry) {
        return timed(fallback_model, || Outcome::Blocked(blocker));
    }
    let plan = match crate::transcribe::entry_plan(s, entry, data_dir) {
        Ok(plan) => plan,
        Err(_) if entry.kind == SttEntryKind::Local => {
            return timed(fallback_model, || Outcome::NotInstalled);
        }
        Err(_) => return timed(fallback_model, || Outcome::Blocked(Blocker::Unsupported)),
    };
    let strict = entry.kind == SttEntryKind::Cloud;
    match plan {
        Plan::WhisperHttp { .. }
        | Plan::SherpaOnline { .. }
        | Plan::SherpaOffline { .. }
        | Plan::QwenHttp { .. }
        | Plan::QwenStream { .. }
        | Plan::WhistleCli { .. } => {
            timed(
                fallback_model,
                || match crate::transcribe::plan_runtime_ready(&plan, s, data_dir) {
                    Ok(()) => Outcome::Installed,
                    Err(_) => Outcome::NotInstalled,
                },
            )
        }
        Plan::SelfHostedWs { url, .. } => timed(fallback_model, || match Url::parse(&url) {
            Ok(url) => tcp_reachable(&url, PROBE_TIMEOUT),
            Err(_) => Outcome::Blocked(Blocker::BadUrl),
        }),
        Plan::CloudOpenAiCompat {
            base, key, model, ..
        } => {
            let shown = model.clone();
            timed(
                shown,
                || match crate::transcribe::openai_transcriptions_url(&base) {
                    Ok(endpoint) => {
                        openai_models(models_url(&endpoint).as_str(), &base, &key, &model, strict)
                    }
                    Err(_) => Outcome::Blocked(Blocker::BadUrl),
                },
            )
        }
        Plan::CloudDeepgram {
            base, key, model, ..
        } => timed(model, || {
            let url = format!("{}/v1/projects", base.trim_end_matches('/'));
            let auth = Auth::Header("Authorization", &format!("Token {key}"));
            match fetch(&url, &base, auth) {
                Ok((code, _)) => status_outcome(code).err().unwrap_or(Outcome::KeyAccepted),
                Err(outcome) => outcome,
            }
        }),
        Plan::CloudGemini { key, model, .. } => {
            let shown = model.clone();
            timed(shown, || {
                model_resource(
                    CloudLlm::Google.base_url(),
                    &model,
                    Auth::Header("x-goog-api-key", &key),
                )
            })
        }
    }
}

/// Test one LLM entry against its provider.
pub fn probe_llm(entry: &LlmEntry) -> TestResult {
    probe_llm_at(entry, entry.provider.base_url())
}

fn probe_llm_at(entry: &LlmEntry, cloud_base: &str) -> TestResult {
    let model = entry.model.trim().to_string();
    if let Some(blocker) = llm_blocker(entry) {
        return timed(model, || Outcome::Blocked(blocker));
    }
    let key = entry.api_key.trim();
    let base = cloud_base.trim_end_matches('/');
    timed(model.clone(), || match (entry.kind, entry.provider) {
        (LlmKind::Local, _) => Outcome::Blocked(Blocker::LocalUnavailable),
        (LlmKind::Ollama, _) => probe_ollama(&entry.url, &model, key),
        (LlmKind::Cloud, CloudLlm::Claude) => model_resource(base, &model, Auth::Anthropic(key)),
        (LlmKind::Cloud, CloudLlm::Google) => {
            model_resource(base, &model, Auth::Header("x-goog-api-key", key))
        }
        (LlmKind::Cloud, CloudLlm::OpenRouter) => {
            // The model list is public: only /key proves the key works.
            match fetch(&format!("{base}/key"), base, Auth::Bearer(key)) {
                Ok((code, _)) => match status_outcome(code) {
                    Ok(()) => openai_models(
                        &format!("{base}/models"),
                        base,
                        "",
                        openrouter_catalog_id(&model),
                        true,
                    ),
                    Err(outcome) => outcome,
                },
                Err(outcome) => outcome,
            }
        }
        (LlmKind::Cloud, CloudLlm::OpenAI | CloudLlm::Groq) => {
            openai_models(&format!("{base}/models"), base, key, &model, true)
        }
    })
}

/// Model list beside an OpenAI-style transcription endpoint
/// (`.../audio/transcriptions` → `.../models`; a custom endpoint name
/// → its sibling `models`). Path segments only: host and query stay.
fn models_url(transcriptions: &Url) -> Url {
    let audio = transcriptions
        .path_segments()
        .and_then(|mut s| s.nth_back(1))
        == Some("audio");
    let mut url = transcriptions.clone();
    if let Ok(mut path) = url.path_segments_mut() {
        path.pop();
        if audio {
            path.pop();
        }
        path.push("models");
    }
    url
}

/// Probe credentials; an empty key sends no auth header.
enum Auth<'a> {
    Bearer(&'a str),
    Header(&'static str, &'a str),
    Anthropic(&'a str),
}

/// Same-origin redirects only: a credential never follows a hop to
/// another scheme, host, or port.
fn client() -> Option<Client> {
    Client::builder()
        .timeout(PROBE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let same_origin = attempt
                .previous()
                .first()
                .is_some_and(|first| first.origin() == attempt.url().origin());
            if same_origin && attempt.previous().len() < 5 {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .ok()
}

/// GET with policy checks. `configured`: the URL the user (or the
/// provider constant) set; the probe must stay on its origin before any
/// credential is attached. `Err` carries a final outcome.
fn fetch(url: &str, configured: &str, auth: Auth) -> Result<(u16, String), Outcome> {
    let parsed = Url::parse(url).map_err(|_| Outcome::Blocked(Blocker::BadUrl))?;
    let same_origin = Url::parse(configured.trim()).is_ok_and(|c| c.origin() == parsed.origin());
    if !same_origin || !matches!(parsed.scheme(), "http" | "https") {
        return Err(Outcome::Blocked(Blocker::BadUrl));
    }
    let client = client().ok_or(Outcome::Network)?;
    let mut req = client.get(parsed).header("Accept", "application/json");
    req = match auth {
        Auth::Bearer(k) if k.trim().is_empty() => req,
        Auth::Bearer(k) => req.bearer_auth(k.trim()),
        Auth::Header(name, k) => req.header(name, k.trim()),
        Auth::Anthropic(k) => req
            .header("x-api-key", k.trim())
            .header("anthropic-version", "2023-06-01"),
    };
    let res = req.send().map_err(|e| transport_outcome(&e))?;
    let code = res.status().as_u16();
    let body = res.text().unwrap_or_default();
    Ok((code, body))
}

fn transport_outcome(e: &reqwest::Error) -> Outcome {
    if e.is_timeout() {
        Outcome::Timeout
    } else if e.is_redirect() {
        Outcome::Redirected
    } else if e.is_connect() {
        Outcome::Unreachable
    } else {
        Outcome::Network
    }
}

fn status_outcome(code: u16) -> Result<(), Outcome> {
    match code {
        200..=299 => Ok(()),
        300..=399 => Err(Outcome::Redirected),
        401 | 403 => Err(Outcome::KeyRejected),
        404 => Err(Outcome::ModelMissing),
        _ => Err(Outcome::Http(code)),
    }
}

/// `GET {base}/models/{model}`: 200 = the key works and the model exists.
fn model_resource(base: &str, model: &str, auth: Auth) -> Outcome {
    let Ok(mut url) = Url::parse(&format!("{}/models", base.trim_end_matches('/'))) else {
        return Outcome::Blocked(Blocker::BadUrl);
    };
    if let Ok(mut path) = url.path_segments_mut() {
        path.push(model);
    }
    match fetch(url.as_str(), base, auth) {
        Ok((code, _)) => status_outcome(code)
            .err()
            .unwrap_or(Outcome::ModelAvailable),
        Err(outcome) => outcome,
    }
}

/// OpenAI-style `{"data":[{"id":...}]}` list. `strict`: the model must
/// be listed (cloud); otherwise an answering server passes.
fn openai_models(url: &str, configured: &str, key: &str, model: &str, strict: bool) -> Outcome {
    let (code, body) = match fetch(url, configured, Auth::Bearer(key)) {
        Ok(r) => r,
        Err(outcome) => return outcome,
    };
    if let Err(outcome) = status_outcome(code) {
        // A missing list endpoint is not a missing model.
        return if outcome == Outcome::ModelMissing {
            Outcome::Http(code)
        } else {
            outcome
        };
    }
    match listed_ids(&body) {
        Some(ids) if ids.iter().any(|id| id == model) => Outcome::ModelAvailable,
        Some(_) if strict => Outcome::ModelMissing,
        None if strict => Outcome::BadReply,
        _ => Outcome::ServerAnswered,
    }
}

fn listed_ids(body: &str) -> Option<Vec<String>> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    Some(
        v.get("data")?
            .as_array()?
            .iter()
            .filter_map(|m| m.get("id")?.as_str().map(str::to_string))
            .collect(),
    )
}

/// OpenRouter routing variants: accepted on any model id and never
/// listed in `/models`. Static variants (`:free`, `:batch`, ...) are
/// listed ids and must match exactly.
/// <https://openrouter.ai/docs/guides/routing/model-variants>
const OPENROUTER_DYNAMIC_VARIANTS: [&str; 4] = ["nitro", "floor", "exacto", "online"];

/// The id OpenRouter's catalog lists for `model`.
fn openrouter_catalog_id(model: &str) -> &str {
    match model.rsplit_once(':') {
        Some((base, variant)) if OPENROUTER_DYNAMIC_VARIANTS.contains(&variant) => base,
        _ => model,
    }
}

/// Read-only `/api/tags` check; never loads the model.
fn probe_ollama(url: &str, model: &str, key: &str) -> Outcome {
    let tags = format!("{}/api/tags", url.trim().trim_end_matches('/'));
    let (code, body) = match fetch(&tags, url, Auth::Bearer(key)) {
        Ok(r) => r,
        Err(outcome) => return outcome,
    };
    if let Err(outcome) = status_outcome(code) {
        return if outcome == Outcome::ModelMissing {
            Outcome::Http(code)
        } else {
            outcome
        };
    }
    match ollama_has_model(&body, model) {
        Some(true) => Outcome::ModelAvailable,
        Some(false) => Outcome::ModelMissing,
        None => Outcome::BadReply,
    }
}

/// Exact Ollama name match; an omitted tag means `:latest`. `None`
/// when the body is not a tags list.
pub fn ollama_has_model(body: &str, model: &str) -> Option<bool> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let want = ollama_name(model);
    Some(v.get("models")?.as_array()?.iter().any(|m| {
        ["name", "model"].iter().any(|field| {
            m.get(field)
                .and_then(|n| n.as_str())
                .is_some_and(|n| ollama_name(n).eq_ignore_ascii_case(&want))
        })
    }))
}

fn ollama_name(name: &str) -> String {
    let name = name.trim();
    // The tag follows the last path segment (`host:5000/ns/model:tag`).
    let last = name.rsplit('/').next().unwrap_or(name);
    if last.contains(':') {
        name.to_string()
    } else {
        format!("{name}:latest")
    }
}

/// TCP connect to every resolved address within one deadline
/// (resolution included). Proves reachability, not the protocol.
fn tcp_reachable(url: &Url, budget: Duration) -> Outcome {
    let deadline = Instant::now() + budget;
    let (tx, rx) = mpsc::channel();
    let target = url.clone();
    // `socket_addrs` blocks without a timeout: resolve on a side thread.
    std::thread::spawn(move || {
        let _ = tx.send(target.socket_addrs(|| None));
    });
    let addrs = match rx.recv_timeout(budget) {
        Ok(Ok(addrs)) if !addrs.is_empty() => addrs,
        Ok(_) => return Outcome::Unreachable,
        Err(_) => return Outcome::Timeout,
    };
    let count = addrs.len();
    for (i, addr) in addrs.iter().enumerate() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Outcome::Timeout;
        }
        // Share what is left so one dead address cannot eat the budget.
        let slice = left / u32::try_from(count - i).unwrap_or(1);
        if TcpStream::connect_timeout(addr, slice).is_ok() {
            return Outcome::Reachable;
        }
    }
    Outcome::Unreachable
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// Loopback HTTP fixture: answers `conns` requests with `respond`
    /// and records each request head.
    struct Fixture {
        base: String,
        seen: Arc<Mutex<Vec<String>>>,
    }

    fn serve(conns: usize, respond: impl Fn(&str) -> String + Send + 'static) -> Fixture {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind");
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(conns) {
                let Ok(mut stream) = stream else { break };
                stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                let mut buf = Vec::new();
                let mut tmp = [0u8; 2048];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut tmp) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let reply = respond(&head);
                log.lock().unwrap().push(head);
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        Fixture {
            base: format!("http://127.0.0.1:{port}"),
            seen,
        }
    }

    fn json(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn stt(kind: SttEntryKind, provider: &str, url: &str, key: &str) -> SttChainEntry {
        SttChainEntry {
            id: "t".into(),
            name: "t".into(),
            kind,
            enabled: true,
            provider: provider.into(),
            url: url.into(),
            model: String::new(),
            api_key: key.into(),
            streaming: true,
        }
    }

    fn llm(kind: LlmKind, provider: CloudLlm, url: &str, model: &str, key: &str) -> LlmEntry {
        LlmEntry {
            kind,
            provider,
            url: url.into(),
            model: model.into(),
            api_key: key.into(),
            ..LlmEntry::ollama("", "")
        }
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mywhisper-provider-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn unknown_stt_vendor_is_rejected_like_dictation() {
        let entry = stt(SttEntryKind::Cloud, "Acme", "", "sk-test");
        assert_eq!(stt_blocker(&entry), Some(Blocker::Unsupported));
        let r = probe_stt(&entry, &SttSettings::default(), Path::new(""));
        assert_eq!(r.outcome, Outcome::Blocked(Blocker::Unsupported));
        assert!(stt_blocker(&stt(SttEntryKind::Cloud, "Groq", "", "k")).is_none());
        assert_eq!(
            stt_blocker(&stt(SttEntryKind::Cloud, "Groq", "", " ")),
            Some(Blocker::NeedKey)
        );
    }

    #[test]
    fn credential_destination_changes_with_vendor_or_server() {
        let a = stt(SttEntryKind::Cloud, "OpenAI", "", "k");
        let b = stt(SttEntryKind::Cloud, "Groq", "", "k");
        assert_ne!(stt_destination(&a), stt_destination(&b));
        let s1 = stt(SttEntryKind::SelfHosted, "", "https://h:8000/v1", "k");
        let s2 = stt(SttEntryKind::SelfHosted, "", "https://h:8000/other", "k");
        let s3 = stt(SttEntryKind::SelfHosted, "", "https://evil:8000/v1", "k");
        let s4 = stt(SttEntryKind::SelfHosted, "", "http://h:8000/v1", "k");
        let s5 = stt(SttEntryKind::SelfHosted, "", "https://h:8000/v1/", "k");
        // Same origin, other service path: the key does not follow.
        assert_ne!(stt_destination(&s1), stt_destination(&s2));
        assert_eq!(stt_destination(&s1), stt_destination(&s5));
        assert_ne!(stt_destination(&s1), stt_destination(&s3));
        assert_ne!(stt_destination(&s1), stt_destination(&s4));
        assert_ne!(stt_destination(&a), stt_destination(&s1));
        let ws = stt(SttEntryKind::SelfHosted, "", "wss://h:8000/v1", "k");
        let ws_slash = stt(SttEntryKind::SelfHosted, "", "wss://h:8000/v1/", "k");
        assert_ne!(stt_destination(&ws), stt_destination(&ws_slash));
        // Query-routed tenants are separate services too.
        let ta = stt(
            SttEntryKind::SelfHosted,
            "",
            "https://h:8000/v1?tenant=a",
            "k",
        );
        let tb = stt(
            SttEntryKind::SelfHosted,
            "",
            "https://h:8000/v1?tenant=b",
            "k",
        );
        assert_ne!(stt_destination(&ta), stt_destination(&tb));
        let o = llm(
            LlmKind::Ollama,
            CloudLlm::OpenAI,
            "http://127.0.0.1:11434",
            "m",
            "",
        );
        let c = llm(
            LlmKind::Cloud,
            CloudLlm::OpenAI,
            "http://127.0.0.1:11434",
            "m",
            "",
        );
        let r = llm(LlmKind::Cloud, CloudLlm::OpenRouter, "", "m", "");
        assert_ne!(llm_destination(&o), llm_destination(&c));
        assert_ne!(llm_destination(&c), llm_destination(&r));
        let first = llm(LlmKind::Ollama, CloudLlm::OpenAI, "http://h/first", "m", "");
        let second = llm(
            LlmKind::Ollama,
            CloudLlm::OpenAI,
            "http://h/second",
            "m",
            "",
        );
        assert_ne!(llm_destination(&first), llm_destination(&second));
    }

    #[test]
    fn fresh_draft_key_binds_to_destination_at_entry() {
        // Draft form: no saved binding; a key typed for A must not
        // reach B after the URL changes.
        let a = stt_destination(&stt(SttEntryKind::SelfHosted, "", "http://a:8000", "k"));
        let b = stt_destination(&stt(SttEntryKind::SelfHosted, "", "http://b:8000", "k"));
        let mut bound = None;
        assert!(key_stays(&mut bound, a.clone(), false));
        assert_eq!(bound, None);
        assert!(key_stays(&mut bound, a.clone(), true));
        assert_eq!(bound.as_deref(), Some(a.as_str()));
        assert!(key_stays(&mut bound, a.clone(), true));
        assert!(
            !key_stays(&mut bound, b.clone(), true),
            "key must be cleared"
        );
        assert_eq!(bound, None);
        // Cleared fields: nothing bound; a new key binds to B.
        assert!(key_stays(&mut bound, b.clone(), false));
        assert!(key_stays(&mut bound, b.clone(), true));
        assert_eq!(bound.as_deref(), Some(b.as_str()));
    }

    #[test]
    fn endpoint_urls_never_change_host_or_absorb_paths_into_query() {
        let t = |base: &str| {
            crate::transcribe::openai_transcriptions_url(base)
                .unwrap()
                .to_string()
        };
        // A host ending in "transcriptions" is a host, not an endpoint.
        assert_eq!(
            t("http://speech-transcriptions"),
            "http://speech-transcriptions/v1/audio/transcriptions"
        );
        assert_eq!(
            t("http://h:8000/v1?tenant=a"),
            "http://h:8000/v1/audio/transcriptions?tenant=a"
        );
        assert_eq!(
            t("https://api.groq.com/openai/v1/audio/transcriptions/"),
            "https://api.groq.com/openai/v1/audio/transcriptions"
        );
        // An existing prefix keeps its encoding and inner slashes.
        assert_eq!(
            t("http://h/team%20alpha/v1"),
            "http://h/team%20alpha/v1/audio/transcriptions"
        );
        assert_eq!(t("http://h//a/v1"), "http://h//a/v1/audio/transcriptions");
        assert_eq!(t("http://h:8000"), "http://h:8000/v1/audio/transcriptions");
        let m = |base: &str| {
            models_url(&crate::transcribe::openai_transcriptions_url(base).unwrap()).to_string()
        };
        assert_eq!(
            m("http://speech-transcriptions"),
            "http://speech-transcriptions/v1/models"
        );
        assert_eq!(
            m("http://h:8000/v1?tenant=a"),
            "http://h:8000/v1/models?tenant=a"
        );
        assert_eq!(
            m("http://h:8000/custom/transcriptions"),
            "http://h:8000/custom/models"
        );
        // End to end: the key reaches the configured server only, on
        // the models path, with the query kept.
        let server = serve(1, |_| json("200 OK", r#"{"data":[]}"#));
        let entry = stt(
            SttEntryKind::SelfHosted,
            "",
            &format!("{}/v1?tenant=a", server.base),
            "dummy-key",
        );
        let r = probe_stt(&entry, &SttSettings::default(), Path::new(""));
        assert_eq!(r.outcome, Outcome::ServerAnswered);
        let heads = server.seen.lock().unwrap();
        assert!(
            heads[0].starts_with("GET /v1/models?tenant=a "),
            "{}",
            heads[0]
        );
    }

    #[test]
    fn probe_refuses_other_origin_before_sending_key() {
        let other = serve(1, |_| json("200 OK", "{}"));
        let url = format!("{}/v1/models", other.base);
        let configured = "http://127.0.0.1:1/v1";
        assert_eq!(
            fetch(&url, configured, Auth::Bearer("dummy-key")).err(),
            Some(Outcome::Blocked(Blocker::BadUrl))
        );
        assert!(other.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn cross_origin_redirect_is_not_followed() {
        let target = serve(1, |_| json("200 OK", r#"{"data":[]}"#));
        let location = format!("{}/v1/models", target.base);
        let origin = serve(1, move |_| {
            format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
        });
        let entry = SttChainEntry {
            api_key: "secret-key".into(),
            ..stt(SttEntryKind::SelfHosted, "", &origin.base, "")
        };
        let r = probe_stt(&entry, &SttSettings::default(), Path::new(""));
        assert_eq!(r.outcome, Outcome::Redirected);
        assert!(!r.outcome.passed());
        let heads = origin.seen.lock().unwrap();
        assert!(heads[0].starts_with("GET /v1/models "), "{}", heads[0]);
        assert!(heads[0].contains("secret-key"));
        assert!(target.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn self_hosted_models_url_follows_runtime_path() {
        let models = |base: &str| {
            models_url(&crate::transcribe::openai_transcriptions_url(base).unwrap()).to_string()
        };
        assert_eq!(models("http://h:8000"), "http://h:8000/v1/models");
        assert_eq!(models("http://h:8000/v1"), "http://h:8000/v1/models");
        assert_eq!(
            models("http://h:8000/v1/audio/transcriptions"),
            "http://h:8000/v1/models"
        );
        assert_eq!(
            models("https://api.openai.com/v1/"),
            "https://api.openai.com/v1/models"
        );
    }

    #[test]
    fn self_hosted_blank_model_is_testable_with_runtime_default() {
        let server = serve(1, |_| json("200 OK", r#"{"data":[{"id":"x"}]}"#));
        let entry = stt(SttEntryKind::SelfHosted, "", &server.base, "");
        assert_eq!(stt_blocker(&entry), None);
        let r = probe_stt(&entry, &SttSettings::default(), Path::new(""));
        assert_eq!(r.outcome, Outcome::ServerAnswered);
        assert_eq!(r.model, "default");
        assert!(r.outcome.passed());
    }

    #[test]
    fn self_hosted_url_validation() {
        for bad in ["http://", "ftp://h", "notaurl"] {
            let e = stt(SttEntryKind::SelfHosted, "", bad, "");
            assert_eq!(stt_blocker(&e), Some(Blocker::BadUrl), "{bad}");
        }
        let ws_off = SttChainEntry {
            streaming: false,
            ..stt(SttEntryKind::SelfHosted, "", "ws://127.0.0.1:6006", "")
        };
        assert_eq!(stt_blocker(&ws_off), Some(Blocker::NeedsStreaming));
        let o = llm(LlmKind::Ollama, CloudLlm::OpenAI, "http://", "m", "");
        assert_eq!(llm_blocker(&o), Some(Blocker::BadUrl));
    }

    #[test]
    fn ws_probe_tries_ipv6_loopback_and_labels_reachability() {
        let Ok(listener) = TcpListener::bind("[::1]:0") else {
            return; // host without IPv6 loopback
        };
        let port = listener.local_addr().unwrap().port();
        let entry = stt(
            SttEntryKind::SelfHosted,
            "",
            &format!("ws://[::1]:{port}"),
            "",
        );
        let r = probe_stt(&entry, &SttSettings::default(), Path::new(""));
        assert_eq!(r.outcome, Outcome::Reachable);
        drop(listener);
    }

    #[test]
    fn ollama_names_match_exactly_with_latest_default() {
        let body = r#"{"models":[{"name":"llama3.2:latest","model":"llama3.2:latest"},{"name":"gemma4:e4b"}]}"#;
        assert_eq!(ollama_has_model(body, "llama3"), Some(false));
        assert_eq!(ollama_has_model(body, "llama3.2"), Some(true));
        assert_eq!(ollama_has_model(body, "llama3.2:latest"), Some(true));
        assert_eq!(ollama_has_model(body, "gemma4"), Some(false));
        assert_eq!(ollama_has_model(body, "gemma4:e4b"), Some(true));
        assert_eq!(ollama_has_model("not json", "x"), None);
        let reg = r#"{"models":[{"name":"reg.local:5000/ns/tiny:latest"}]}"#;
        assert_eq!(ollama_has_model(reg, "reg.local:5000/ns/tiny"), Some(true));
    }

    #[test]
    fn ollama_probe_reports_missing_model() {
        let server = serve(1, |_| {
            json("200 OK", r#"{"models":[{"name":"llama3.2:latest"}]}"#)
        });
        let entry = llm(
            LlmKind::Ollama,
            CloudLlm::OpenAI,
            &server.base,
            "llama3",
            "",
        );
        assert_eq!(probe_llm(&entry).outcome, Outcome::ModelMissing);
        assert!(server.seen.lock().unwrap()[0].starts_with("GET /api/tags "));
    }

    #[test]
    fn openrouter_needs_authenticated_key_and_listed_model() {
        let server = serve(2, |head| {
            if head.starts_with("GET /api/v1/key ") {
                if head.contains("Bearer good") {
                    json("200 OK", r#"{"data":{"label":"x"}}"#)
                } else {
                    json("401 Unauthorized", "{}")
                }
            } else {
                json("200 OK", r#"{"data":[{"id":"moonshotai/kimi-k2"}]}"#)
            }
        });
        let base = format!("{}/api/v1", server.base);
        let entry =
            |model: &str, key: &str| llm(LlmKind::Cloud, CloudLlm::OpenRouter, "", model, key);
        assert_eq!(
            probe_llm_at(&entry("moonshotai/kimi-k2", "good"), &base).outcome,
            Outcome::ModelAvailable
        );
        let bad = serve(1, |_| json("401 Unauthorized", "{}"));
        assert_eq!(
            probe_llm_at(
                &entry("moonshotai/kimi-k2", "bad"),
                &format!("{}/api/v1", bad.base)
            )
            .outcome,
            Outcome::KeyRejected
        );
        let missing = serve(2, |head| {
            if head.starts_with("GET /api/v1/key ") {
                json("200 OK", "{}")
            } else {
                json("200 OK", r#"{"data":[{"id":"other/model"}]}"#)
            }
        });
        assert_eq!(
            probe_llm_at(
                &entry("moonshotai/kimi-k2", "good"),
                &format!("{}/api/v1", missing.base)
            )
            .outcome,
            Outcome::ModelMissing
        );
        let heads = server.seen.lock().unwrap();
        assert!(heads[0].starts_with("GET /api/v1/key "));
        // The public list never needs the key.
        assert!(!heads[1].contains("Bearer"));
    }

    #[test]
    fn whistle_needs_model_and_engine() {
        let data = scratch("whistle");
        let s = SttSettings {
            local_provider: LocalSttProvider::Whistle,
            ..SttSettings::default()
        };
        let local = stt(SttEntryKind::Local, "", "", "");
        let model_dir = crate::stt::model_dir(&data, crate::stt::find_model("whistle-v1").unwrap());
        let engine = crate::transcribe::needle_engine_path(&data);
        crate::stt::fake_install(&data, crate::stt::NEEDLE_ENGINE_ID);
        assert_eq!(probe_stt(&local, &s, &data).outcome, Outcome::NotInstalled);
        // An empty model file is not an install (no pinned size).
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("whistle.cact"), b"").unwrap();
        assert_eq!(probe_stt(&local, &s, &data).outcome, Outcome::NotInstalled);
        crate::stt::fake_install(&data, "whistle-v1");
        let r = probe_stt(&local, &s, &data);
        assert_eq!(r.outcome, Outcome::Installed);
        assert_eq!(r.model, "whistle-v1");
        // Same for an empty engine binary.
        std::fs::remove_dir_all(engine.parent().unwrap()).unwrap();
        std::fs::create_dir_all(engine.parent().unwrap()).unwrap();
        std::fs::write(&engine, b"").unwrap();
        assert_eq!(probe_stt(&local, &s, &data).outcome, Outcome::NotInstalled);
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn local_whisper_and_sherpa_need_runtime_pack() {
        let data = scratch("runtime");
        let local = stt(SttEntryKind::Local, "", "", "");
        let touch = |path: std::path::PathBuf| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"").unwrap();
        };
        // Whisper: model installed; whisper-cpu missing, then empty.
        let whisper = SttSettings {
            local_provider: LocalSttProvider::OpenAi,
            gpu_backend: crate::settings::GpuBackendSetting::Cpu,
            ..SttSettings::default()
        };
        crate::stt::fake_install(&data, &whisper.whisper_model);
        assert_eq!(
            probe_stt(&local, &whisper, &data).outcome,
            Outcome::NotInstalled
        );
        let pack = crate::stt::GPU_PACKS
            .iter()
            .find(|p| p.id == "whisper-cpu")
            .unwrap();
        touch(crate::stt::gpu_bin_dir(&data, pack).join("whisper-server"));
        assert_eq!(
            probe_stt(&local, &whisper, &data).outcome,
            Outcome::NotInstalled
        );
        crate::stt::fake_install(&data, "whisper-cpu");
        assert_eq!(
            probe_stt(&local, &whisper, &data).outcome,
            Outcome::Installed
        );
        // Sherpa (Oruk, offline): empty model files are not a model.
        let oruk = SttSettings {
            local_provider: LocalSttProvider::Oruk,
            sherpa_backend: crate::settings::SherpaBackendSetting::Cpu,
            ..SttSettings::default()
        };
        crate::stt::fake_install(&data, "sherpa-cpu");
        let dir = crate::stt::model_dir(&data, crate::stt::find_model("orukeet-v0.1.0").unwrap());
        for f in [
            "encoder.int8.onnx",
            "decoder.int8.onnx",
            "joiner.int8.onnx",
            "tokens.txt",
        ] {
            touch(dir.join(f));
        }
        assert_eq!(
            probe_stt(&local, &oruk, &data).outcome,
            Outcome::NotInstalled
        );
        crate::stt::fake_install(&data, "orukeet-v0.1.0");
        assert_eq!(probe_stt(&local, &oruk, &data).outcome, Outcome::Installed);
        // The server binary alone is not the sherpa pack.
        let pack = crate::stt::SHERPA_PACKS
            .iter()
            .find(|p| p.id == "sherpa-cpu")
            .unwrap();
        std::fs::remove_file(crate::stt::sherpa_bin_dir(&data, pack).join("lib/libonnxruntime.so"))
            .unwrap();
        assert_eq!(
            probe_stt(&local, &oruk, &data).outcome,
            Outcome::NotInstalled
        );
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn local_config_tracks_provider_model_and_runtime() {
        let base = SttSettings {
            local_provider: LocalSttProvider::OpenAi,
            ..SttSettings::default()
        };
        let cohere = SttSettings {
            local_provider: LocalSttProvider::Cohere,
            ..base.clone()
        };
        let model = SttSettings {
            whisper_model: "other".into(),
            ..base.clone()
        };
        let runtime = SttSettings {
            gpu_backend: crate::settings::GpuBackendSetting::Vulkan,
            ..base.clone()
        };
        let sherpa = SttSettings {
            sherpa_backend: crate::settings::SherpaBackendSetting::Cuda,
            ..base.clone()
        };
        for changed in [&cohere, &model, &runtime, &sherpa] {
            assert_ne!(local_config(&base), local_config(changed));
        }
        assert_eq!(local_config(&base), local_config(&base.clone()));
    }

    #[test]
    fn catalog_ids_match_exactly_except_openrouter_routing_variants() {
        assert_eq!(openrouter_catalog_id("a/b:nitro"), "a/b");
        assert_eq!(openrouter_catalog_id("a/b:floor"), "a/b");
        assert_eq!(openrouter_catalog_id("a/b:free"), "a/b:free");
        assert_eq!(openrouter_catalog_id("a/b:typo"), "a/b:typo");
        let openai = serve(1, |_| json("200 OK", r#"{"data":[{"id":"whisper-1"}]}"#));
        let typo = llm(LlmKind::Cloud, CloudLlm::OpenAI, "", "whisper-1:typo", "k");
        assert_eq!(
            probe_llm_at(&typo, &format!("{}/v1", openai.base)).outcome,
            Outcome::ModelMissing
        );
        let router = serve(4, |head| {
            if head.starts_with("GET /api/v1/key ") {
                json("200 OK", "{}")
            } else {
                json("200 OK", r#"{"data":[{"id":"a/b"}]}"#)
            }
        });
        let base = format!("{}/api/v1", router.base);
        let or = |model: &str| llm(LlmKind::Cloud, CloudLlm::OpenRouter, "", model, "k");
        assert_eq!(
            probe_llm_at(&or("a/b:free"), &base).outcome,
            Outcome::ModelMissing
        );
        assert_eq!(
            probe_llm_at(&or("a/b:nitro"), &base).outcome,
            Outcome::ModelAvailable
        );
    }

    #[test]
    fn plain_http_with_key_is_testable_like_dictation() {
        let lan = stt(SttEntryKind::SelfHosted, "", "http://192.168.1.5:8000", "k");
        assert_eq!(stt_blocker(&lan), None);
        let ollama = llm(
            LlmKind::Ollama,
            CloudLlm::OpenAI,
            "http://10.0.0.2:11434",
            "m",
            "t",
        );
        assert_eq!(llm_blocker(&ollama), None);
    }

    #[test]
    fn status_mapping_never_passes_errors() {
        assert_eq!(status_outcome(200), Ok(()));
        assert_eq!(status_outcome(302), Err(Outcome::Redirected));
        assert_eq!(status_outcome(401), Err(Outcome::KeyRejected));
        assert_eq!(status_outcome(403), Err(Outcome::KeyRejected));
        assert_eq!(status_outcome(429), Err(Outcome::Http(429)));
        assert!(!Outcome::Blocked(Blocker::NeedKey).passed());
    }
}
