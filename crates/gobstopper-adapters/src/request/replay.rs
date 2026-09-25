//! Offline replay: rebuild the requests a recorded Claude Code or Codex
//! session sent, run each through the request engine as the proxy would
//! have, and check every outgoing history for broken tool pairing. No
//! provider is called; sizes are the engine's estimates, not billed tokens.

use super::{CliffConfig, Dialect, Engine};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize)]
pub struct Compaction {
    /// Zero-based request index.
    pub request: usize,
    pub est_tokens_before: u64,
    pub est_tokens_after: u64,
    pub messages_before: usize,
    pub messages_after: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ReplayReport {
    pub dialect: &'static str,
    pub requests: usize,
    pub compacted: usize,
    pub reused_prefix: usize,
    /// Requests still over the applied threshold after compaction.
    pub over_threshold_after: usize,
    /// Requests whose verbatim head forced a higher threshold.
    pub raised_threshold: usize,
    pub peak_est_tokens_in: u64,
    pub peak_est_tokens_out: u64,
    pub last_est_tokens_in: u64,
    pub last_est_tokens_out: u64,
    /// Sum over requests of estimated input tokens, without and with the
    /// proxy. A proxy for context carried, not a bill: caching is ignored.
    pub total_est_tokens_in: u64,
    pub total_est_tokens_out: u64,
    /// Outgoing histories the proxy left with an unpaired tool call or
    /// result although the recorded history was paired.
    pub pairing_violations: usize,
    /// Requests whose recorded history was already unpaired (interrupted or
    /// rewound turns); not counted as proxy violations.
    pub source_pairing_violations: usize,
    /// First request index with a proxy-introduced violation.
    pub first_violation: Option<usize>,
    pub compactions: Vec<Compaction>,
}

/// Rebuild the Anthropic `messages` history of a Claude Code transcript:
/// main-chain user and assistant records only, assistant records split per
/// content block merged back by message id, reset at a provider compaction.
pub fn claude_history(raw: &[u8]) -> Vec<Value> {
    let mut history: Vec<Value> = Vec::new();
    let mut last_assistant_id: Option<String> = None;
    for line in raw.split(|b| *b == b'\n') {
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let kind = record.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "system"
            && record.get("subtype").and_then(Value::as_str) == Some("compact_boundary")
        {
            history.clear();
            last_assistant_id = None;
            continue;
        }
        if !matches!(kind, "user" | "assistant")
            || record
                .get("isSidechain")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            continue;
        }
        let Some(message) = record.get("message") else {
            continue;
        };
        let role = message.get("role").and_then(Value::as_str).unwrap_or(kind);
        let content = message.get("content").cloned().unwrap_or(Value::Null);
        let id = message
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string);
        if role == "assistant" && id.is_some() && id == last_assistant_id {
            if let (Some(previous), Some(blocks)) = (
                history
                    .last_mut()
                    .and_then(|m| m.get_mut("content"))
                    .and_then(Value::as_array_mut),
                content.as_array(),
            ) {
                previous.extend(blocks.iter().cloned());
                continue;
            }
        }
        last_assistant_id = if role == "assistant" { id } else { None };
        history.push(json!({"role": role, "content": content}));
    }
    history
}

/// Rebuild the Responses `input` history of a Codex rollout: `response_item`
/// payloads in order, restarted from `replacement_history` at a compaction.
pub fn codex_history(raw: &[u8]) -> Vec<Value> {
    let mut history = Vec::new();
    for line in raw.split(|b| *b == b'\n') {
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let Some(payload) = record.get("payload") else {
            continue;
        };
        match record.get("type").and_then(Value::as_str) {
            Some("response_item") => history.push(payload.clone()),
            Some("compacted") => {
                history = payload
                    .get("replacement_history")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
            }
            _ => {}
        }
    }
    history
}

/// Read a Claude Code or Codex transcript and rebuild its request history.
pub fn history_from_file(
    provider: gobstopper_core::Provider,
    path: &std::path::Path,
) -> anyhow::Result<(Dialect, Vec<Value>)> {
    let raw = crate::transaction::read(path)?;
    match provider {
        gobstopper_core::Provider::ClaudeCode => Ok((Dialect::Anthropic, claude_history(&raw))),
        gobstopper_core::Provider::Codex => Ok((Dialect::Responses, codex_history(&raw))),
    }
}

/// Indexes after which the client sent a request: the history ends with
/// input and the next item is a model turn (or the recording ends).
fn request_points(history: &[Value], dialect: Dialect) -> Vec<usize> {
    (1..=history.len())
        .filter(|&end| {
            !dialect.is_assistant(&history[end - 1])
                && history
                    .get(end)
                    .is_none_or(|next| dialect.is_assistant(next))
        })
        .collect()
}

/// Replay `history` through a fresh engine. `fixed_tokens` stands in for
/// the system prompt and tool definitions, which transcripts do not record.
pub fn replay(
    history: &[Value],
    dialect: Dialect,
    cfg: CliffConfig,
    fixed_tokens: u64,
) -> ReplayReport {
    let engine = Engine::new(cfg.clone());
    let filler = "x".repeat((fixed_tokens as usize).saturating_mul(4));
    let mut report = ReplayReport {
        dialect: dialect.name(),
        ..ReplayReport::default()
    };
    for (index, end) in request_points(history, dialect).into_iter().enumerate() {
        let mut body = Map::new();
        body.insert("model".into(), json!("replay"));
        let fixed_key = match dialect {
            Dialect::Anthropic => "system",
            Dialect::Responses => "instructions",
            // Chat Completions has no top-level system field; the filler
            // stands in for tool definitions and framing bytes a replay
            // cannot see.
            Dialect::ChatCompletions => "tools",
        };
        body.insert(fixed_key.into(), json!(filler));
        body.insert(
            dialect.messages_key().into(),
            Value::Array(history[..end].to_vec()),
        );
        let Some(ctx) = engine.prepare(body, dialect) else {
            continue;
        };
        report.requests += 1;
        report.peak_est_tokens_in = report.peak_est_tokens_in.max(ctx.est_tokens_in);
        report.peak_est_tokens_out = report.peak_est_tokens_out.max(ctx.est_tokens_out);
        report.last_est_tokens_in = ctx.est_tokens_in;
        report.last_est_tokens_out = ctx.est_tokens_out;
        report.total_est_tokens_in += ctx.est_tokens_in;
        report.total_est_tokens_out += ctx.est_tokens_out;
        if ctx.est_tokens_out > ctx.threshold_tokens {
            report.over_threshold_after += 1;
        }
        if ctx.threshold_tokens > cfg.threshold_tokens {
            report.raised_threshold += 1;
        }
        if ctx.compacted {
            report.compacted += 1;
            report.compactions.push(Compaction {
                request: index,
                est_tokens_before: ctx.est_tokens_in,
                est_tokens_after: ctx.est_tokens_out,
                messages_before: ctx.original_len(),
                messages_after: ctx.messages().len(),
            });
        } else if ctx.matched {
            report.reused_prefix += 1;
        }
        if !pairing_intact(&history[..end], dialect) {
            report.source_pairing_violations += 1;
        } else if ctx.modified && !pairing_intact(ctx.messages(), dialect) {
            report.pairing_violations += 1;
            report.first_violation.get_or_insert(index);
        }
    }
    report
}

fn ids<'a>(blocks: &'a [Value], kind: &str, field: &str) -> Vec<&'a str> {
    blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some(kind))
        .filter_map(|block| block.get(field).and_then(Value::as_str))
        .collect()
}

/// Every tool result answers a call the model made earlier in the same
/// history, and every call before the final turn is answered. For
/// Anthropic the answer must sit in the message right after the call.
pub fn pairing_intact(messages: &[Value], dialect: Dialect) -> bool {
    match dialect {
        Dialect::Anthropic => {
            // The API merges consecutive same-role messages into one turn
            // (Claude Code records parallel tool results separately).
            let mut turns: Vec<(&str, Vec<Value>)> = Vec::new();
            for message in messages {
                let role = message.get("role").and_then(Value::as_str).unwrap_or("");
                let blocks = message
                    .get("content")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                match turns.last_mut() {
                    Some((last, content)) if *last == role => content.extend(blocks),
                    _ => turns.push((role, blocks)),
                }
            }
            for (i, (role, content)) in turns.iter().enumerate() {
                let results = ids(content, "tool_result", "tool_use_id");
                if *role == "user" && !results.is_empty() {
                    let previous = i.checked_sub(1).map(|p| &turns[p].1[..]).unwrap_or(&[]);
                    let calls: HashSet<&str> =
                        ids(previous, "tool_use", "id").into_iter().collect();
                    if results.iter().any(|id| !calls.contains(id)) {
                        return false;
                    }
                }
                let calls = ids(content, "tool_use", "id");
                if *role == "assistant" && !calls.is_empty() && i + 1 < turns.len() {
                    let answered: HashSet<&str> =
                        ids(&turns[i + 1].1, "tool_result", "tool_use_id")
                            .into_iter()
                            .collect();
                    if calls.iter().any(|id| !answered.contains(id)) {
                        return false;
                    }
                }
            }
            true
        }
        Dialect::Responses => {
            let mut calls = HashSet::new();
            let mut answered = HashSet::new();
            for item in messages {
                let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
                let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
                    continue;
                };
                if kind.ends_with("_output") {
                    if !calls.contains(call_id) {
                        return false;
                    }
                    answered.insert(call_id);
                } else if kind.ends_with("_call") {
                    calls.insert(call_id);
                }
            }
            // A call may await its output only in the final model turn.
            let tail_start = messages
                .iter()
                .rposition(|item| !dialect.is_assistant(item))
                .map_or(0, |i| i + 1);
            messages[..tail_start].iter().all(|item| {
                let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
                !kind.ends_with("_call")
                    || item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .is_none_or(|id| answered.contains(id))
            })
        }
        Dialect::ChatCompletions => {
            // A `tool` message must directly follow the assistant message
            // whose `tool_calls` it answers; every call except those in the
            // final model turn must be answered by the tool run after it.
            let mut pending: HashSet<&str> = HashSet::new();
            for message in messages {
                let role = message.get("role").and_then(Value::as_str).unwrap_or("");
                match role {
                    "assistant" => {
                        if !pending.is_empty() {
                            return false;
                        }
                        pending = message
                            .get("tool_calls")
                            .and_then(Value::as_array)
                            .map(|calls| {
                                calls
                                    .iter()
                                    .filter_map(|call| call.get("id").and_then(Value::as_str))
                                    .collect()
                            })
                            .unwrap_or_default();
                    }
                    "tool" => {
                        let Some(id) = message.get("tool_call_id").and_then(Value::as_str) else {
                            return false;
                        };
                        if pending.is_empty() || !pending.remove(id) {
                            return false;
                        }
                    }
                    _ => {
                        if !pending.is_empty() {
                            return false;
                        }
                    }
                }
            }
            // Reaching the end means every tool message answered its call
            // and no earlier assistant left calls open; calls still pending
            // at the end are the live edge of a recording.
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::*;
    use super::*;

    fn transcript(records: &[Value]) -> Vec<u8> {
        records
            .iter()
            .map(|record| serde_json::to_string(record).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes()
    }

    #[test]
    fn claude_records_merge_by_message_id_and_reset_at_compaction() {
        let raw = transcript(&[
            json!({"type": "user", "message": {"role": "user", "content": "old task"}}),
            json!({"type": "system", "subtype": "compact_boundary"}),
            json!({"type": "user", "message": {"role": "user", "content": "task"}}),
            json!({"type": "assistant", "message": {"id": "m1", "role": "assistant", "content": [{"type": "thinking", "thinking": "t"}]}}),
            json!({"type": "assistant", "message": {"id": "m1", "role": "assistant", "content": [{"type": "tool_use", "id": "a", "name": "bash", "input": {}}]}}),
            json!({"type": "user", "isSidechain": true, "message": {"role": "user", "content": "subagent"}}),
            json!({"type": "attachment", "message": {"role": "user", "content": "ignored"}}),
            json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "a", "content": "ok"}]}}),
        ]);
        let history = claude_history(&raw);
        assert_eq!(history.len(), 3);
        assert_eq!(history[0]["content"], "task");
        assert_eq!(history[1]["content"].as_array().unwrap().len(), 2);
        assert!(pairing_intact(&history, Dialect::Anthropic));
    }

    #[test]
    fn replay_counts_requests_compactions_and_keeps_pairing() {
        let history = a_session(20, 3000);
        let cfg = CliffConfig {
            threshold_tokens: 3_000,
            keep_recent: 1,
            ..CliffConfig::default()
        };
        let report = replay(&history, Dialect::Anthropic, cfg, 200);
        assert_eq!(report.requests, 21);
        assert!(report.compacted >= 2 && report.reused_prefix >= 1);
        assert_eq!(report.pairing_violations, 0);
        assert!(report.peak_est_tokens_out < report.peak_est_tokens_in);
        assert!(report.total_est_tokens_out < report.total_est_tokens_in);
    }

    #[test]
    fn pairing_check_catches_orphans() {
        let orphan = vec![a_user("task"), a_result("missing", "x")];
        assert!(!pairing_intact(&orphan, Dialect::Anthropic));
        let unanswered = vec![
            a_user("task"),
            a_assistant("", Some(("a", "bash", json!({})))),
            a_user("no result"),
        ];
        assert!(!pairing_intact(&unanswered, Dialect::Anthropic));
        let responses = vec![
            json!({"type": "function_call", "call_id": "c", "name": "shell", "arguments": "{}"}),
            json!({"type": "message", "role": "user", "content": "next"}),
        ];
        assert!(!pairing_intact(&responses, Dialect::Responses));
        // Chat Completions: the tool run must answer its own call ids.
        let call = json!({"id": "call_1", "type": "function",
            "function": {"name": "bash", "arguments": "{}"}});
        let paired = vec![
            json!({"role": "user", "content": "task"}),
            json!({"role": "assistant", "content": null, "tool_calls": [call]}),
            json!({"role": "tool", "tool_call_id": "call_1", "content": "ok"}),
        ];
        assert!(pairing_intact(&paired, Dialect::ChatCompletions));
        let mut orphan = paired.clone();
        orphan[2]["tool_call_id"] = json!("other");
        assert!(!pairing_intact(&orphan, Dialect::ChatCompletions));
        let unanswered = vec![
            json!({"role": "user", "content": "task"}),
            json!({"role": "assistant", "content": null, "tool_calls": [call]}),
            json!({"role": "user", "content": "next"}),
        ];
        assert!(!pairing_intact(&unanswered, Dialect::ChatCompletions));
        // A trailing assistant call is the live edge, not a violation.
        let live = vec![
            json!({"role": "user", "content": "task"}),
            json!({"role": "assistant", "content": null, "tool_calls": [call]}),
        ];
        assert!(pairing_intact(&live, Dialect::ChatCompletions));
    }

    #[test]
    fn chat_replay_keeps_tool_calls_paired() {
        let mut history = vec![
            json!({"role": "system", "content": "you are an agent"}),
            json!({"role": "user", "content": "fix the bug"}),
        ];
        for i in 0..20 {
            history.push(json!({"role": "assistant", "content": format!("step {i}"),
                "tool_calls": [{"id": format!("call_{i}"), "type": "function",
                    "function": {"name": "bash", "arguments": format!("{{\"cmd\":\"s{i}\"}}")}}]}));
            history.push(json!({"role": "tool", "tool_call_id": format!("call_{i}"),
                "content": "R".repeat(3000)}));
        }
        let cfg = CliffConfig {
            threshold_tokens: 3_000,
            keep_recent: 1,
            ..CliffConfig::default()
        };
        let report = replay(&history, Dialect::ChatCompletions, cfg, 200);
        assert!(report.compacted >= 2);
        assert_eq!(report.pairing_violations, 0);
        assert_eq!(report.source_pairing_violations, 0);
        assert!(report.total_est_tokens_out < report.total_est_tokens_in);
    }
}
