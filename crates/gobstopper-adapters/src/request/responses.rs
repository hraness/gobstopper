//! OpenAI Responses dialect (Codex and other stateless clients that resend
//! the whole `input`).
//!
//! The system prompt rides in `instructions`, which is never touched. A
//! model step spans several items (`reasoning`, then a message or a tool
//! call), followed by tool outputs; providers reject a tool call whose
//! paired reasoning item is missing, so turn grouping keeps contiguous
//! model-output runs together. Item `id` and `status` are excluded from
//! digests: some clients strip ids, and status is lifecycle metadata.

use super::{
    canonical_json, digest_value, str_field, strip_task_notifications, truncate, CliffConfig,
    SUMMARY_HEADER,
};
use serde_json::{json, Value};

const MODEL_ITEM_TYPES: &[&str] = &[
    "reasoning",
    "function_call",
    "custom_tool_call",
    "local_shell_call",
    "web_search_call",
    "tool_search_call",
    "image_generation_call",
];

fn item_type(item: &Value) -> &str {
    match item.get("type").and_then(Value::as_str) {
        Some(kind) if !kind.is_empty() => kind,
        _ if item.get("role").is_some() => "message",
        _ => "other",
    }
}

fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|part| {
                matches!(
                    str_field(part, "type"),
                    "input_text" | "output_text" | "text"
                )
            })
            .map(|part| str_field(part, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        None | Some(Value::Null) => String::new(),
        Some(other) => other.to_string(),
    }
}

fn canonical_content(content: Option<&Value>) -> Value {
    match content {
        Some(Value::String(text)) => json!([["text", text]]),
        Some(Value::Array(parts)) => Value::Array(
            parts
                .iter()
                .filter(|part| part.is_object())
                .map(|part| match str_field(part, "type") {
                    "input_text" | "output_text" | "text" => {
                        json!(["text", str_field(part, "text")])
                    }
                    _ => json!(["other", canonical_json(part)]),
                })
                .collect(),
        ),
        _ => json!([]),
    }
}

fn string_or_canonical(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(other) => canonical_json(other),
        None => String::new(),
    }
}

pub fn digest_message(item: &Value) -> String {
    let kind = item_type(item);
    let canonical = match kind {
        "message" => json!([
            "message",
            str_field(item, "role"),
            canonical_content(item.get("content"))
        ]),
        "reasoning" => json!([
            "reasoning",
            str_field(item, "encrypted_content"),
            canonical_json(item.get("summary").unwrap_or(&json!([]))),
        ]),
        "function_call_output" => json!([
            "function_call_output",
            str_field(item, "call_id"),
            string_or_canonical(item.get("output")),
        ]),
        _ if kind.ends_with("_call") => json!([
            kind,
            str_field(item, "call_id"),
            str_field(item, "name"),
            string_or_canonical(item.get("arguments")),
        ]),
        _ => {
            let mut stripped = item.clone();
            if let Some(map) = stripped.as_object_mut() {
                map.remove("id");
                map.remove("status");
            }
            json!(["other", canonical_json(&stripped)])
        }
    };
    digest_value(&canonical)
}

pub fn is_assistant(item: &Value) -> bool {
    match item_type(item) {
        "message" => str_field(item, "role") == "assistant",
        kind => MODEL_ITEM_TYPES.contains(&kind),
    }
}

pub fn is_summary_message(item: &Value) -> bool {
    item_type(item) == "message"
        && str_field(item, "role") == "user"
        && content_text(item.get("content")).starts_with(SUMMARY_HEADER)
}

pub fn summarize_message(item: &Value, cfg: &CliffConfig) -> Vec<String> {
    let kind = item_type(item);
    match kind {
        "message" => {
            let role = str_field(item, "role");
            let text = content_text(item.get("content"));
            let text = text.trim();
            if text.is_empty() {
                return Vec::new();
            }
            if role == "assistant" {
                return vec![format!(
                    "assistant: {}",
                    truncate(text, cfg.thought_max_chars)
                )];
            }
            if text.starts_with(SUMMARY_HEADER) {
                return Vec::new();
            }
            if role == "user" {
                let text = strip_task_notifications(text);
                let text = text.trim();
                if text.is_empty() {
                    return Vec::new();
                }
                return vec![format!("user: {}", truncate(text, cfg.human_max_chars))];
            }
            vec![format!("{role}: {}", truncate(text, cfg.human_max_chars))]
        }
        "reasoning" => {
            if !cfg.keep_thinking {
                return Vec::new();
            }
            let text = item
                .get("summary")
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter(|part| part.is_object())
                        .map(|part| str_field(part, "text"))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            let text = text.trim();
            if text.is_empty() {
                // Encrypted-only reasoning has nothing readable to fold.
                return Vec::new();
            }
            vec![format!(
                "thinking: {}",
                truncate(text, cfg.thinking_max_chars)
            )]
        }
        // `custom_tool_call_output` and similar outputs count as results.
        _ if kind == "function_call_output" || kind.ends_with("_call_output") => {
            let output = match item.get("output") {
                Some(Value::String(text)) => text.clone(),
                Some(other) => {
                    let text = content_text(Some(other));
                    if text.is_empty() {
                        canonical_json(other)
                    } else {
                        text
                    }
                }
                None => String::new(),
            };
            let output = output.trim();
            if !output.is_empty() && output.chars().count() <= cfg.result_max_chars {
                vec![format!("result: {output}")]
            } else {
                Vec::new()
            }
        }
        _ if kind.ends_with("_call") => {
            let args = string_or_canonical(item.get("arguments"));
            let name = match str_field(item, "name") {
                "" => kind,
                name => name,
            };
            vec![format!("[{name}] {}", truncate(&args, cfg.cmd_max_chars))]
        }
        _ => Vec::new(),
    }
}

pub fn user_message(text: String) -> Value {
    json!({
        "type": "message",
        "role": "user",
        "content": [{"type": "input_text", "text": text}],
    })
}

#[cfg(test)]
mod tests {
    use super::super::{compact, Dialect};
    use super::*;

    fn reasoning(n: usize) -> Value {
        json!({"type": "reasoning", "id": format!("rs_{n}"), "encrypted_content": format!("enc{n}"),
               "summary": [{"type": "summary_text", "text": format!("plan {n}")}]})
    }

    fn call(n: usize) -> Value {
        json!({"type": "function_call", "id": format!("fc_{n}"), "call_id": format!("call_{n}"),
               "name": "shell", "arguments": format!("{{\"command\":[\"make\",\"{n}\"]}}")})
    }

    fn output(n: usize, text: &str) -> Value {
        json!({"type": "function_call_output", "call_id": format!("call_{n}"), "output": text})
    }

    fn codex_history(steps: usize) -> Vec<Value> {
        let mut input = vec![
            json!({"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": "<environment_context>cwd</environment_context>"}]}),
            json!({"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": "Fix the build."}]}),
        ];
        for n in 0..steps {
            input.push(reasoning(n));
            input.push(call(n));
            let text = if n % 2 == 0 {
                "O".repeat(4000)
            } else {
                format!("step {n} ok")
            };
            input.push(output(n, &text));
        }
        input
    }

    #[test]
    fn a_model_step_stays_one_turn() {
        let input = codex_history(4);
        let turns = Dialect::Responses.group_turns(&input[2..]);
        assert_eq!(turns.len(), 4);
        assert!(turns.iter().all(|turn| turn.len() == 3));
    }

    #[test]
    fn digest_ignores_id_and_status_but_tracks_encrypted_content() {
        let mut changed = call(1);
        changed["id"] = json!("other");
        changed["status"] = json!("completed");
        assert_eq!(digest_message(&call(1)), digest_message(&changed));
        let mut rotated = reasoning(1);
        rotated["encrypted_content"] = json!("different");
        assert_ne!(digest_message(&reasoning(1)), digest_message(&rotated));
    }

    #[test]
    fn compacts_a_codex_history_with_paired_steps_kept() {
        let input = codex_history(8);
        let cfg = CliffConfig {
            keep_recent: 2,
            ..CliffConfig::default()
        };
        let result = compact(&input, Dialect::Responses, &cfg).unwrap();
        assert_eq!(result.head_len, 2);
        let summary = content_text(result.summary.get("content"));
        assert!(summary.starts_with(SUMMARY_HEADER));
        assert!(summary.contains("thinking: plan 0"));
        assert!(summary.contains("[shell] {\"command\":[\"make\",\"0\"]}"));
        assert!(summary.contains("result: step 1 ok"));
        assert!(!summary.contains("OOOO"));
        // The kept tail starts with a reasoning item and keeps its call.
        let tail = &result.messages[3..];
        assert_eq!(tail.len(), 6);
        assert_eq!(item_type(&tail[0]), "reasoning");
        assert_eq!(item_type(&tail[1]), "function_call");
        assert!(is_summary_message(&result.messages[2]));
    }

    #[test]
    fn custom_tool_outputs_count_as_results() {
        let item = json!({"type": "custom_tool_call_output", "call_id": "c", "output": "Done!"});
        assert_eq!(
            summarize_message(&item, &CliffConfig::default()),
            vec!["result: Done!".to_string()]
        );
    }
}
