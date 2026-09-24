//! Codex rollout adapter (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`).
//!
//! Record dialect (one JSON object per line):
//!   `{"timestamp": ..., "ordinal": n, "type": ..., "payload": {...}}`
//!
//! Context-carrying records are `response_item` payloads (messages,
//! function calls and outputs, reasoning). Both `token_usage_record` and
//! `event_msg`/`token_count` lines carry provider accounting. The latest
//! request's input plus output approximates current context occupancy;
//! thread/total usage supplies cumulative counters. `token_count.info`
//! also advertises the active model's context window when available.

use gobstopper_core::estimate::estimate_tokens;
use gobstopper_core::model::{ItemKind, SessionHandle, TranscriptItem, UsageSample};
use gobstopper_core::plan::{DigestBlock, Edit};
use gobstopper_core::{Provider, Transcript};
use serde_json::Value;
#[cfg(test)]
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::AdapterError;

/// Bytes read from the tail when only recent usage accounting is needed.
const TAIL_SCAN_BYTES: u64 = 512 * 1024;

fn classify(payload_type: &str, role: Option<&str>) -> ItemKind {
    match payload_type {
        "message" => match role {
            Some("user") => ItemKind::User,
            Some("assistant") => ItemKind::Assistant,
            Some("system" | "developer") => ItemKind::System,
            _ => ItemKind::Meta,
        },
        // Inter-agent traffic is assistant-authored content.
        "agent_message" => ItemKind::Assistant,
        "function_call" | "custom_tool_call" | "local_shell_call" => ItemKind::ToolCall,
        "function_call_output" | "custom_tool_call_output" => ItemKind::ToolResult,
        "reasoning" => ItemKind::Reasoning,
        // `compaction` records are the provider's own prior compaction
        // boundaries; keep them visible to strategies as Meta.
        _ => ItemKind::Meta,
    }
}

/// Serialized length of a value, used for the context estimate.
fn value_len(v: &Value) -> usize {
    v.to_string().len()
}

/// Concatenated text of a tool `output` payload, whether it is a plain
/// string (`function_call_output`) or an array of `input_text` blocks
/// (`custom_tool_call_output`).
fn output_text(payload: &Value) -> Option<String> {
    match payload.get("type").and_then(Value::as_str)? {
        "function_call_output" => payload
            .get("output")
            .and_then(Value::as_str)
            .map(String::from),
        "custom_tool_call_output" => {
            payload
                .get("output")
                .and_then(Value::as_array)
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                            Some("input_text") => {
                                b.get("text").and_then(Value::as_str).map(String::from)
                            }
                            _ => None,
                        })
                        .collect()
                })
        }
        _ => None,
    }
}

/// Short opening of a user message, for digest `goal` generation.
fn user_prompt_summary(payload: &Value) -> Option<String> {
    const MAX_SUMMARY: usize = 200;
    let text = crate::payload::text(payload.get("content")?);
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= MAX_SUMMARY {
        return Some(text);
    }
    Some(text.chars().take(MAX_SUMMARY).collect())
}

/// Short tail snippet of a tool output, annotated with the matching call
/// name and arguments.
fn annotated_output_summary(
    payload: &Value,
    calls: &std::collections::HashMap<String, ToolCallMeta>,
) -> Option<String> {
    const MAX_SUMMARY: usize = 240;
    const TAIL: usize = 180;
    let text = output_text(payload)?;
    if text.is_empty() {
        return None;
    }
    let call_id = payload
        .get("call_id")
        .or_else(|| payload.get("tool_call_id"))
        .or_else(|| payload.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("?");
    let call = calls
        .get(call_id)
        .map(|meta| meta.label.as_str())
        .unwrap_or("?");
    let tail = if text.chars().count() <= TAIL {
        text
    } else {
        let tail = text
            .chars()
            .rev()
            .take(TAIL)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        format!("...{tail}")
    };
    let full = format!("{call} => {tail}");
    if full.chars().count() <= MAX_SUMMARY {
        return Some(full);
    }
    Some(full.chars().take(MAX_SUMMARY).collect())
}

pub(crate) fn record_call_id(payload: &Value) -> Option<&str> {
    payload
        .get("call_id")
        .or_else(|| payload.get("tool_call_id"))
        .or_else(|| payload.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
}

fn call_id(payload: &Value) -> Option<String> {
    record_call_id(payload).map(str::to_string)
}

pub(crate) fn supported_item(payload: &Value) -> bool {
    match payload.get("type").and_then(Value::as_str) {
        Some("message") => {
            matches!(
                payload.get("role").and_then(Value::as_str),
                Some("user" | "assistant" | "system" | "developer")
            ) && payload.get("content").is_some_and(|v| {
                v.as_array().is_some_and(Vec::is_empty) || crate::payload::supported_content(v)
            })
        }
        Some("function_call" | "custom_tool_call" | "local_shell_call") => {
            record_call_id(payload).is_some()
        }
        Some("function_call_output") => {
            record_call_id(payload).is_some()
                && supported_output_keys(payload)
                && payload.get("output").is_some_and(Value::is_string)
        }
        Some("custom_tool_call_output") => {
            record_call_id(payload).is_some()
                && supported_output_keys(payload)
                && payload.get("output").is_some_and(|v| {
                    crate::payload::supported_content(v)
                        && v.as_array().is_some_and(|a| {
                            a.iter().all(|b| {
                                b.get("type").and_then(Value::as_str) == Some("input_text")
                            })
                        })
                })
        }
        Some("reasoning" | "agent_message") => true,
        _ => false,
    }
}

fn supported_output_keys(payload: &Value) -> bool {
    payload.as_object().is_some_and(|object| {
        object.keys().all(|key| {
            matches!(
                key.as_str(),
                "type"
                    | "output"
                    | "call_id"
                    | "tool_call_id"
                    | "id"
                    | "internal_chat_message_metadata_passthrough"
            )
        })
    })
}

#[derive(Clone)]
struct ToolCallMeta {
    name: String,
    label: String,
}

fn call_label(payload: &Value) -> ToolCallMeta {
    let name = payload
        .get("name")
        .or_else(|| payload.get("command"))
        .and_then(Value::as_str)
        .unwrap_or("?");
    let args = match payload.get("arguments").or_else(|| payload.get("input")) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    let args = if args.chars().count() > 80 {
        args.chars().take(80).collect::<String>() + "..."
    } else {
        args
    };
    ToolCallMeta {
        name: name.to_string(),
        label: format!("{name}({args})"),
    }
}

/// Elidable payload bytes: tool output bodies only.
fn elidable_bytes(payload: &Value) -> Option<u64> {
    let output = payload.get("output")?;
    if !supported_item(payload) || !crate::payload::supported_content(output) {
        return None;
    }
    let text = output_text(payload)?;
    let bytes = text.len() as u64;
    (bytes > 256).then_some(bytes)
}

/// Parse a full rollout file into a normalized transcript.
pub fn load(handle: SessionHandle) -> Result<Transcript, AdapterError> {
    let bytes = crate::transaction::read(&handle.path)?;
    load_bytes(handle, &bytes)
}

pub fn load_bytes(handle: SessionHandle, bytes: &[u8]) -> Result<Transcript, AdapterError> {
    if bytes.len() as u64 > crate::transaction::max_transcript_bytes() {
        return Err(AdapterError::InvalidEdit("transcript exceeds byte limit"));
    }
    let file = std::io::Cursor::new(bytes);
    let mut items: Vec<TranscriptItem> = Vec::new();
    let mut window_start = 0;
    let mut ambiguous = false;
    let mut usage = UsageSample::default();
    let mut calls: std::collections::HashMap<String, ToolCallMeta> =
        std::collections::HashMap::new();
    for (line_index, line) in BufReader::new(file).lines().enumerate() {
        if line_index >= gobstopper_core::validation::MAX_ITEMS {
            return Err(AdapterError::InvalidEdit("transcript exceeds record limit"));
        }
        let line = line.map_err(|e| AdapterError::Io {
            path: handle.path.clone(),
            source: e,
        })?;
        let Ok(record) = crate::payload::decode_record(&line) else {
            ambiguous |= !line.trim().is_empty();
            continue;
        };
        ambiguous |= !record.is_object();
        ambiguous |= record
            .get("ordinal")
            .is_some_and(|value| value.as_u64().is_none());
        match record.get("type").and_then(Value::as_str) {
            Some("token_usage_record" | "event_msg") => absorb_usage(&record, &mut usage),
            Some("response_item") => {
                let payload = &record["payload"];
                let ptype = payload.get("type").and_then(Value::as_str).unwrap_or("");
                let role = payload.get("role").and_then(Value::as_str);
                let kind = classify(ptype, role);
                let elidable = elidable_bytes(payload);
                let est = if kind == ItemKind::Meta {
                    0
                } else {
                    estimate_tokens(value_len(payload))
                };
                let item_call_id = call_id(payload);
                if kind == ItemKind::ToolCall {
                    if let Some(id) = &item_call_id {
                        calls.insert(id.clone(), call_label(payload));
                    }
                }
                let summary = match kind {
                    ItemKind::User => user_prompt_summary(payload),
                    ItemKind::ToolResult => annotated_output_summary(payload, &calls),
                    _ => None,
                };
                let label = item_call_id
                    .as_deref()
                    .and_then(|id| calls.get(id))
                    .map(|meta| meta.name.clone())
                    .unwrap_or_else(|| ptype.to_string());
                let tool_use_ids = if kind == ItemKind::ToolResult {
                    item_call_id.into_iter().collect()
                } else {
                    Vec::new()
                };
                let payload_sha256 = elidable.and_then(|_| {
                    payload
                        .get("output")
                        .and_then(|output| crate::payload::fingerprint(std::iter::once(output)))
                });
                items.push(TranscriptItem {
                    line_index,
                    kind,
                    est_tokens: est,
                    elidable_bytes: elidable,
                    elidable_parts: u32::from(elidable.is_some()),
                    label,
                    summary,
                    uuid: record
                        .get("id")
                        .or_else(|| payload.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    parent_uuid: record
                        .get("parent_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    tool_use_ids,
                    payload_sha256,
                });
            }
            Some("compacted") => {
                for item in &mut items[window_start..] {
                    item.est_tokens = 0;
                    item.elidable_bytes = None;
                    item.elidable_parts = 0;
                    item.tool_use_ids.clear();
                    item.payload_sha256 = None;
                }
                calls.clear();
                window_start = items.len();
                usage.reset_context();
                // Post-compaction files carry live context inside
                // `replacement_history`; its tool outputs stay elidable.
                let payload = &record["payload"];
                let history = payload.get("replacement_history").and_then(Value::as_array);
                if history.is_some_and(|h| h.len() > gobstopper_core::validation::MAX_ITEMS) {
                    return Err(AdapterError::InvalidEdit(
                        "compacted history exceeds record limit",
                    ));
                }
                ambiguous |= history.is_none();
                if let Some(history) = history {
                    for item in history {
                        if matches!(
                            item.get("type").and_then(Value::as_str),
                            Some("function_call" | "custom_tool_call" | "local_shell_call")
                        ) {
                            if let Some(id) = call_id(item) {
                                calls.insert(id, call_label(item));
                            }
                        }
                    }
                }
                // Count only items apply would actually stub — the
                // per-output floor must match `apply_elide` exactly or a
                // record full of small outputs reads as elidable forever
                // while every apply stubs nothing.
                let (bytes, parts) = payload
                    .get("replacement_history")
                    .and_then(Value::as_array)
                    .filter(|items| items.iter().all(supported_item))
                    .map(|items| {
                        items.iter().filter_map(elidable_bytes).fold(
                            (0u64, 0u32),
                            |(bytes, parts), size| {
                                (bytes.saturating_add(size), parts.saturating_add(1))
                            },
                        )
                    })
                    .unwrap_or_default();
                let elidable = (bytes > 0).then_some(bytes);
                let payload_sha256 = payload
                    .get("replacement_history")
                    .and_then(Value::as_array)
                    .filter(|items| items.iter().all(supported_item))
                    .and_then(|items| {
                        crate::payload::fingerprint(items.iter().filter_map(|item| {
                            elidable_bytes(item).and_then(|_| item.get("output"))
                        }))
                    });
                items.push(TranscriptItem {
                    line_index,
                    kind: ItemKind::ToolResult,
                    est_tokens: estimate_tokens(
                        payload
                            .get("replacement_history")
                            .map(value_len)
                            .unwrap_or(0),
                    ),

                    elidable_bytes: elidable,
                    elidable_parts: parts,
                    label: "compacted".to_string(),
                    summary: None,
                    uuid: record.get("id").and_then(Value::as_str).map(str::to_string),
                    parent_uuid: None,
                    tool_use_ids: Vec::new(),
                    payload_sha256,
                });
            }
            _ => {}
        }
    }
    let ambiguous_identity = crate::verify::verify(Provider::Codex, bytes)
        .iter()
        .any(|f| f.code == "duplicate_tool_call_id" || f.code == "duplicate_tool_result");
    if ambiguous || ambiguous_identity {
        usage.invalidate_context();
        usage.lifetime_scope = gobstopper_core::model::LifetimeScope::Partial;
        for item in &mut items {
            item.est_tokens = 0;
            item.elidable_bytes = None;
            item.elidable_parts = 0;
            item.tool_use_ids.clear();
            item.payload_sha256 = None;
        }
    }
    Ok(Transcript {
        session: handle,
        items,
        usage,
    })
}

fn absorb_usage(record: &Value, sample: &mut UsageSample) {
    let payload = &record["payload"];
    let (last, total, window) = match record["type"].as_str() {
        Some("token_usage_record") => (
            &payload["usage"],
            &payload["thread_token_usage"],
            &payload["model_context_window"],
        ),
        Some("event_msg") if payload["type"].as_str() == Some("token_count") => {
            let info = &payload["info"];
            (
                &info["last_token_usage"],
                &info["total_token_usage"],
                &info["model_context_window"],
            )
        }
        _ => return,
    };
    // Each dialect may appear for the same request, so replace cumulative
    // counters instead of adding them. Cached input is already in input.
    // Rate-limit-only token_count events have null info; missing fields must
    // not erase the last known usage or provider-advertised window.
    sample.observe_cumulative_report(
        last["input_tokens"]
            .as_u64()
            .map(|input| input.saturating_add(last["output_tokens"].as_u64().unwrap_or(0))),
        total["input_tokens"].as_u64(),
        total["cached_input_tokens"].as_u64(),
        window.as_u64(),
    );
    if last.as_object().is_some_and(|fields| {
        ["input_tokens", "output_tokens"]
            .iter()
            .any(|key| fields.get(*key).is_some_and(|value| !value.is_u64()))
    }) {
        sample.invalidate_context();
    }
    if total.as_object().is_some_and(|fields| {
        ["input_tokens", "cached_input_tokens"]
            .iter()
            .any(|key| fields.get(*key).is_some_and(|value| !value.is_u64()))
    }) {
        sample.lifetime_scope = gobstopper_core::model::LifetimeScope::Partial;
    }
}

/// Cheap usage pass for `detect`: read only the tail of the file.
pub fn scan_usage(path: &Path) -> UsageSample {
    let mut sample = UsageSample::default();
    let Some((records, _complete)) = crate::tail_records(path, TAIL_SCAN_BYTES) else {
        sample.invalidate_context();
        return sample;
    };
    for record in records {
        match record.get("type").and_then(Value::as_str) {
            Some("token_usage_record" | "event_msg") => absorb_usage(&record, &mut sample),
            Some("compacted") => sample.reset_context(),
            _ => {}
        }
    }
    sample
}

/// Read the parent thread id from a Codex session head, if this is a
/// sub-agent or forked thread. Returns `None` for a root/user thread.
pub fn parent_thread(path: &Path) -> Option<String> {
    for record in crate::payload::head_records(path, 64) {
        if record.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        let payload = &record["payload"];
        // Native fork/subagent fields.
        if let Some(parent) = payload
            .get("parent_thread_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                payload
                    .get("forked_from_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
        {
            return Some(parent);
        }
        // Multi-agent `source.subagent.thread_spawn.parent_thread_id`.
        if let Some(parent) = payload
            .get("source")
            .and_then(|s| s.get("subagent"))
            .and_then(|s| s.get("thread_spawn"))
            .and_then(|s| s.get("parent_thread_id"))
            .and_then(Value::as_str)
        {
            return Some(parent.to_string());
        }
    }
    None
}

/// True when the session was spawned as a multi-agent v2 sub-agent.
/// The app-server rejects `thread/resume` on these ("resume the parent
/// first"), so provider-native compaction must target the parent instead.
/// Plain `forked_from_id` threads resume fine and are not flagged here.
pub fn is_subagent_thread(path: &Path) -> bool {
    for record in crate::payload::head_records(path, 64) {
        if record.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        return record["payload"]
            .get("source")
            .and_then(|s| s.get("subagent"))
            .is_some();
    }
    false
}

/// Read session identity from the head of the file.
pub fn scan_meta(path: &Path) -> (Option<String>, Option<PathBuf>) {
    for record in crate::payload::head_records(path, 64) {
        if record.get("type").and_then(Value::as_str) == Some("session_meta") {
            let payload = &record["payload"];
            let id = payload
                .get("id")
                .or_else(|| payload.get("session_id"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let cwd = payload
                .get("cwd")
                .and_then(Value::as_str)
                .map(PathBuf::from);
            return (id, cwd);
        }
    }
    (None, None)
}

fn stub_for(template: &str, bytes: u64, kind: &str) -> String {
    template
        .replace("{bytes}", &bytes.to_string())
        .replace("{kind}", kind)
}

/// Apply an elide edit in place: rewrite only the `output` field of the
/// targeted records. Line order and count are preserved — the rollout's
/// `ordinal` sequence is never disturbed.
fn apply_elide(
    raw: &str,
    targets: &std::collections::HashSet<usize>,
    stub_template: &str,
    per_item_stubs: &std::collections::BTreeMap<usize, String>,
) -> (String, u64) {
    let render = |idx: usize, old: u64, kind: &str| {
        per_item_stubs
            .get(&idx)
            .cloned()
            .unwrap_or_else(|| stub_for(stub_template, old, kind))
    };
    let mut reclaimed = 0u64;
    let mut out = String::with_capacity(raw.len());
    for (idx, line) in raw.split_inclusive('\n').enumerate() {
        if !targets.contains(&idx) {
            out.push_str(line);
            continue;
        }
        let trimmed = line.trim_end_matches('\n');
        match crate::payload::decode_record(trimmed) {
            Ok(mut record) => {
                let kind = record.get("type").and_then(Value::as_str);
                if !matches!(kind, Some("response_item" | "compacted")) {
                    out.push_str(line);
                    continue;
                }
                let is_compacted = kind == Some("compacted");
                let Some(payload) = record.get_mut("payload") else {
                    out.push_str(line);
                    continue;
                };
                let kind = payload
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("output")
                    .to_string();
                if is_compacted {
                    // Stub each output inside replacement_history in place.
                    let mut stubbed_any = false;
                    if let Some(items) = payload
                        .get_mut("replacement_history")
                        .and_then(Value::as_array_mut)
                    {
                        for it in items.iter_mut() {
                            let Some(old) = elidable_bytes(it) else {
                                continue;
                            };
                            let Some(output) = it.get_mut("output") else {
                                continue;
                            };
                            let changed = crate::payload::elide(output, render(idx, old, &kind));
                            reclaimed += changed;
                            stubbed_any |= changed > 0;
                        }
                    }
                    // Nothing met the floor — pass the original bytes
                    // through rather than re-serializing for zero gain.
                    if !stubbed_any {
                        out.push_str(line);
                        continue;
                    }
                } else {
                    let Some(old) = elidable_bytes(payload) else {
                        out.push_str(line);
                        continue;
                    };
                    let changed =
                        crate::payload::elide(&mut payload["output"], render(idx, old, &kind));
                    if changed == 0 {
                        out.push_str(line);
                        continue;
                    }
                    reclaimed += changed;
                }
                out.push_str(
                    &serde_json::to_string(&record).unwrap_or_else(|_| trimmed.to_string()),
                );
                if line.ends_with('\n') {
                    out.push('\n');
                }
            }
            Err(_) => out.push_str(line),
        }
    }
    (out, reclaimed)
}

pub(crate) fn digest_text(digest: &DigestBlock) -> String {
    let mut s = String::from("[gobstopper state card]\n");
    if let Some(goal) = &digest.goal {
        s.push_str(&format!("goal: {goal}\n"));
    }
    if let Some(summary) = &digest.summary {
        s.push_str(&format!("summary: {summary}\n"));
    }
    for c in &digest.concepts {
        s.push_str(&format!("concept: {c}\n"));
    }
    for f in &digest.files_touched {
        s.push_str(&format!("file: {f}\n"));
    }
    for d in &digest.decisions {
        s.push_str(&format!("decision: {d}\n"));
    }
    for e in &digest.errors {
        s.push_str(&format!("error: {e}\n"));
    }
    for t in &digest.open_tasks {
        s.push_str(&format!("todo: {t}\n"));
    }
    if let Some(work) = &digest.current_work {
        s.push_str(&format!("current: {work}\n"));
    }
    if let Some(ctx) = &digest.context {
        s.push_str(&format!("context: {ctx}\n"));
    }
    s.push_str(&format!(
        "(covers {} earlier records)\n",
        digest.covers_items
    ));
    s
}

/// Direct provider-file mutation is disabled: an arbitrary path and an idle
/// observation cannot establish compatible lifetime custody. Use [`transform`]
/// to prepare bytes for an independently admitted, no-clobber copy.
pub fn apply(_path: &Path, _edits: &[Edit]) -> Result<u64, AdapterError> {
    Err(AdapterError::DirectMutationDisabled)
}

/// Transform detached transcript bytes without reading or writing any file.
/// Provider-control edits retain their no-op lowering here; dispatch belongs
/// to the session owner. Generated digest identities need not be deterministic.
pub fn transform(original: &[u8], edits: &[Edit]) -> Result<Vec<u8>, AdapterError> {
    crate::payload::check_edit_bounds(edits)?;
    crate::transaction::prepare(Provider::Codex, original, |text| apply_inner(text, edits))
}

fn apply_inner(original: &str, edits: &[Edit]) -> Result<String, AdapterError> {
    let mut raw = original.to_string();
    for edit in edits {
        match edit {
            Edit::Elide {
                line_indexes,
                stub_template,
                per_item_stubs,
            } => {
                let targets = crate::payload::elision_targets(Provider::Codex, &raw, line_indexes)?;
                raw = apply_elide(&raw, &targets, stub_template, per_item_stubs).0;
            }
            Edit::InjectDigest { digest } => {
                // Appended as a user message; Codex rebuilds context from
                // rollout items on resume, so a trailing state card lands
                // in the next turn's context.
                let text = digest_text(digest);
                let line = serde_json::json!({
                    "timestamp": null,
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "user",
                        "content": [{"type": "input_text", "text": text}],
                    }
                });
                crate::transaction::append_record(&mut raw, &line)?;
            }
            Edit::ProviderCompact { .. } => {}
            Edit::CacheEdit { .. } => {}
        }
    }
    Ok(raw)
}

pub fn provider() -> Provider {
    Provider::Codex
}

#[cfg(test)]
mod usage_tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn assert_usage(records: &[Value], expected: UsageSample) {
        let path = Scratch(std::env::temp_dir().join(format!(
            "gob-codex-usage-{}-{}.jsonl",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        let raw = records
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(&path.0, &raw).unwrap();
        let handle = SessionHandle {
            provider: Provider::Codex,
            session_id: "usage-test".into(),
            path: path.0.clone(),
            cwd: None,
            age_secs: 0,
        };
        for sample in [scan_usage(&path.0), load(handle).unwrap().usage] {
            assert_eq!(sample.context_tokens, expected.context_tokens);
            assert_eq!(sample.lifetime_input_tokens, expected.lifetime_input_tokens);
            assert_eq!(
                sample.lifetime_cached_tokens,
                expected.lifetime_cached_tokens
            );
            assert_eq!(sample.model_context_window, expected.model_context_window);
        }
    }

    fn token_count(info: Value) -> Value {
        json!({"type": "event_msg", "payload": {"type": "token_count", "info": info}})
    }

    #[test]
    fn is_subagent_thread_flags_only_subagent_source() {
        let write = |payload: Value| {
            let path = Scratch(std::env::temp_dir().join(format!(
                "gob-codex-sub-{}-{}.jsonl",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )));
            fs::write(
                &path.0,
                json!({"type": "session_meta", "payload": payload}).to_string() + "\n",
            )
            .unwrap();
            path
        };
        // Multi-agent spawn marker → sub-agent (resume rejected).
        assert!(is_subagent_thread(
            &write(json!({"source": {"subagent": {"thread_spawn": {"parent_thread_id": "p"}}}})).0
        ));
        // Plain fork → resumable, not a sub-agent.
        assert!(!is_subagent_thread(
            &write(json!({"forked_from_id": "p", "source": "vscode"})).0
        ));
        // Root thread.
        assert!(!is_subagent_thread(&write(json!({"source": "vscode"})).0));
    }

    #[test]
    fn current_token_count_uses_last_usage_for_context_and_total_for_lifetime() {
        assert_usage(
            &[token_count(json!({
                "last_token_usage": {"input_tokens": 82620, "cached_input_tokens": 75776, "output_tokens": 130, "total_tokens": 82750},
                "total_token_usage": {"input_tokens": 647640, "cached_input_tokens": 583680, "output_tokens": 2420, "total_tokens": 650060},
                "model_context_window": 258400
            }))],
            UsageSample {
                context_tokens: 82750,
                lifetime_input_tokens: 647640,
                lifetime_cached_tokens: 583680,
                model_context_window: Some(258400),
                context_state: gobstopper_core::model::ContextState::Reported,
                lifetime_scope: gobstopper_core::model::LifetimeScope::Full,
            },
        );
    }

    #[test]
    fn mixed_usage_dialects_preserve_window_without_double_counting() {
        assert_usage(
            &[
                token_count(json!({
                    "last_token_usage": {"input_tokens": 1000, "output_tokens": 100},
                    "total_token_usage": {"input_tokens": 5000, "cached_input_tokens": 4000},
                    "model_context_window": 258400
                })),
                json!({"type": "token_usage_record", "payload": {
                    "usage": {"input_tokens": 2000, "output_tokens": 200},
                    "thread_token_usage": {"input_tokens": 7000, "cached_input_tokens": 5500}
                }}),
                token_count(json!({
                    "last_token_usage": {"input_tokens": 2000, "output_tokens": 200},
                    "total_token_usage": {"input_tokens": 7000, "cached_input_tokens": 5500},
                    "model_context_window": null
                })),
                token_count(Value::Null),
                json!({"type": "event_msg", "payload": {"type": "task_complete"}}),
            ],
            UsageSample {
                context_tokens: 2200,
                lifetime_input_tokens: 7000,
                lifetime_cached_tokens: 5500,
                model_context_window: Some(258400),
                context_state: gobstopper_core::model::ContextState::Reported,
                lifetime_scope: gobstopper_core::model::LifetimeScope::Full,
            },
        );
    }

    #[test]
    fn legacy_usage_without_window_remains_unknown() {
        assert_usage(
            &[json!({"type": "token_usage_record", "payload": {
                "usage": {"input_tokens": 2000, "output_tokens": 200},
                "thread_token_usage": {"input_tokens": 7000, "cached_input_tokens": 5500}
            }})],
            UsageSample {
                context_tokens: 2200,
                lifetime_input_tokens: 7000,
                lifetime_cached_tokens: 5500,
                model_context_window: None,
                context_state: gobstopper_core::model::ContextState::Reported,
                lifetime_scope: gobstopper_core::model::LifetimeScope::Full,
            },
        );
    }

    #[test]
    fn compaction_clears_stale_context_but_retains_accounting_and_window() {
        assert_usage(
            &[
                token_count(json!({
                    "last_token_usage": {"input_tokens": 2000, "output_tokens": 200},
                    "total_token_usage": {"input_tokens": 7000, "cached_input_tokens": 5500},
                    "model_context_window": 258400
                })),
                json!({"type": "compacted", "payload": {"replacement_history": []}}),
                token_count(Value::Null),
            ],
            UsageSample {
                context_tokens: 0,
                lifetime_input_tokens: 7000,
                lifetime_cached_tokens: 5500,
                model_context_window: Some(258400),
                context_state: gobstopper_core::model::ContextState::Reset,
                lifetime_scope: gobstopper_core::model::LifetimeScope::Full,
            },
        );
    }

    #[test]
    fn newest_valid_window_wins_and_missing_counters_do_not_erase_usage() {
        assert_usage(
            &[
                token_count(json!({
                    "last_token_usage": {"input_tokens": 2000, "output_tokens": 200},
                    "total_token_usage": {"input_tokens": 7000, "cached_input_tokens": 5500},
                    "model_context_window": 258400
                })),
                token_count(json!({"model_context_window": 1000000})),
                token_count(json!({"model_context_window": 0})),
            ],
            UsageSample {
                context_tokens: 2200,
                lifetime_input_tokens: 7000,
                lifetime_cached_tokens: 5500,
                model_context_window: Some(1000000),
                context_state: gobstopper_core::model::ContextState::Reported,
                lifetime_scope: gobstopper_core::model::LifetimeScope::Full,
            },
        );
    }
}
