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

/// Discovery inspects a complete structural/usage projection under these
/// limits. Payloads are decoded one record at a time and are not retained.
pub const USAGE_SCAN_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const USAGE_SCAN_MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;

fn value_len(v: &Value) -> usize {
    match v {
        Value::String(s) => s.len(),
        other => other.to_string().len(),
    }
}

/// Only documented text and result blocks are eligible in this synthetic
/// dialect. A mixed unknown block makes the complete rewrite anchor unavailable.
fn supported_result_message(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("user")
        && message
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks
                    .iter()
                    .all(|block| match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            crate::payload::supported_content(&Value::Array(vec![block.clone()]))
                        }
                        Some("tool_result") => {
                            block
                                .get("tool_use_id")
                                .and_then(Value::as_str)
                                .is_some_and(|id| {
                                    !id.is_empty()
                                        && id.len() <= 256
                                        && !id.chars().any(char::is_control)
                                })
                                && block
                                    .get("content")
                                    .is_some_and(crate::payload::supported_content)
                                && block.as_object().is_some_and(|o| {
                                    o.keys().all(|key| {
                                        matches!(
                                            key.as_str(),
                                            "type" | "tool_use_id" | "content" | "is_error"
                                        )
                                    })
                                })
                                && block.get("is_error").is_none_or(Value::is_boolean)
                        }
                        _ => false,
                    })
            })
}

/// Sum of elidable bytes across a user line's tool_result blocks.
fn tool_result_bytes(message: &Value) -> u64 {
    if !supported_result_message(message) {
        return 0;
    }
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

fn context_usage(line: &Value) -> UsageSample {
    use gobstopper_core::model::{ContextComponents, ContextReason};
    let mut sample = UsageSample::default();
    let Some(metrics) = line.pointer("/message/usage").and_then(Value::as_object) else {
        sample.invalidate_context_because(if line.pointer("/message/usage").is_some() {
            ContextReason::MalformedComponent
        } else {
            ContextReason::MissingComponent
        });
        sample.lifetime_scope = gobstopper_core::model::LifetimeScope::Partial;
        return sample;
    };
    let mut reason = None;
    let mut component = |key: &str, optional: bool| match metrics.get(key) {
        // Preserve the existing omission convention for optional counters;
        // explicit null has no established zero semantics.
        None if optional => Some(0),
        Some(value) if value.is_null() => {
            if reason != Some(ContextReason::MalformedComponent) {
                reason = Some(ContextReason::NullComponent);
            }
            None
        }
        Some(value) if value.as_u64().is_some() => value.as_u64(),
        Some(_) => {
            reason = Some(ContextReason::MalformedComponent);
            None
        }
        None => {
            reason.get_or_insert(ContextReason::MissingComponent);
            None
        }
    };
    let components = ContextComponents {
        input_tokens: component("input_tokens", false),
        cache_read_tokens: component("cache_read_input_tokens", true),
        cache_creation_tokens: component("cache_creation_input_tokens", true),
        output_tokens: component("output_tokens", true),
    };
    sample.observe_context_components(components, reason);
    let input = components
        .input_tokens
        .unwrap_or(0)
        .checked_add(components.cache_read_tokens.unwrap_or(0))
        .and_then(|n| n.checked_add(components.cache_creation_tokens.unwrap_or(0)));
    sample.lifetime_input_tokens = input.unwrap_or(0);
    sample.lifetime_cached_tokens = components
        .cache_read_tokens
        .unwrap_or(0)
        .min(sample.lifetime_input_tokens);
    sample.lifetime_scope = if sample.reported_context().is_some() && input.is_some() {
        gobstopper_core::model::LifetimeScope::Full
    } else {
        gobstopper_core::model::LifetimeScope::Partial
    };
    sample
}

fn absorb_usage(line: &Value, sample: &mut UsageSample) {
    use gobstopper_core::model::LifetimeScope;
    if line.get("type").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let reading = context_usage(line);
    let input = sample
        .lifetime_input_tokens
        .checked_add(reading.lifetime_input_tokens);
    let cached = sample
        .lifetime_cached_tokens
        .checked_add(reading.lifetime_cached_tokens);
    if let Some(input) = input {
        sample.lifetime_input_tokens = input;
    }
    if let Some(cached) = cached {
        sample.lifetime_cached_tokens = cached.min(sample.lifetime_input_tokens);
    }
    if reading.lifetime_scope == LifetimeScope::Partial || input.is_none() || cached.is_none() {
        sample.lifetime_scope = LifetimeScope::Partial;
    } else if sample.lifetime_scope != LifetimeScope::Partial {
        sample.lifetime_scope = LifetimeScope::Full;
    }
}

type ClaudeLinks = Vec<(usize, String, Option<String>)>;

/// Structural evidence retained by both discovery and full loading. No
/// message text or tool payload survives a push into this projection.
#[derive(Default)]
struct UsageProjection {
    usage: UsageSample,
    links: ClaudeLinks,
    context_samples: Vec<(usize, UsageSample)>,
    context_resets: Vec<usize>,
    last_prompt_leaf: Option<String>,
    fallback_leaf: Option<String>,
    invalid_record: bool,
    ambiguous: bool,
}

fn valid_link_id(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
}

impl UsageProjection {
    fn push(&mut self, line: usize, record: &Value) {
        if !record.is_object() {
            self.invalid_record = true;
            return;
        }
        absorb_usage(record, &mut self.usage);
        if record.get("subtype").and_then(Value::as_str) == Some("compact_boundary") {
            self.context_resets.push(line);
        }
        if record.get("type").and_then(Value::as_str) == Some("assistant") {
            self.context_samples.push((line, context_usage(record)));
        }
        if record.get("uuid").is_none()
            && (matches!(
                record.get("type").and_then(Value::as_str),
                Some("user" | "assistant")
            ) || record.get("subtype").and_then(Value::as_str) == Some("compact_boundary"))
        {
            // A message without a link cannot be classified as live or dead;
            // it must not silently preserve an older reading. Unlinked
            // attachments and system notices are permitted by this dialect:
            // they remain protected context without changing message ancestry.
            self.ambiguous = true;
        }
        if let Some(uuid) = record.get("uuid") {
            if !valid_link_id(uuid)
                || !record
                    .get("parentUuid")
                    .is_none_or(|v| v.is_null() || valid_link_id(v))
            {
                self.ambiguous = true;
                return;
            }
            let uuid = uuid.as_str().expect("validated link");
            self.links.push((
                line,
                uuid.to_string(),
                record
                    .get("parentUuid")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            ));
            if matches!(
                record.get("type").and_then(Value::as_str),
                Some("user" | "assistant" | "attachment")
            ) {
                self.fallback_leaf = Some(uuid.to_string());
            }
        }
        if record.get("type").and_then(Value::as_str) == Some("last-prompt") {
            match record
                .get("leafUuid")
                .filter(|v| valid_link_id(v))
                .and_then(Value::as_str)
            {
                Some(uuid) => self.last_prompt_leaf = Some(uuid.to_string()),
                None => self.ambiguous = true,
            }
        }
    }

    fn finish(mut self) -> (UsageSample, ClaudeLinks, std::collections::HashSet<usize>) {
        use gobstopper_core::model::{ContextReason, ContextState, LifetimeScope};
        let leaf = self.last_prompt_leaf.or(self.fallback_leaf);
        let live = leaf
            .as_deref()
            .filter(|_| !self.ambiguous && !self.invalid_record)
            .map(|leaf| live_branch(&self.links, leaf))
            .unwrap_or_default();
        self.usage.context_tokens = 0;
        self.usage.context_state = ContextState::Absent;
        if self.invalid_record
            || self.ambiguous
            || (live.is_empty() && (leaf.is_some() || !self.links.is_empty()))
        {
            self.usage
                .invalidate_context_because(if self.invalid_record {
                    ContextReason::InvalidRecord
                } else {
                    ContextReason::InvalidAncestry
                });
            self.usage.lifetime_scope = LifetimeScope::Partial;
        } else if let Some((_, context)) = self
            .context_samples
            .iter()
            .rev()
            .find(|(line, _)| live.contains(line))
        {
            self.usage.context_tokens = context.context_tokens;
            self.usage.context_state = context.context_state;
            self.usage.context_components = context.context_components;
            self.usage.context_reason = context.context_reason;
        }
        let newest = self
            .context_samples
            .iter()
            .rev()
            .find(|(line, _)| live.contains(line))
            .map(|(line, _)| *line);
        if self
            .context_resets
            .iter()
            .any(|line| live.contains(line) && newest.is_none_or(|sample| *line > sample))
        {
            self.usage.reset_context();
        }
        (self.usage, self.links, live)
    }
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
    let mut projection = UsageProjection::default();
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
        if line.trim().is_empty() {
            continue;
        }
        match crate::payload::decode_record(&line) {
            Ok(record) => projection.push(line_index, &record),
            Err(_) => projection.invalid_record = true,
        }
    }
    let (mut usage, links, live) = projection.finish();

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
        let Ok(record) = crate::payload::decode_record(&line) else {
            continue;
        };
        if live.contains(&line_index) {
            collect_tool_uses(&record, &mut tool_uses);
        }
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
    let ambiguous_calls = crate::verify::verify(Provider::ClaudeCode, bytes)
        .iter()
        .any(|f| f.code == "duplicate_tool_call_id");
    if ambiguous_calls {
        usage.invalidate_context();
    }
    for item in &mut items {
        let unavailable = ambiguous_calls
            || live.is_empty()
            || linked.contains(&item.line_index) && !live.contains(&item.line_index)
            || item.elidable_bytes.is_some() && !linked.contains(&item.line_index);
        if unavailable {
            item.est_tokens = 0;
            item.elidable_bytes = None;
            item.elidable_parts = 0;
            item.tool_use_ids.clear();
            item.payload_sha256 = None;
            item.label.push_str(if live.is_empty() {
                " (unresolved branch)"
            } else {
                " (dead branch)"
            });
        }
    }
    Ok(Transcript {
        session: handle,
        items,
        usage,
    })
}

/// Line indexes on the branch from the named leaf back to the root.
pub(crate) fn live_branch(
    links: &[(usize, String, Option<String>)],
    leaf: &str,
) -> std::collections::HashSet<usize> {
    use std::collections::{HashMap, HashSet};
    let mut by_id = HashMap::new();
    for (line, id, parent) in links {
        if id.is_empty()
            || by_id
                .insert(id.as_str(), (*line, parent.as_deref()))
                .is_some()
        {
            return HashSet::new();
        }
    }
    let mut live = HashSet::new();
    let mut cursor = Some(leaf);
    while let Some(uuid) = cursor {
        let Some(&(line, parent)) = by_id.get(uuid) else {
            return HashSet::new();
        };
        if !live.insert(line) {
            return HashSet::new();
        }
        if parent.is_some_and(|id| {
            by_id
                .get(id)
                .is_none_or(|(parent_line, _)| *parent_line >= line)
        }) {
            return HashSet::new();
        }
        cursor = parent;
    }
    live
}

/// Read a complete bounded structural projection, retaining only graph links
/// and numeric usage. An incomplete suffix cannot prove ancestry or lifetime
/// totals. Normal source writes/replacements observed during the read invalidate
/// the complete result; this does not claim custody against a hostile writer.
pub fn scan_usage(path: &Path) -> UsageSample {
    scan_usage_checked(path, || {})
}

fn scan_usage_checked(path: &Path, after_read: impl FnOnce()) -> UsageSample {
    use gobstopper_core::model::ContextReason;
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        // A platform without Unix inode/ctime identity cannot bind this scan.
        #[cfg(not(unix))]
        return Err(ContextReason::SourceUnavailable);
        #[allow(unreachable_code)]
        let mut file = options
            .open(path)
            .map_err(|_| ContextReason::SourceUnavailable)?;
        let before = file
            .metadata()
            .map_err(|_| ContextReason::SourceUnavailable)?;
        if !before.is_file() {
            return Err(ContextReason::SourceUnavailable);
        }
        let limit = USAGE_SCAN_MAX_BYTES.min(crate::transaction::max_transcript_bytes());
        if before.len() > limit {
            return Err(ContextReason::ReadLimit);
        }
        if !fs::symlink_metadata(path).is_ok_and(|named| same_scan_source(&before, &named)) {
            return Err(ContextReason::SourceChanged);
        }
        let mut projection = UsageProjection::default();
        let mut consumed = 0u64;
        {
            use std::io::Read;
            let mut reader = BufReader::new((&mut file).take(before.len() + 1));
            let mut line = Vec::new();
            let mut line_index = 0usize;
            loop {
                line.clear();
                let bytes = (&mut reader)
                    .take(USAGE_SCAN_MAX_RECORD_BYTES + 1)
                    .read_until(b'\n', &mut line)
                    .map_err(|_| ContextReason::SourceUnavailable)?;
                if bytes == 0 {
                    break;
                }
                consumed += bytes as u64;
                if line_index >= gobstopper_core::validation::MAX_ITEMS
                    || bytes as u64 > USAGE_SCAN_MAX_RECORD_BYTES
                    || consumed > limit
                {
                    return Err(ContextReason::ReadLimit);
                }
                let raw = std::str::from_utf8(&line).map_err(|_| ContextReason::InvalidRecord)?;
                if !raw.trim().is_empty() {
                    let record = crate::payload::decode_record(raw)
                        .map_err(|_| ContextReason::InvalidRecord)?;
                    projection.push(line_index, &record);
                }
                line_index += 1;
            }
        }
        after_read();
        if consumed != before.len()
            || !file
                .metadata()
                .is_ok_and(|after| same_scan_source(&before, &after))
            || !fs::symlink_metadata(path).is_ok_and(|named| same_scan_source(&before, &named))
        {
            return Err(ContextReason::SourceChanged);
        }
        Ok(projection.finish().0)
    })();
    result.unwrap_or_else(|reason| {
        let mut sample = UsageSample::default();
        sample.invalidate_context_because(reason);
        sample
    })
}

fn same_scan_source(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        after.is_file()
            && before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        let _ = (before, after);
        false
    }
}

/// Read session identity from any line that carries it.
pub fn scan_meta(path: &Path) -> (Option<String>, Option<PathBuf>) {
    let (mut id, mut cwd) = (None, None);
    for record in crate::payload::head_records(path, 64) {
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
    let Ok(mut record) = crate::payload::decode_record(line) else {
        return (line.to_string(), 0);
    };
    if record.get("type").and_then(Value::as_str) != Some("user")
        || !supported_result_message(&record["message"])
    {
        return (line.to_string(), 0);
    }
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
    targets: &std::collections::HashSet<usize>,
    stub_template: &str,
    per_item_stubs: &std::collections::BTreeMap<usize, String>,
) -> (String, u64) {
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
    crate::transaction::prepare(Provider::ClaudeCode, original, |text| {
        apply_inner(text, edits)
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
            } => {
                let targets =
                    crate::payload::elision_targets(Provider::ClaudeCode, &raw, line_indexes)?;
                raw = apply_elide(&raw, &targets, stub_template, per_item_stubs).0;
            }
            Edit::InjectDigest { digest } => {
                // Append the digest as a synthetic user line and follow it
                // with a fresh `last-prompt`/`mode` tail. Claude's resume
                // indexer expects this tail to recognize the fork; without it
                // the file is discoverable but `claude --resume` reports that
                // no conversation exists.
                let text = digest_text(digest);
                let parsed: Vec<Value> = raw
                    .lines()
                    .filter_map(|line| crate::payload::decode_record(line).ok())
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
                let links: Vec<_> = parsed
                    .iter()
                    .enumerate()
                    .filter_map(|(line, r)| {
                        r.get("uuid").and_then(Value::as_str).map(|id| {
                            (
                                line,
                                id.to_string(),
                                r.get("parentUuid")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                            )
                        })
                    })
                    .collect();
                if canonical_leaf.is_some_and(|leaf| live_branch(&links, leaf).is_empty()) {
                    return Err(AdapterError::InvalidEdit(
                        "Claude digest attachment has ambiguous or broken linkage",
                    ));
                }
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
/// A valid record with a live pid is a conservative liveness hint: a session
/// open-but-quiet in a TUI may have no recent transcript write. This does not
/// prove process/session ownership, authorize mutation, or establish that an
/// absent/invalid record describes an idle session.
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
        let Ok(bytes) = crate::transaction::read_with_limit(&path, 64 * 1024) else {
            continue;
        };
        let Some(v) = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| crate::payload::decode_record(text).ok())
        else {
            continue;
        };
        let (Some(pid), Some(session_id)) = (
            v.get("pid")
                .and_then(Value::as_u64)
                .and_then(|pid| i32::try_from(pid).ok())
                .filter(|pid| *pid > 0),
            v.get("sessionId").and_then(Value::as_str).filter(|id| {
                !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
            }),
        ) else {
            continue;
        };
        if pid_alive(pid) {
            live.insert(
                session_id.to_string(),
                v.get("status")
                    .and_then(Value::as_str)
                    .filter(|status| status.len() <= 128 && !status.chars().any(char::is_control))
                    .unwrap_or("")
                    .to_string(),
            );
        }
    }
    live
}

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal zero only probes one positive PID; it delivers no signal
    // and never targets a process group. A denied/unknown probe remains live.
    (unsafe { libc::kill(pid, 0) == 0 })
        || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
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
struct NativeClaudeChild(std::process::Child);

impl NativeClaudeChild {
    fn exit_success(&mut self) -> std::io::Result<Option<bool>> {
        #[cfg(unix)]
        {
            // SAFETY: this is our unreaped child. WNOWAIT reserves its identity
            // until Drop cleans the process group and then reaps the leader.
            let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.0.id() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if rc == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if unsafe { info.si_pid() } == 0 {
                return Ok(None);
            }
            Ok(Some(
                info.si_code == libc::CLD_EXITED && unsafe { info.si_status() } == 0,
            ))
        }
        #[cfg(not(unix))]
        self.0
            .try_wait()
            .map(|status| status.map(|status| status.success()))
    }
}

impl Drop for NativeClaudeChild {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: the private process group was created at spawn. Its leader
        // remains unreaped, so this signal cannot target a reused process ID.
        unsafe {
            libc::kill(-(self.0.id() as libc::pid_t), libc::SIGKILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

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
    if !cfg!(unix)
        || timeout_secs == 0
        || timeout_secs > 3600
        || session_id.is_empty()
        || session_id.len() > 256
        || session_id.starts_with('-')
        || !session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(io_err(
            std::io::ErrorKind::InvalidInput,
            "unsupported platform, session identity, or deadline for native Claude operation"
                .to_string(),
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
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            match command.spawn() {
                Ok(c) => break NativeClaudeChild(c),
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
        match child.exit_success() {
            Ok(Some(true)) => return Ok(()),
            Ok(Some(false)) => {
                return Err(io_err(
                    std::io::ErrorKind::Other,
                    "claude native process exited unsuccessfully; outcome unresolved".to_string(),
                ))
            }
            Ok(None) if Instant::now() >= deadline => {
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
        write(
            &sessions.join("duplicate.json"),
            &format!(r#"{{"pid":99999999,"pid":{me},"sessionId":"duplicate"}}"#),
        );
        for pid in [0, u64::from(me) + (1_u64 << 32), u64::MAX] {
            write(
                &sessions.join(format!("invalid-{pid}.json")),
                &format!(r#"{{"pid":{pid},"sessionId":"invalid-pid"}}"#),
            );
        }
        write(
            &sessions.join("oversized.json"),
            &format!(
                r#"{{"pid":{me},"sessionId":"oversized","padding":"{}"}}"#,
                "x".repeat(64 * 1024),
            ),
        );
        let outside = home.join("linked-record");
        write(&outside, &format!(r#"{{"pid":{me},"sessionId":"linked"}}"#));
        std::os::unix::fs::symlink(&outside, sessions.join("linked.json")).unwrap();
        let fifo = sessions.join("pipe.json");
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: a fresh isolated fixture pathname; no process is signaled.
        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
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

#[cfg(test)]
mod tests {
    use super::*;
    use gobstopper_core::model::{ContextReason, ContextState, LifetimeScope};
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "gobstopper-claude-usage-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> PathBuf {
            self.0.join("session.jsonl")
        }
        fn write(&self, records: &[Value]) -> Vec<u8> {
            let bytes = records
                .iter()
                .map(|r| format!("{r}\n"))
                .collect::<String>()
                .into_bytes();
            fs::write(self.path(), &bytes).unwrap();
            bytes
        }
        fn handle(&self) -> SessionHandle {
            SessionHandle {
                provider: Provider::ClaudeCode,
                session_id: "s".into(),
                path: self.path(),
                cwd: None,
                age_secs: 0,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn reading(id: &str, parent: Option<&str>, n: u64) -> Value {
        json!({"type":"assistant","uuid":id,"parentUuid":parent,"message":{"role":"assistant","content":[],"usage":{"input_tokens":n,"output_tokens":1}}})
    }

    #[test]
    fn long_valid_history_recovers_complete_ancestry_and_lifetime() {
        let fx = Fixture::new();
        let mut records = vec![reading("root", None, 10)];
        let mut parent = "root".to_string();
        for i in 0..40 {
            let id = format!("user-{i}");
            records.push(json!({"type":"user","uuid":id,"parentUuid":parent,"message":{"role":"user","content":"x".repeat(20_000)}}));
            parent = id;
        }
        records.push(reading("live", Some(&parent), 100));
        records.push(reading("dead", Some("root"), 900));
        records.push(json!({"type":"last-prompt","leafUuid":"live"}));
        let bytes = fx.write(&records);
        assert!(bytes.len() > 512 * 1024);
        let scanned = scan_usage(&fx.path());
        assert_eq!(scanned.reported_context(), Some(101));
        assert_eq!(scanned.lifetime_scope, LifetimeScope::Full);
        assert_eq!(scanned.lifetime_input_tokens, 1010);
        assert_eq!(scanned, load_bytes(fx.handle(), &bytes).unwrap().usage);
        // Breaking an ancestor outside the old suffix is still a hard failure.
        records[1]["parentUuid"] = json!("missing-interior-parent");
        fx.write(&records);
        let invalid = scan_usage(&fx.path());
        assert_eq!(invalid.context_reason, Some(ContextReason::InvalidAncestry));
        assert_eq!(invalid.reported_context(), None);
        assert_eq!(invalid.lifetime_scope, LifetimeScope::Partial);
    }

    #[test]
    fn complete_projection_rejects_ambiguous_and_invalid_linkage() {
        let fx = Fixture::new();
        let base = vec![
            reading("root", None, 10),
            reading("leaf", Some("root"), 100),
        ];
        let mut variants = Vec::new();
        let mut duplicate = base.clone();
        duplicate.push(reading("root", None, 9));
        variants.push(duplicate);
        let mut cycle = base.clone();
        cycle[0]["parentUuid"] = json!("leaf");
        variants.push(cycle);
        let mut missing = base.clone();
        missing[1]["parentUuid"] = json!("missing");
        variants.push(missing);
        let mut malformed = base.clone();
        malformed[1]["parentUuid"] = json!(42);
        variants.push(malformed);
        let mut unlinked = base.clone();
        unlinked[1].as_object_mut().unwrap().remove("uuid");
        variants.push(unlinked);
        let mut oversized = base.clone();
        oversized[1]["uuid"] = json!("x".repeat(257));
        variants.push(oversized);
        let mut last_prompt = base;
        last_prompt.push(json!({"type":"last-prompt","leafUuid":null}));
        variants.push(last_prompt);
        for records in variants {
            let bytes = fx.write(&records);
            let sample = scan_usage(&fx.path());
            assert_eq!(sample.reported_context(), None);
            assert_eq!(sample.context_reason, Some(ContextReason::InvalidAncestry));
            assert_eq!(sample, load_bytes(fx.handle(), &bytes).unwrap().usage);
        }
        fs::write(fx.path(), b"{\"uuid\":\"root\",\"uuid\":\"other\"}\n").unwrap();
        assert_eq!(
            scan_usage(&fx.path()).context_reason,
            Some(ContextReason::InvalidRecord)
        );
    }

    #[test]
    fn selected_leaf_without_any_graph_is_unknown_not_absent() {
        let fx = Fixture::new();
        let bytes = fx.write(&[json!({"type":"last-prompt","leafUuid":"missing-leaf"})]);
        for sample in [
            scan_usage(&fx.path()),
            load_bytes(fx.handle(), &bytes).unwrap().usage,
        ] {
            assert_eq!(sample.context_state, ContextState::Unknown);
            assert_eq!(sample.context_reason, Some(ContextReason::InvalidAncestry));
            assert_eq!(sample.lifetime_scope, LifetimeScope::Partial);
            assert_eq!(sample.reported_context(), None);
            assert_eq!(sample.measured_component_subtotal(), None);
        }
        fx.write(&[]);
        assert_eq!(scan_usage(&fx.path()).context_state, ContextState::Absent);
    }

    #[test]
    fn unlinked_attachments_preserve_valid_graph_and_discovery_usage() {
        let fx = Fixture::new();
        let bytes = include_bytes!("../tests/fixtures/dialects-v1/claude-branch.jsonl");
        fs::write(fx.path(), bytes).unwrap();
        let transcript = load_bytes(fx.handle(), bytes).unwrap();
        let discovered = scan_usage(&fx.path());
        assert_eq!(discovered.reported_context(), Some(112));
        assert_eq!(discovered, transcript.usage);
        let attachment = transcript
            .items
            .iter()
            .find(|item| item.line_index == 8)
            .unwrap();
        assert!(attachment.est_tokens > 0);
        assert!(attachment.elidable_bytes.is_none());
        let live_tool = transcript
            .items
            .iter()
            .find(|item| item.line_index == 3)
            .unwrap();
        assert_eq!(live_tool.elidable_bytes, Some(304));
        let dead_tool = transcript
            .items
            .iter()
            .find(|item| item.line_index == 2)
            .unwrap();
        assert!(dead_tool.elidable_bytes.is_none());
    }

    #[test]
    fn reset_and_partial_readings_follow_the_selected_branch() {
        let fx = Fixture::new();
        let mut partial = reading("partial", Some("root"), 100);
        partial["message"]["usage"]["cache_creation_input_tokens"] = Value::Null;
        let records = vec![reading("root", None, 10), partial];
        let bytes = fx.write(&records);
        let sample = scan_usage(&fx.path());
        assert_eq!(sample.reported_context(), None);
        assert_eq!(sample.measured_component_subtotal(), Some(101));
        assert_eq!(sample.context_reason, Some(ContextReason::NullComponent));
        assert_eq!(sample, load_bytes(fx.handle(), &bytes).unwrap().usage);
        let mut reset = records;
        reset.push(json!({"type":"system","subtype":"compact_boundary","uuid":"reset","parentUuid":"partial"}));
        reset.push(json!({"type":"last-prompt","leafUuid":"reset"}));
        fx.write(&reset);
        let sample = scan_usage(&fx.path());
        assert_eq!(sample.context_state, ContextState::Reset);
        assert_eq!(sample.context_components, None);
        assert_eq!(sample.measured_component_subtotal(), None);
        reset.push(reading("after", Some("reset"), 0));
        reset.push(json!({"type":"last-prompt","leafUuid":"after"}));
        fx.write(&reset);
        assert_eq!(scan_usage(&fx.path()).reported_context(), Some(1));
    }

    #[test]
    fn missing_metrics_and_overflow_cannot_establish_full_usage() {
        let fx = Fixture::new();
        let mut missing = reading("last", Some("root"), 100);
        missing["message"].as_object_mut().unwrap().remove("usage");
        let mut overflow = reading("last", Some("root"), u64::MAX);
        overflow["message"]["usage"]["output_tokens"] = json!(1);
        for (last, reason) in [
            (missing, ContextReason::MissingComponent),
            (overflow, ContextReason::Overflow),
        ] {
            let bytes = fx.write(&[reading("root", None, 10), last]);
            let sample = scan_usage(&fx.path());
            assert_eq!(sample.context_reason, Some(reason));
            assert_eq!(sample.reported_context(), None);
            assert_eq!(sample.lifetime_scope, LifetimeScope::Partial);
            assert_eq!(sample, load_bytes(fx.handle(), &bytes).unwrap().usage);
        }
    }

    #[test]
    fn usage_scan_enforces_total_record_and_count_limits() {
        let fx = Fixture::new();
        let file = fs::File::create(fx.path()).unwrap();
        file.set_len(USAGE_SCAN_MAX_BYTES + 1).unwrap();
        assert_eq!(
            scan_usage(&fx.path()).context_reason,
            Some(ContextReason::ReadLimit)
        );
        fx.write(&[
            json!({"type":"progress","padding":"x".repeat(USAGE_SCAN_MAX_RECORD_BYTES as usize)}),
        ]);
        assert_eq!(
            scan_usage(&fx.path()).context_reason,
            Some(ContextReason::ReadLimit)
        );
        fs::write(
            fx.path(),
            vec![b'\n'; gobstopper_core::validation::MAX_ITEMS],
        )
        .unwrap();
        assert_eq!(scan_usage(&fx.path()).context_state, ContextState::Absent);
        fs::write(
            fx.path(),
            vec![b'\n'; gobstopper_core::validation::MAX_ITEMS + 1],
        )
        .unwrap();
        assert_eq!(
            scan_usage(&fx.path()).context_reason,
            Some(ContextReason::ReadLimit)
        );
    }

    #[test]
    fn source_change_or_replacement_invalidates_the_whole_reading() {
        use std::io::Write;
        let fx = Fixture::new();
        let records = [reading("root", None, 10)];
        for kind in ["append", "rewrite", "replace", "truncate"] {
            fx.write(&records);
            let changed = scan_usage_checked(&fx.path(), || match kind {
                "append" => fs::OpenOptions::new()
                    .append(true)
                    .open(fx.path())
                    .unwrap()
                    .write_all(b"\n")
                    .unwrap(),
                "rewrite" => {
                    fx.write(&[reading("root", None, 11)]);
                }
                "replace" => {
                    let next = fx.0.join("replacement");
                    fs::write(&next, fs::read(fx.path()).unwrap()).unwrap();
                    fs::rename(next, fx.path()).unwrap();
                }
                _ => {
                    fs::File::create(fx.path()).unwrap();
                }
            });
            assert_eq!(
                changed.context_reason,
                Some(ContextReason::SourceChanged),
                "{kind}"
            );
            assert_eq!(changed.reported_context(), None);
            assert_eq!(changed.lifetime_scope, LifetimeScope::Absent);
            assert_eq!(changed.lifetime_input_tokens, 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn scan_refuses_symlinks_special_files_and_unavailable_sources() {
        use std::os::unix::fs::symlink;
        let fx = Fixture::new();
        let path = fx.path();
        assert_eq!(
            scan_usage(&path).context_reason,
            Some(ContextReason::SourceUnavailable)
        );
        let target = fx.0.join("target");
        fs::write(&target, "{}\n").unwrap();
        symlink(&target, &path).unwrap();
        assert_eq!(
            scan_usage(&path).context_reason,
            Some(ContextReason::SourceUnavailable)
        );
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert_eq!(
            scan_usage(&path).context_reason,
            Some(ContextReason::SourceUnavailable)
        );
    }
}
