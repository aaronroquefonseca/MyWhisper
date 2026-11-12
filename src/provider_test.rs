//! Lightweight provider connectivity tests (no heavy inference).
//!
//! ONE pattern for every chain row (STT + LLM, local and remote) and
//! every add/edit draft form: one Test button, same placement, same
//! inline reporting (`latency + excerpt/status`). Mirrors the phone
//! app (`MyWhisperMobile` `ProviderTest`): disabled fail-closed via
//! `enabled`, busy shows the testing copy, a disabled button shows
//! `disabled_reason` so the blocker stays explicit under a stale result.
//!
//! Probes are network-light and never run a full transcribe/generate:
//! - Self-hosted http(s) / OpenAI-compatible cloud: `GET {base}/v1/models`
//!   (bearer when a key is set). Only 2xx reads as reachable; gated
//!   401/403 reads as FAIL (wrong or missing key), never PASS.
//! - `ws(s)` streaming servers (sherpa protocol) speak no HTTP: a short
//!   TCP connect proves the address answers.
//! - Deepgram: `GET https://api.deepgram.com/v1/projects` (`Token` auth).
//! - Gemini (STT + LLM): `GET .../v1beta/models?pageSize=1` (`x-goog-api-key`).
//! - Ollama: read-only `GET {url}/api/tags` checked against the
//!   configured model name. Never loads or pins the model (that is what
//!   "Apply to model" is for).
//! - Local chain STT: installed-pack check (no inference, no server).
//! - Local LLM: not implemented (always disabled with a reason).

use std::net::ToSocketAddrs;
use std::time::{Duration, Instant};

/// Inline excerpt cap (matches the phone app width).
pub const EXCERPT_MAX: usize = 120;

/// Budget for server/remote probes (lightweight, no transcribe).
pub const SERVER_TIMEOUT: Duration = Duration::from_secs(10);

/// Unified row outcome: PASS = ran + non-blank + under timeout with no
/// error. Excerpt carries the reply/status, else the error.
#[derive(Debug, Clone)]
pub struct TestResult {
    pub passed: bool,
    pub latency_ms: u64,
    pub excerpt: String,
    pub error: Option<String>,
    pub timed_out: bool,
}

/// Config errors for the STT kinds (pure, UI maps to strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SttError {
    None,
    NeedUrl,
    NeedModel,
    BadUrl,
    BadBase,
}

/// Config errors for the LLM kinds (pure, UI maps to strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmError {
    None,
    NeedUrl,
    NeedModel,
    BadUrl,
}

/// Single-line excerpt, capped with an ellipsis.
pub fn excerpt(output: &str, max_chars: usize) -> String {
    let flat: String = output.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    let cut: String = flat.chars().take(max_chars).collect();
    format!("{}…", cut.trim_end())
}

/// THE mapping: every kind reports through here. PASS = no error +
/// under timeout + non-blank output.
pub fn map_result(
    output: Option<&str>,
    latency: Duration,
    timeout: Duration,
    error: Option<String>,
) -> TestResult {
    let timed_out = latency > timeout;
    let excerpt_text = match (&output, &error) {
        (Some(o), _) if !o.trim().is_empty() => excerpt(o, EXCERPT_MAX),
        (_, Some(e)) if !e.trim().is_empty() => excerpt(e, EXCERPT_MAX),
        _ => String::new(),
    };
    let passed = error.is_none() && !timed_out && output.is_some_and(|o| !o.trim().is_empty());
    TestResult {
        passed,
        latency_ms: latency.as_millis().min(u128::from(u64::MAX)) as u64,
        excerpt: excerpt_text,
        error,
        timed_out,
    }
}

/// Generic probe runner: measures wall-clock, catches errors, maps via
/// [`map_result`]. Callers inject the lightweight check.
pub fn run_probe(timeout: Duration, probe: impl FnOnce() -> Result<String, String>) -> TestResult {
    let start = Instant::now();
    match probe() {
        Ok(out) => map_result(Some(&out), start.elapsed(), timeout, None),
        Err(e) => map_result(None, start.elapsed(), timeout, Some(e)),
    }
}

/// Trimmed bearer token for probe auth (`None` when blank). Probes send
/// `Authorization: Bearer <token>` only when non-blank.
pub fn bearer_token(api_key: &str) -> Option<String> {
    let t = api_key.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Throw unless 2xx: every probe GET targets a genuine endpoint
/// (model lists, project lists, tag lists). A gated 401/403 means the
/// key is wrong or missing; any other non-2xx (404 wrong base, 429
/// quota) names a real problem — all read as FAIL, never PASS.
pub fn require_http_2xx(code: u16) -> Result<(), String> {
    if (200..300).contains(&code) {
        Ok(())
    } else {
        Err(format!("http {code}"))
    }
}

/// STT config validation (rows and draft form share one rule).
/// SelfHosted needs URL + model; Cloud needs model (blank URL =
/// vendor default).
pub fn validate_stt(kind: &str, url: &str, model: &str) -> SttError {
    let u = url.trim();
    let m = model.trim();
    if kind == "Local" {
        return SttError::None;
    }
    if m.is_empty() {
        return SttError::NeedModel;
    }
    if kind == "SelfHosted" {
        if u.is_empty() {
            return SttError::NeedUrl;
        }
        let Some(scheme) = u.split("://").next() else {
            return SttError::BadUrl;
        };
        if !u.contains("://") || !matches!(scheme, "http" | "https" | "ws" | "wss") {
            return SttError::BadUrl;
        }
        return SttError::None;
    }
    // Cloud (+ unknown): blank URL = vendor default.
    if !u.is_empty() {
        let Some(scheme) = u.split("://").next() else {
            return SttError::BadBase;
        };
        if !u.contains("://") || !matches!(scheme, "http" | "https") {
            return SttError::BadBase;
        }
    }
    SttError::None
}

/// LLM config validation. Ollama needs URL + model; Cloud needs model
/// (blank URL = vendor default); Local is always disabled elsewhere.
pub fn validate_llm(kind: &str, url: &str, model: &str) -> LlmError {
    let u = url.trim();
    let m = model.trim();
    if kind == "Local" {
        return LlmError::None;
    }
    if m.is_empty() {
        return LlmError::NeedModel;
    }
    if kind == "Ollama" {
        if u.is_empty() {
            return LlmError::NeedUrl;
        }
        let Some(scheme) = u.split("://").next() else {
            return LlmError::BadUrl;
        };
        if !u.contains("://") || !matches!(scheme, "http" | "https") {
            return LlmError::BadUrl;
        }
        return LlmError::None;
    }
    if !u.is_empty() {
        let Some(scheme) = u.split("://").next() else {
            return LlmError::BadUrl;
        };
        if !u.contains("://") || !matches!(scheme, "http" | "https") {
            return LlmError::BadUrl;
        }
    }
    LlmError::None
}

/// Server STT button goes live when the config validates.
pub fn stt_server_test_allowed(kind: &str, url: &str, model: &str) -> bool {
    validate_stt(kind, url, model) == SttError::None
}

/// Remote LLM button goes live when the config validates.
pub fn llm_remote_test_allowed(kind: &str, url: &str, model: &str) -> bool {
    validate_llm(kind, url, model) == LlmError::None
}

/// Whether the disabled-reason line renders under the test button.
/// Renders even when a stale result lingers (the blocker is explicit,
/// never hidden behind old output).
pub fn show_disabled_reason(enabled: bool, busy: bool, reason: Option<&str>) -> bool {
    !enabled && !busy && reason.is_some()
}

/// STT cloud base URL for the probe (matches the transcribe dispatch).
fn stt_cloud_base(provider: &str) -> &'static str {
    match provider {
        "Groq" => "https://api.groq.com/openai/v1",
        "Mistral" => "https://api.mistral.ai/v1",
        "Deepgram" => "https://api.deepgram.com",
        "Gemini" => "https://generativelanguage.googleapis.com/v1beta",
        _ => "https://api.openai.com/v1",
    }
}

/// Join a server base + `/v1/models` (tolerates bases that already
/// include `/v1`).
pub fn join_models_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/v1/models") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    }
}

fn blocking_client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("client: {e}"))
}

/// TCP reachability for `ws(s)` streaming servers (sherpa protocol):
/// no HTTP surface exists, so a short connect proves the address
/// answers without speaking the protocol or running inference.
fn tcp_reachable(url: &str, model: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url.trim()).map_err(|_| format!("bad address · {model}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("bad address · {model}"))?;
    let port = parsed.port_or_known_default().unwrap_or(80);
    // Bracket IPv6 literals (`::1` → `[::1]:port`).
    let addr = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let timeout = Duration::from_secs(5);
    std::net::TcpStream::connect_timeout(
        &addr
            .to_socket_addrs()
            .map_err(|_| format!("unresolved · {model}"))?
            .next()
            .ok_or_else(|| format!("unresolved · {model}"))?,
        timeout,
    )
    .map_err(|e| format!("unreachable · {e}"))?;
    Ok(format!("reachable · {model}"))
}

/// Lightweight STT endpoint check (no transcribe).
///
/// * `kind`: `"SelfHosted"` | `"Cloud"` (anything else = config-ok).
/// * `provider`: cloud vendor name (`OpenAI`/`Groq`/`Mistral`/`Gemini`/`Deepgram`;
///   ignored for self-hosted).
/// * `url`: self-hosted server URL (ignored for cloud: vendor base is used).
pub fn probe_stt_endpoint(
    kind: &str,
    url: &str,
    model: &str,
    api_key: &str,
    provider: &str,
) -> TestResult {
    let m = model.trim().to_string();
    if kind != "SelfHosted" && kind != "Cloud" {
        return run_probe(SERVER_TIMEOUT, || Ok(format!("config ok · {m}")));
    }
    if kind == "SelfHosted" {
        let base = url.trim().to_string();
        let m = model.trim().to_string();
        if base.to_ascii_lowercase().starts_with("ws") {
            // Streaming servers speak no HTTP: a short TCP connect
            // proves the address answers (real check, not config-ok).
            return run_probe(SERVER_TIMEOUT, move || tcp_reachable(&base, &m));
        }
        if base.is_empty() {
            return run_probe(SERVER_TIMEOUT, || Ok(format!("config ok · {m}")));
        }
        let k = api_key.trim().to_string();
        return run_probe(SERVER_TIMEOUT, move || {
            let client = blocking_client(SERVER_TIMEOUT)?;
            probe_openai_compat_models(&client, &base, &m, &k)
        });
    }
    // Cloud: vendor base is authoritative (the row URL field is unused).
    probe_stt_cloud(provider, model, api_key)
}

/// Probe a cloud STT vendor by name (used by chain rows where the URL
/// field is unused and the vendor base is authoritative).
pub fn probe_stt_cloud(provider: &str, model: &str, api_key: &str) -> TestResult {
    let m = model.trim().to_string();
    let k = api_key.trim().to_string();
    let p = provider.trim().to_string();
    run_probe(SERVER_TIMEOUT, move || {
        let client = blocking_client(SERVER_TIMEOUT)?;
        match p.as_str() {
            "Deepgram" => {
                let res = client
                    .get("https://api.deepgram.com/v1/projects")
                    .header("Authorization", format!("Token {k}"))
                    .send()
                    .map_err(|e| format!("deepgram: {e}"))?;
                let code = res.status().as_u16();
                require_http_2xx(code).map_err(|e| format!("deepgram: {e}"))?;
                Ok(format!("reachable · {m}"))
            }
            "Gemini" => {
                let res = client
                    .get("https://generativelanguage.googleapis.com/v1beta/models?pageSize=1")
                    .header("x-goog-api-key", &k)
                    .send()
                    .map_err(|e| format!("gemini: {e}"))?;
                let code = res.status().as_u16();
                require_http_2xx(code).map_err(|e| format!("gemini: {e}"))?;
                Ok(format!("reachable · {m}"))
            }
            _ => {
                let base = stt_cloud_base(&p);
                probe_openai_compat_models(&client, base, &m, &k)
            }
        }
    })
}

fn probe_openai_compat_models(
    client: &reqwest::blocking::Client,
    base: &str,
    model: &str,
    api_key: &str,
) -> Result<String, String> {
    // `/v1/models` is a genuine GET endpoint (unlike POST-only
    // transcribe routes): any non-2xx names a real problem (wrong
    // base, bad key), so only 2xx reads as reachable.
    let url = join_models_url(base);
    let mut req = client.get(&url).header("Accept", "application/json");
    if let Some(token) = bearer_token(api_key) {
        req = req.bearer_auth(token);
    }
    let res = req.send().map_err(|e| format!("probe: {e}"))?;
    let code = res.status().as_u16();
    require_http_2xx(code).map_err(|e| format!("probe: {e} · {model}"))?;
    Ok(format!("reachable · {model}"))
}

/// Ollama probe: read-only `GET {url}/api/tags` checked against the
/// configured model name. Never loads or pins the model — that side
/// effect belongs to "Apply to model". Bearer token included when set
/// (reverse-proxy setups).
pub fn probe_ollama(url: &str, model: &str, api_key: &str) -> TestResult {
    let url = url.trim().to_string();
    let model = model.trim().to_string();
    let k = api_key.trim().to_string();
    run_probe(SERVER_TIMEOUT, move || {
        let client = blocking_client(SERVER_TIMEOUT)?;
        let mut req = client
            .get(format!("{}/api/tags", url.trim_end_matches('/')))
            .header("Accept", "application/json");
        if let Some(token) = bearer_token(&k) {
            req = req.bearer_auth(token);
        }
        let res = req.send().map_err(|e| format!("ollama: {e}"))?;
        let code = res.status().as_u16();
        require_http_2xx(code).map_err(|e| format!("ollama: {e}"))?;
        let body = res.text().map_err(|e| format!("ollama body: {e}"))?;
        if body
            .to_ascii_lowercase()
            .contains(&model.to_ascii_lowercase())
        {
            Ok(format!("model ok · {model}"))
        } else {
            Err(format!("model not listed · {model}"))
        }
    })
}

/// Remote LLM probe by provider name (zero-billing model-list GETs).
/// `provider` is the `CloudLlm::name()` string; `base` is its
/// `base_url()` (call sites pass both so this stays UI-free).
pub fn probe_llm_cloud(provider: &str, base: &str, model: &str, api_key: &str) -> TestResult {
    let p = provider.trim().to_string();
    let b = base.trim().to_string();
    let m = model.trim().to_string();
    let k = api_key.trim().to_string();
    run_probe(SERVER_TIMEOUT, move || {
        let client = blocking_client(SERVER_TIMEOUT)?;
        match p.as_str() {
            "Claude" => {
                let res = client
                    .get(format!("{}/models", b.trim_end_matches('/')))
                    .header("x-api-key", &k)
                    .header("anthropic-version", "2023-06-01")
                    .send()
                    .map_err(|e| format!("claude: {e}"))?;
                let code = res.status().as_u16();
                require_http_2xx(code).map_err(|e| format!("claude: {e}"))?;
                Ok(format!("reachable · {m}"))
            }
            "Google" => {
                let res = client
                    .get(format!("{}/models?pageSize=1", b.trim_end_matches('/')))
                    .header("x-goog-api-key", &k)
                    .send()
                    .map_err(|e| format!("google: {e}"))?;
                let code = res.status().as_u16();
                require_http_2xx(code).map_err(|e| format!("google: {e}"))?;
                Ok(format!("reachable · {m}"))
            }
            _ => {
                let url = format!("{}/models", b.trim_end_matches('/'));
                let mut req = client.get(&url).header("Accept", "application/json");
                if let Some(token) = bearer_token(&k) {
                    req = req.bearer_auth(token);
                }
                let res = req.send().map_err(|e| format!("probe: {e}"))?;
                let code = res.status().as_u16();
                require_http_2xx(code).map_err(|e| format!("probe: {e} · {m}"))?;
                Ok(format!("reachable · {m}"))
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_caps_with_ellipsis() {
        assert_eq!(excerpt("  hello   world ", 120), "hello world");
        let long = "a ".repeat(100);
        let out = excerpt(&long, 120);
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= 121);
    }

    #[test]
    fn map_result_pass_fail_timeout_rules() {
        let ok = map_result(
            Some("HTTP 200 · whisper-1"),
            Duration::from_millis(50),
            SERVER_TIMEOUT,
            None,
        );
        assert!(ok.passed);
        assert!(!ok.excerpt.is_empty());
        let fail = map_result(
            None,
            Duration::from_millis(50),
            SERVER_TIMEOUT,
            Some("http 401 · m".to_string()),
        );
        assert!(!fail.passed);
        assert!(fail.excerpt.contains("401"));
        let slow = map_result(
            Some("HTTP 200 · m"),
            Duration::from_secs(30),
            SERVER_TIMEOUT,
            None,
        );
        assert!(!slow.passed && slow.timed_out);
        let blank = map_result(Some("   "), Duration::from_millis(5), SERVER_TIMEOUT, None);
        assert!(!blank.passed);
    }

    #[test]
    fn validate_stt_rules_mirror_phone_app() {
        assert_eq!(validate_stt("Local", "", ""), SttError::None);
        assert_eq!(validate_stt("SelfHosted", "", "m"), SttError::NeedUrl);
        assert_eq!(
            validate_stt("SelfHosted", "http://h:8000", ""),
            SttError::NeedModel
        );
        assert_eq!(validate_stt("SelfHosted", "ftp://h", "m"), SttError::BadUrl);
        assert_eq!(
            validate_stt("SelfHosted", "ws://h:6006", "m"),
            SttError::None
        );
        assert_eq!(validate_stt("Cloud", "", ""), SttError::NeedModel);
        assert_eq!(validate_stt("Cloud", "", "whisper-1"), SttError::None);
        assert_eq!(validate_stt("Cloud", "notaurl", "m"), SttError::BadBase);
    }

    #[test]
    fn validate_llm_rules_mirror_phone_app() {
        assert_eq!(validate_llm("Ollama", "", "m"), LlmError::NeedUrl);
        assert_eq!(
            validate_llm("Ollama", "http://127.0.0.1:11434", ""),
            LlmError::NeedModel
        );
        assert_eq!(
            validate_llm("Ollama", "http://127.0.0.1:11434", "gemma4:e4b"),
            LlmError::None
        );
        assert_eq!(validate_llm("Cloud", "", ""), LlmError::NeedModel);
        assert_eq!(validate_llm("Cloud", "", "gpt-6-luna"), LlmError::None);
    }

    #[test]
    fn gated_status_never_passes() {
        // Model-list GETs are genuine endpoints: only 2xx passes.
        assert!(require_http_2xx(401).is_err());
        assert!(require_http_2xx(403).is_err());
        assert!(require_http_2xx(404).is_err());
        assert!(require_http_2xx(429).is_err());
        assert!(require_http_2xx(200).is_ok());
        assert!(require_http_2xx(299).is_ok());
    }

    #[test]
    fn test_buttons_gate_on_validation() {
        assert!(stt_server_test_allowed("SelfHosted", "http://h:8000", "m"));
        assert!(!stt_server_test_allowed("SelfHosted", "", "m"));
        assert!(llm_remote_test_allowed(
            "Ollama",
            "http://127.0.0.1:11434",
            "gemma4:e4b"
        ));
        assert!(!llm_remote_test_allowed("Ollama", "", "m"));
        assert!(!show_disabled_reason(true, false, Some("x")));
        assert!(show_disabled_reason(false, false, Some("x")));
        assert!(!show_disabled_reason(false, true, Some("x")));
    }

    #[test]
    fn models_url_join_tolerates_v1_bases() {
        assert_eq!(join_models_url("http://h:8000"), "http://h:8000/v1/models");
        assert_eq!(
            join_models_url("http://h:8000/v1"),
            "http://h:8000/v1/models"
        );
        assert_eq!(
            join_models_url("https://api.openai.com/v1/"),
            "https://api.openai.com/v1/models"
        );
    }
}
