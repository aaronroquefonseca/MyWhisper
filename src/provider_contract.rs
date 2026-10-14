//! Canonical completed answers. Never deliver thoughts, refusals, or truncated output.
//!
//! R37–R38: capability-aware requests plus provider-specific
//! completed-answer parsing. Every parser accepts only a normally
//! completed reply and returns the concatenated answer text:
//! - Claude: `stop_reason` end_turn/stop_sequence; all `text` blocks
//!   joined, `thinking`/`redacted_thinking` skipped, any other block
//!   (tool_use, server_tool_use, web_search, …) rejected.
//! - Gemini: first candidate `finishReason` STOP; non-thought `text`
//!   parts joined, `thought:true` skipped, non-text parts rejected.
//! - OpenAI-compatible chat: first choice `finish_reason` stop, no
//!   `refusal`, no `tool_calls`, string `content` required.
//! - Ollama `/api/chat` (stream:false): `done:true` plus
//!   `done_reason:"stop"`, `message.content` string required,
//!   `message.thinking` ignored, `message.tool_calls` rejected.
//!
//! Empty completed text is a successful parse (`Ok("")`); the cleanup
//! runner treats it as "try next / fall back" so dictation never goes
//! silent. Truncation (`max_tokens`, `length`, `MAX_TOKENS`, …),
//! refusals, malformed blocks and tool calls are all `Err`, which the
//! runner also turns into fallback — never into partial dictation.
//!
//! Thinking tri-state (owned request side; storage migration is #18):
//! `None` omits the param (model default), `Some(false)` forces off,
//! `Some(true)` forces on via the model's supported mode. Unknown
//! Claude capabilities are an error so the caller falls back instead
//! of sending a 400-invalid budget. Live model/thinking capabilities
//! still need authenticated disposable checks (see R37 live gates).
use serde_json::{Value, json};

/// Claude Messages request. `max_tokens` 16384 leaves room for the
/// 1024 thinking budget plus the answer (budget must be < max_tokens,
/// minimum 1024). Adaptive models decide themselves; enabled models
/// need the explicit budget; unknown models error so the caller keeps
/// the model default instead of sending a rejected request.
///
/// Adaptive (per 2026 docs): 4-6/4-7/4-8, 5.x, Mythos/Fable families.
/// Enabled-only: 4-5 family + 3.7 Sonnet. Everything else → Err.
/// `Some(false)` sends `disabled` (rejected by always-on Fable 5 /
/// Mythos 5 — live gate must confirm; the runner falls back on 400).
pub(crate) fn claude_request(
    model: &str,
    thinking: Option<bool>,
    system: &str,
    user: &str,
) -> Result<Value, String> {
    let mut body = json!({"model":model,"max_tokens":16384,"system":system,"messages":[{"role":"user","content":user}]});
    if let Some(enabled) = thinking {
        if !enabled {
            body["thinking"] = json!({"type":"disabled"});
        } else if [
            "claude-haiku-5",
            "claude-sonnet-5",
            "claude-opus-5",
            "claude-sonnet-4-6",
            "claude-sonnet-4-7",
            "claude-sonnet-4-8",
            "claude-opus-4-6",
            "claude-opus-4-7",
            "claude-opus-4-8",
            "claude-mythos",
            "claude-fable",
        ]
        .iter()
        .any(|prefix| model.starts_with(prefix))
        {
            body["thinking"] = json!({"type":"adaptive"});
        } else if [
            "claude-haiku-4-5",
            "claude-sonnet-4-5",
            "claude-opus-4-5",
            "claude-3-7-sonnet",
        ]
        .iter()
        .any(|prefix| model.starts_with(prefix))
        {
            body["thinking"] = json!({"type":"enabled","budget_tokens":1024});
        } else {
            return Err(
                "thinking capabilities unknown for this Claude model; select model default".into(),
            );
        }
    }
    Ok(body)
}

/// Ollama `/api/chat` request (stream:false, pinned sampling mirrors
/// OpenWhispr's local bridge). Tri-state: `None` omits `think` (model
/// default from `/api/show`), `Some(v)` forces it. String levels
/// ("high"/"low"/…) are model-defined and intentionally not sent here;
/// the settings UI stores only the bool tri-state.
pub(crate) fn ollama_request(model: &str, think: Option<bool>, system: &str, user: &str) -> Value {
    let mut body = json!({
        "model": model,
        "stream": false,
        "options": {
            "temperature": 0.0,
            "top_p": 0.9,
            "top_k": 40,
            "repeat_penalty": 1.1,
        },
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
    });
    if let Some(think) = think {
        body["think"] = Value::Bool(think);
    }
    body
}

/// Gemini `generateContent` request. Tri-state: `None` omits
/// `thinkingConfig` (server default), `Some(true)` = dynamic budget
/// (`-1`), `Some(false)` = off (`0`). `thinkingBudget` is legacy but
/// still accepted; `thinking_level` migration is a live-gate follow-up.
pub(crate) fn gemini_request(think: Option<bool>, system: &str, user: &str) -> Value {
    let mut body = json!({
        "contents": [{
            "parts": [
                {"text": system},
                {"text": user},
            ],
        }],
        "generationConfig": {"temperature": 0.0},
    });
    if let Some(think) = think {
        body["generationConfig"]["thinkingConfig"] =
            json!({"thinkingBudget": if think { -1 } else { 0 }});
    }
    body
}

pub(crate) fn claude_answer(value: &Value) -> Result<String, String> {
    if !matches!(
        value["stop_reason"].as_str(),
        Some("end_turn" | "stop_sequence")
    ) {
        return Err("Claude reply did not complete normally".into());
    }
    let blocks = value["content"]
        .as_array()
        .ok_or("Claude reply has no content blocks")?;
    let mut text = String::new();
    for block in blocks {
        match block["type"].as_str() {
            Some("text") => text.push_str(
                block["text"]
                    .as_str()
                    .ok_or("Claude text block is malformed")?,
            ),
            Some("thinking" | "redacted_thinking") => {}
            _ => return Err("Claude reply contains an unsupported answer block".into()),
        }
    }
    Ok(text)
}

pub(crate) fn gemini_answer(value: &Value) -> Result<String, String> {
    let candidate = value
        .pointer("/candidates/0")
        .ok_or("Gemini reply has no candidate")?;
    if candidate["finishReason"].as_str() != Some("STOP") {
        return Err("Gemini reply did not complete normally".into());
    }
    let parts = candidate
        .pointer("/content/parts")
        .and_then(Value::as_array)
        .ok_or("Gemini reply has no answer parts")?;
    let mut text = String::new();
    for part in parts {
        if part["thought"].as_bool() == Some(true) {
            continue;
        }
        if let Some(answer) = part["text"].as_str() {
            text.push_str(answer);
        } else {
            return Err("Gemini reply contains a non-text answer".into());
        }
    }
    Ok(text)
}

pub(crate) fn chat_answer(value: &Value) -> Result<String, String> {
    let choice = value
        .pointer("/choices/0")
        .ok_or("chat reply has no choice")?;
    if choice["finish_reason"].as_str() != Some("stop")
        || !choice["message"]["refusal"].is_null()
        || !choice["message"]["tool_calls"].is_null()
    {
        return Err("chat reply did not complete normally".into());
    }
    choice["message"]["content"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "chat reply has no text content".into())
}

pub(crate) fn ollama_answer(value: &Value) -> Result<String, String> {
    if value["done"].as_bool() != Some(true) || value["done_reason"].as_str() != Some("stop") {
        return Err("Ollama reply did not complete normally".into());
    }
    let message = value
        .pointer("/message")
        .ok_or("Ollama reply has no message")?;
    if !message["tool_calls"].is_null() {
        return Err("Ollama reply requested tools, not text".into());
    }
    message["content"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "Ollama reply has no text content".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn claude_budget_and_adaptive_modes_match_model_capabilities() {
        for model in [
            "claude-haiku-4-5",
            "claude-sonnet-4-5-20250929",
            "claude-opus-4-5",
            "claude-3-7-sonnet-20250219",
        ] {
            let v = claude_request(model, Some(true), "system", "user").unwrap();
            assert_eq!(v["thinking"]["type"], "enabled");
            assert!(v["thinking"]["budget_tokens"].as_u64().unwrap() >= 1024);
            assert!(v["thinking"]["budget_tokens"].as_u64() < v["max_tokens"].as_u64());
        }
        for model in [
            "claude-haiku-5-5",
            "claude-sonnet-5-20260101",
            "claude-opus-5",
            "claude-sonnet-4-6",
            "claude-sonnet-4-7",
            "claude-sonnet-4-8",
            "claude-opus-4-6",
            "claude-opus-4-7",
            "claude-opus-4-8",
            "claude-mythos-preview",
            "claude-mythos-5",
            "claude-fable-5",
        ] {
            assert_eq!(
                claude_request(model, Some(true), "s", "u").unwrap()["thinking"],
                json!({"type":"adaptive"}),
                "model {model}"
            );
            // Disabled is still sent (API may 400 on always-on models;
            // the runner falls back — live gate must confirm).
            assert_eq!(
                claude_request(model, Some(false), "s", "u").unwrap()["thinking"]["type"],
                "disabled",
                "model {model}"
            );
        }
        // Unknown capabilities must not guess a mode: error so the
        // caller keeps the model default.
        for model in [
            "future-model",
            "claude-sonnet-4-4",
            "claude-opus-4-4",
            "claude-sonnet-4",
            "claude-opus-4",
            "gpt-5",
            "",
        ] {
            assert!(
                claude_request(model, Some(true), "s", "u").is_err(),
                "model {model}"
            );
        }
        assert!(
            claude_request("future-model", None, "s", "u")
                .unwrap()
                .get("thinking")
                .is_none()
        );
        // Tri-state: None omits everywhere.
        assert!(
            claude_request("claude-sonnet-4-5", None, "s", "u")
                .unwrap()
                .get("thinking")
                .is_none()
        );
    }
    #[test]
    fn ollama_and_gemini_tristate_omit_or_force() {
        for think in [None, Some(true), Some(false)] {
            let v = ollama_request("qwen3", think, "sys", "user");
            match think {
                None => assert!(v.get("think").is_none()),
                Some(b) => assert_eq!(v["think"], json!(b)),
            }
            assert_eq!(v["stream"], json!(false));
            let g = gemini_request(think, "sys", "user");
            match think {
                None => assert!(g["generationConfig"].get("thinkingConfig").is_none()),
                Some(true) => assert_eq!(
                    g["generationConfig"]["thinkingConfig"],
                    json!({"thinkingBudget": -1})
                ),
                Some(false) => assert_eq!(
                    g["generationConfig"]["thinkingConfig"],
                    json!({"thinkingBudget": 0})
                ),
            }
        }
    }
    #[test]
    fn thoughts_never_become_answers_and_partial_replies_are_rejected() {
        let mut claude = json!({"stop_reason":"end_turn","content":[{"type":"thinking","thinking":"secret"},{"type":"text","text":"Hello "},{"type":"text","text":"世界"}]});
        assert_eq!(claude_answer(&claude).unwrap(), "Hello 世界");
        claude["stop_reason"] = json!("max_tokens");
        assert!(claude_answer(&claude).is_err());
        let mut gemini = json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"thought":true,"text":"secret"},{"text":"Answer"}]}}]});
        assert_eq!(gemini_answer(&gemini).unwrap(), "Answer");
        gemini["candidates"][0]["finishReason"] = json!("MAX_TOKENS");
        assert!(gemini_answer(&gemini).is_err());
        for reason in ["length", "content_filter", "tool_calls", ""] {
            assert!(
                chat_answer(
                    &json!({"choices":[{"finish_reason":reason,"message":{"content":"partial"}}]})
                )
                .is_err()
            );
        }
        assert!(
            ollama_answer(
                &json!({"done":false,"done_reason":"stop","message":{"content":"partial"}})
            )
            .is_err()
        );
        // Missing done_reason is also incomplete.
        assert!(ollama_answer(&json!({"done":true,"message":{"content":"x"}})).is_err());
    }
    #[test]
    fn refusals_tool_calls_and_malformed_blocks_are_rejected() {
        // Claude: tool_use / unknown blocks never become answers;
        // thinking/redacted_thinking are skipped.
        assert!(
            claude_answer(
                &json!({"stop_reason":"end_turn","content":[{"type":"tool_use","id":"1"}]})
            )
            .is_err()
        );
        assert!(
            claude_answer(
                &json!({"stop_reason":"end_turn","content":[{"type":"server_tool_use"}]})
            )
            .is_err()
        );
        assert!(
            claude_answer(&json!({"stop_reason":"end_turn","content":[{"type":"text"}]})).is_err()
        );
        assert!(
            claude_answer(&json!({"stop_reason":"refusal","content":[{"type":"text","text":"x"}]}))
                .is_err()
        );
        assert_eq!(
            claude_answer(
                &json!({"stop_reason":"end_turn","content":[{"type":"redacted_thinking","data":"x"},{"type":"text","text":"ok"}]})
            )
            .unwrap(),
            "ok"
        );
        // Gemini: functionCall / non-text parts rejected even on STOP.
        assert!(
            gemini_answer(
                &json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"functionCall":{"name":"f"}}]}}]})
            )
            .is_err()
        );
        assert!(gemini_answer(&json!({"candidates":[]})).is_err());
        // Chat: refusal string, null content, or tool_calls rejected.
        assert!(chat_answer(
            &json!({"choices":[{"finish_reason":"stop","message":{"content":null,"refusal":null}}]})
        )
        .is_err());
        assert!(chat_answer(
            &json!({"choices":[{"finish_reason":"stop","message":{"content":"x","refusal":"no"}}]})
        )
        .is_err());
        assert!(chat_answer(
            &json!({"choices":[{"finish_reason":"stop","message":{"content":"x","tool_calls":[]}}]})
        )
        .is_err());
        assert!(
            chat_answer(
                &json!({"choices":[{"finish_reason":"tool_calls","message":{"content":"x"}}]})
            )
            .is_err()
        );
        // Ollama: tool_calls rejected, thinking ignored, content required.
        assert!(
            ollama_answer(
                &json!({"done":true,"done_reason":"stop","message":{"content":"x","tool_calls":[]}})
            )
            .is_err()
        );
        assert_eq!(
            ollama_answer(
                &json!({"done":true,"done_reason":"stop","message":{"content":"hi 🎙️","thinking":"secret"}})
            )
            .unwrap(),
            "hi 🎙️"
        );
        assert!(
            ollama_answer(&json!({"done":true,"done_reason":"stop","message":{"content":null}}))
                .is_err()
        );
        assert!(ollama_answer(&json!({"done":true,"done_reason":"stop"})).is_err());
    }
    #[test]
    fn long_unicode_answers_concatenate_without_truncation() {
        let chunk = "世界🎙️café ";
        let long: String = chunk.repeat(2000);
        // Split at a char boundary (midpoint backed up to one).
        let mut mid = long.len() / 2;
        while !long.is_char_boundary(mid) {
            mid -= 1;
        }
        let claude = json!({"stop_reason":"end_turn","content":[{"type":"text","text":&long[..mid]},{"type":"text","text":&long[mid..]}]});
        assert_eq!(claude_answer(&claude).unwrap(), long);
        let mut gmid = 100;
        while !long.is_char_boundary(gmid) {
            gmid -= 1;
        }
        let gemini = json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":&long[..gmid]},{"thought":true,"text":"secret"},{"text":&long[gmid..]}]}}]});
        assert_eq!(gemini_answer(&gemini).unwrap(), long);
        let chat = json!({"choices":[{"finish_reason":"stop","message":{"content":long}}]});
        assert_eq!(chat_answer(&chat).unwrap(), long);
        let ollama = json!({"done":true,"done_reason":"stop","message":{"content":long}});
        // Re-borrow to avoid move issues in the assertions above.
        let expected = chunk.repeat(2000);
        assert_eq!(
            chat_answer(
                &json!({"choices":[{"finish_reason":"stop","message":{"content":expected}}]})
            )
            .unwrap(),
            expected
        );
        assert_eq!(ollama_answer(&ollama).unwrap(), expected);
    }

    #[test]
    fn completed_policy_empty_answers_are_successes() {
        assert_eq!(
            claude_answer(&json!({"stop_reason":"end_turn","content":[{"type":"text","text":""}]}))
                .unwrap(),
            ""
        );
        assert_eq!(
            gemini_answer(
                &json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":""}]}}]})
            )
            .unwrap(),
            ""
        );
        assert_eq!(
            chat_answer(&json!({"choices":[{"finish_reason":"stop","message":{"content":""}}]}))
                .unwrap(),
            ""
        );
        assert_eq!(
            ollama_answer(&json!({"done":true,"done_reason":"stop","message":{"content":""}}))
                .unwrap(),
            ""
        );
    }

    #[test]
    fn malformed_unsupported_and_refusal_replies_are_rejected() {
        // Claude: missing blocks, malformed text, unsupported tool blocks.
        assert!(claude_answer(&json!({"stop_reason":"end_turn"})).is_err());
        assert!(
            claude_answer(&json!({"stop_reason":"end_turn","content":[{"type":"text"}]})).is_err()
        );
        assert!(
            claude_answer(
                &json!({"stop_reason":"end_turn","content":[{"type":"tool_use","id":"1"}]})
            )
            .is_err()
        );
        assert!(
            claude_answer(
                &json!({"stop_reason":"tool_use","content":[{"type":"text","text":"x"}]})
            )
            .is_err()
        );
        // Redacted thinking is skipped, never delivered.
        assert_eq!(
            claude_answer(
                &json!({"stop_reason":"end_turn","content":[{"type":"redacted_thinking","data":"s"},{"type":"text","text":"ok"}]})
            )
            .unwrap(),
            "ok"
        );
        // Gemini: missing candidate/parts, non-text function payloads.
        assert!(gemini_answer(&json!({})).is_err());
        assert!(
            gemini_answer(&json!({"candidates":[{"finishReason":"STOP","content":{}}]})).is_err()
        );
        assert!(gemini_answer(
            &json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"functionCall":{"name":"f"}}]}}]})
        )
        .is_err());
        assert!(gemini_answer(
            &json!({"candidates":[{"finishReason":"SAFETY","content":{"parts":[{"text":"x"}]}}]})
        )
        .is_err());
        // Chat: refusal strings, null content, missing choice.
        assert!(chat_answer(
            &json!({"choices":[{"finish_reason":"stop","message":{"content":"x","refusal":"r"}}]})
        )
        .is_err());
        assert!(
            chat_answer(&json!({"choices":[{"finish_reason":"stop","message":{"content":null}}]}))
                .is_err()
        );
        assert!(chat_answer(&json!({})).is_err());
        // Ollama: wrong done flags, missing content.
        assert!(
            ollama_answer(&json!({"done":true,"done_reason":"load","message":{"content":"x"}}))
                .is_err()
        );
        assert!(ollama_answer(&json!({"done":true,"done_reason":"stop","message":{}})).is_err());
        assert!(ollama_answer(&json!({"done":true,"message":{"content":"x"}})).is_err());
    }

    #[test]
    fn claude_tri_state_thinking_is_exhaustive() {
        // None omits the block on every model (server default).
        for model in ["claude-haiku-4-5", "claude-opus-4-7", "future-model"] {
            assert!(
                claude_request(model, None, "s", "u")
                    .unwrap()
                    .get("thinking")
                    .is_none()
            );
        }
        // Some(false) disables thinking on every model, even unknown ones.
        for model in ["claude-sonnet-4-5", "future-model"] {
            assert_eq!(
                claude_request(model, Some(false), "s", "u").unwrap()["thinking"]["type"],
                "disabled"
            );
        }
        // Request echoes model/system/user untouched.
        let v = claude_request("claude-sonnet-4-5", None, "sys", "usr").unwrap();
        assert_eq!(v["model"], "claude-sonnet-4-5");
        assert_eq!(v["system"], "sys");
        assert_eq!(v["messages"][0]["content"], "usr");
    }
}
