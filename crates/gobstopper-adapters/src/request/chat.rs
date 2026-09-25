//! OpenAI Chat Completions dialect (`/chat/completions`).
//!
//! Served by OpenAI-compatible providers and clients that resend the whole
//! `messages` array: opencode's `openai-compatible` provider, Crush's
//! `openai-compat` type, Aider, and Goose's `openai` engine. There is no
//! top-level system field; `system`/`developer` messages ride inside the
//! list and stay in the verbatim head like every other pre-model message.
//! An assistant turn may carry several `tool_calls`; each is answered by a
//! `tool` message echoing its `tool_call_id`.

use super::{
    canonical_json, digest_value, sha256_hex, str_field, strip_task_notifications, truncate,
    CliffConfig, SUMMARY_HEADER,
};
use serde_json::{json, Value};

/// Part text for content arrays: `text` parts carry the words; `image_url`,
/// `input_audio` and `file` parts are payloads, not text.
fn part_text(part: &Value) -> Option<&str> {
    match str_field(part, "type") {
        "text" => Some(str_field(part, "text")),
        "refusal" => Some(str_field(part, "refusal")),
        _ => None,
    }
}

fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|part| part.is_object())
            .filter_map(part_text)
            .collect::<Vec<_>>()
            .join("\n"),
        None | Some(Value::Null) => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Payload-bearing parts hash to their contents so prefix identity survives
/// reserialization; opaque parts keep their full canonical form.
fn canonical_part(part: &Value) -> Value {
    let kind = str_field(part, "type");
    match kind {
        "text" => json!(["text", str_field(part, "text")]),
        "refusal" => json!(["refusal", str_field(part, "refusal")]),
        "image_url" => {
            let url = part.get("image_url").unwrap_or(&Value::Null);
            let url = url.get("url").cloned().unwrap_or_else(|| url.clone());
            let payload = match &url {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            json!(["image_url", sha256_hex(payload.as_bytes())])
        }
        "input_audio" => {
            let audio = part.get("input_audio").unwrap_or(&Value::Null);
            let data = audio.get("data").and_then(Value::as_str).unwrap_or("");
            json!(["input_audio", sha256_hex(data.as_bytes())])
        }
        _ => json!(["other", canonical_json(part)]),
    }
}

fn canonical_content(content: Option<&Value>) -> Value {
    match content {
        Some(Value::String(text)) => json!([["text", text]]),
        Some(Value::Array(parts)) => Value::Array(
            parts
                .iter()
                .filter(|part| part.is_object())
                .map(canonical_part)
                .collect(),
        ),
        _ => json!([]),
    }
}

/// `tool_calls` reduce to (id, name, arguments); `reasoning_content` folds
/// in as thinking text for providers that split reasoning out of content.
fn canonical_calls(message: &Value) -> Vec<Value> {
    message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .filter(|call| call.is_object())
                .map(|call| {
                    let function = call.get("function").unwrap_or(&Value::Null);
                    json!([
                        str_field(call, "id"),
                        str_field(function, "name"),
                        function
                            .get("arguments")
                            .map(|args| match args {
                                Value::String(text) => text.clone(),
                                other => canonical_json(other),
                            })
                            .unwrap_or_default(),
                    ])
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn digest_message(message: &Value) -> String {
    digest_value(&json!([
        str_field(message, "role"),
        canonical_content(message.get("content")),
        canonical_calls(message),
        str_field(message, "tool_call_id"),
        str_field(message, "reasoning_content"),
        str_field(message, "name"),
    ]))
}

pub fn is_assistant(message: &Value) -> bool {
    str_field(message, "role") == "assistant"
}

pub fn is_summary_message(message: &Value) -> bool {
    str_field(message, "role") == "user"
        && content_text(message.get("content")).starts_with(SUMMARY_HEADER)
}

fn human_part(text: &str, cfg: &CliffConfig) -> Option<String> {
    if text.starts_with(SUMMARY_HEADER) {
        return None;
    }
    let text = strip_task_notifications(text);
    let text = text.trim();
    (!text.is_empty()).then(|| format!("user: {}", truncate(text, cfg.human_max_chars)))
}

/// An assistant message contributes its text, its reasoning text, and one
/// signature line per tool call; `tool_call_id`s are bookkeeping, not prose.
fn summarize_assistant(message: &Value, cfg: &CliffConfig) -> Vec<String> {
    let mut lines = Vec::new();
    if cfg.keep_thinking {
        let thinking = str_field(message, "reasoning_content").trim();
        if !thinking.is_empty() {
            lines.push(format!(
                "thinking: {}",
                truncate(thinking, cfg.thinking_max_chars)
            ));
        }
    }
    let text = content_text(message.get("content"));
    let text = text.trim();
    if !text.is_empty() {
        lines.push(format!(
            "assistant: {}",
            truncate(text, cfg.thought_max_chars)
        ));
    }
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls.iter().filter(|call| call.is_object()) {
            let function = call.get("function").unwrap_or(&Value::Null);
            let name = match str_field(function, "name") {
                "" => str_field(call, "type"),
                name => name,
            };
            let args = function
                .get("arguments")
                .map(|args| match args {
                    Value::String(text) => text.clone(),
                    other => canonical_json(other),
                })
                .unwrap_or_default();
            lines.push(format!("[{name}] {}", truncate(&args, cfg.cmd_max_chars)));
        }
    }
    if lines.is_empty() {
        Vec::new()
    } else {
        vec![lines.join("\n")]
    }
}

pub fn summarize_message(message: &Value, cfg: &CliffConfig) -> Vec<String> {
    match str_field(message, "role") {
        "assistant" => summarize_assistant(message, cfg),
        "user" => match message.get("content") {
            Some(Value::String(text)) => human_part(text, cfg).into_iter().collect(),
            Some(Value::Array(parts)) => parts
                .iter()
                .filter(|part| part.is_object())
                .filter_map(|part| human_part(part_text(part)?, cfg))
                .collect(),
            _ => Vec::new(),
        },
        // `system`/`developer` and any other in-list directive fold like
        // instructions; empty ones disappear.
        "system" | "developer" => {
            let text = content_text(message.get("content"));
            let text = text.trim();
            if text.is_empty() {
                Vec::new()
            } else {
                vec![format!("system: {}", truncate(text, cfg.human_max_chars))]
            }
        }
        "tool" => {
            let text = content_text(message.get("content"));
            let text = text.trim();
            if !text.is_empty() && text.chars().count() <= cfg.result_max_chars {
                vec![format!("result: {text}")]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

pub fn user_message(text: String) -> Value {
    json!({"role": "user", "content": text})
}

#[cfg(test)]
mod tests {
    use super::super::{compact, Dialect};
    use super::*;

    fn cfg(keep_recent: usize) -> CliffConfig {
        CliffConfig {
            keep_recent,
            ..CliffConfig::default()
        }
    }

    fn user(text: &str) -> Value {
        json!({"role": "user", "content": text})
    }

    fn assistant(text: &str, calls: usize) -> Value {
        if calls == 0 {
            return json!({"role": "assistant", "content": text});
        }
        json!({"role": "assistant", "content": text, "tool_calls": (0..calls).map(|i| json!({
            "id": format!("call_{i}"),
            "type": "function",
            "function": {"name": "bash", "arguments": format!("{{\"command\":\"step {i}\"}}")},
        })).collect::<Vec<_>>()})
    }

    fn tool(n: usize, text: &str) -> Value {
        json!({"role": "tool", "tool_call_id": format!("call_{n}"), "content": text})
    }

    fn session(steps: usize, result_chars: usize) -> Vec<Value> {
        let mut messages = vec![
            json!({"role": "system", "content": "you are an agent"}),
            user("fix the bug"),
        ];
        for i in 0..steps {
            messages.push(assistant(&format!("step {i}: running"), 1));
            messages.push(tool(0, &"R".repeat(result_chars)));
        }
        messages
    }

    #[test]
    fn assistant_turns_pair_with_their_tool_results() {
        let messages = session(6, 2000);
        let result = compact(&messages, Dialect::ChatCompletions, &cfg(2)).unwrap();
        assert_eq!(result.head_len, 2);
        let summary = content_text(result.summary.get("content"));
        assert!(summary.starts_with(SUMMARY_HEADER));
        assert!(summary.contains("assistant: step 0: running"));
        assert!(summary.contains("[bash] {\"command\":\"step 0\"}"));
        assert!(!summary.contains("RRRR"));
        // Head + summary + two kept turns of (assistant, tool).
        assert_eq!(result.messages.len(), 2 + 1 + 4);
        assert_eq!(result.messages[result.messages.len() - 1]["role"], "tool");
    }

    #[test]
    fn adjacent_assistant_messages_form_one_turn() {
        let mut messages = session(4, 2000);
        // A second assistant message right after another stays in the same
        // step, so the kept tail cannot split a multi-message turn.
        messages.push(json!({"role": "assistant", "content": "part one"}));
        messages.push(json!({"role": "assistant", "content": "part two"}));
        let result = compact(&messages, Dialect::ChatCompletions, &cfg(1)).unwrap();
        let tail = &result.messages[result.head_len + 1..];
        assert_eq!(tail[0]["content"], "part one");
        assert_eq!(tail[1]["content"], "part two");
    }

    #[test]
    fn digest_tracks_calls_and_reasoning_but_not_envelope() {
        let plain = json!({"role": "assistant", "content": "hi"});
        assert_eq!(digest_message(&plain), digest_message(&plain));
        let with_call = json!({"role": "assistant", "content": "hi", "tool_calls": [
            {"id": "c", "type": "function", "function": {"name": "x", "arguments": "{}"}}
        ]});
        assert_ne!(digest_message(&plain), digest_message(&with_call));
        let reasoned = json!({"role": "assistant", "content": "hi", "reasoning_content": "hmm"});
        assert_ne!(digest_message(&plain), digest_message(&reasoned));
        let part_image = json!({"role": "user", "content": [
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}
        ]});
        let other_image = json!({"role": "user", "content": [
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,BBB"}}
        ]});
        assert_ne!(digest_message(&part_image), digest_message(&other_image));
    }

    #[test]
    fn prior_summary_is_recognized_and_never_folded() {
        let summary = user_message(format!("{SUMMARY_HEADER}\n\nuser: old"));
        assert!(is_summary_message(&summary));
        assert!(summarize_message(&summary, &cfg(3)).is_empty());
        assert!(!is_summary_message(&user("normal")));
        // Re-compacting a grown history replaces the prior summary rather
        // than nesting it.
        let mut grown = compact(&session(6, 2000), Dialect::ChatCompletions, &cfg(1))
            .unwrap()
            .messages;
        for i in 6..10 {
            grown.push(assistant(&format!("step {i}: running"), 1));
            grown.push(tool(0, &"N".repeat(2000)));
        }
        let second = compact(&grown, Dialect::ChatCompletions, &cfg(1)).unwrap();
        let found: Vec<_> = second
            .messages
            .iter()
            .filter(|m| is_summary_message(m))
            .collect();
        assert_eq!(found.len(), 1);
        assert!(!content_text(found[0].get("content")).contains("user: old"));
        assert!(content_text(found[0].get("content")).contains("step 8: running"));
    }

    #[test]
    fn short_tool_results_stay_and_long_ones_drop() {
        let mut messages = session(4, 2000);
        messages.insert(4, tool(9, "all 5 tests passed"));
        let result = compact(&messages, Dialect::ChatCompletions, &cfg(1)).unwrap();
        let summary = content_text(result.summary.get("content"));
        assert!(summary.contains("result: all 5 tests passed"));
    }
}
