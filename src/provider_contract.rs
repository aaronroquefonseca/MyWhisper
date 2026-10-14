//! Canonical completed answers. Never deliver thoughts, refusals, or truncated output.
use serde_json::{Value, json};

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
        } else if model.starts_with("claude-haiku-5-5")
            || model.starts_with("claude-sonnet-5")
            || model.starts_with("claude-opus-5")
            || model.starts_with("claude-sonnet-4-6")
            || model.starts_with("claude-opus-4-6")
            || model.starts_with("claude-opus-4-7")
            || model.starts_with("claude-mythos")
        {
            body["thinking"] = json!({"type":"adaptive"});
        } else if [
            "claude-haiku-4-5",
            "claude-sonnet-4-5",
            "claude-opus-4-5",
            "claude-sonnet-4-",
            "claude-opus-4-",
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
    if choice["finish_reason"].as_str() != Some("stop") || !choice["message"]["refusal"].is_null() {
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
    value
        .pointer("/message/content")
        .and_then(Value::as_str)
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
        ] {
            let v = claude_request(model, Some(true), "system", "user").unwrap();
            assert_eq!(v["thinking"]["type"], "enabled");
            assert!(v["thinking"]["budget_tokens"].as_u64().unwrap() >= 1024);
            assert!(v["thinking"]["budget_tokens"].as_u64() < v["max_tokens"].as_u64());
        }
        for model in [
            "claude-haiku-5-5",
            "claude-sonnet-4-6",
            "claude-opus-4-7",
            "claude-sonnet-5",
        ] {
            assert_eq!(
                claude_request(model, Some(true), "s", "u").unwrap()["thinking"],
                json!({"type":"adaptive"})
            );
            assert_eq!(
                claude_request(model, Some(false), "s", "u").unwrap()["thinking"]["type"],
                "disabled"
            );
        }
        assert!(claude_request("future-model", Some(true), "s", "u").is_err());
        assert!(
            claude_request("future-model", None, "s", "u")
                .unwrap()
                .get("thinking")
                .is_none()
        );
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
