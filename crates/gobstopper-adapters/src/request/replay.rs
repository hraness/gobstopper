//! Offline replay: rebuild the requests a recorded Claude Code or Codex
//! session sent, run each through the request engine as the proxy would
//! have, and check every outgoing history for broken tool pairing. No
//! provider is called; sizes are the engine's estimates, not billed tokens.

use super::{canonical_json, CliffConfig, Dialect, Engine};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize)]
pub struct Compaction {
    /// Zero-based request index.
    pub request: usize,
    pub est_tokens_before: u64,
    pub est_tokens_after: u64,
    pub messages_before: usize,
    pub messages_after: usize,
    /// Estimated tokens of the outgoing list after the compaction: the
    /// verbatim head, the summary and the kept tail. Sizes only.
    pub head_tokens: u64,
    pub summary_tokens: u64,
    pub tail_tokens: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ReplayReport {
    pub dialect: &'static str,
    pub requests: usize,
    pub compacted: usize,
    pub reused_prefix: usize,
    /// Requests still over the applied threshold after compaction.
    pub over_threshold_after: usize,
    /// Requests whose verbatim head forced a threshold above the one
    /// selected for them.
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
    /// Estimated prompt-cache reads and writes summed over requests. A
    /// request whose outgoing list extends the previous one reads the
    /// previous request's estimate and writes the rest; any other request
    /// (a compaction, a changed substitution, a rewritten history) reads
    /// only the fixed fields. Estimates without price multipliers.
    pub est_cache_read_tokens: u64,
    pub est_cache_write_tokens: u64,
    /// Tool calls whose read key (tool name plus `url`, plus `file_path`
    /// with `offset` and `limit`, or else the whole input) matches an
    /// earlier call in the same history.
    pub repeated_reads: usize,
    /// Repeated reads whose earlier result was still verbatim in the
    /// outgoing request the repeating response answered.
    pub repeated_reads_covered: usize,
    /// Compactions on consecutive requests.
    pub back_to_back_compactions: usize,
    /// Fewest requests between two compactions (`None` below two).
    pub min_compaction_gap: Option<usize>,
}

/// Rebuild the Anthropic `messages` history of a Claude Code transcript:
/// main-chain user and assistant records only, assistant records split per
/// content block merged back by message id, reset at a provider compaction.
/// A subagent's own transcript file (`subagents/.../agent-*.jsonl`) marks
/// every record as a sidechain; when the first conversational record is a
/// sidechain, the file is that subagent's chain and its records are kept.
pub fn claude_history(raw: &[u8]) -> Vec<Value> {
    let mut history: Vec<Value> = Vec::new();
    let mut last_assistant_id: Option<String> = None;
    let mut subagent_file: Option<bool> = None;
    for line in raw.split(|b| *b == b'\n') {
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let kind = record.get("type").and_then(Value::as_str).unwrap_or("");
        let sidechain = record
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if matches!(kind, "user" | "assistant") && subagent_file.is_none() {
            subagent_file = Some(sidechain);
        }
        if kind == "system"
            && record.get("subtype").and_then(Value::as_str) == Some("compact_boundary")
        {
            history.clear();
            last_assistant_id = None;
            continue;
        }
        if !matches!(kind, "user" | "assistant") || sidechain != (subagent_file == Some(true)) {
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
    let engine = Engine::new(cfg);
    let filler = "x".repeat((fixed_tokens as usize).saturating_mul(4));
    let mut report = ReplayReport {
        dialect: dialect.name(),
        ..ReplayReport::default()
    };
    let points = request_points(history, dialect);
    let calls = tool_calls(history, dialect);
    let mut next_call = 0;
    let mut earlier: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut previous: Option<(Vec<Value>, u64)> = None;
    let mut last_compaction: Option<usize> = None;
    for (index, &end) in points.iter().enumerate() {
        let response_end = points.get(index + 1).copied().unwrap_or(history.len());
        while let Some(call) = calls.get(next_call).filter(|call| call.msg < end) {
            earlier.entry(&call.key).or_default().push(&call.id);
            next_call += 1;
        }
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
        let ctx = engine.prepare(body, dialect);
        // Repeats the response to this request issued, checked against
        // what this request actually sent.
        let outgoing = ctx.as_ref().map_or(&history[..end], |ctx| ctx.messages());
        let mut answered: Option<HashSet<&str>> = None;
        while let Some(call) = calls.get(next_call).filter(|call| call.msg < response_end) {
            if let Some(ids) = earlier.get(call.key.as_str()) {
                report.repeated_reads += 1;
                let answered = answered.get_or_insert_with(|| result_ids(outgoing, dialect));
                if ids.iter().any(|id| answered.contains(id)) {
                    report.repeated_reads_covered += 1;
                }
            }
            earlier.entry(&call.key).or_default().push(&call.id);
            next_call += 1;
        }
        let Some(ctx) = ctx else {
            continue;
        };
        report.requests += 1;
        let sent = ctx.messages();
        let read = match &previous {
            Some((prefix, est))
                if sent.len() >= prefix.len() && sent[..prefix.len()] == prefix[..] =>
            {
                *est
            }
            _ => ctx.est_fixed_tokens.min(ctx.est_tokens_out),
        };
        report.est_cache_read_tokens += read;
        report.est_cache_write_tokens += ctx.est_tokens_out.saturating_sub(read);
        previous = Some((sent.to_vec(), ctx.est_tokens_out));
        report.peak_est_tokens_in = report.peak_est_tokens_in.max(ctx.est_tokens_in);
        report.peak_est_tokens_out = report.peak_est_tokens_out.max(ctx.est_tokens_out);
        report.last_est_tokens_in = ctx.est_tokens_in;
        report.last_est_tokens_out = ctx.est_tokens_out;
        report.total_est_tokens_in += ctx.est_tokens_in;
        report.total_est_tokens_out += ctx.est_tokens_out;
        if ctx.est_tokens_out > ctx.threshold_tokens {
            report.over_threshold_after += 1;
        }
        if ctx.threshold_tokens > ctx.base_threshold_tokens {
            report.raised_threshold += 1;
        }
        if ctx.compacted {
            report.compacted += 1;
            if let Some(last) = last_compaction {
                let gap = index - last;
                if gap == 1 {
                    report.back_to_back_compactions += 1;
                }
                report.min_compaction_gap =
                    Some(report.min_compaction_gap.map_or(gap, |min| min.min(gap)));
            }
            last_compaction = Some(index);
            report.compactions.push(Compaction {
                request: index,
                est_tokens_before: ctx.est_tokens_in,
                est_tokens_after: ctx.est_tokens_out,
                messages_before: ctx.original_len(),
                messages_after: ctx.messages().len(),
                head_tokens: ctx.est_head_tokens,
                summary_tokens: ctx.est_summary_tokens,
                tail_tokens: ctx.est_tail_tokens,
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

/// A tool call in a history: its message index, pairing id and read key.
struct ToolCall {
    msg: usize,
    id: String,
    key: String,
}

/// Every tool call in `history`, in order. Anthropic `tool_use` blocks,
/// Chat `tool_calls` (arguments parsed as JSON when they parse), and
/// Responses `*_call` items (`arguments`, `input` or `action`).
fn tool_calls(history: &[Value], dialect: Dialect) -> Vec<ToolCall> {
    fn text<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
        value.get(field).and_then(Value::as_str)
    }
    let parsed = |value: Option<&Value>| match value {
        Some(Value::String(text)) => {
            serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.clone()))
        }
        Some(other) => other.clone(),
        None => Value::Null,
    };
    let mut calls = Vec::new();
    for (msg, message) in history.iter().enumerate() {
        let mut push = |id: Option<&str>, name: &str, input: Value| {
            if let Some(id) = id {
                calls.push(ToolCall {
                    msg,
                    id: id.to_string(),
                    key: read_key(name, &input),
                });
            }
        };
        match dialect {
            Dialect::Anthropic => {
                if !dialect.is_assistant(message) {
                    continue;
                }
                for block in message
                    .get("content")
                    .and_then(Value::as_array)
                    .map_or(&[][..], Vec::as_slice)
                {
                    if text(block, "type") == Some("tool_use") {
                        let name = text(block, "name").unwrap_or("");
                        push(text(block, "id"), name, parsed(block.get("input")));
                    }
                }
            }
            Dialect::ChatCompletions => {
                for call in message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .map_or(&[][..], Vec::as_slice)
                {
                    let function = call.get("function").unwrap_or(&Value::Null);
                    let name = text(function, "name").unwrap_or("");
                    push(text(call, "id"), name, parsed(function.get("arguments")));
                }
            }
            Dialect::Responses => {
                let kind = text(message, "type").unwrap_or("");
                if kind.ends_with("_call") {
                    let name = text(message, "name").unwrap_or(kind);
                    let input = ["arguments", "input", "action"]
                        .iter()
                        .find_map(|field| message.get(*field));
                    push(text(message, "call_id"), name, parsed(input));
                }
            }
        }
    }
    calls
}

/// The key a repeated read is matched on: the tool name with `url` when
/// the input has a string one, with `file_path`, `offset` and `limit` when
/// it has a string `file_path`, and otherwise with the canonical input.
fn read_key(name: &str, input: &Value) -> String {
    let key = if let Some(url) = input.get("url").and_then(Value::as_str) {
        json!([name, "url", url])
    } else if let Some(path) = input.get("file_path").and_then(Value::as_str) {
        let field = |field: &str| input.get(field).cloned().unwrap_or(Value::Null);
        json!([name, "file", path, field("offset"), field("limit")])
    } else {
        json!([name, "input", canonical_json(input)])
    };
    canonical_json(&key)
}

/// Pairing ids of the tool results present in `messages`. The engine only
/// keeps results verbatim (a summary is text), so presence means verbatim.
fn result_ids(messages: &[Value], dialect: Dialect) -> HashSet<&str> {
    let mut found = HashSet::new();
    for message in messages {
        match dialect {
            Dialect::Anthropic => {
                if let Some(blocks) = message.get("content").and_then(Value::as_array) {
                    found.extend(ids(blocks, "tool_result", "tool_use_id"));
                }
            }
            Dialect::ChatCompletions => {
                if message.get("role").and_then(Value::as_str) == Some("tool") {
                    found.extend(message.get("tool_call_id").and_then(Value::as_str));
                }
            }
            Dialect::Responses => {
                let kind = message.get("type").and_then(Value::as_str).unwrap_or("");
                if kind.ends_with("_output") {
                    found.extend(message.get("call_id").and_then(Value::as_str));
                }
            }
        }
    }
    found
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

    /// A subagent's own transcript (`subagents/.../agent-*.jsonl`) marks
    /// every record as a sidechain; those records are its history.
    #[test]
    fn a_subagent_transcript_keeps_its_sidechain_records() {
        let mut records = vec![
            json!({"type": "attachment", "isSidechain": true, "message": {"role": "user", "content": "ignored"}}),
            json!({"type": "user", "isSidechain": true, "message": {"role": "user", "content": "task"}}),
        ];
        for i in 0..12 {
            let id = format!("t{i}");
            records.push(json!({"type": "assistant", "isSidechain": true, "message": {"id": format!("m{i}"), "role": "assistant", "content": [{"type": "text", "text": format!("step {i}")}]}}));
            records.push(json!({"type": "assistant", "isSidechain": true, "message": {"id": format!("m{i}"), "role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "bash", "input": {"cmd": i}}]}}));
            records.push(json!({"type": "user", "isSidechain": true, "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": "R".repeat(3000)}]}}));
        }
        let history = claude_history(&transcript(&records));
        assert_eq!(history.len(), 25);
        assert_eq!(history[0]["content"], "task");
        assert_eq!(history[1]["content"].as_array().unwrap().len(), 2);
        assert!(pairing_intact(&history, Dialect::Anthropic));
        let cfg = CliffConfig {
            threshold_tokens: 3_000,
            keep_recent: 1,
            ..CliffConfig::default()
        };
        let report = replay(&history, Dialect::Anthropic, cfg, 200);
        assert_eq!(report.requests, 13);
        assert!(report.compacted >= 1);
        assert_eq!(report.pairing_violations, 0);
    }

    /// `raised_threshold` counts requests whose verbatim head pushed the
    /// applied threshold above the selected one, and only those.
    #[test]
    fn only_a_large_verbatim_head_counts_as_a_raised_threshold() {
        let history = |task: String| {
            let mut history = vec![json!({"role": "user", "content": task})];
            for i in 0..12 {
                let id = format!("t{i}");
                history.push(json!({"role": "assistant", "content": [
                    {"type": "tool_use", "id": id, "name": "bash", "input": {"cmd": i}}
                ]}));
                history.push(json!({"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": id, "content": "R".repeat(3000)}
                ]}));
            }
            history
        };
        let cfg = CliffConfig {
            threshold_tokens: 3_000,
            keep_recent: 1,
            ..CliffConfig::default()
        };
        let small = replay(
            &history("task".into()),
            Dialect::Anthropic,
            cfg.clone(),
            200,
        );
        assert!(small.compacted >= 1);
        assert_eq!(small.raised_threshold, 0);
        let large = replay(&history("T".repeat(10_000)), Dialect::Anthropic, cfg, 200);
        assert!(large.requests > 0);
        assert_eq!(large.raised_threshold, large.requests);
    }

    /// A main-session transcript skips the sidechain records a subagent
    /// wrote into it, wherever they appear, and keeps the main chain.
    #[test]
    fn a_main_transcript_skips_sidechain_records() {
        let raw = transcript(&[
            json!({"type": "attachment", "message": {"role": "user", "content": "ignored"}}),
            json!({"type": "user", "message": {"role": "user", "content": "task"}}),
            json!({"type": "user", "isSidechain": true, "message": {"role": "user", "content": "subagent task"}}),
            json!({"type": "assistant", "isSidechain": true, "message": {"id": "s1", "role": "assistant", "content": [{"type": "text", "text": "subagent"}]}}),
            json!({"type": "assistant", "isSidechain": false, "message": {"id": "m1", "role": "assistant", "content": [{"type": "tool_use", "id": "a", "name": "bash", "input": {}}]}}),
            json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "a", "content": "ok"}]}}),
            json!({"type": "assistant", "isSidechain": true, "message": {"id": "s2", "role": "assistant", "content": [{"type": "text", "text": "subagent again"}]}}),
            json!({"type": "assistant", "message": {"id": "m2", "role": "assistant", "content": [{"type": "text", "text": "done"}]}}),
        ]);
        let history = claude_history(&raw);
        let roles: Vec<&str> = history
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["user", "assistant", "user", "assistant"]);
        assert_eq!(history[0]["content"], "task");
        assert_eq!(history[3]["content"][0]["text"], "done");
        assert!(!serde_json::to_string(&history)
            .unwrap()
            .contains("subagent"));
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

    fn chat_session(steps: usize, result_chars: usize) -> Vec<Value> {
        let mut history = vec![
            json!({"role": "system", "content": "you are an agent"}),
            json!({"role": "user", "content": "fix the bug"}),
        ];
        for i in 0..steps {
            history.push(json!({"role": "assistant", "content": format!("step {i}"),
                "tool_calls": [{"id": format!("call_{i}"), "type": "function",
                    "function": {"name": "bash", "arguments": format!("{{\"cmd\":\"s{i}\"}}")}}]}));
            history.push(json!({"role": "tool", "tool_call_id": format!("call_{i}"),
                "content": "R".repeat(result_chars)}));
        }
        history
    }

    /// Per request, from replays of growing prefixes (a replay of the first
    /// k requests is the first k requests of the full replay): cache read,
    /// estimate sent, and whether it compacted.
    fn per_request(
        history: &[Value],
        dialect: Dialect,
        cfg: &CliffConfig,
    ) -> Vec<(u64, u64, bool)> {
        let mut rows = Vec::new();
        let mut read_so_far = 0;
        for (k, end) in request_points(history, dialect).into_iter().enumerate() {
            let report = replay(&history[..end], dialect, cfg.clone(), 200);
            assert_eq!(report.requests, k + 1);
            assert_eq!(
                report.est_cache_read_tokens + report.est_cache_write_tokens,
                report.total_est_tokens_out
            );
            let compacted = report.compactions.last().is_some_and(|c| c.request == k);
            rows.push((
                report.est_cache_read_tokens - read_so_far,
                report.last_est_tokens_out,
                compacted,
            ));
            read_so_far = report.est_cache_read_tokens;
        }
        rows
    }

    fn fixed_estimate(dialect: Dialect) -> u64 {
        let key = match dialect {
            Dialect::Anthropic => "system",
            Dialect::Responses => "instructions",
            Dialect::ChatCompletions => "tools",
        };
        let mut body = Map::new();
        body.insert("model".into(), json!("replay"));
        body.insert(key.into(), json!("x".repeat(800)));
        body.insert(
            dialect.messages_key().into(),
            json!([{"role": "user", "content": "t"}]),
        );
        Engine::new(CliffConfig::default())
            .prepare(body, dialect)
            .unwrap()
            .est_fixed_tokens
    }

    #[test]
    fn cache_reads_follow_extensions_and_reset_to_the_fixed_fields_on_compaction() {
        let cfg = CliffConfig {
            threshold_tokens: 3_000,
            keep_recent: 1,
            ..CliffConfig::default()
        };
        for (dialect, history) in [
            (Dialect::Anthropic, a_session(16, 3000)),
            (Dialect::ChatCompletions, chat_session(16, 1500)),
        ] {
            let fixed = fixed_estimate(dialect);
            let rows = per_request(&history, dialect, &cfg);
            let compactions = rows.iter().filter(|row| row.2).count();
            assert!(compactions >= 2, "{dialect:?}: {compactions} compactions");
            assert!(rows.len() > compactions + 2);
            for (k, &(read, _, compacted)) in rows.iter().enumerate() {
                let expected = if k == 0 || compacted {
                    fixed
                } else {
                    rows[k - 1].1
                };
                assert_eq!(read, expected, "{dialect:?} request {k}");
            }
            let report = replay(&history, dialect, cfg.clone(), 200);
            let compactions: Vec<usize> = report.compactions.iter().map(|c| c.request).collect();
            let gaps: Vec<usize> = compactions.windows(2).map(|w| w[1] - w[0]).collect();
            assert_eq!(report.min_compaction_gap, gaps.iter().copied().min());
            assert_eq!(
                report.back_to_back_compactions,
                gaps.iter().filter(|gap| **gap == 1).count()
            );
            for c in &report.compactions {
                assert!(c.summary_tokens > 0 && c.tail_tokens > 0);
                assert!(c.head_tokens + c.summary_tokens + c.tail_tokens <= c.est_tokens_after);
            }
        }
    }

    #[test]
    fn read_keys_match_urls_file_ranges_and_canonical_inputs() {
        let key = |name: &str, input: Value| read_key(name, &input);
        assert_eq!(
            key(
                "WebFetch",
                json!({"url": "https://a.example/x", "prompt": "summarize"})
            ),
            key(
                "WebFetch",
                json!({"prompt": "list the links", "url": "https://a.example/x"})
            )
        );
        assert_ne!(
            key("WebFetch", json!({"url": "https://a.example/x"})),
            key("WebFetch", json!({"url": "https://a.example/y"}))
        );
        assert_ne!(
            key("WebFetch", json!({"url": "https://a.example/x"})),
            key("Browse", json!({"url": "https://a.example/x"}))
        );
        assert_eq!(
            key(
                "Read",
                json!({"file_path": "/r/a.rs", "offset": 10, "limit": 50})
            ),
            key(
                "Read",
                json!({"limit": 50, "offset": 10, "file_path": "/r/a.rs"})
            )
        );
        assert_ne!(
            key(
                "Read",
                json!({"file_path": "/r/a.rs", "offset": 10, "limit": 50})
            ),
            key(
                "Read",
                json!({"file_path": "/r/a.rs", "offset": 60, "limit": 50})
            )
        );
        assert_ne!(
            key("Read", json!({"file_path": "/r/a.rs"})),
            key("Read", json!({"file_path": "/r/a.rs", "limit": 50}))
        );
        assert_eq!(
            key("Bash", json!({"command": "ls", "timeout": 5})),
            key("Bash", json!({"timeout": 5, "command": "ls"}))
        );
        assert_ne!(
            key("Bash", json!({"command": "ls"})),
            key("Bash", json!({"command": "ls -l"}))
        );
    }

    /// A session that reads `url` and a file range early, then pads with
    /// `pad` large steps, then repeats both reads and one fresh read.
    fn anthropic_rereads(pad: usize) -> Vec<Value> {
        let fetch = |id: &str, prompt: &str| {
            a_assistant(
                "",
                Some((
                    id,
                    "WebFetch",
                    json!({"url": "https://a.example/doc", "prompt": prompt}),
                )),
            )
        };
        let read = |id: &str, offset: u64| {
            a_assistant(
                "",
                Some((
                    id,
                    "Read",
                    json!({"file_path": "/r/a.rs", "offset": offset, "limit": 40}),
                )),
            )
        };
        let mut history = vec![a_user("study the docs")];
        history.extend([fetch("f1", "summarize"), a_result("f1", &"P".repeat(2000))]);
        history.extend([read("r1", 1), a_result("r1", &"F".repeat(2000))]);
        for i in 0..pad {
            let id = format!("pad{i}");
            history.push(a_assistant(
                "",
                Some((&id, "Bash", json!({"command": format!("step {i}")}))),
            ));
            history.push(a_result(&id, &"X".repeat(3000)));
        }
        history.extend([fetch("f2", "list links"), a_result("f2", &"P".repeat(2000))]);
        history.extend([read("r2", 1), a_result("r2", &"F".repeat(2000))]);
        history.extend([read("r3", 41), a_result("r3", &"F".repeat(2000))]);
        history
    }

    fn chat_rereads(pad: usize) -> Vec<Value> {
        let call = |id: &str, name: &str, arguments: &str| {
            json!({"role": "assistant", "content": null, "tool_calls": [{"id": id, "type": "function",
                "function": {"name": name, "arguments": arguments}}]})
        };
        let result =
            |id: &str, text: String| json!({"role": "tool", "tool_call_id": id, "content": text});
        let mut history = vec![json!({"role": "user", "content": "study the docs"})];
        history.push(call(
            "f1",
            "fetch",
            r#"{"url":"https://a.example/doc","prompt":"summarize"}"#,
        ));
        history.push(result("f1", "P".repeat(2000)));
        history.push(call("b1", "bash", r#"{"command":"cat notes","timeout":5}"#));
        history.push(result("b1", "N".repeat(2000)));
        for i in 0..pad {
            let id = format!("pad{i}");
            history.push(call(&id, "bash", &format!(r#"{{"command":"step {i}"}}"#)));
            history.push(result(&id, "X".repeat(3000)));
        }
        history.push(call(
            "f2",
            "fetch",
            r#"{"prompt":"list links","url":"https://a.example/doc"}"#,
        ));
        history.push(result("f2", "P".repeat(2000)));
        history.push(call("b2", "bash", r#"{"timeout":5,"command":"cat notes"}"#));
        history.push(result("b2", "N".repeat(2000)));
        history.push(call("b3", "bash", r#"{"command":"cat other"}"#));
        history.push(result("b3", "N".repeat(2000)));
        history
    }

    #[test]
    fn repeated_reads_are_covered_only_while_the_earlier_result_is_sent() {
        let roomy = CliffConfig {
            threshold_tokens: 1_000_000,
            ..CliffConfig::default()
        };
        let tight = CliffConfig {
            threshold_tokens: 3_000,
            keep_recent: 1,
            ..CliffConfig::default()
        };
        for (dialect, history) in [
            (Dialect::Anthropic, anthropic_rereads(8)),
            (Dialect::ChatCompletions, chat_rereads(8)),
        ] {
            assert!(pairing_intact(&history, dialect));
            let report = replay(&history, dialect, roomy.clone(), 200);
            assert_eq!(report.compacted, 0);
            assert_eq!(report.repeated_reads, 2, "{dialect:?}");
            assert_eq!(report.repeated_reads_covered, 2, "{dialect:?}");
            let report = replay(&history, dialect, tight.clone(), 200);
            assert!(report.compacted >= 1);
            assert_eq!(report.repeated_reads, 2, "{dialect:?}");
            assert_eq!(report.repeated_reads_covered, 0, "{dialect:?}");
        }
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
