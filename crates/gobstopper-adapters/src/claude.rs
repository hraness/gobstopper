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
                .map(|b| {
                    b.get("content")
                        .map(crate::payload::eligible_bytes)
                        .unwrap_or(0)
                })
                .sum()
        })
        .unwrap_or(0)
}

fn has_tool_result(message: &Value) -> bool {
    tool_result_bytes(message) > 256
}

/// Short opening of a plain user prompt, for digest `goal` generation.
fn user_prompt_summary(message: &Value) -> Option<String> {
    const MAX_SUMMARY: usize = 200;
    let text = crate::payload::text(message.get("content")?);
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= MAX_SUMMARY {
        return Some(text);
    }
    Some(text.chars().take(MAX_SUMMARY).collect())
}

/// Collect every assistant `tool_use` block by its `id`, returning a short
/// "name(input)" label for the matching `tool_result` summary.
#[derive(Clone)]
struct ToolUseMeta {
    name: String,
    label: String,
}

fn collect_tool_uses(record: &Value, map: &mut std::collections::HashMap<String, ToolUseMeta>) {
    if record.get("type").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let Some(content) = record
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let Some(id) = block.get("id").and_then(Value::as_str) else {
            continue;
        };
        let name = block.get("name").and_then(Value::as_str).unwrap_or("?");
        let input = match block.get("input").cloned().unwrap_or(Value::Null) {
            Value::String(s) => s,
            other => other.to_string(),
        };
        let input = if input.chars().count() > 80 {
            input.chars().take(80).collect::<String>() + "..."
        } else {
            input
        };
        if map.len() < gobstopper_core::validation::MAX_ITEMS {
            map.insert(
                id.to_string(),
                ToolUseMeta {
                    name: name.to_string(),
                    label: format!("{name}({input})"),
                },
            );
        }
    }
}

/// Short tail snippet of the tool_result blocks inside a user line,
/// annotated with the matching tool_use name and input.
fn tool_result_summary(
    message: &Value,
    tool_uses: &std::collections::HashMap<String, ToolUseMeta>,
) -> Option<String> {
    const MAX_SUMMARY: usize = 240;
    const TAIL: usize = 120;
    let content = message.get("content").and_then(Value::as_array)?;
    let mut parts = Vec::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let tool_use_id = block
            .get("tool_use_id")
            .and_then(Value::as_str)
            .unwrap_or("?");
        let call = tool_uses
            .get(tool_use_id)
            .map(|meta| meta.label.as_str())
            .unwrap_or("?");
        let output = block
            .get("content")
            .map(crate::payload::text)
            .unwrap_or_default();
        let tail = if output.chars().count() <= TAIL {
            output
        } else {
            let tail = output
                .chars()
                .rev()
                .take(TAIL)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>();
            format!("...{tail}")
        };
        parts.push(format!("{call} => {tail}"));
    }
    if parts.is_empty() {
        return None;
    }
    let full = parts.join("; ");
    if full.chars().count() <= MAX_SUMMARY {
        return Some(full);
    }
    Some(full.chars().take(MAX_SUMMARY).collect())
}

fn tool_result_metadata(
    message: &Value,
    tool_uses: &std::collections::HashMap<String, ToolUseMeta>,
) -> (u32, Vec<String>, String, Option<String>) {
    let mut parts = 0u32;
    let mut ids = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    if let Some(blocks) = message.get("content").and_then(Value::as_array) {
        for block in blocks {
            if block.get("type").and_then(Value::as_str) != Some("tool_result")
                || block
                    .get("content")
                    .map(crate::payload::eligible_bytes)
                    .unwrap_or(0)
                    == 0
            {
                continue;
            }
            parts = parts.saturating_add(1).min(1_000_000);
            if let Some(id) = block.get("tool_use_id").and_then(Value::as_str) {
                if ids.len() < 64 {
                    ids.push(id.to_string());
                }
                if let Some(meta) = tool_uses.get(id) {
                    names.insert(meta.name.clone());
                }
            }
        }
    }
    let label = if names.is_empty() {
        "tool_result".to_string()
    } else {
        names.into_iter().collect::<Vec<_>>().join("+")
    };
    let payload_sha256 = message
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| {
            crate::payload::fingerprint(blocks.iter().filter_map(|block| {
                (block.get("type").and_then(Value::as_str) == Some("tool_result"))
                    .then(|| block.get("content"))
                    .flatten()
                    .filter(|content| crate::payload::eligible_bytes(content) > 0)
            }))
        });
    (parts, ids, label, payload_sha256)
}

fn context_usage(line: &Value) -> Option<u64> {
    if line.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let usage = line.pointer("/message/usage")?;
    let get = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let input = get("input_tokens")
        .saturating_add(get("cache_read_input_tokens"))
        .saturating_add(get("cache_creation_input_tokens"));
    let context = input.saturating_add(get("output_tokens"));
    (context > 0).then_some(context)
}

fn absorb_usage(line: &Value, sample: &mut UsageSample) {
    if line.get("type").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let Some(usage) = line.pointer("/message/usage") else {
        return;
    };
    let get = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let input = get("input_tokens")
        .saturating_add(get("cache_read_input_tokens"))
        .saturating_add(get("cache_creation_input_tokens"));
    let context = input.saturating_add(get("output_tokens"));
    if context > 0 {
        sample.context_tokens = context;
    }
    sample.lifetime_input_tokens = sample.lifetime_input_tokens.saturating_add(input);
    sample.lifetime_cached_tokens = sample
        .lifetime_cached_tokens
        .saturating_add(get("cache_read_input_tokens"));
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
    if bytes.len() as u64 > crate::transaction::max_transcript_bytes() {
        return Err(AdapterError::InvalidEdit("transcript exceeds byte limit"));
    }
    let mut usage = UsageSample::default();
    let mut links: Vec<(usize, String, Option<String>)> = Vec::new();
    let mut context_samples = Vec::new();
    let mut last_prompt_leaf = None;
    let mut fallback_leaf = None;
    let mut tool_uses = std::collections::HashMap::new();
    for (line_index, line) in BufReader::new(std::io::Cursor::new(bytes))
        .lines()
        .enumerate()
    {
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
        absorb_usage(&record, &mut usage);
        if let Some(context) = context_usage(&record) {
            context_samples.push((line_index, context));
        }
        if let Some(uuid) = record.get("uuid").and_then(Value::as_str) {
            links.push((
                line_index,
                uuid.to_string(),
                record
                    .get("parentUuid")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            ));
            if matches!(
                record.get("type").and_then(Value::as_str),
                Some("user") | Some("assistant") | Some("attachment")
            ) {
                fallback_leaf = Some(uuid.to_string());
            }
        }
        if record.get("type").and_then(Value::as_str) == Some("last-prompt") {
            if let Some(uuid) = record.get("leafUuid").and_then(Value::as_str) {
                last_prompt_leaf = Some(uuid.to_string());
            }
        }
        collect_tool_uses(&record, &mut tool_uses);
    }
    let leaf_uuid = last_prompt_leaf.or(fallback_leaf);
    let live = leaf_uuid
        .as_deref()
        .map(|leaf| live_branch(&links, leaf))
        .unwrap_or_default();
    if !live.is_empty() {
        usage.context_tokens = context_samples
            .iter()
            .rev()
            .find(|(line, _)| live.contains(line))
            .map(|(_, context)| *context)
            .unwrap_or(usage.context_tokens);
    }

    let mut items = Vec::new();
    for (line_index, line) in BufReader::new(std::io::Cursor::new(bytes))
        .lines()
        .enumerate()
    {
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
        let ltype = record.get("type").and_then(Value::as_str).unwrap_or("");
        let (kind, elidable) = match ltype {
            "user" => {
                let message = &record["message"];
                let elidable = has_tool_result(message)
                    .then(|| tool_result_bytes(message))
                    .filter(|b| *b > 0);
                (
                    if elidable.is_some() {
                        ItemKind::ToolResult
                    } else {
                        ItemKind::User
                    },
                    elidable,
                )
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
        let summary = match kind {
            ItemKind::User => user_prompt_summary(&record["message"]),
            ItemKind::ToolResult => tool_result_summary(&record["message"], &tool_uses),
            _ => None,
        };
        let (elidable_parts, tool_use_ids, label, payload_sha256) = if kind == ItemKind::ToolResult
        {
            tool_result_metadata(&record["message"], &tool_uses)
        } else {
            (0, Vec::new(), ltype.to_string(), None)
        };
        items.push(TranscriptItem {
            line_index,
            kind,
            est_tokens: est,
            elidable_bytes: elidable,
            elidable_parts,
            label,
            summary,
            uuid: record
                .get("uuid")
                .and_then(Value::as_str)
                .map(str::to_string),
            parent_uuid: record
                .get("parentUuid")
                .and_then(Value::as_str)
                .map(str::to_string),
            tool_use_ids,
            payload_sha256,
        });
    }
    // Only uuid-bearing lines can be proven dead; lines without linkage
    // (attachments, system notices) stay conservatively live.
    let linked: std::collections::HashSet<usize> = links.iter().map(|(l, _, _)| *l).collect();
    for item in &mut items {
        let provably_dead = linked.contains(&item.line_index)
            && !live.is_empty()
            && !live.contains(&item.line_index);
        if provably_dead {
            item.est_tokens = 0;
            item.elidable_bytes = None;
            item.elidable_parts = 0;
            item.tool_use_ids.clear();
            item.payload_sha256 = None;
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
fn live_branch(
    links: &[(usize, String, Option<String>)],
    leaf: &str,
) -> std::collections::HashSet<usize> {
    use std::collections::{HashMap, HashSet};
    let parent_of: HashMap<&str, Option<&str>> = links
        .iter()
        .map(|(_, u, p)| (u.as_str(), p.as_deref()))
        .collect();
    let line_of: HashMap<&str, usize> = links.iter().map(|(l, u, _)| (u.as_str(), *l)).collect();
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
    let records = crate::tail_records(path, TAIL_SCAN_BYTES);
    let mut sample = UsageSample::default();
    let mut links = Vec::new();
    for (line, record) in records.iter().enumerate() {
        absorb_usage(record, &mut sample);
        if let Some(uuid) = record.get("uuid").and_then(Value::as_str) {
            links.push((
                line,
                uuid.to_string(),
                record
                    .get("parentUuid")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            ));
        }
    }
    let leaf = records
        .iter()
        .rev()
        .find(|record| record.get("type").and_then(Value::as_str) == Some("last-prompt"))
        .and_then(|record| record.get("leafUuid").and_then(Value::as_str))
        .or_else(|| {
            records.iter().rev().find_map(|record| {
                matches!(
                    record.get("type").and_then(Value::as_str),
                    Some("user") | Some("assistant") | Some("attachment")
                )
                .then(|| record.get("uuid").and_then(Value::as_str))
                .flatten()
            })
        });
    if let Some(leaf) = leaf {
        let live = live_branch(&links, leaf);
        sample.context_tokens = records
            .iter()
            .enumerate()
            .rev()
            .find(|(line, record)| live.contains(line) && context_usage(record).is_some())
            .and_then(|(_, record)| context_usage(record))
            .unwrap_or(sample.context_tokens);
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
/// for the elided block content. `stub_override` is complete stub text
/// (e.g. a model-written digest) that bypasses `{bytes}` substitution.
fn elide_line(line: &str, stub_template: &str, stub_override: Option<&str>) -> (String, u64) {
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
        let Some(content) = block.get_mut("content") else {
            continue;
        };
        let old = crate::payload::eligible_bytes(content);
        let stub = stub_override
            .map(str::to_string)
            .unwrap_or_else(|| stub_for(stub_template, old, "tool_result"));
        reclaimed += crate::payload::elide(content, stub);
    }
    if reclaimed == 0 {
        return (line.to_string(), 0);
    }
    (
        serde_json::to_string(&record).unwrap_or_else(|_| line.to_string()),
        reclaimed,
    )
}

fn apply_elide(
    raw: &str,
    line_indexes: &[usize],
    stub_template: &str,
    per_item_stubs: &std::collections::BTreeMap<usize, String>,
) -> (String, u64) {
    let targets: std::collections::HashSet<usize> = line_indexes.iter().copied().collect();
    let mut reclaimed = 0u64;
    let mut out = String::with_capacity(raw.len());
    for (idx, line) in raw.split_inclusive('\n').enumerate() {
        if !targets.contains(&idx) {
            out.push_str(line);
            continue;
        }
        let (rewritten, bytes) = elide_line(
            line.trim_end_matches('\n'),
            stub_template,
            per_item_stubs.get(&idx).map(String::as_str),
        );
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

/// Execute a plan's edits against a session file. `ProviderCompact` is a
/// no-op here — the CLI routes it to the provider instead.
pub fn apply(path: &Path, edits: &[Edit]) -> Result<u64, AdapterError> {
    crate::transaction::apply(Provider::ClaudeCode, path, |candidate| {
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
                let canonical_leaf = parsed
                    .iter()
                    .rev()
                    .find(|r| r.get("type").and_then(Value::as_str) == Some("last-prompt"))
                    .and_then(|r| r.get("leafUuid").and_then(Value::as_str))
                    .or_else(|| {
                        parsed.iter().rev().find_map(|r| {
                            matches!(
                                r.get("type").and_then(Value::as_str),
                                Some("user") | Some("assistant")
                            )
                            .then(|| r.get("uuid").and_then(Value::as_str))
                            .flatten()
                        })
                    });
                let last_leaf = canonical_leaf.and_then(|leaf| {
                    parsed
                        .iter()
                        .find(|r| r.get("uuid").and_then(Value::as_str) == Some(leaf))
                });
                let last_user = parsed
                    .iter()
                    .rev()
                    .find(|r| r.get("type").and_then(Value::as_str) == Some("user"));
                let last_mode = parsed
                    .iter()
                    .rev()
                    .find(|r| r.get("type").and_then(Value::as_str) == Some("mode"));
                let parent = canonical_leaf.map(str::to_string);
                let session_id = parsed.iter().find_map(|r| {
                    r.get("sessionId")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                });
                let mode = last_mode
                    .and_then(|r| r.get("mode").and_then(Value::as_str))
                    .unwrap_or("auto");
                let get = |key: &str| {
                    last_user
                        .and_then(|r| r.get(key).and_then(Value::as_str))
                        .or_else(|| last_leaf.and_then(|r| r.get(key).and_then(Value::as_str)))
                        .map(str::to_string)
                };
                let digest_uuid = crate::fork::generate_session_id(Path::new("claude-digest"));
                let last_prompt_uuid =
                    crate::fork::generate_session_id(Path::new("claude-last-prompt"));
                let prompt_id = crate::fork::generate_session_id(Path::new("claude-prompt"));
                let mode_uuid = crate::fork::generate_session_id(Path::new("claude-mode"));
                let digest_user = serde_json::json!({
                    "type": "user",
                    "uuid": &digest_uuid,
                    "parentUuid": parent,
                    "promptId": &prompt_id,
                    "timestamp": get("timestamp").unwrap_or_default(),
                    "permissionMode": get("permissionMode").unwrap_or_else(|| "auto".to_string()),
                    "promptSource": get("promptSource").unwrap_or_else(|| "cli".to_string()),
                    "userType": get("userType").unwrap_or_else(|| "external".to_string()),
                    "entrypoint": get("entrypoint").unwrap_or_else(|| "cli".to_string()),
                    "cwd": get("cwd").unwrap_or_else(|| std::env::var("HOME").unwrap_or_default()),
                    "sessionId": session_id,
                    "version": get("version").unwrap_or_else(|| "2.1.0".to_string()),
                    "gitBranch": get("gitBranch").unwrap_or_else(|| "main".to_string()),
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
            Edit::CacheEdit { .. } => {}
        }
    }
    Ok(raw)
}

pub fn provider() -> Provider {
    Provider::ClaudeCode
}

/// Claude Code writes `~/.claude/sessions/<pid>.json` for each running
/// process — `{pid, sessionId, status}` where status is `busy`/`idle`.
/// That is authoritative liveness: a session open-but-quiet in a TUI is
/// still owned, while its transcript file may sit untouched for minutes
/// (file mtime alone misreads that as idle and was the source of the
/// ChangedDuringWrite apply races).
///
/// Returns session_id → status for every session claimed by a *live*
/// pid; stale records from dead processes are ignored.
pub fn live_sessions(claude_home: &Path) -> std::collections::HashMap<String, String> {
    let mut live = std::collections::HashMap::new();
    let Ok(entries) = fs::read_dir(claude_home.join("sessions")) else {
        return live;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let (Some(pid), Some(session_id)) = (
            v.get("pid").and_then(Value::as_u64),
            v.get("sessionId").and_then(Value::as_str),
        ) else {
            continue;
        };
        if pid_alive(pid as i32) {
            live.insert(
                session_id.to_string(),
                v.get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            );
        }
    }
    live
}

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", "--", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn pid_alive(_pid: i32) -> bool {
    // Without a POSIX liveness probe, assume claimed: skipping a live
    // session is safe, mutating one is not.
    true
}

/// Ask Claude Code to compact a closed session natively:
/// `claude --resume <id> -p /compact` loads the transcript, runs the
/// provider's own summarization turn, and writes a `compact_boundary` +
/// continuation back to the session file — including firing the
/// provider's `PreCompact`/`SessionStart` hooks (which is where
/// gobstopper's pre-compact snapshot lands).
///
/// Safe only when no live process claims the session: resuming a
/// session open in a TUI would fork it. Callers must check
/// `live_sessions` first. Bounded wait; the child is killed on timeout.
pub fn headless_compact(
    claude_bin: &Path,
    session_id: &str,
    timeout_secs: u64,
) -> Result<(), AdapterError> {
    headless_compact_in_home(claude_bin, session_id, timeout_secs, None)
}

/// Run native compaction against the same Claude state root used for discovery.
pub fn headless_compact_in_home(
    claude_bin: &Path,
    session_id: &str,
    timeout_secs: u64,
    claude_home: Option<&Path>,
) -> Result<(), AdapterError> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let io_err = |kind, msg: String| AdapterError::Io {
        path: claude_bin.to_path_buf(),
        source: std::io::Error::new(kind, msg),
    };
    // Session ids are provider UUIDs; reject anything that could read as
    // a flag to the claude CLI.
    if session_id.is_empty()
        || session_id.starts_with('-')
        || !session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(io_err(
            std::io::ErrorKind::InvalidInput,
            format!("refusing unusual session id for --resume: {session_id:?}"),
        ));
    }
    // ETXTBSY can surface briefly when the binary was just (re)installed
    // or written by a test; retry the spawn a few times before failing.
    let mut child = {
        let mut attempt = 0u32;
        loop {
            let mut command = Command::new(claude_bin);
            command
                .args(["--resume", session_id, "-p", "/compact"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Some(home) = claude_home {
                command.env("CLAUDE_CONFIG_DIR", home);
            }
            match command.spawn() {
                Ok(c) => break c,
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 5 => {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(50 * attempt as u64));
                }
                Err(e) => {
                    return Err(io_err(
                        e.kind(),
                        format!("spawn claude --resume -p /compact: {e}"),
                    ))
                }
            }
        }
    };
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(io_err(
                    std::io::ErrorKind::Other,
                    format!("claude --resume -p /compact exited {status}"),
                ))
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io_err(
                    std::io::ErrorKind::TimedOut,
                    "claude --resume -p /compact timed out".to_string(),
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(250)),
            Err(e) => {
                return Err(AdapterError::Io {
                    path: claude_bin.to_path_buf(),
                    source: e,
                })
            }
        }
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gobstopper-claude-test-{}-{}-{tag}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn live_sessions_reports_only_live_pids() {
        let home = tmpdir("live");
        let sessions = home.join("sessions");
        let me = std::process::id();
        // A live pid (this test process) claims a session.
        write(
            &sessions.join(format!("{me}.json")),
            &format!(r#"{{"pid":{me},"sessionId":"live-sess","status":"idle","cwd":"/tmp"}}"#),
        );
        // A dead pid claims another — macOS pids stay under 100000, so
        // this is safely unowned.
        write(
            &sessions.join("99999999.json"),
            r#"{"pid":99999999,"sessionId":"dead-sess","status":"busy","cwd":"/tmp"}"#,
        );
        // Malformed and non-json records are ignored.
        write(&sessions.join("junk.json"), "not json");
        write(&sessions.join("notes.txt"), "{}");
        let live = live_sessions(&home);
        assert_eq!(live.get("live-sess").map(String::as_str), Some("idle"));
        assert!(!live.contains_key("dead-sess"));
        assert_eq!(live.len(), 1);
        // Missing sessions dir → empty map, not an error.
        let empty = tmpdir("empty");
        assert!(live_sessions(&empty).is_empty());
        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&empty).ok();
    }

    #[cfg(unix)]
    #[test]
    fn headless_compact_runs_resume_compact_and_checks_exit() {
        let dir = tmpdir("compact");
        let bin = dir.join("claude");
        let argv_out = dir.join("argv.txt");
        let home_out = dir.join("home.txt");
        write(
            &bin,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '%s' \"$CLAUDE_CONFIG_DIR\" > '{}'\nexit 0\n",
                argv_out.display(), home_out.display()
            ),
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }
        headless_compact(&bin, "sess-abc_123", 30).unwrap();
        let argv = fs::read_to_string(&argv_out).unwrap();
        assert_eq!(argv, "--resume\nsess-abc_123\n-p\n/compact\n");
        let configured_home = dir.join("explicit claude home");
        headless_compact_in_home(&bin, "sess-abc_123", 30, Some(&configured_home)).unwrap();
        assert_eq!(
            fs::read_to_string(&home_out).unwrap(),
            configured_home.to_string_lossy()
        );

        // Non-zero exit surfaces as an error.
        write(&bin, "#!/bin/sh\nexit 3\n");
        assert!(headless_compact(&bin, "sess-abc_123", 30).is_err());

        // Unusual session ids are refused before spawn.
        write(&bin, "#!/bin/sh\nexit 0\n");
        assert!(headless_compact(&bin, "--dangerous", 30).is_err());
        assert!(headless_compact(&bin, "has space", 30).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn headless_compact_kills_on_timeout() {
        let dir = tmpdir("timeout");
        let bin = dir.join("claude");
        write(&bin, "#!/bin/sh\nsleep 60\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let start = std::time::Instant::now();
        let err = headless_compact(&bin, "sess-abc", 1).unwrap_err();
        assert!(start.elapsed() < std::time::Duration::from_secs(30));
        assert!(err.to_string().contains("timed out"), "got: {err}");
        fs::remove_dir_all(&dir).ok();
    }
}
