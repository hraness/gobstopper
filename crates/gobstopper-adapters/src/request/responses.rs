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
    canonical_json, carry_assistant, carry_human, digest_value, other_fields, str_field,
    strip_task_notifications, truncate, CliffConfig, SUMMARY_HEADER,
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
        Some(Value::String(text)) => json!([["text", text, {}]]),
        Some(Value::Array(parts)) => Value::Array(
            parts
                .iter()
                .filter(|part| part.is_object())
                .map(|part| match str_field(part, "type") {
                    "input_text" | "output_text" | "text" => {
                        json!([
                            "text",
                            str_field(part, "text"),
                            other_fields(part, &["type", "text"])
                        ])
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

/// Provider call kinds have different payload fields (`input` for freeform
/// calls, `arguments` for functions, `action` for local shell calls). Keep
/// unknown semantic fields as well: a new call kind must not alias an old
/// cached prefix merely because it does not use `arguments`.
fn canonical_call(item: &Value) -> Value {
    let mut call = item.clone();
    if let Some(fields) = call.as_object_mut() {
        fields.remove("id");
        fields.remove("status");
    }
    call
}

fn call_input(item: &Value) -> String {
    let key = if item_type(item) == "custom_tool_call" {
        "input"
    } else {
        "arguments"
    };
    match item.get(key) {
        Some(value) => string_or_canonical(Some(value)),
        None => canonical_json(&canonical_call(item)),
    }
}

pub fn digest_message(item: &Value) -> String {
    let kind = item_type(item);
    let canonical = match kind {
        "message" => json!([
            "message",
            str_field(item, "role"),
            canonical_content(item.get("content")),
            other_fields(item, &["type", "id", "status", "role", "content"]),
        ]),
        "reasoning" => json!([
            "reasoning",
            str_field(item, "encrypted_content"),
            canonical_json(item.get("summary").unwrap_or(&json!([]))),
            other_fields(
                item,
                &["type", "id", "status", "encrypted_content", "summary"]
            ),
        ]),
        "function_call_output" => json!([
            "function_call_output",
            str_field(item, "call_id"),
            string_or_canonical(item.get("output")),
            other_fields(item, &["type", "id", "status", "call_id", "output"]),
        ]),
        _ if kind.ends_with("_call") => json!([kind, canonical_call(item)]),
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
        kind => MODEL_ITEM_TYPES.contains(&kind) || kind.ends_with("_call"),
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
            let args = call_input(item);
            let name = match str_field(item, "name") {
                "" => kind,
                name => name,
            };
            vec![format!("[{name}] {}", truncate(&args, cfg.cmd_max_chars))]
        }
        _ => Vec::new(),
    }
}

/// Text parts of a message's content, in order; images, files and audio
/// are payloads, not words.
fn text_parts(content: Option<&Value>) -> Vec<&str> {
    match content {
        Some(Value::String(text)) => vec![text.as_str()],
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|part| {
                matches!(
                    str_field(part, "type"),
                    "input_text" | "output_text" | "text"
                )
            })
            .map(|part| str_field(part, "text"))
            .collect(),
        _ => Vec::new(),
    }
}

/// The conversation's words in `items`, oldest first, for the carried
/// section of later summaries: the text of user-role messages and the
/// assistant's visible text. System and developer messages, reasoning,
/// tool calls, tool outputs and prior summaries are never carried. Tool
/// outputs are separate items, so no harness text rides in a user message
/// the way a skill body does in Anthropic's tool-result messages; the
/// context items Codex sends again as user messages (`<environment_context>`,
/// `<user_instructions>`, `# AGENTS.md instructions`) are skipped. Each
/// message yields at most one text part, followed by one part per text part
/// that is wholly a queued message.
pub fn carry_parts(items: &[Value]) -> Vec<String> {
    let mut parts = Vec::new();
    for item in items {
        if item_type(item) != "message" || is_summary_message(item) {
            continue;
        }
        let texts = text_parts(item.get("content"));
        match str_field(item, "role") {
            "user" => parts.extend(carry_human(&texts)),
            "assistant" => parts.extend(carry_assistant(&texts)),
            _ => {}
        }
    }
    parts
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
    use super::super::fixtures::{
        burst_results, carried, carry_cfg, chain_against_fresh, mixed_results, reconvergences,
        tail_cfg,
    };
    use super::super::{compact, Dialect};
    use super::*;

    #[test]
    fn custom_input_and_unknown_call_fields_are_semantic() {
        let call = json!({"type":"custom_tool_call", "call_id":"exec-1", "name":"exec", "input":"inspect_lock_graph()"});
        let mut changed = call.clone();
        changed["input"] = json!("release_lock_graph()");
        assert_ne!(digest_message(&call), digest_message(&changed));
        assert!(summarize_message(&call, &CliffConfig::default())
            .join("\n")
            .contains("inspect_lock_graph()"));
        changed = call.clone();
        changed["id"] = json!("provider-id");
        changed["status"] = json!("completed");
        assert_eq!(digest_message(&call), digest_message(&changed));

        let call =
            json!({"type":"future_tool_call", "call_id":"future", "action":{"command":"read"}});
        let mut changed = call.clone();
        changed["action"]["command"] = json!("write");
        assert_ne!(digest_message(&call), digest_message(&changed));
    }
    use serde_json::Map;

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

    /// Request bodies of a Codex session that grows one model step per
    /// request; `size(n)` is the length of step n's output.
    fn stepped_bodies(steps: usize, size: impl Fn(usize) -> usize) -> Vec<Map<String, Value>> {
        let mut input = codex_history(0);
        (0..steps)
            .map(|n| {
                input.push(json!({"type": "reasoning", "id": format!("rs_{n}"),
                    "encrypted_content": format!("enc{n}"),
                    "summary": [{"type": "summary_text", "text": format!("plan {n} {}", "t".repeat(200))}]}));
                input.push(call(n));
                input.push(output(n, &"O".repeat(size(n))));
                let Value::Object(body) = json!({
                    "model": "gpt-x",
                    "instructions": "You are a coding agent.",
                    "input": input.clone(),
                }) else {
                    unreachable!()
                };
                body
            })
            .collect()
    }

    #[test]
    fn a_live_chain_equals_a_fresh_prepare_at_a_tail_percent() {
        let steps = chain_against_fresh(
            &tail_cfg(12_000),
            Dialect::Responses,
            &stepped_bodies(100, mixed_results),
        );
        let unequal: Vec<usize> = (0..steps.len()).filter(|&i| !steps[i].equal).collect();
        assert!(unequal.is_empty(), "requests {unequal:?} differ");
        assert!(steps.iter().filter(|s| s.compacted).count() >= 4);
        assert!(steps.iter().all(|s| s.rung == 0));
        // The budget kept more than `keep_recent` turns.
        assert!(steps.iter().any(|s| s.compacted && s.tail_turns > 3));
    }

    #[test]
    fn a_rung_one_burst_reconverges_with_a_fresh_prepare() {
        let steps = chain_against_fresh(
            &tail_cfg(6_000),
            Dialect::Responses,
            &stepped_bodies(90, burst_results),
        );
        assert!(steps.iter().any(|s| s.rung == 1));
        assert!(reconvergences(&steps) >= 3);
        // After the last burst the chains agree for good.
        assert!(steps[60..].iter().all(|s| s.equal));
    }

    fn message(role: &str, kind: &str, text: &str) -> Value {
        json!({"type": "message", "role": role, "content": [{"type": kind, "text": text}]})
    }

    #[test]
    fn carry_keeps_user_and_visible_assistant_text_of_a_codex_history() {
        let items = vec![
            user_message(format!("{SUMMARY_HEADER}\n\nuser: old instruction")),
            message(
                "developer",
                "input_text",
                "<permissions>sandboxed</permissions>",
            ),
            json!({"type": "message", "role": "system", "content": "system directive"}),
            json!({"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": "Fix the build."},
                {"type": "input_image", "image_url": "data:image/png;base64,AAAA"},
                {"type": "input_text", "text": "Then run the tests."}
            ]}),
            reasoning(0),
            call(0),
            output(0, "error: missing semicolon"),
            json!({"type": "custom_tool_call", "call_id": "c1", "name": "apply_patch", "input": "*** Begin Patch"}),
            json!({"type": "custom_tool_call_output", "call_id": "c1", "output": "patched ok"}),
            json!({"type": "message", "role": "assistant", "content": [
                {"type": "output_text", "text": "Fixed the semicolon."},
                {"type": "output_text", "text": "Tests pass."}
            ]}),
            json!({"type": "message", "role": "user", "content": "Now ship it."}),
        ];
        assert_eq!(
            carry_parts(&items),
            vec![
                "user: Fix the build.\nThen run the tests.",
                "assistant: Fixed the semicolon.\nTests pass.",
                "user: Now ship it.",
            ]
        );
        let joined = carry_parts(&items).join("\n");
        for hidden in [
            "old instruction",
            "permissions",
            "system directive",
            "plan 0",
            "enc0",
            "make",
            "missing semicolon",
            "Begin Patch",
            "patched ok",
            "base64",
        ] {
            assert!(!joined.contains(hidden), "{hidden} leaked");
        }
    }

    #[test]
    fn carry_strips_reminders_and_notifications_and_keeps_queued_messages() {
        let items = vec![
            message(
                "user",
                "input_text",
                "<system-reminder>harness context</system-reminder>Check the logs.\n\
                 <task-notification>agent done</task-notification>",
            ),
            message(
                "user",
                "input_text",
                "<system-reminder>The user sent a new message while you were working:\n\
                 Stop after the first failure.</system-reminder>",
            ),
            message(
                "assistant",
                "output_text",
                "Reading the logs.\n---\nDone.<system-reminder>",
            ),
            message(
                "user",
                "input_text",
                "Also <system-reminder>The user sent a new message while you were working:\n\
                 INLINE-SPAN</system-reminder>continue.",
            ),
        ];
        assert_eq!(
            carry_parts(&items),
            vec![
                "user: Check the logs.",
                "user: Stop after the first failure.",
                "assistant: Reading the logs.\n- - -\nDone.<system-reminder>",
                "user: Also continue.",
            ]
        );
    }

    #[test]
    fn carry_skips_the_context_items_codex_sends_again_later() {
        // Codex re-sends these as user messages after the first reply
        // (18 and 23 of the 300 newest local sessions).
        let items = vec![
            message("assistant", "output_text", "First reply."),
            message(
                "user",
                "input_text",
                "# AGENTS.md instructions for /work\n\n<INSTRUCTIONS>\nAGENTS-BODY\n</INSTRUCTIONS>",
            ),
            message(
                "user",
                "input_text",
                "<environment_context>\n  <cwd>/work</cwd>\n  ENV-BODY\n</environment_context>",
            ),
            json!({"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": "<user_instructions>\nUSER-INSTRUCTIONS\n</user_instructions>"},
                {"type": "input_text", "text": "Keep going."}
            ]}),
        ];
        assert_eq!(
            carry_parts(&items),
            vec!["assistant: First reply.", "user: Keep going."]
        );
    }

    #[test]
    fn carry_of_a_compacted_codex_history_skips_the_summary() {
        let cfg = CliffConfig {
            keep_recent: 2,
            ..CliffConfig::default()
        };
        let mut items = compact(&codex_history(6), Dialect::Responses, &cfg)
            .unwrap()
            .messages;
        assert!(items.iter().any(is_summary_message));
        items.push(message("assistant", "output_text", "Build fixed."));
        let parts = carry_parts(&items);
        assert!(parts.iter().all(|part| !part.contains(SUMMARY_HEADER)));
        assert_eq!(
            parts.last().map(String::as_str),
            Some("assistant: Build fixed.")
        );
    }

    /// `stepped_bodies` with conversation: every fifth step is an assistant
    /// reply and a user instruction instead of a tool call, so a carry holds
    /// both.
    fn talk_bodies(steps: usize, size: impl Fn(usize) -> usize) -> Vec<Map<String, Value>> {
        let mut input = codex_history(0);
        (0..steps)
            .map(|n| {
                if n % 5 == 4 {
                    let reply = format!("Reply {n}: part {n} is done.");
                    input.push(message("assistant", "output_text", &reply));
                    let next = format!("Instruction {n}: now take part {}.", n + 1);
                    input.push(message("user", "input_text", &next));
                } else {
                    input.push(json!({"type": "reasoning", "id": format!("rs_{n}"),
                        "encrypted_content": format!("enc{n}"),
                        "summary": [{"type": "summary_text", "text": format!("plan {n} {}", "t".repeat(200))}]}));
                    input.push(call(n));
                    input.push(output(n, &"O".repeat(size(n))));
                }
                let Value::Object(body) = json!({
                    "model": "gpt-x",
                    "instructions": "You are a coding agent.",
                    "input": input.clone(),
                }) else {
                    unreachable!()
                };
                body
            })
            .collect()
    }

    // C3-7: determinism with a nonempty carry.

    #[test]
    fn a_live_chain_with_a_carry_equals_a_fresh_prepare() {
        let steps = chain_against_fresh(
            &carry_cfg(12_000),
            Dialect::Responses,
            &talk_bodies(100, mixed_results),
        );
        let unequal: Vec<usize> = (0..steps.len()).filter(|&i| !steps[i].equal).collect();
        assert!(unequal.is_empty(), "requests {unequal:?} differ");
        assert!(steps.iter().filter(|s| s.compacted).count() >= 4);
        assert!(steps.iter().all(|s| s.rung == 0));
        // Nonempty carries and carried sections were compared.
        assert!(carried(&steps) >= 20, "{}", carried(&steps));
    }

    #[test]
    fn a_rung_one_burst_with_a_carry_reconverges_with_a_fresh_prepare() {
        let steps = chain_against_fresh(
            &carry_cfg(6_000),
            Dialect::Responses,
            &talk_bodies(90, burst_results),
        );
        assert!(steps.iter().any(|s| s.rung == 1));
        assert!(reconvergences(&steps) >= 1);
        let again = (1..steps.len())
            .find(|&i| !steps[i - 1].equal && steps[i].equal)
            .unwrap();
        assert!(steps[again].carry_parts > 0, "request {again}");
        // After the last burst the chains agree for good, carry included.
        assert!(steps[60..].iter().all(|s| s.equal));
        assert!(carried(&steps[60..]) >= 10, "{}", carried(&steps[60..]));
    }
}
