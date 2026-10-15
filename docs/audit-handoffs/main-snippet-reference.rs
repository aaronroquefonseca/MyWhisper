fn finalize_transcript(
    raw: &str,
    lm: &crate::settings::LanguageModelSettings,
    words: &[String],
    snips: &[(String, String)],
) -> String {
    let matches = crate::textnorm::find_snippet_matches(raw, snips);
    if matches.is_empty() {
        return post_process(raw, lm, words);
    }
    // Safe fallback before cleanup: user text already resembling
    // markers never enters the protected request, so it survives
    // verbatim and can never tangle with real placeholders.
    if contains_marker_like(raw) {
        eprintln!("[stt] marker-like user text; deterministic fallback");
        return crate::textnorm::expand_snippets(raw, snips);
    }
    eprintln!("[stt] snippets recognized: {}", matches.len());
    // No verifiable namespace means no protected request: expand
    // deterministically instead of risking a collision.
    let Some(token) = snippet_token(raw) else {
        eprintln!("[stt] no fresh placeholder namespace; deterministic fallback");
        return crate::textnorm::expand_snippets(raw, snips);
    };
    let mut protected = String::with_capacity(raw.len());
    let mut replacements = Vec::with_capacity(matches.len());
    let mut cursor = 0;
    for (idx, m) in matches.iter().enumerate() {
        protected.push_str(&raw[cursor..m.start]);
        protected.push_str(&snippet_placeholder(&token, idx));
        cursor = m.end;
        replacements.push(m.replacement.clone());
    }
    protected.push_str(&raw[cursor..]);
    match cleanup_once(&protected, lm, words, Some(&placeholder_instruction())) {
        Some(cleaned) => match restore_protected(&cleaned, &token, &replacements) {
            Some(final_) => {
                eprintln!("[stt] snippets restored literally");
                final_
            }
            None => {
                eprintln!("[stt] placeholder contract broken; deterministic fallback");
                crate::textnorm::expand_snippets(raw, snips)
            }
        },
        // Disabled, failed, or unavailable cleanup: same fallback.
        None => crate::textnorm::expand_snippets(raw, snips),
    }
}

/// Placeholder vocabulary: ASCII word characters only, so cleanup
/// models have no reason to reformat them. Unpredictability comes
/// from a 64-bit random token per utterance, but randomness alone is
/// not the collision defense: `snippet_token` verifies the namespace
/// is absent from the input, `finalize_transcript` refuses to protect
/// input that already resembles markers, and `restore_protected`
/// rejects any imitator — well-formed or malformed — in the response.
const SNIPPET_TAG_PREFIX: &str = "__MSG_SNIPPET_";
const SNIPPET_TAG_SUFFIX: &str = "__";

fn snippet_placeholder(token: &str, index: usize) -> String {
    format!("{SNIPPET_TAG_PREFIX}{token}_{index}{SNIPPET_TAG_SUFFIX}")
}

/// Attempts before giving up on a fresh namespace. Exhaustion is
/// practically unreachable (it needs dozens of colliding candidates),
/// and the caller falls back deterministically on `None`.
const TOKEN_ATTEMPTS: usize = 64;

/// Per-utterance token from a namespace verified absent from the
/// input, or `None` when no fresh candidate appears. Every candidate
/// is checked against the raw text — including RNG-failure counter
/// values — so no path returns an unverified namespace. (The raw text
/// is already known marker-free here, so this is defense in depth;
/// the check is cheap.)
fn snippet_token(raw: &str) -> Option<String> {
    static FALLBACK: AtomicU64 = AtomicU64::new(1);
    let mut next = || {
        let mut bytes = [0u8; 8];
        if getrandom::fill(&mut bytes).is_ok() {
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        } else {
            let n = FALLBACK.fetch_add(1, Ordering::Relaxed);
            format!("{:016x}", n.wrapping_add(std::process::id() as u64))
        }
    };
    snippet_token_with(raw, &mut next)
}

/// Controlled candidate generation behind `snippet_token`: first
/// candidate absent from `raw` wins, `None` after `TOKEN_ATTEMPTS`
/// rejections. Tests inject colliding sequences deterministically
/// instead of waiting on random collisions.
fn snippet_token_with(raw: &str, next: &mut impl FnMut() -> String) -> Option<String> {
    for _ in 0..TOKEN_ATTEMPTS {
        let candidate = next();
        if !candidate.is_empty() && !raw.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Extra system-prompt section while placeholders are active.
/// Instructions alone are not trusted: `restore_protected` enforces
/// the contract on the response.
fn placeholder_instruction() -> String {
    format!(
        "Protected snippet tokens look like {SNIPPET_TAG_PREFIX}<hex>_<n>{SNIPPET_TAG_SUFFIX} \
         (letters, digits and underscores only). Copy each one into the cleaned transcript \
         verbatim, exactly once, in its original position and order. Never translate, reformat, \
         split, merge, remove, or duplicate them, and never invent new ones."
    )
}

/// Every reserved-stem occurrence (byte start, byte end). Validation
/// rule: `MSG`, then a separator run of underscores and ASCII
/// whitespace in any order and any length (including none), then
/// `SNIPPET`, all ASCII case-insensitive. One rule covers contiguous
/// markers, glued copies, and every damaged variant: truncated,
/// rewrapped (`**MSG_SNIPPET_..**`), recased, and whitespace on
/// either side of (or inside) the separator (`__MSG_ SNIPPET_..__`,
/// `__MSG_\nSNIPPET_..__`, `__MSG\t_\tSNIPPET_..__`). Pure ASCII
/// matching keeps all indices on char boundaries.
fn reserved_stem_spans(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        if bytes[i..i + 3].eq_ignore_ascii_case(b"MSG") {
            let mut j = i + 3;
            while j < bytes.len() && (bytes[j] == b'_' || bytes[j].is_ascii_whitespace()) {
                j += 1;
            }
            if j + 7 <= bytes.len() && bytes[j..j + 7].eq_ignore_ascii_case(b"SNIPPET") {
                out.push((i, j + 7));
                i = j + 7;
                continue;
            }
        }
        // Advance one char to stay on a byte boundary.
        i += text[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
    out
}

/// True when the text holds anything placeholder-shaped: a
/// well-formed marker or any reserved-stem fragment (truncated,
/// whitespace-altered, rewrapped, any case).
fn contains_marker_like(text: &str) -> bool {
    !reserved_stem_spans(text).is_empty()
}
/// Every well-formed placeholder occurrence: (byte start, byte end,
/// full token text). Well-formed but foreign tokens are returned and
/// rejected as unexpected; malformed fragments are caught separately
/// by `reserved_stem_spans`.
fn scan_placeholders(text: &str) -> Vec<(usize, usize, String)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if text[i..].starts_with(SNIPPET_TAG_PREFIX) {
            let mut j = i + SNIPPET_TAG_PREFIX.len();
            while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
                j += 1;
            }
            if j > i + SNIPPET_TAG_PREFIX.len() && j < bytes.len() && bytes[j] == b'_' {
                let mut k = j + 1;
                let digits = k;
                while k < bytes.len() && bytes[k].is_ascii_digit() {
                    k += 1;
                }
                if k > digits && text[k..].starts_with(SNIPPET_TAG_SUFFIX) {
                    let end = k + SNIPPET_TAG_SUFFIX.len();
                    out.push((i, end, text[i..end].to_string()));
                    i = end;
                    continue;
                }
            }
        }
        // Advance one char to stay on a byte boundary.
        i += text[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
    out
}

/// Validate the cleanup response against the expected placeholders
/// and splice the saved replacements verbatim. `None` on any
/// contract breach: wrong count, wrong token, wrong order (covers
/// missing, altered, duplicated, reordered, and unexpected
/// well-formed placeholders), or any reserved-stem occurrence outside
/// a validated placeholder span (covers malformed fragments:
/// truncated, whitespace-altered, rewrapped, or recased copies).
/// Only the cleanup response is scanned — restored replacement text
/// is never inspected, so replacements stay literal even when they
/// hold marker-like text. Never runs snippet matching on the result.
fn restore_protected(cleaned: &str, token: &str, replacements: &[String]) -> Option<String> {
    let found = scan_placeholders(cleaned);
    if found.len() != replacements.len() {
        return None;
    }
    for (index, (_, _, text)) in found.iter().enumerate() {
        if *text != snippet_placeholder(token, index) {
            return None;
        }
    }
    // Every reserved-stem occurrence must sit inside a validated
    // placeholder span; anything else is a malformed fragment.
    let spans: Vec<(usize, usize)> = found.iter().map(|(start, end, _)| (*start, *end)).collect();
    for (stem_start, stem_end) in reserved_stem_spans(cleaned) {
        if !spans
            .iter()
            .any(|(start, end)| *start <= stem_start && stem_end <= *end)
        {
            return None;
        }
    }
    let mut out = String::with_capacity(cleaned.len());
    let mut cursor = 0;
    for ((start, end, _), rep) in found.iter().zip(replacements.iter()) {
        out.push_str(&cleaned[cursor..*start]);
        out.push_str(rep);
        cursor = *end;
    }
    out.push_str(&cleaned[cursor..]);
    Some(out)
}

/// Store in history (raw + final text, never audio) and paste the
/// FINAL transcript via the configured clipboard pipeline.
/// Retention: history off pastes without storing; auto-delete purges
/// old rows first (tombstones survive).
fn deliver(raw: String, secs: f64, wav: Option<Vec<u8>>) {
    eprintln!("[stt] raw: {raw}");
    let paths = match crate::paths::AppPaths::create() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[stt] paths: {e}");
            return;
        }
    };
    let store = match crate::store::Store::open(&paths.db_file()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[stt] store: {e}");
            return;
        }
    };
    let settings = store.load_settings();
    // Snippets keyed per trigger (comma-separated aliases flatten to
    // one entry each). Recognition runs inside finalize_transcript
    // BEFORE cleanup, with placeholders guarding the matched spans.
    // Raw stays pure for the raw view and history; final carries
    // literal replacements.
    let snips: Vec<(String, String)> = settings
        .dictionary
        .snippets
        .iter()
        .flat_map(|s| s.triggers().into_iter().map(|t| (t, s.replacement.clone())))
        .collect();
    let text = finalize_transcript(
        &raw,
        &settings.language_models,
        &settings.dictionary.words,
        &snips,
    );
    eprintln!("[stt] final: {text}");
    if settings.privacy.keep_audio_recordings && settings.privacy.audio_retention_days > 0 {
        let data_dir = paths
            .db_file()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let n = crate::audio::purge_old_audio(&data_dir, settings.privacy.audio_retention_days);
        if n > 0 {
            eprintln!("[stt] audio auto-delete: {n} files");
        }
    }
    if settings.privacy.transcript_auto_delete_days > 0
        && let Err(e) = store.purge_old_history(settings.privacy.transcript_auto_delete_days)
    {
        eprintln!("[stt] auto-delete: {e}");
    }
    if settings.privacy.history_enabled {
        let words = text.split_whitespace().count() as u32;
        let node = crate::device::DeviceIdentity::load_or_create(&paths.device_key_file())
            .map(|i| i.node_id)
            .unwrap_or_else(|_| "unknown".to_string());
        match store.add_history(&raw, &text, words, secs.round() as i64, &node) {
            Ok(uuid) => {
                if let Some(wav) = wav {
                    let data_dir = paths
                        .db_file()
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_default();
                    let dir = crate::audio::audio_vault_dir(&data_dir);
                    match std::fs::create_dir_all(&dir)
                        .and_then(|_| std::fs::write(dir.join(format!("{uuid}.wav")), wav))
                    {
                        Ok(()) => eprintln!("[stt] audio kept: {uuid}.wav"),
                        Err(e) => eprintln!("[stt] audio vault write: {e}"),
                    }
                }
            }
            Err(e) => eprintln!("[stt] history: {e}"),
        }
    } else {
        eprintln!("[stt] history disabled: pasting without storing");
    }
    crate::clipboard::run_pipeline(&text, &settings.preferences);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_net_defaults_and_overrides() {
        let s = SttSettings::default();
        let net = server_net(&s);
        assert_eq!(net.host, "127.0.0.1");
        assert_eq!(net.port_override, None);
        assert_eq!(engine_port(&net, 8080), 8080);
        let mut s = s;
        s.server_port = "9090".to_string();
        s.lan_expose = true;
        let net = server_net(&s);
        assert_eq!(net.host, "0.0.0.0");
        assert_eq!(engine_port(&net, 6006), 9090);
        s.server_port = "abc".to_string(); // invalid -> no override
        assert_eq!(server_net(&s).port_override, None);
        s.server_port = "0".to_string(); // 0 -> no override
        assert_eq!(server_net(&s).port_override, None);
    }

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
    fn online_segments_accumulate() {
        // Two endpointed segments + trailing partial: the 2-minute
        // bug was keeping only the last message.
        let mut sess = OnlineSession {
            preview: false,
            ws: None,
            segments: Vec::new(),
            current: String::new(),
            read_timeout: std::time::Duration::from_secs(1),
        };
        assert!(!sess.ingest(r#"{"text":"hello"}"#));
        assert!(!sess.ingest(r#"{"text":"hello world"}"#));
        assert!(sess.ingest(r#"{"text":"hello world","is_final":true}"#));
        assert!(!sess.ingest(r#"{"text":"second part"}"#));
        assert_eq!(sess.transcript(), "hello world second part");
        assert!(sess.ingest(r#"{"text":"second part here","is_final":true}"#));
        assert_eq!(sess.transcript(), "hello world second part here");
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
    fn prompt_defaults_and_suffix() {
        assert!(effective_prompt("").contains("transcript cleanup engine"));
        assert_eq!(effective_prompt("custom"), "custom");
        assert_eq!(effective_prompt("   "), effective_prompt(""));
        assert!(dictionary_suffix(&[]).is_none());
        let s = dictionary_suffix(&["Acme".to_string(), "Widget".to_string()]).unwrap();
        assert!(s.contains("Acme, Widget"));
        assert!(s.starts_with("Custom Dictionary"));
    }

    #[test]
    fn lang_param_auto_omits() {
        assert_eq!(lang_param("auto"), None);
        assert_eq!(lang_param(""), None);
        assert_eq!(lang_param("es"), Some("es"));
    }

    #[test]
    fn whistle_plan_needs_files() {
        let s = SttSettings {
            local_provider: LocalSttProvider::Whistle,
            ..Default::default()
        };
        let err = plan_for(&s, Path::new("/nonexistent")).unwrap_err();
        assert!(err.contains("whistle"), "{err}");
    }

    /// Live spec-thread check (needs whisper-server on :8080):
    /// synthetic audio through the real thread + channel + merge.
    /// Ignored by default; run explicitly with a server up.
    #[test]
    #[ignore]
    fn spec_thread_roundtrip() {
        use std::time::Duration;
        let rec = Arc::new(Mutex::new(Recorder::default()));
        rec.lock().unwrap().start(44_100);
        // 4 s of loud-ish tone: decodes (to something), exercises
        // passes, agreement, and the done channel.
        let tone: Vec<i16> = (0..44_100 * 4)
            .map(|i| {
                (15_000.0 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 44_100.0).sin()) as i16
            })
            .collect();
        rec.lock().unwrap().push(&tone);
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let r = rec.clone();
        let s = stop.clone();
        let data = crate::paths::AppPaths::create().unwrap().data_dir;
        std::thread::spawn(move || {
            run_spec(
                r,
                s,
                data,
                SpecConfig {
                    update_secs: 1.0,
                    overlap: 16_000,
                    whisper_port: 8080,
                },
                tx,
                Arc::new(AtomicBool::new(true)),
            )
        });
        std::thread::sleep(Duration::from_secs(5));
        stop.store(true, Ordering::Relaxed);
        let (committed, clen, buf) = rx.recv_timeout(Duration::from_secs(120)).unwrap();
        eprintln!("spec committed={committed:?} clen={clen} buf={}", buf.len());
        assert!(buf.len() >= 60_000, "buffer doesn't cover 4 s");
    }

    #[test]
    fn whistle_stdout_shapes() {
        assert_eq!(
            whistle_parse_stdout("  hello world \n"),
            Some("hello world".into())
        );
        assert_eq!(
            whistle_parse_stdout(r#"{"text":" hi ","tokens_per_second":99}"#),
            Some("hi".into())
        );
        assert_eq!(
            whistle_parse_stdout(r#"{"audio_text":"lights off"}"#),
            Some("lights off".into())
        );
        assert_eq!(whistle_parse_stdout("   \n  "), None);
        assert_eq!(whistle_parse_stdout(r#"{"text":"   "}"#), None);
    }

    #[test]
    fn whistle_output_text_or_json() {
        // Bare transcript passes through trimmed.
        let engine = Path::new("needle");
        let model = Path::new("whistle.cact");
        // No engine on disk: error names the failure, no panic.
        assert!(whistle_pass(engine, model, Path::new("/nonexistent.wav"), None, "auto").is_err());
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
        let s = SttSettings {
            local_provider: LocalSttProvider::Cohere,
            ..Default::default()
        };
        let err = plan_for(&s, Path::new("/nonexistent")).unwrap_err();
        assert!(err.contains("Cohere"), "{err}");
    }

    #[test]
    fn plan_routing_needs_files() {
        // Whisper tiny not under /nonexistent → actionable missing error.
        let mut s = SttSettings {
            local_provider: LocalSttProvider::OpenAi,
            whisper_model: "tiny".to_string(),
            ..Default::default()
        };
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
    fn plans_selfhosted_ws_and_bearer() {
        use crate::settings::{SttChainEntry, SttEntryKind};
        let entry = |url: &str, streaming: bool, key: &str| SttChainEntry {
            id: "t".to_string(),
            name: "t".to_string(),
            kind: SttEntryKind::SelfHosted,
            enabled: true,
            provider: String::new(),
            url: url.to_string(),
            model: String::new(),
            api_key: key.to_string(),
            streaming,
        };
        // ws:// + streaming = sherpa-protocol plan (never spawned).
        let s = SttSettings {
            entries: vec![entry("ws://127.0.0.1:6006", true, "")],
            ..Default::default()
        };
        let plans = plans_for(&s, Path::new("/nonexistent")).unwrap();
        assert!(matches!(plans[0].1, Plan::SelfHostedWs { .. }));
        assert!(plans[0].1.streaming());
        // wss:// resolves the same (TLS at connect time).
        let s = SttSettings {
            entries: vec![entry("wss://example.com:6006", true, "")],
            ..Default::default()
        };
        let plans = plans_for(&s, Path::new("/nonexistent")).unwrap();
        assert!(matches!(plans[0].1, Plan::SelfHostedWs { .. }));
        // http + token = OpenAI-compat with bearer key; streaming
        // toggle inert for http.
        let s = SttSettings {
            entries: vec![entry("http://127.0.0.1:8080", true, "tok")],
            ..Default::default()
        };
        let plans = plans_for(&s, Path::new("/nonexistent")).unwrap();
        match &plans[0].1 {
            Plan::CloudOpenAiCompat { key, .. } => assert_eq!(key, "tok"),
            p => panic!("expected compat, got {}", p.describe()),
        }
        assert!(!plans[0].1.streaming());
        // ws:// + streaming off = compat fallback.
        let s = SttSettings {
            entries: vec![entry("ws://127.0.0.1:6006", false, "")],
            ..Default::default()
        };
        let plans = plans_for(&s, Path::new("/nonexistent")).unwrap();
        assert!(matches!(plans[0].1, Plan::CloudOpenAiCompat { .. }));
    }

    #[test]
    fn plan_cloud_needs_key() {
        use crate::settings::{SttChainEntry, SttEntryKind};
        let s = SttSettings {
            entries: vec![SttChainEntry {
                id: "t".to_string(),
                name: "t".to_string(),
                kind: SttEntryKind::Cloud,
                enabled: true,
                provider: "OpenAI".to_string(),
                url: String::new(),
                model: "whisper-1".to_string(),
                api_key: String::new(),
                streaming: true,
            }],
            ..Default::default()
        };
        let err = plan_for(&s, Path::new("/nonexistent")).unwrap_err();
        assert!(err.contains("API key"), "{err}");
    }

    #[test]
    fn chain_falls_back_past_broken_first() {
        use crate::settings::{SttChainEntry, SttEntryKind};
        // First entry unresolvable (missing files), second fine is
        // covered by plans_for collecting only Ok plans.
        let s = SttSettings {
            entries: vec![
                SttChainEntry {
                    id: "bad".to_string(),
                    name: "bad-local".to_string(),
                    kind: SttEntryKind::Local,
                    enabled: true,
                    provider: String::new(),
                    url: String::new(),
                    model: String::new(),
                    api_key: String::new(),
                    streaming: true,
                },
                SttChainEntry {
                    id: "ok".to_string(),
                    name: "ok-cloud".to_string(),
                    kind: SttEntryKind::Cloud,
                    enabled: true,
                    provider: "OpenAI".to_string(),
                    url: String::new(),
                    model: "whisper-1".to_string(),
                    api_key: "k".to_string(),
                    streaming: true,
                },
            ],
            ..Default::default()
        };
        // Local default provider with missing model dir fails; the
        // cloud entry resolves: plans_for returns just it.
        let plans = plans_for(&s, Path::new("/nonexistent")).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].0, "ok-cloud");
    }

    /// Live whistle FIFO check (needs needle + model on disk):
    /// synthetic PCM through the real streaming session.
    /// Ignored by default; run explicitly with files present.
    #[test]
    #[ignore]
    fn whistle_stream_roundtrip() {
        let _ = crate::runtime::init();
        let home = std::env::var("HOME").unwrap();
        let data = PathBuf::from(format!("{home}/.local/share/mywhisper"));
        let engine = data.join("bin/needle/needle");
        let model = data.join("models/whistle/whistle-v1/whistle.cact");
        if !engine.exists() || !model.exists() {
            eprintln!("SKIP: needle/model missing");
            return;
        }
        // espeak speech at 16 kHz (built by the shell harness
        // into /tmp/wtest16k.wav);decodes to real words.
        let pcm: Vec<i16> = {
            let raw = std::fs::read("/tmp/wtest16k.wav").expect("harness wav");
            assert!(raw.len() > 44);
            raw[44..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect()
        };
        let mut stream = WhistleStream::open(&engine, &model, None, "auto").expect("open");
        for chunk in pcm.chunks(8000) {
            stream.feed(chunk, 16_000).expect("feed");
            std::thread::sleep(Duration::from_millis(100));
        }
        let text = stream.finish().expect("finish");
        eprintln!("whistle stream text: {text:?}");
    }

    /// Pipeline regression: one post-cleanup expansion pass in every
    /// cleanup mode (disabled, enabled-but-empty, chain-failed).
    /// Fixtures are disposable (no transcripts, credentials, or
    /// services; the failing chain errors without network).
    #[test]
    fn pipeline_expands_once_after_cleanup() {
        use crate::settings::{CloudLlm, LanguageModelSettings, LlmEntry, LlmKind};
        // Cleanup disabled: raw passes through.
        let off = LanguageModelSettings {
            cleanup_enabled: false,
            ..Default::default()
        };
        // Cleanup enabled but no options: passthrough with a log line.
        let empty = LanguageModelSettings {
            cleanup_enabled: true,
            entries: Vec::new(),
            ..Default::default()
        };
        // Cleanup enabled but the chain fails (Local kind errors
        // without touching the network): raw is kept.
        let failing = LanguageModelSettings {
            cleanup_enabled: true,
            entries: vec![LlmEntry {
                id: "t".to_string(),
                name: "t".to_string(),
                kind: LlmKind::Local,
                enabled: true,
                url: String::new(),
                model: "m".to_string(),
                provider: CloudLlm::OpenAI,
                api_key: String::new(),
                keep_alive: String::new(),
                num_ctx: None,
                thinking: None,
            }],
            ..Default::default()
        };
        let words: Vec<String> = Vec::new();
        // Self-containing replacement: exactly one insertion in every
        // cleanup mode. A pre+post pipeline doubles it whenever the
        // cleanup is a passthrough ("go" -> "go now now").
        let snips = vec![("go".to_string(), "go now".to_string())];
        for lm in [&off, &empty, &failing] {
            assert_eq!(finalize_transcript("go", lm, &words, &snips), "go now");
        }
        // Replacement naming another trigger stays literal.
        let snips = vec![
            ("signature".to_string(), "Regards, Oscar".to_string()),
            ("Oscar".to_string(), "ANOTHER SNIPPET".to_string()),
        ];
        for lm in [&off, &empty, &failing] {
            assert_eq!(
                finalize_transcript("signature", lm, &words, &snips),
                "Regards, Oscar"
            );
        }
        // Normalized trigger forms still match when the
        // transcription itself holds them (matching runs on the raw
        // text before cleanup).
        let snips = vec![("omw".to_string(), "On my way!".to_string())];
        assert_eq!(
            crate::textnorm::expand_snippets("OMW", &snips),
            "On my way!"
        );
        // Passthrough modes otherwise keep the raw wording.
        assert_eq!(
            finalize_transcript("plain talk", &off, &words, &[]),
            "plain talk"
        );
    }

    /// Synthetic loopback cleanup provider: a minimal HTTP server
    /// speaking just enough of Ollama's `/api/chat` for `llm_cleanup`
    /// (one POST per connection, one canned JSON reply). Disposable:
    /// loopback only, ephemeral port, bounded connections and time,
    /// no credentials, no user data.
    struct FakeCleanup {
        base_url: String,
        bodies: Arc<Mutex<Vec<String>>>,
        transcripts: Arc<Mutex<Vec<String>>>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl FakeCleanup {
        fn serve<F>(conns: usize, respond: F) -> Self
        where
            F: Fn(&str) -> String + Send + Sync + 'static,
        {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback bind");
            let port = listener.local_addr().expect("port").port();
            listener.set_nonblocking(true).expect("nonblocking");
            let respond = Arc::new(respond);
            let respond: Arc<dyn Fn(&str) -> String + Send + Sync> = respond;
            let bodies = Arc::new(Mutex::new(Vec::new()));
            let transcripts = Arc::new(Mutex::new(Vec::new()));
            let worker_bodies = bodies.clone();
            let worker_transcripts = transcripts.clone();
            let handle = std::thread::spawn(move || {
                let start = Instant::now();
                let mut served = 0;
                while served < conns && start.elapsed() < Duration::from_secs(30) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            Self::answer(stream, &worker_bodies, &worker_transcripts, &respond);
                            served += 1;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5))
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                base_url: format!("http://127.0.0.1:{port}"),
                bodies,
                transcripts,
                handle: Some(handle),
            }
        }

        fn answer(
            mut stream: std::net::TcpStream,
            bodies: &Arc<Mutex<Vec<String>>>,
            transcripts: &Arc<Mutex<Vec<String>>>,
            respond: &Arc<dyn Fn(&str) -> String + Send + Sync>,
        ) {
            use std::io::{Read, Write};
            stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut tmp) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                }
            }
            let head = String::from_utf8_lossy(&buf).into_owned();
            let head_end = head.find("\r\n\r\n").map(|i| i + 4).unwrap_or(head.len());
            let len = head[..head_end]
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            buf.drain(..head_end.min(buf.len()));
            while buf.len() < len {
                match stream.read(&mut tmp) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                }
            }
            let body = String::from_utf8_lossy(&buf).into_owned();
            bodies.lock().map(|mut b| b.push(body.clone())).ok();
            let mut transcript = String::new();
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body)
                && let Some(messages) = parsed.get("messages").and_then(|m| m.as_array())
            {
                for message in messages {
                    let is_user = message.get("role").and_then(|r| r.as_str()) == Some("user");
                    if !is_user {
                        continue;
                    }
                    if let Some(content) = message.get("content").and_then(|c| c.as_str())
                        && let Some(inner) = content
                            .strip_prefix("<transcript>")
                            .and_then(|t| t.strip_suffix("</transcript>"))
                    {
                        transcript = inner.to_string();
                    }
                    break;
                }
            }
            transcripts
                .lock()
                .map(|mut t| t.push(transcript.clone()))
                .ok();
            let reply =
                serde_json::json!({"message": {"content": respond(&transcript)}}).to_string();
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .as_bytes(),
            );
        }

        fn settings(&self) -> crate::settings::LanguageModelSettings {
            use crate::settings::{CloudLlm, LanguageModelSettings, LlmEntry, LlmKind};
            LanguageModelSettings {
                cleanup_enabled: true,
                entries: vec![LlmEntry {
                    id: "fake".to_string(),
                    name: "fake".to_string(),
                    kind: LlmKind::Ollama,
                    enabled: true,
                    url: self.base_url.clone(),
                    model: "fake".to_string(),
                    provider: CloudLlm::OpenAI,
                    api_key: String::new(),
                    keep_alive: String::new(),
                    num_ctx: None,
                    thinking: None,
                }],
                ..Default::default()
            }
        }

        fn bodies(&self) -> Vec<String> {
            self.bodies.lock().map(|b| b.clone()).unwrap_or_default()
        }

        fn transcripts(&self) -> Vec<String> {
            self.transcripts
                .lock()
                .map(|t| t.clone())
                .unwrap_or_default()
        }
    }

    impl Drop for FakeCleanup {
        fn drop(&mut self) {
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    #[test]
    fn pipeline_cleanup_never_sees_triggers_or_replacements() {
        // The model rewrites surrounding text; placeholders ride
        // through untouched and restore literally.
        let server = FakeCleanup::serve(1, |t| t.replace("call me", "ring me"));
        let lm = server.settings();
        let snips = vec![("brb".to_string(), "be right back".to_string())];
        let words: Vec<String> = Vec::new();
        assert_eq!(
            finalize_transcript("brb, call me", &lm, &words, &snips),
            "be right back, ring me"
        );
        let bodies = server.bodies();
        let transcripts = server.transcripts();
        assert_eq!(transcripts.len(), 1);
        assert!(transcripts[0].contains("__MSG_SNIPPET_"));
        assert!(!transcripts[0].contains("be right back"));
        assert!(!transcripts[0].contains("brb"));
        // The request also instructs the model to preserve tokens.
        assert!(bodies[0].contains("verbatim"));
    }

    #[test]
    fn pipeline_success_path_restores_verbatim_without_rescanning() {
        // Echo cleanup: replacements restore literally, never
        // re-matched, so self-containing and cross-snippet pairs
        // cannot recurse even with a live cleanup.
        let server = FakeCleanup::serve(2, |t| t.to_string());
        let lm = server.settings();
        let words: Vec<String> = Vec::new();
        let snips = vec![("go".to_string(), "go now".to_string())];
        assert_eq!(finalize_transcript("go", &lm, &words, &snips), "go now");
        let snips = vec![
            ("signature".to_string(), "Regards, Oscar".to_string()),
            ("Oscar".to_string(), "ANOTHER SNIPPET".to_string()),
        ];
        assert_eq!(
            finalize_transcript("signature", &lm, &words, &snips),
            "Regards, Oscar"
        );
    }

    #[test]
    fn pipeline_unavailable_cleanup_falls_back_deterministically() {
        // Nothing listens on port 1: the chain fails without touching
        // any real service, and the raw text expands once.
        use crate::settings::{CloudLlm, LanguageModelSettings, LlmEntry, LlmKind};
        let down = LanguageModelSettings {
            cleanup_enabled: true,
            entries: vec![LlmEntry {
                id: "down".to_string(),
                name: "down".to_string(),
                kind: LlmKind::Ollama,
                enabled: true,
                url: "http://127.0.0.1:1".to_string(),
                model: "m".to_string(),
                provider: CloudLlm::OpenAI,
                api_key: String::new(),
                keep_alive: String::new(),
                num_ctx: None,
                thinking: None,
            }],
            ..Default::default()
        };
        let words: Vec<String> = Vec::new();
        let snips = vec![("go".to_string(), "go now".to_string())];
        assert_eq!(finalize_transcript("go", &down, &words, &snips), "go now");
    }

    #[test]
    fn pipeline_rejects_missing_placeholder() {
        let server = FakeCleanup::serve(1, |t| {
            let mut s = t.to_string();
            if let Some((start, end, _)) = scan_placeholders(&s).first() {
                let (start, end) = (*start, *end);
                s.replace_range(start..end, "");
            }
            s
        });
        let lm = server.settings();
        let words: Vec<String> = Vec::new();
        let snips = vec![("brb".to_string(), "be right back".to_string())];
        assert_eq!(
            finalize_transcript("brb, call me", &lm, &words, &snips),
            "be right back, call me"
        );
    }

    #[test]
    fn pipeline_rejects_altered_placeholder() {
        // Same shape, different token: the response is discarded.
        let server = FakeCleanup::serve(1, |t| {
            let mut s = t.to_string();
            if let Some((_, _, text)) = scan_placeholders(&s).first() {
                let mut bad = text.clone();
                let hex_at = bad.find("__MSG_SNIPPET_").unwrap() + "__MSG_SNIPPET_".len();
                let flip = if bad.as_bytes()[hex_at] == b'f' {
                    "e"
                } else {
                    "f"
                };
                bad.replace_range(hex_at..hex_at + 1, flip);
                s = s.replacen(text, &bad, 1);
            }
            s
        });
        let lm = server.settings();
        let words: Vec<String> = Vec::new();
        let snips = vec![("brb".to_string(), "be right back".to_string())];
        assert_eq!(
            finalize_transcript("brb, call me", &lm, &words, &snips),
            "be right back, call me"
        );
    }

    #[test]
    fn pipeline_rejects_duplicated_placeholder() {
        let server = FakeCleanup::serve(1, |t| {
            if let Some((_, _, text)) = scan_placeholders(t).first() {
                format!("{t} {text}")
            } else {
                t.to_string()
            }
        });
        let lm = server.settings();
        let words: Vec<String> = Vec::new();
        let snips = vec![("brb".to_string(), "be right back".to_string())];
        assert_eq!(
            finalize_transcript("brb, call me", &lm, &words, &snips),
            "be right back, call me"
        );
    }

    #[test]
    fn pipeline_rejects_reordered_placeholders() {
        let server = FakeCleanup::serve(1, |t| {
            let found = scan_placeholders(t);
            if found.len() == 2 {
                let s = t.replacen(&found[0].2, "__TMP_SWAP__", 1);
                let s = s.replacen(&found[1].2, &found[0].2, 1);
                s.replacen("__TMP_SWAP__", &found[1].2, 1)
            } else {
                t.to_string()
            }
        });
        let lm = server.settings();
        let words: Vec<String> = Vec::new();
        let snips = vec![
            ("brb".to_string(), "be right back".to_string()),
            ("omw".to_string(), "on my way".to_string()),
        ];
        assert_eq!(
            finalize_transcript("brb omw", &lm, &words, &snips),
            "be right back on my way"
        );
    }

    #[test]
    fn pipeline_rejects_unexpected_placeholder() {
        let server = FakeCleanup::serve(1, |t| format!("{t} __MSG_SNIPPET_deadbeef_99__"));
        let lm = server.settings();
        let words: Vec<String> = Vec::new();
        let snips = vec![("brb".to_string(), "be right back".to_string())];
        assert_eq!(
            finalize_transcript("brb, call me", &lm, &words, &snips),
            "be right back, call me"
        );
    }

    #[test]
    fn pipeline_user_text_resembling_placeholders_falls_back_safely() {
        // The lookalike is not ours: cleanup is skipped up front (the
        // server sees zero requests), the trigger expands once, and
        // the lookalike survives literally.
        let server = FakeCleanup::serve(0, |t| t.to_string());
        let lm = server.settings();
        let words: Vec<String> = Vec::new();
        let snips = vec![("brb".to_string(), "be right back".to_string())];
        assert_eq!(
            finalize_transcript(
                "brb notes __MSG_SNIPPET_deadbeef_0__ end",
                &lm,
                &words,
                &snips
            ),
            "be right back notes __MSG_SNIPPET_deadbeef_0__ end"
        );
        assert!(server.transcripts().is_empty());
    }

    #[test]
    fn pipeline_malformed_lookalike_in_raw_skips_cleanup() {
        // Malformed user text never reaches the model either.
        let server = FakeCleanup::serve(0, |t| t.to_string());
        let lm = server.settings();
        let words: Vec<String> = Vec::new();
        let snips = vec![("brb".to_string(), "be right back".to_string())];
        assert_eq!(
            finalize_transcript("brb says __MSG_SNIPPET_zz_0__ hi", &lm, &words, &snips),
            "be right back says __MSG_SNIPPET_zz_0__ hi"
        );
        assert!(server.transcripts().is_empty());
    }

    /// Malformed-fragment matrix: cleanup preserves the real marker,
    /// rewrites surrounding text, and appends one debris copy. Every
    /// response must fall back to the once-expanded original.
    #[test]
    fn pipeline_rejects_truncated_marker_copy() {
        let server = FakeCleanup::serve(1, |t| {
            let cleaned = t.replace("call me", "ring me");
            if let Some((_, _, text)) = scan_placeholders(t).first() {
                format!("{} {}", cleaned, &text[..text.len() - 5])
            } else {
                cleaned
            }
        });
        let lm = server.settings();
        let words: Vec<String> = Vec::new();
        let snips = vec![("brb".to_string(), "be right back".to_string())];
        assert_eq!(
            finalize_transcript("brb, call me", &lm, &words, &snips),
            "be right back, call me"
        );
    }

    #[test]
    fn pipeline_rejects_marker_missing_closing_underscore() {
        let server = FakeCleanup::serve(1, |t| {
            let cleaned = t.replace("call me", "ring me");
            if let Some((_, _, text)) = scan_placeholders(t).first() {
                format!("{} {}", cleaned, &text[..text.len() - 1])
            } else {
                cleaned
            }
        });
        let lm = server.settings();
        let words: Vec<String> = Vec::new();
        let snips = vec![("brb".to_string(), "be right back".to_string())];
        assert_eq!(
            finalize_transcript("brb, call me", &lm, &words, &snips),
            "be right back, call me"
        );
    }

    #[test]
    fn pipeline_rejects_marker_with_star_index() {
        let server = FakeCleanup::serve(1, |t| {
            let cleaned = t.replace("call me", "ring me");
            if let Some((_, _, text)) = scan_placeholders(t).first() {
                let core = text.strip_suffix("__").unwrap_or(text);
                let cut = core.rfind('_').unwrap_or(core.len());
                format!("{} {}_*x*_", cleaned, &core[..cut])
            } else {
                cleaned
            }
        });
        let lm = server.settings();
        let words: Vec<String> = Vec::new();
        let snips = vec![("brb".to_string(), "be right back".to_string())];
        assert_eq!(
            finalize_transcript("brb, call me", &lm, &words, &snips),
            "be right back, call me"
        );
    }

    #[test]
    fn pipeline_rejects_marker_with_spaced_index() {
        let server = FakeCleanup::serve(1, |t| {
            let cleaned = t.replace("call me", "ring me");
            if let Some((_, _, text)) = scan_placeholders(t).first() {
                let core = text.strip_suffix("__").unwrap_or(text);
                let cut = core.rfind('_').unwrap_or(core.len());
                format!("{} {}__ 0__", cleaned, &core[..cut])
            } else {
                cleaned
            }
        });
        let lm = server.settings();
        let words: Vec<String> = Vec::new();
        let snips = vec![("brb".to_string(), "be right back".to_string())];
        assert_eq!(
            finalize_transcript("brb, call me", &lm, &words, &snips),
            "be right back, call me"
        );
    }

    #[test]
    fn pipeline_rejects_star_wrapped_marker() {
        let server = FakeCleanup::serve(1, |t| {
            format!(
                "{} **MSG_SNIPPET_nothex_0**",
                t.replace("call me", "ring me")
            )
        });
        let lm = server.settings();
        let words: Vec<String> = Vec::new();
        let snips = vec![("brb".to_string(), "be right back".to_string())];
        assert_eq!(
            finalize_transcript("brb, call me", &lm, &words, &snips),
            "be right back, call me"
        );
    }

    #[test]
    fn pipeline_rejects_whitespace_altered_marker_copies() {
        // Space after the prefix and before the suffix: the stem is
        // intact but the shape is broken, so both must fall back.
        let alters: [fn(&str) -> String; 2] = [
            |text| text.replacen("__MSG_SNIPPET_", "__MSG_SNIPPET_ ", 1),
            |text| format!("{} __", &text[..text.len() - 2]),
        ];
        for alter in alters {
            let server = FakeCleanup::serve(1, move |t| {
                let cleaned = t.replace("call me", "ring me");
                if let Some((_, _, text)) = scan_placeholders(t).first() {
                    format!("{} {}", cleaned, alter(text))
                } else {
                    cleaned
                }
            });
            let lm = server.settings();
            let words: Vec<String> = Vec::new();
            let snips = vec![("brb".to_string(), "be right back".to_string())];
            assert_eq!(
                finalize_transcript("brb, call me", &lm, &words, &snips),
                "be right back, call me"
            );
        }
    }

    #[test]
    fn pipeline_replacements_with_marker_like_text_stay_verbatim() {
        // Restoration never scans what it inserts: marker-like
        // replacement content lands literally in the final text.
        let server = FakeCleanup::serve(2, |t| t.replace("call me", "ring me"));
        let lm = server.settings();
        let words: Vec<String> = Vec::new();
        let snips = vec![(
            "brb".to_string(),
            "be right back __MSG_SNIPPET_deadbeef_0__ ok".to_string(),
        )];
        assert_eq!(
            finalize_transcript("brb, call me", &lm, &words, &snips),
            "be right back __MSG_SNIPPET_deadbeef_0__ ok, ring me"
        );
        let snips = vec![(
            "brb".to_string(),
            "see __MSG_SNIPPET_oops__ now".to_string(),
        )];
        assert_eq!(
            finalize_transcript("brb, call me", &lm, &words, &snips),
            "see __MSG_SNIPPET_oops__ now, ring me"
        );
    }

    #[test]
    fn marker_like_detection_and_token_namespaces() {
        assert!(!contains_marker_like("plain talk, call me"));
        assert!(!contains_marker_like("a code snippet here"));
        assert!(contains_marker_like("__MSG_SNIPPET_ab12_0__"));
        assert!(contains_marker_like("**MSG_SNIPPET_nothex_0**"));
        assert!(contains_marker_like("__MSG_SNIPPET_ab12_0_"));
        assert!(contains_marker_like("__msg_snippet_ab12_0__"));
        assert!(contains_marker_like("say __MSG SNIPPET_ab12_0__ now"));
        assert!(contains_marker_like("__MSG_ SNIPPET_ab12_0__"));
        assert!(contains_marker_like("__MSG\t_\tSNIPPET_ab12_0__"));
        // Fresh namespaces avoid the input hex on every path.
        let raw = "brb here";
        let token = snippet_token(raw).expect("fresh namespace");
        assert_eq!(token.len(), 16);
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!raw.contains(&token));
    }

    #[test]
    fn snippet_token_with_uses_controlled_candidates() {
        // First candidate collides with the input, second is fresh:
        // the fresh one wins without touching the RNG.
        let raw = "brb says cafe1234 loudly";
        let mut pool = vec!["cafe1234".to_string(), "zz".to_string()].into_iter();
        let token = snippet_token_with(raw, &mut || pool.next().unwrap_or_default());
        assert_eq!(token.as_deref(), Some("zz"));
        // An endlessly colliding generator exhausts the attempts and
        // reports None instead of returning an unverified namespace.
        let token = snippet_token_with("brb here", &mut || "brb".to_string());
        assert_eq!(token, None);
    }

    /// Separator-whitespace matrix: the response keeps the intact
    /// expected placeholder, rewrites surrounding text, and appends a
    /// damaged duplicate with whitespace around the separator. Each
    /// must fall back to the once-expanded original.
    #[test]
    fn pipeline_rejects_separator_whitespace_duplicates() {
        let damaged: [fn(&str) -> String; 3] = [
            |text| text.replacen("__MSG_SNIPPET_", "__MSG_ SNIPPET_", 1),
            |text| text.replacen("__MSG_SNIPPET_", "__MSG_\nSNIPPET_", 1),
            |text| text.replacen("__MSG_SNIPPET_", "__MSG\t_\tSNIPPET_", 1),
        ];
        for make_damaged in damaged {
            let server = FakeCleanup::serve(1, move |t| {
                let cleaned = t.replace("call me", "ring me");
                if let Some((_, _, text)) = scan_placeholders(t).first() {
                    format!("{} {}", cleaned, make_damaged(text))
                } else {
                    cleaned
                }
            });
            let lm = server.settings();
            let words: Vec<String> = Vec::new();
            let snips = vec![("brb".to_string(), "be right back".to_string())];
            assert_eq!(
                finalize_transcript("brb, call me", &lm, &words, &snips),
                "be right back, call me"
            );
        }
    }

    #[test]
    fn pipeline_tokens_are_scoped_per_utterance() {
        let server = FakeCleanup::serve(2, |t| t.to_string());
        let lm = server.settings();
        let words: Vec<String> = Vec::new();
        let snips = vec![("go".to_string(), "go now".to_string())];
        assert_eq!(finalize_transcript("go", &lm, &words, &snips), "go now");
        assert_eq!(
            finalize_transcript("go again", &lm, &words, &snips),
            "go now again"
        );
        let transcripts = server.transcripts();
        assert_eq!(transcripts.len(), 2);
        let first = scan_placeholders(&transcripts[0]);
        let second = scan_placeholders(&transcripts[1]);
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_ne!(first[0].2, second[0].2);
    }

    #[test]
    fn pipeline_multiple_overlapping_longest_first() {
        use crate::settings::LanguageModelSettings;
        let off = LanguageModelSettings {
            cleanup_enabled: false,
            ..Default::default()
        };
        let words: Vec<String> = Vec::new();
        let snips = vec![
            ("ask".to_string(), "ASKED".to_string()),
            ("investor ask".to_string(), "INVESTOR ASKED".to_string()),
            ("york".to_string(), "Y".to_string()),
            ("new york".to_string(), "NY".to_string()),
        ];
        assert_eq!(
            finalize_transcript("ask the investor ask now", &off, &words, &snips),
            "ASKED the INVESTOR ASKED now"
        );
        assert_eq!(finalize_transcript("new york", &off, &words, &snips), "NY");
        assert_eq!(
            finalize_transcript("ask ask", &off, &words, &snips),
            "ASKED ASKED"
        );
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
        let net = ServerNet {
            host: "127.0.0.1",
            port_override: None,
        };
        ensure_sherpa_offline(
            &data_dir,
            crate::settings::SherpaBackendSetting::Cpu,
            &dir,
            "en",
            &net,
        )
        .expect("spawn+probe");
        let text =
            sherpa_offline_transcribe(&vec![0.0f32; 16_000], SHERPA_OFFLINE_PORT).expect("decode");
        assert!(text.trim().is_empty(), "silence gave {text:?}");
    }
}
