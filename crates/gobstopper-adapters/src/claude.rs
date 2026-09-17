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
use std::io::{BufRead, BufReader};
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
                .map(|b| b.get("content").map(crate::payload::eligible_bytes).unwrap_or(0))
                .sum()
        })
        .unwrap_or(0)
}

fn has_tool_result(message: &Value) -> bool {
    tool_result_bytes(message) > 256
}

/// Short tail snippet of the tool_result blocks inside a user line.
fn tool_result_summary(message: &Value) -> Option<String> {
    const MAX_SUMMARY: usize = 200;
    let text: String = message
        .get("content")
        .and_then(Value::as_array)?
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        .filter_map(|b| b.get("content"))
        .map(crate::payload::text)
        .collect();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= MAX_SUMMARY {
        return Some(text);
    }
    Some(text.chars().rev().take(MAX_SUMMARY).collect::<String>().chars().rev().collect())
}

fn absorb_usage(line: &Value, sample: &mut UsageSample) {
    if line.get("type").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let Some(usage) = line.pointer("/message/usage") else {
        return;
    };
    let get = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let input = get("input_tokens").saturating_add(get("cache_read_input_tokens"))
        .saturating_add(get("cache_creation_input_tokens"));
    let context = input.saturating_add(get("output_tokens"));
    if context > 0 {
        sample.context_tokens = context;
    }
    sample.lifetime_input_tokens = sample.lifetime_input_tokens.saturating_add(input);
    sample.lifetime_cached_tokens = sample.lifetime_cached_tokens.saturating_add(get("cache_read_input_tokens"));
}

/// Parse a full session file into a normalized transcript.
///
/// Claude sessions are trees: every line links `uuid -> parentUuid`, and
/// only the branch ending at the latest leaf is live context. Dead
/// branches (edited prompts, abandoned retries) occupy file bytes but no
/// context tokens, so they are excluded from estimates and elision.
pub fn load(handle: SessionHandle) -> Result<Transcript, AdapterError> {
    let bytes = crate::transaction::read(&handle.path)?;
    load_bytes(handle, &bytes)
}

pub fn load_bytes(handle: SessionHandle, bytes: &[u8]) -> Result<Transcript, AdapterError> {
    if bytes.len() as u64 > crate::transaction::MAX_TRANSCRIPT_BYTES { return Err(AdapterError::InvalidEdit("transcript exceeds byte limit")); }
    let file = std::io::Cursor::new(bytes);
    let mut usage = UsageSample::default();
    let mut records: Vec<(usize, Value)> = Vec::new();
    for (line_index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| AdapterError::Io {
            path: handle.path.clone(),
            source: e,
        })?;
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        absorb_usage(&record, &mut usage);
        records.push((line_index, record));
    }

    // (line_index, uuid, parent_uuid) for live-branch resolution.
    let mut links: Vec<(usize, String, Option<String>)> = Vec::new();
    for (line_index, record) in &records {
        if let Some(uuid) = record.get("uuid").and_then(Value::as_str) {
            links.push((
                *line_index,
                uuid.to_string(),
                record
                    .get("parentUuid")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            ));
        }
    }

    // The canonical leaf is the leafUuid of the most recent last-prompt,
    // if one exists; otherwise the last user/assistant/attachment uuid.
    // Provider sessions interleave sidechains and bookkeeping after the real
    // conversation tip, so file order alone can point at a dead branch.
    let mut leaf_uuid: Option<String> = None;
    for (_, record) in records.iter().rev() {
        if record.get("type").and_then(Value::as_str) == Some("last-prompt") {
            if let Some(uuid) = record.get("leafUuid").and_then(Value::as_str) {
                leaf_uuid = Some(uuid.to_string());
                break;
            }
        }
    }
    if leaf_uuid.is_none() {
        for (_, record) in records.iter().rev() {
            if record.get("uuid").and_then(Value::as_str).is_some()
                && matches!(
                    record.get("type").and_then(Value::as_str),
                    Some("user") | Some("assistant") | Some("attachment")
                )
            {
                leaf_uuid = record.get("uuid").and_then(Value::as_str).map(str::to_string);
                break;
            }
        }
    }
    let live = leaf_uuid
        .as_deref()
        .map(|leaf| live_branch(&links, leaf))
        .unwrap_or_default();

    let mut items = Vec::new();
    for (line_index, record) in &records {
        let ltype = record.get("type").and_then(Value::as_str).unwrap_or("");
        let (kind, elidable) = match ltype {
            "user" => {
                let message = &record["message"];
                let elidable = has_tool_result(message)
                    .then(|| tool_result_bytes(message))
                    .filter(|b| *b > 0);
                (if elidable.is_some() { ItemKind::ToolResult } else { ItemKind::User }, elidable)
            }
            "assistant" => (ItemKind::Assistant, None),
            "system" => (ItemKind::System, None),
            "summary" | "file-history-snapshot" | "attachment" => (ItemKind::Meta, None),
            _ => (ItemKind::Meta, None),
        };
        if matches!(kind, ItemKind::Meta) && !matches!(ltype, "attachment") {
            continue; // bookkeeping lines never reach the context window
        }
        let est = estimate_tokens(value_len(&record["message"]));
        items.push(TranscriptItem {
            line_index: *line_index,
            kind,
            est_tokens: est,
            elidable_bytes: elidable,
            label: format!("{ltype}@{line_index}"),
            summary: tool_result_summary(&record["message"]),
        });
    }
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

/// Line indexes on the branch from the named leaf back to the root.
fn live_branch(links: &[(usize, String, Option<String>)], leaf: &str) -> std::collections::HashSet<usize> {
    use std::collections::{HashMap, HashSet};
    let parent_of: HashMap<&str, Option<&str>> = links
        .iter()
        .map(|(_, u, p)| (u.as_str(), p.as_deref()))
        .collect();
    let line_of: HashMap<&str, usize> = links
        .iter()
        .map(|(l, u, _)| (u.as_str(), *l))
        .collect();
    let mut live = HashSet::new();
    let mut cursor = Some(leaf);
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
    for record in crate::tail_records(path, TAIL_SCAN_BYTES) {
        absorb_usage(&record, &mut sample);
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
        let Some(content) = block.get_mut("content") else { continue };
        let old = crate::payload::eligible_bytes(content);
        reclaimed += crate::payload::elide(content, stub_for(stub_template, old, "tool_result"));
    }
    if reclaimed == 0 { return (line.to_string(), 0); }
    (
        serde_json::to_string(&record).unwrap_or_else(|_| line.to_string()),
        reclaimed,
    )
}

fn apply_elide(raw: &str, line_indexes: &[usize], stub_template: &str) -> (String, u64) {
    let targets: std::collections::HashSet<usize> = line_indexes.iter().copied().collect();
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
    (out, reclaimed)
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
    crate::transaction::apply(Provider::ClaudeCode, path, |candidate| apply_inner(candidate, edits))
}

fn apply_inner(original: &str, edits: &[Edit]) -> Result<String, AdapterError> {
    let mut raw = original.to_string();
    for edit in edits {
        match edit {
            Edit::Elide {
                line_indexes,
                stub_template,
            } => raw = apply_elide(&raw, line_indexes, stub_template).0,
            Edit::InjectDigest { digest } => {
                // Append the digest as a synthetic user line and follow it
                // with a fresh `last-prompt`/`mode` tail. Claude's resume
                // indexer expects this tail to recognize the fork; without it
                // the file is discoverable but `claude --resume` reports that
                // no conversation exists.
                let text = digest_text(digest);
                let parsed: Vec<Value> = raw
                    .lines()
                    .filter_map(|line| serde_json::from_str(line).ok())
                    .collect();
                let last_leaf = parsed.iter().rev().find(|r| {
                    matches!(
                        r.get("type").and_then(Value::as_str),
                        Some("user") | Some("assistant")
                    )
                });
                let last_mode = parsed.iter().rev().find(|r| {
                    r.get("type").and_then(Value::as_str) == Some("mode")
                });
                let parent = last_leaf
                    .and_then(|r| r.get("uuid").and_then(Value::as_str))
                    .map(str::to_string);
                let session_id = parsed
                    .iter()
                    .find_map(|r| r.get("sessionId").and_then(Value::as_str).map(str::to_string));
                let mode = last_mode
                    .and_then(|r| r.get("mode").and_then(Value::as_str))
                    .unwrap_or("auto");
                let digest_uuid = crate::fork::generate_session_id(Path::new("claude-digest"));
                let last_prompt_uuid = crate::fork::generate_session_id(Path::new("claude-last-prompt"));
                let mode_uuid = crate::fork::generate_session_id(Path::new("claude-mode"));
                let digest_user = serde_json::json!({
                    "type": "user",
                    "uuid": &digest_uuid,
                    "parentUuid": parent,
                    "sessionId": session_id,
                    "message": {"role": "user", "content": text},
                });
                crate::transaction::append_record(&mut raw, &digest_user)?;
                let last_prompt = serde_json::json!({
                    "type": "last-prompt",
                    "uuid": &last_prompt_uuid,
                    "lastPrompt": text,
                    "leafUuid": &digest_uuid,
                    "parentUuid": &digest_uuid,
                    "sessionId": session_id,
                });
                crate::transaction::append_record(&mut raw, &last_prompt)?;
                let mode_rec = serde_json::json!({
                    "type": "mode",
                    "uuid": &mode_uuid,
                    "mode": mode,
                    "parentUuid": &last_prompt_uuid,
                    "sessionId": session_id,
                });
                crate::transaction::append_record(&mut raw, &mode_rec)?;
            }
            Edit::ProviderCompact { .. } => {}
        }
    }
    Ok(raw)
}

pub fn provider() -> Provider {
    Provider::ClaudeCode
}
