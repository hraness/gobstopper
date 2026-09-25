//! Anthropic Messages dialect (Claude Code).
//!
//! `system` is a top-level request field, never touched and not part of the
//! hash chain. Tool results are user-role messages with `tool_result`
//! blocks; one user message may mix them with human text. `cache_control`
//! markers move between requests and thinking signatures are provider
//! metadata, so both are excluded from digests.

use super::{
    canonical_json, digest_value, sha256_hex, str_field, strip_task_notifications, truncate,
    CliffConfig, SUMMARY_HEADER,
};
use serde_json::{json, Value};

fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| match part {
                Value::String(text) => Some(text.as_str()),
                Value::Object(_) if part.get("type").and_then(Value::as_str) == Some("text") => {
                    Some(str_field(part, "text"))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        None | Some(Value::Null) => String::new(),
        Some(other) => other.to_string(),
    }
}

fn canonical_block(block: &Value) -> Value {
    let kind = str_field(block, "type");
    match kind {
        "text" => json!(["text", str_field(block, "text")]),
        "tool_use" => json!([
            "tool_use",
            str_field(block, "id"),
            str_field(block, "name"),
            canonical_json(block.get("input").unwrap_or(&json!({}))),
        ]),
        "tool_result" => json!([
            "tool_result",
            str_field(block, "tool_use_id"),
            result_text(block.get("content")),
            block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ]),
        "thinking" => json!(["thinking", str_field(block, "thinking")]),
        "redacted_thinking" => json!(["redacted_thinking", str_field(block, "data")]),
        "image" | "document" => {
            let source = block.get("source").cloned().unwrap_or(Value::Null);
            let payload = source
                .get("data")
                .or_else(|| source.get("url"))
                .map(|value| match value {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            json!([
                kind,
                str_field(&source, "type"),
                sha256_hex(payload.as_bytes())
            ])
        }
        _ => {
            let mut reduced = block.clone();
            if let Some(map) = reduced.as_object_mut() {
                map.remove("cache_control");
            }
            json!(["other", canonical_json(&reduced)])
        }
    }
}

pub fn digest_message(message: &Value) -> String {
    let blocks: Vec<Value> = match message.get("content") {
        Some(Value::String(text)) => vec![json!(["text", text])],
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.is_object())
            .map(canonical_block)
            .collect(),
        _ => Vec::new(),
    };
    digest_value(&json!([str_field(message, "role"), blocks]))
}

pub fn is_assistant(message: &Value) -> bool {
    str_field(message, "role") == "assistant"
}

pub fn is_summary_message(message: &Value) -> bool {
    if str_field(message, "role") != "user" {
        return false;
    }
    match message.get("content") {
        Some(Value::String(text)) => text.starts_with(SUMMARY_HEADER),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .find(|block| str_field(block, "type") == "text")
            .is_some_and(|block| str_field(block, "text").starts_with(SUMMARY_HEADER)),
        _ => false,
    }
}

fn summarize_assistant(message: &Value, cfg: &CliffConfig) -> Vec<String> {
    let mut texts = Vec::new();
    let mut thinkings = Vec::new();
    let mut signatures = Vec::new();
    match message.get("content") {
        Some(Value::String(text)) => texts.push(text.as_str()),
        Some(Value::Array(blocks)) => {
            for block in blocks {
                match str_field(block, "type") {
                    "text" => texts.push(str_field(block, "text")),
                    // Kept as text: a signed block is never re-sent from the
                    // compacted region, only its content.
                    "thinking" if cfg.keep_thinking => thinkings.push(str_field(block, "thinking")),
                    "tool_use" => {
                        let args = canonical_json(block.get("input").unwrap_or(&json!({})));
                        let name = block.get("name").and_then(Value::as_str).unwrap_or("?");
                        signatures.push(format!("[{name}] {}", truncate(&args, cfg.cmd_max_chars)));
                    }
                    // Redacted thinking and images are dropped from summaries.
                    _ => {}
                }
            }
        }
        _ => {}
    }
    let join_nonblank = |parts: &[&str]| {
        parts
            .iter()
            .filter(|part| !part.trim().is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    };
    let mut lines = Vec::new();
    let thinking = truncate(&join_nonblank(&thinkings), cfg.thinking_max_chars);
    if !thinking.is_empty() {
        lines.push(format!("thinking: {thinking}"));
    }
    let thought = truncate(&join_nonblank(&texts), cfg.thought_max_chars);
    if !thought.is_empty() {
        lines.push(format!("assistant: {thought}"));
    }
    if !signatures.is_empty() {
        lines.push(signatures.join("\n"));
    }
    if lines.is_empty() {
        Vec::new()
    } else {
        vec![lines.join("\n")]
    }
}

fn human_part(text: &str, cfg: &CliffConfig) -> Option<String> {
    if text.starts_with(SUMMARY_HEADER) {
        return None;
    }
    let text = strip_task_notifications(text);
    let text = text.trim();
    (!text.is_empty()).then(|| format!("user: {}", truncate(text, cfg.human_max_chars)))
}

/// Human text verbatim (sanity-capped); tool results kept only when short;
/// a prior summary dropped entirely rather than merged forward.
fn summarize_user(message: &Value, cfg: &CliffConfig) -> Vec<String> {
    match message.get("content") {
        Some(Value::String(text)) => human_part(text, cfg).into_iter().collect(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| match str_field(block, "type") {
                "text" => human_part(str_field(block, "text"), cfg),
                "tool_result" => {
                    let text = result_text(block.get("content"));
                    let text = text.trim();
                    (!text.is_empty() && text.chars().count() <= cfg.result_max_chars)
                        .then(|| format!("result: {text}"))
                }
                // Images and documents in the compacted region are dropped.
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub fn summarize_message(message: &Value, cfg: &CliffConfig) -> Vec<String> {
    match str_field(message, "role") {
        "assistant" => summarize_assistant(message, cfg),
        "user" => summarize_user(message, cfg),
        // In-array system directives: content-ful ones fold like
        // instructions; directive-only forms (`content: []`) disappear.
        "system" => {
            let text = match message.get("content") {
                Some(Value::String(text)) => text.clone(),
                Some(Value::Array(blocks)) => blocks
                    .iter()
                    .filter(|block| str_field(block, "type") == "text")
                    .map(|block| str_field(block, "text"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            let text = text.trim();
            if text.is_empty() {
                Vec::new()
            } else {
                vec![format!("system: {}", truncate(text, cfg.human_max_chars))]
            }
        }
        _ => Vec::new(),
    }
}

pub fn user_message(text: String) -> Value {
    json!({"role": "user", "content": text})
}

/// A content-ful system message must precede an assistant message or end
/// the array, so it may not sit before the injected user-role summary.
pub fn trim_from_head(message: &Value) -> bool {
    str_field(message, "role") == "system"
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::*;
    use super::*;

    fn cfg() -> CliffConfig {
        CliffConfig::default()
    }

    #[test]
    fn digest_ignores_cache_control_and_signatures() {
        let plain = json!({"role": "user", "content": [
            {"type": "text", "text": "hi"},
            {"type": "custom", "value": 1}
        ]});
        let marked = json!({"role": "user", "content": [
            {"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}},
            {"type": "custom", "value": 1, "cache_control": {"type": "ephemeral"}}
        ]});
        assert_eq!(digest_message(&plain), digest_message(&marked));
        let signed = json!({"role": "assistant", "content": [
            {"type": "thinking", "thinking": "hmm", "signature": "abc"}
        ]});
        let resigned = json!({"role": "assistant", "content": [
            {"type": "thinking", "thinking": "hmm", "signature": "xyz"}
        ]});
        assert_eq!(digest_message(&signed), digest_message(&resigned));
    }

    #[test]
    fn string_and_block_content_digest_the_same_and_content_changes_it() {
        let string = a_user("hello");
        let blocks = json!({"role": "user", "content": [{"type": "text", "text": "hello"}]});
        assert_eq!(digest_message(&string), digest_message(&blocks));
        assert_ne!(digest_message(&string), digest_message(&a_user("hello!")));
        let short = a_result("t1", "ok");
        let listed = json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": [{"type": "text", "text": "ok"}]}
        ]});
        assert_eq!(digest_message(&short), digest_message(&listed));
    }

    #[test]
    fn mixed_user_message_keeps_human_text_and_short_results_only() {
        let message = json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "a", "content": "short output"},
            {"type": "tool_result", "tool_use_id": "b", "content": "L".repeat(501)},
            {"type": "text", "text": "  please also check the logs  "},
            {"type": "image", "source": {"type": "base64", "data": "AAAA"}}
        ]});
        assert_eq!(
            summarize_message(&message, &cfg()),
            vec![
                "result: short output".to_string(),
                "user: please also check the logs".to_string()
            ]
        );
    }

    #[test]
    fn assistant_summary_keeps_thinking_text_and_truncates_signatures() {
        let message = json!({"role": "assistant", "content": [
            {"type": "thinking", "thinking": "consider the parser", "signature": "s"},
            {"type": "text", "text": "Reading the parser."},
            {"type": "tool_use", "id": "t", "name": "read", "input": {"path": "p".repeat(200)}}
        ]});
        let parts = summarize_message(&message, &cfg());
        assert_eq!(parts.len(), 1);
        let lines: Vec<&str> = parts[0].lines().collect();
        assert_eq!(lines[0], "thinking: consider the parser");
        assert_eq!(lines[1], "assistant: Reading the parser.");
        assert!(lines[2].starts_with("[read] {\"path\":\"ppp"));
        assert!(lines[2].ends_with("..."));
        let lean = CliffConfig {
            keep_thinking: false,
            ..cfg()
        };
        assert!(!summarize_message(&message, &lean)[0].contains("thinking:"));
    }

    #[test]
    fn prior_summary_is_recognized_and_never_folded() {
        let summary = user_message(format!("{SUMMARY_HEADER}\n\nuser: old"));
        assert!(is_summary_message(&summary));
        assert!(summarize_message(&summary, &cfg()).is_empty());
        assert!(!is_summary_message(&a_user("normal")));
    }
}
