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
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
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

/// Elidable payload bytes: tool output bodies only. `output` is a string
/// on `function_call_output` and an array of `input_text` blocks on
/// `custom_tool_call_output`.
fn elidable_bytes(payload: &Value) -> Option<u64> {
    let output = payload.get("output")?;
    let bytes = match output {
        Value::String(s) => s.len(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str).map(str::len))
            .sum(),
        other => value_len(other),
    };
    (bytes > 256).then_some(bytes as u64)
}

/// Parse a full rollout file into a normalized transcript.
pub fn load(handle: SessionHandle) -> Result<Transcript, AdapterError> {
    let file = fs::File::open(&handle.path).map_err(|e| AdapterError::Io {
        path: handle.path.clone(),
        source: e,
    })?;
    let mut items = Vec::new();
    let mut usage = UsageSample::default();
    for (line_index, line) in BufReader::new(file).lines().enumerate() {
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
                let est = elidable
                    .map(|b| estimate_tokens(b as usize))
                    .unwrap_or_else(|| estimate_tokens(value_len(payload)));
                items.push(TranscriptItem {
                    line_index,
                    kind,
                    est_tokens: est,
                    elidable_bytes: elidable,
                    label: format!("{ptype}@{line_index}"),
                });
            }
            Some("compacted") => {
                // Post-compaction files carry live context inside
                // `replacement_history`; its tool outputs stay elidable.
                let payload = &record["payload"];
                let bytes: u64 = payload
                    .get("replacement_history")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|i| i.get("output").map(elidable_output_bytes))
                            .sum()
                    })
                    .unwrap_or(0);
                let elidable = (bytes > 256).then_some(bytes);
                items.push(TranscriptItem {
                    line_index,
                    kind: ItemKind::ToolResult,
                    est_tokens: estimate_tokens(bytes.max(256) as usize),
                    elidable_bytes: elidable,
                    label: format!("compacted@{line_index}"),
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
    sample.context_tokens = get("usage", "input_tokens") + get("usage", "output_tokens");
    sample.lifetime_input_tokens = get("thread_token_usage", "input_tokens");
    sample.lifetime_cached_tokens = get("thread_token_usage", "cached_input_tokens");
}

/// Cheap usage pass for `detect`: read only the tail of the file.
pub fn scan_usage(path: &Path) -> UsageSample {
    let mut sample = UsageSample::default();
    let Ok(mut file) = fs::File::open(path) else {
        return sample;
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(TAIL_SCAN_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return sample;
    }
    let mut buf = String::new();
    if file.read_to_string(&mut buf).is_err() {
        return sample;
    }
    for line in buf.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record.get("type").and_then(Value::as_str) == Some("token_usage_record") {
            absorb_usage(&record, &mut sample);
        }
    }
    sample
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

/// Byte size of an `output` field, shared by classify and rewrite paths.
fn elidable_output_bytes(output: &Value) -> u64 {
    (match output {
        Value::String(s) => s.len(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str).map(str::len))
            .sum(),
        other => value_len(other),
    }) as u64
}

fn stub_for(template: &str, bytes: u64, kind: &str) -> String {
    template
        .replace("{bytes}", &bytes.to_string())
        .replace("{kind}", kind)
}

/// Apply an elide edit in place: rewrite only the `output` field of the
/// targeted records. Line order and count are preserved — the rollout's
/// `ordinal` sequence is never disturbed.
fn apply_elide(path: &Path, line_indexes: &[usize], stub_template: &str) -> Result<u64, AdapterError> {
    let targets: std::collections::HashSet<usize> = line_indexes.iter().copied().collect();
    let raw = fs::read_to_string(path).map_err(|e| AdapterError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
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
                let is_compacted =
                    record.get("type").and_then(Value::as_str) == Some("compacted");
                let Some(payload) = record.get_mut("payload") else {
                    out.push_str(line);
                    continue;
                };
                let kind = payload
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("output")
                    .to_string();
                let stub_value = |old: u64, was_array: bool| {
                    if was_array {
                        serde_json::json!([
                            {"type": "input_text", "text": stub_for(stub_template, old, &kind)}
                        ])
                    } else {
                        Value::String(stub_for(stub_template, old, &kind))
                    }
                };
                if is_compacted {
                    // Stub each output inside replacement_history in place.
                    if let Some(items) = payload
                        .get_mut("replacement_history")
                        .and_then(Value::as_array_mut)
                    {
                        for it in items.iter_mut() {
                            let Some(o) = it.get("output") else { continue };
                            let old = elidable_output_bytes(o);
                            if old <= 256 {
                                continue;
                            }
                            let was_array = o.is_array();
                            it["output"] = stub_value(old, was_array);
                            reclaimed += old;
                        }
                    }
                } else {
                    let Some(o) = payload.get("output") else {
                        out.push_str(line);
                        continue;
                    };
                    let old = elidable_output_bytes(o);
                    let was_array = o.is_array();
                    payload["output"] = stub_value(old, was_array);
                    reclaimed += old;
                }
                out.push_str(&serde_json::to_string(&record).unwrap_or_else(|_| trimmed.to_string()));
                if line.ends_with('\n') {
                    out.push('\n');
                }
            }
            Err(_) => out.push_str(line),
        }
    }
    let tmp = path.with_extension("jsonl.gobstopper-tmp");
    fs::File::create(&tmp)
        .and_then(|mut f| f.write_all(out.as_bytes()))
        .map_err(|e| AdapterError::Io {
            path: tmp.clone(),
            source: e,
        })?;
    fs::rename(&tmp, path).map_err(|e| AdapterError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(reclaimed)
}

pub(crate) fn digest_text(digest: &DigestBlock) -> String {
    let mut s = String::from("[gobstopper state card]\n");
    if let Some(goal) = &digest.goal {
        s.push_str(&format!("goal: {goal}\n"));
    }
    for d in &digest.decisions {
        s.push_str(&format!("decision: {d}\n"));
    }
    for f in &digest.files_touched {
        s.push_str(&format!("file: {f}\n"));
    }
    for t in &digest.open_tasks {
        s.push_str(&format!("todo: {t}\n"));
    }
    s.push_str(&format!("(covers {} earlier records)\n", digest.covers_items));
    s
}

/// Execute a plan's edits against a rollout file. `ProviderCompact` is a
/// no-op here — the CLI routes it to the provider instead.
pub fn apply(path: &Path, edits: &[Edit]) -> Result<u64, AdapterError> {
    let mut reclaimed = 0u64;
    for edit in edits {
        match edit {
            Edit::Elide {
                line_indexes,
                stub_template,
            } => reclaimed += apply_elide(path, line_indexes, stub_template)?,
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
                let mut f = fs::OpenOptions::new()
                    .append(true)
                    .open(path)
                    .map_err(|e| AdapterError::Io {
                        path: path.to_path_buf(),
                        source: e,
                    })?;
                writeln!(f, "{line}").map_err(|e| AdapterError::Io {
                    path: path.to_path_buf(),
                    source: e,
                })?;
            }
            Edit::ProviderCompact { .. } => {}
        }
    }
    Ok(reclaimed)
}

pub fn provider() -> Provider {
    Provider::Codex
}
