//! Codex rollout adapter (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`).
//!
//! Record dialect (one JSON object per line):
//!   `{"timestamp": ..., "ordinal": n, "type": ..., "payload": {...}}`
//!
//! Context-carrying records are `response_item` payloads (messages,
//! function calls and outputs, reasoning). `token_usage_record` lines
//! carry the provider's own accounting — `usage.input_tokens` of the last
//! record approximates current context occupancy, and
//! `thread_token_usage` is the cumulative quota burn.

use gobstopper_core::estimate::estimate_tokens;
use gobstopper_core::model::{ItemKind, SessionHandle, TranscriptItem, UsageSample};
use gobstopper_core::plan::{DigestBlock, Edit};
use gobstopper_core::{Provider, Transcript};
use serde_json::Value;
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
            _ => ItemKind::System,
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

fn call_id(payload: &Value) -> Option<String> {
    payload
        .get("call_id")
        .or_else(|| payload.get("tool_call_id"))
        .or_else(|| payload.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
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
    if bytes.len() as u64 > crate::transaction::MAX_TRANSCRIPT_BYTES {
        return Err(AdapterError::InvalidEdit("transcript exceeds byte limit"));
    }
    let file = std::io::Cursor::new(bytes);
    let mut items: Vec<TranscriptItem> = Vec::new();
    let mut window_start = 0;
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
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match record.get("type").and_then(Value::as_str) {
            Some("token_usage_record") => absorb_usage(&record, &mut usage),
            Some("response_item") => {
                let payload = &record["payload"];
                let ptype = payload.get("type").and_then(Value::as_str).unwrap_or("");
                let role = payload.get("role").and_then(Value::as_str);
                let kind = classify(ptype, role);
                let elidable = elidable_bytes(payload);
                let est = estimate_tokens(value_len(payload));
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
                }
                window_start = items.len();
                usage.context_tokens = 0;
                // Post-compaction files carry live context inside
                // `replacement_history`; its tool outputs stay elidable.
                let payload = &record["payload"];
                // Count only items apply would actually stub — the
                // per-output floor must match `apply_elide` exactly or a
                // record full of small outputs reads as elidable forever
                // while every apply stubs nothing.
                let (bytes, parts) = payload
                    .get("replacement_history")
                    .and_then(Value::as_array)
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
                    .and_then(|items| {
                        crate::payload::fingerprint(items.iter().filter_map(|item| {
                            item.get("output")
                                .filter(|output| crate::payload::eligible_bytes(output) > 0)
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
    Ok(Transcript {
        session: handle,
        items,
        usage,
    })
}

fn absorb_usage(record: &Value, sample: &mut UsageSample) {
    let payload = &record["payload"];
    let get = |scope: &str, key: &str| -> u64 {
        payload
            .get(scope)
            .and_then(|s| s.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    // `input_tokens` already includes the cached portion on this schema.
    sample.context_tokens =
        get("usage", "input_tokens").saturating_add(get("usage", "output_tokens"));
    sample.lifetime_input_tokens = get("thread_token_usage", "input_tokens");
    sample.lifetime_cached_tokens = get("thread_token_usage", "cached_input_tokens");
}

/// Cheap usage pass for `detect`: read only the tail of the file.
pub fn scan_usage(path: &Path) -> UsageSample {
    let mut sample = UsageSample::default();
    for record in crate::tail_records(path, TAIL_SCAN_BYTES) {
        match record.get("type").and_then(Value::as_str) {
            Some("token_usage_record") => absorb_usage(&record, &mut sample),
            Some("compacted") => sample.context_tokens = 0,
            _ => {}
        }
    }
    sample
}

/// Read the parent thread id from a Codex session head, if this is a
/// sub-agent or forked thread. Returns `None` for a root/user thread.
pub fn parent_thread(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    for line in BufReader::new(file).lines().take(64) {
        let Ok(line) = line else { break };
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
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

/// Read session identity from the head of the file.
pub fn scan_meta(path: &Path) -> (Option<String>, Option<PathBuf>) {
    let Ok(file) = fs::File::open(path) else {
        return (None, None);
    };
    for line in BufReader::new(file).lines().take(64) {
        let Ok(line) = line else { break };
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
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
    line_indexes: &[usize],
    stub_template: &str,
    per_item_stubs: &std::collections::BTreeMap<usize, String>,
) -> (String, u64) {
    let targets: std::collections::HashSet<usize> = line_indexes.iter().copied().collect();
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
        match serde_json::from_str::<Value>(trimmed) {
            Ok(mut record) => {
                let is_compacted = record.get("type").and_then(Value::as_str) == Some("compacted");
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

/// Execute a plan's edits against a rollout file. `ProviderCompact` is a
/// no-op here — the CLI routes it to the provider instead.
pub fn apply(path: &Path, edits: &[Edit]) -> Result<u64, AdapterError> {
    crate::transaction::apply(Provider::Codex, path, |candidate| {
        apply_inner(candidate, edits)
    })
}

fn apply_inner(original: &str, edits: &[Edit]) -> Result<String, AdapterError> {
    let mut raw = original.to_string();
    for edit in edits {
        match edit {
            Edit::Elide {
                line_indexes,
                stub_template,
                per_item_stubs,
            } => raw = apply_elide(&raw, line_indexes, stub_template, per_item_stubs).0,
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
