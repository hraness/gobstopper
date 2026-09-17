//! Claude Code session adapter (`~/.claude/projects/<cwd-slug>/<id>.jsonl`).
//!
//! Record dialect: one JSON object per line, linked by `uuid`/`parentUuid`
//! into a conversation tree. Context-carrying lines are `user` and
//! `assistant`; `tool_result` blocks live inside user message content.
//! Assistant lines carry `message.usage` with the provider's accounting —
//! `input + cache_read + cache_creation` approximates context occupancy.
//!
//! Rewrite rule: lines are never removed. Elision replaces tool_result
//! payloads in place so the uuid chain stays intact.

use gobstopper_core::estimate::estimate_tokens;
use gobstopper_core::model::{ItemKind, SessionHandle, TranscriptItem, UsageSample};
use gobstopper_core::plan::{DigestBlock, Edit};
use gobstopper_core::{Provider, Transcript};
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::AdapterError;

const TAIL_SCAN_BYTES: u64 = 512 * 1024;

fn value_len(v: &Value) -> usize {
    match v {
        Value::String(s) => s.len(),
        other => other.to_string().len(),
    }
}

/// Sum of elidable bytes across a user line's tool_result blocks.
fn tool_result_bytes(message: &Value) -> u64 {
    message
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                .map(|b| b.get("content").map(value_len).unwrap_or(0) as u64)
                .sum()
        })
        .unwrap_or(0)
}

fn has_tool_result(message: &Value) -> bool {
    tool_result_bytes(message) > 256
}

fn absorb_usage(line: &Value, sample: &mut UsageSample) {
    if line.get("type").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let Some(usage) = line.pointer("/message/usage") else {
        return;
    };
    let get = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let context = get("input_tokens")
        + get("cache_read_input_tokens")
        + get("cache_creation_input_tokens")
        + get("output_tokens");
    if context > 0 {
        sample.context_tokens = context;
    }
    sample.lifetime_input_tokens += get("input_tokens")
        + get("cache_read_input_tokens")
        + get("cache_creation_input_tokens");
    sample.lifetime_cached_tokens += get("cache_read_input_tokens");
}

/// Parse a full session file into a normalized transcript.
///
/// Claude sessions are trees: every line links `uuid -> parentUuid`, and
/// only the branch ending at the latest leaf is live context. Dead
/// branches (edited prompts, abandoned retries) occupy file bytes but no
/// context tokens, so they are excluded from estimates and elision.
pub fn load(handle: SessionHandle) -> Result<Transcript, AdapterError> {
    let file = fs::File::open(&handle.path).map_err(|e| AdapterError::Io {
        path: handle.path.clone(),
        source: e,
    })?;
    let mut items = Vec::new();
    let mut usage = UsageSample::default();
    // (line_index, uuid, parent_uuid) for live-branch resolution.
    let mut links: Vec<(usize, String, Option<String>)> = Vec::new();
    for (line_index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| AdapterError::Io {
            path: handle.path.clone(),
            source: e,
        })?;
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        absorb_usage(&record, &mut usage);
        if let Some(uuid) = record.get("uuid").and_then(Value::as_str) {
            links.push((
                line_index,
                uuid.to_string(),
                record
                    .get("parentUuid")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            ));
        }
        let ltype = record.get("type").and_then(Value::as_str).unwrap_or("");
        let (kind, elidable) = match ltype {
            "user" => {
                let message = &record["message"];
                let elidable = has_tool_result(message)
                    .then(|| tool_result_bytes(message))
                    .filter(|b| *b > 0);
                (ItemKind::User, elidable)
            }
            "assistant" => (ItemKind::Assistant, None),
            "system" => (ItemKind::System, None),
            "summary" | "file-history-snapshot" | "attachment" => (ItemKind::Meta, None),
            _ => (ItemKind::Meta, None),
        };
        if matches!(kind, ItemKind::Meta) && !matches!(ltype, "attachment") {
            continue; // bookkeeping lines never reach the context window
        }
        let est = elidable
            .map(|b| estimate_tokens(b as usize))
            .unwrap_or_else(|| estimate_tokens(line.len()));
        items.push(TranscriptItem {
            line_index,
            kind,
            est_tokens: est,
            elidable_bytes: elidable,
            label: format!("{ltype}@{line_index}"),
        });
    }
    let live = live_branch(&links);
    // Only uuid-bearing lines can be proven dead; lines without linkage
    // (attachments, system notices) stay conservatively live.
    let linked: std::collections::HashSet<usize> =
        links.iter().map(|(l, _, _)| *l).collect();
    for item in &mut items {
        let provably_dead = linked.contains(&item.line_index)
            && !live.is_empty()
            && !live.contains(&item.line_index);
        if provably_dead {
            item.est_tokens = 0;
            item.elidable_bytes = None;
            item.label.push_str(" (dead branch)");
        }
    }
    Ok(Transcript {
        session: handle,
        items,
        usage,
    })
}

/// Line indexes on the branch from the latest leaf back to the root.
/// Empty when the file carries no uuid linkage (e.g. bridge-only logs) —
/// callers treat that as "everything is live".
fn live_branch(links: &[(usize, String, Option<String>)]) -> std::collections::HashSet<usize> {
    use std::collections::{HashMap, HashSet};
    if links.is_empty() {
        return HashSet::new();
    }
    let parent_of: HashMap<&str, Option<&str>> = links
        .iter()
        .map(|(_, u, p)| (u.as_str(), p.as_deref()))
        .collect();
    let line_of: HashMap<&str, usize> = links
        .iter()
        .map(|(l, u, _)| (u.as_str(), *l))
        .collect();
    let mut live = HashSet::new();
    let mut cursor = Some(links.last().unwrap().1.as_str());
    let mut steps = 0usize;
    while let Some(uuid) = cursor {
        if let Some(&line) = line_of.get(uuid) {
            live.insert(line);
        }
        cursor = parent_of.get(uuid).copied().flatten();
        steps += 1;
        if steps > links.len() {
            break; // cycle guard
        }
    }
    live
}

/// Cheap usage pass for `detect`: read only the tail of the file.
pub fn scan_usage(path: &Path) -> UsageSample {
    let mut sample = UsageSample::default();
    let Ok(mut file) = fs::File::open(path) else {
        return sample;
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if file.seek(SeekFrom::Start(len.saturating_sub(TAIL_SCAN_BYTES))).is_err() {
        return sample;
    }
    let mut buf = String::new();
    if file.read_to_string(&mut buf).is_err() {
        return sample;
    }
    for line in buf.lines() {
        if let Ok(record) = serde_json::from_str::<Value>(line) {
            absorb_usage(&record, &mut sample);
        }
    }
    sample
}

/// Read session identity from any line that carries it.
pub fn scan_meta(path: &Path) -> (Option<String>, Option<PathBuf>) {
    let Ok(file) = fs::File::open(path) else {
        return (None, None);
    };
    let (mut id, mut cwd) = (None, None);
    for line in BufReader::new(file).lines().take(64) {
        let Ok(line) = line else { break };
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if id.is_none() {
            id = record
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if cwd.is_none() {
            cwd = record.get("cwd").and_then(Value::as_str).map(PathBuf::from);
        }
        if id.is_some() && cwd.is_some() {
            break;
        }
    }
    (id, cwd)
}

fn stub_for(template: &str, bytes: u64, kind: &str) -> String {
    template
        .replace("{bytes}", &bytes.to_string())
        .replace("{kind}", kind)
}

/// Replace every tool_result payload inside a user line with a stub.
/// The line — and therefore the uuid chain — is preserved verbatim except
/// for the elided block content.
fn elide_line(line: &str, stub_template: &str) -> (String, u64) {
    let Ok(mut record) = serde_json::from_str::<Value>(line) else {
        return (line.to_string(), 0);
    };
    let Some(blocks) = record
        .get_mut("message")
        .and_then(|m| m.get_mut("content"))
        .and_then(Value::as_array_mut)
    else {
        return (line.to_string(), 0);
    };
    let mut reclaimed = 0u64;
    for block in blocks.iter_mut() {
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let old = block.get("content").map(value_len).unwrap_or(0) as u64;
        if old <= 256 {
            continue;
        }
        block["content"] = Value::String(stub_for(stub_template, old, "tool_result"));
        reclaimed += old;
    }
    (
        serde_json::to_string(&record).unwrap_or_else(|_| line.to_string()),
        reclaimed,
    )
}

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
        let (rewritten, bytes) = elide_line(line.trim_end_matches('\n'), stub_template);
        reclaimed += bytes;
        out.push_str(&rewritten);
        if line.ends_with('\n') {
            out.push('\n');
        }
    }
    crate::write_if_unchanged(path, raw.as_bytes(), &out)?;
    Ok(reclaimed)
}

fn digest_text(digest: &DigestBlock) -> String {
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

/// Execute a plan's edits against a session file. `ProviderCompact` is a
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
                // Appended as a synthetic user line. Fresh uuid, no parent:
                // Claude tolerates orphan tips on resume and the state card
                // lands in the next turn's context.
                let text = digest_text(digest);
                let line = serde_json::json!({
                    "type": "user",
                    "uuid": format!("gobstopper-{:016x}", std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64).unwrap_or(0)),
                    "message": {"role": "user", "content": text},
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
    Provider::ClaudeCode
}
