//! Structural checks for the frozen, synthetic transcript dialects.
//!
//! Findings describe JSON validity, unambiguous identity/linkage, supported
//! rewrite shapes, and ordered tool pairing within the effective context.
//! They contain closed messages and physical line indexes, never source content.
//! Passing these checks is not provider resume qualification or a proof that a
//! summary preserves the conversation's meaning.
//!
//! Claude and Devin graph parents must precede children; duplicate identities,
//! missing heads and cycles make context unavailable. Tool pairing considers
//! the selected live branch. Codex pairing considers only the newest compacted
//! replacement history and subsequent response items, including subrecord order.
//! Codex supports call_id, tool_call_id, then id identity aliases in that order;
//! Devin accepts exact IDs or one unambiguous trailing-# nonce namespace.
//!
//! Invalid nonblank JSON is an error except a sole torn final line, which is a
//! partial_tail warning. Duplicate object keys remain errors even at EOF. Depth,
//! byte and record limits are checked; unknown shapes are not elision authority.

use gobstopper_core::Provider;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// How badly a finding hurts resumability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Resume will break or silently drop context.
    Error,
    /// Suspicious but usually survivable.
    Warning,
}

/// One structural problem found in a transcript file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VerifyFinding {
    pub severity: Severity,
    /// Zero-based JSONL line the finding attaches to, when line-scoped.
    pub line_index: Option<usize>,
    /// Stable machine-readable code, e.g. `"broken_parent_chain"`.
    pub code: &'static str,
    /// Human-readable detail. Never contains transcript content, uuids,
    /// or paths — counts and line indexes only.
    pub message: String,
}

fn error(line_index: Option<usize>, code: &'static str, message: &str) -> VerifyFinding {
    VerifyFinding {
        severity: Severity::Error,
        line_index,
        code,
        message: message.to_string(),
    }
}

fn warning(line_index: Option<usize>, code: &'static str, message: &str) -> VerifyFinding {
    VerifyFinding {
        severity: Severity::Warning,
        line_index,
        code,
        message: message.to_string(),
    }
}

/// Verify a transcript held in memory. Pure: no I/O, no provider calls.
pub fn verify(provider: Provider, bytes: &[u8]) -> Vec<VerifyFinding> {
    if bytes.len() as u64 > crate::transaction::max_transcript_bytes() {
        return vec![error(
            None,
            "transcript_limit",
            "transcript exceeds byte limit",
        )];
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return vec![error(
            None,
            "invalid_utf8",
            "transcript contains invalid UTF-8",
        )];
    };
    let lines: Vec<&str> = text
        .lines()
        .take(gobstopper_core::validation::MAX_ITEMS + 1)
        .collect();
    if lines.len() > gobstopper_core::validation::MAX_ITEMS {
        return vec![error(
            None,
            "transcript_limit",
            "transcript exceeds record limit",
        )];
    }
    if lines.is_empty() {
        return vec![warning(None, "empty", "file is empty")];
    }

    let mut findings = Vec::new();
    let mut records: Vec<Option<Value>> = Vec::with_capacity(lines.len());
    let mut bad_lines: Vec<usize> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            findings.push(warning(Some(i), "blank_line", "line is blank whitespace"));
            records.push(None);
            continue;
        }
        match crate::payload::decode_record(line) {
            Ok(v) => {
                if !v.is_object() || !v.get("type").is_some_and(Value::is_string) {
                    findings.push(error(
                        Some(i),
                        "invalid_record",
                        "record must be an object with a string type",
                    ));
                }
                records.push(Some(v));
            }
            Err(error) => {
                if error.classify() == serde_json::error::Category::Data {
                    findings.push(crate::verify::error(
                        Some(i),
                        "ambiguous_json",
                        "record contains duplicate JSON keys",
                    ));
                } else {
                    bad_lines.push(i);
                }
                records.push(None);
            }
        }
    }

    // A single failure on the final line is almost always a torn write at
    // the tail of a live session — warn instead of erroring.
    if bad_lines.len() == 1 && bad_lines[0] == lines.len() - 1 {
        findings.push(warning(
            Some(bad_lines[0]),
            "partial_tail",
            "final line is not valid JSON (possible torn write at tail of live session)",
        ));
    } else {
        for &i in &bad_lines {
            findings.push(error(Some(i), "invalid_json", "line is not valid JSON"));
        }
    }

    match provider {
        Provider::ClaudeCode => verify_claude(&records, &mut findings),
        Provider::Codex => verify_codex(&records, &mut findings),
        Provider::Devin => verify_devin(&records, &mut findings),
    }

    findings.sort_by_key(|f| f.line_index.unwrap_or(usize::MAX));
    findings
}

/// Read a transcript file and verify it.
pub fn verify_path(
    provider: Provider,
    path: &std::path::Path,
) -> std::io::Result<Vec<VerifyFinding>> {
    let bytes =
        crate::transaction::read(path).map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(verify(provider, &bytes))
}

/// Claude Code dialect checks: `uuid`/`parentUuid` linkage and
/// `tool_use`/`tool_result` pairing across `message.content[]` blocks.
fn verify_claude(records: &[Option<Value>], findings: &mut Vec<VerifyFinding>) {
    let mut seen_uuids: HashSet<&str> = HashSet::new();
    // (id, line_index) for assistant tool_use blocks and all tool_result blocks.
    let mut tool_uses: Vec<(&str, usize)> = Vec::new();
    let mut tool_results: Vec<(&str, usize)> = Vec::new();

    for (i, record) in records.iter().enumerate() {
        let Some(record) = record else { continue };

        if record.get("uuid").is_some() {
            if !record
                .get("uuid")
                .and_then(Value::as_str)
                .is_some_and(|id| {
                    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
                })
            {
                findings.push(error(
                    Some(i),
                    "invalid_uuid",
                    "uuid must be a bounded nonempty string",
                ));
            }
            if !record.get("parentUuid").is_none_or(|v| {
                v.is_null()
                    || v.as_str().is_some_and(|id| {
                        !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
                    })
            }) {
                findings.push(error(
                    Some(i),
                    "invalid_parent_uuid",
                    "parentUuid must be null or a bounded nonempty string",
                ));
            }
        }
        if record.get("type").and_then(Value::as_str) == Some("last-prompt")
            && !record
                .get("leafUuid")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty())
        {
            findings.push(error(
                Some(i),
                "invalid_chain_head",
                "last-prompt requires a nonempty leafUuid",
            ));
        }
        // Chain check — only records that carry a uuid participate.
        if let Some(uuid) = record.get("uuid").and_then(Value::as_str) {
            if let Some(parent) = record.get("parentUuid").and_then(Value::as_str) {
                if !seen_uuids.contains(parent) {
                    findings.push(error(
                        Some(i),
                        "broken_parent_chain",
                        "parentUuid does not match any uuid on an earlier line",
                    ));
                }
            }
            if !seen_uuids.insert(uuid) {
                findings.push(error(
                    Some(i),
                    "duplicate_uuid",
                    "uuid already appears on an earlier record",
                ));
            }
        }

        let is_assistant = record.get("type").and_then(Value::as_str) == Some("assistant")
            && record.pointer("/message/role").and_then(Value::as_str) == Some("assistant");
        let is_user = record.get("type").and_then(Value::as_str) == Some("user")
            && record.pointer("/message/role").and_then(Value::as_str) == Some("user");
        let Some(blocks) = record
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") if is_assistant => {
                    if let Some(id) = block.get("id").and_then(Value::as_str) {
                        tool_uses.push((id, i));
                    }
                }
                // Only the supported user envelope can answer a tool use.
                Some("tool_result") if is_user => {
                    if let Some(id) = block.get("tool_use_id").and_then(Value::as_str) {
                        tool_results.push((id, i));
                    }
                }
                _ => {}
            }
        }
    }

    let links: Vec<_> = records
        .iter()
        .enumerate()
        .filter_map(|(line, record)| {
            let r = record.as_ref()?;
            Some((
                line,
                r.get("uuid")?.as_str()?.to_string(),
                r.get("parentUuid")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            ))
        })
        .collect();
    let explicit = records.iter().enumerate().rev().find_map(|(line, r)| {
        let r = r.as_ref()?;
        (r.get("type").and_then(Value::as_str) == Some("last-prompt"))
            .then(|| {
                r.get("leafUuid")
                    .and_then(Value::as_str)
                    .map(|id| (line, id))
            })
            .flatten()
    });
    if let Some((line, id)) = explicit {
        if !seen_uuids.contains(id) {
            findings.push(error(
                Some(line),
                "missing_chain_head",
                "last-prompt names an absent leafUuid",
            ));
        }
    }
    let leaf = explicit.map(|(_, id)| id).or_else(|| {
        records.iter().rev().find_map(|r| {
            let r = r.as_ref()?;
            matches!(
                r.get("type").and_then(Value::as_str),
                Some("user" | "assistant" | "attachment")
            )
            .then(|| r.get("uuid").and_then(Value::as_str))
            .flatten()
        })
    });
    if let Some(leaf) = leaf {
        let live = crate::claude::live_branch(&links, leaf);
        if !live.is_empty() {
            tool_uses.retain(|(_, line)| live.contains(line));
            tool_results.retain(|(_, line)| live.contains(line));
        }
    }
    let mut first_use: HashMap<&str, usize> = HashMap::new();
    let mut last_result: HashMap<&str, usize> = HashMap::new();
    for &(id, line) in &tool_uses {
        if first_use.insert(id, line).is_some() {
            findings.push(error(
                Some(line),
                "duplicate_tool_call_id",
                "tool call ID is duplicated in effective context",
            ));
        }
    }
    for &(id, line) in &tool_results {
        last_result.insert(id, line);
    }
    for &(id, line) in &tool_uses {
        let answered = last_result.get(id).is_some_and(|&rline| rline > line);
        if !answered {
            findings.push(error(
                Some(line),
                "orphaned_tool_use",
                "tool_use block has no matching tool_result in a later record",
            ));
        }
    }
    for &(id, line) in &tool_results {
        let preceded = first_use.get(id).is_some_and(|&uline| uline < line);
        if !preceded {
            findings.push(warning(
                Some(line),
                "orphaned_tool_result",
                "tool_result block has no matching tool_use in an earlier record",
            ));
        }
    }
}

/// Devin dialect checks against the canonical export form (see
/// `devin::export_bytes`): line 0 is a `session_meta` record naming
/// `main_chain_id`; every later line is a `message_node` with a unique
/// `node_id`, a `parent_node_id` that resolves to an earlier node, and a
/// `chat_message` object. Assistant `tool_calls[].id` values must be
/// answered by a later `role == "tool"` node's `tool_call_id`, and every
/// tool node's `tool_call_id` must match an earlier assistant call.
fn verify_devin(records: &[Option<Value>], findings: &mut Vec<VerifyFinding>) {
    let mut seen_nodes: HashSet<i64> = HashSet::new();
    let mut saw_meta = false;
    let mut head = None;
    let mut parents = HashMap::new();
    let mut node_lines = HashMap::new();
    // (call id, line_index) for assistant calls; tool nodes likewise.
    let mut calls: Vec<(&str, usize)> = Vec::new();
    let mut results: Vec<(&str, usize)> = Vec::new();

    for (i, record) in records.iter().enumerate() {
        let Some(record) = record else { continue };
        match record.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                if saw_meta {
                    findings.push(error(
                        Some(i),
                        "duplicate_session_meta",
                        "export repeats session metadata",
                    ));
                }
                saw_meta = true;
                head = record.get("main_chain_id").and_then(Value::as_i64);
                if record
                    .get("main_chain_id")
                    .is_some_and(|v| !v.is_null() && v.as_i64().is_none_or(|id| id < 0))
                {
                    findings.push(error(
                        Some(i),
                        "invalid_chain_head",
                        "main_chain_id must be null or a nonnegative integer",
                    ));
                }
                if i != 0 {
                    findings.push(warning(
                        Some(i),
                        "misplaced_session_meta",
                        "session_meta record is not the first line",
                    ));
                }
                if record.get("main_chain_id").is_none() {
                    findings.push(error(
                        Some(i),
                        "missing_main_chain",
                        "session_meta record lacks main_chain_id",
                    ));
                }
            }
            Some("message_node") => {
                let Some(node_id) = record.get("node_id").and_then(Value::as_i64) else {
                    findings.push(error(
                        Some(i),
                        "missing_node_id",
                        "message_node record lacks an integer node_id",
                    ));
                    continue;
                };
                if node_id < 0 {
                    findings.push(error(
                        Some(i),
                        "invalid_node_id",
                        "node_id must be nonnegative",
                    ));
                }
                if let Some(parent) = record.get("parent_node_id").and_then(Value::as_i64) {
                    if !seen_nodes.contains(&parent) {
                        findings.push(error(
                            Some(i),
                            "broken_parent_chain",
                            "parent_node_id does not match any earlier node_id",
                        ));
                    }
                } else if !record.get("parent_node_id").is_some_and(Value::is_null) {
                    findings.push(error(
                        Some(i),
                        "invalid_parent_id",
                        "parent_node_id must be null or an integer",
                    ));
                }
                if !seen_nodes.insert(node_id) {
                    findings.push(error(
                        Some(i),
                        "duplicate_node_id",
                        "node_id already appears on an earlier record",
                    ));
                }
                parents.insert(
                    node_id,
                    record.get("parent_node_id").and_then(Value::as_i64),
                );
                node_lines.insert(node_id, i);
                match record.get("chat_message") {
                    Some(msg) if msg.is_object() => match msg.get("role").and_then(Value::as_str) {
                        Some("assistant") => {
                            if let Some(tcs) = msg.get("tool_calls").and_then(Value::as_array) {
                                for call in tcs {
                                    if let Some(id) = call.get("id").and_then(Value::as_str) {
                                        calls.push((id, i));
                                    }
                                }
                            }
                        }
                        Some("tool") => {
                            if let Some(id) = msg.get("tool_call_id").and_then(Value::as_str) {
                                results.push((id, i));
                            }
                        }
                        _ => {}
                    },
                    _ => findings.push(error(
                        Some(i),
                        "invalid_chat_message",
                        "chat_message is missing or not an object",
                    )),
                }
            }
            _ => {}
        }
    }
    if !saw_meta {
        findings.push(error(
            None,
            "missing_session_meta",
            "export has no session_meta record",
        ));
    }

    if head.is_some_and(|id| !seen_nodes.contains(&id)) {
        findings.push(error(
            Some(0),
            "missing_chain_head",
            "main_chain_id names an absent node",
        ));
    }
    let mut live = HashSet::new();
    let mut cursor = head;
    let mut resolved = true;
    while let Some(id) = cursor {
        let Some(parent) = parents.get(&id) else {
            resolved = false;
            break;
        };
        if !live.insert(node_lines[&id]) {
            resolved = false;
            break;
        }
        cursor = *parent;
    }
    if resolved && !live.is_empty() {
        calls.retain(|(_, line)| live.contains(line));
        results.retain(|(_, line)| live.contains(line));
    }
    let mut by_id: HashMap<String, Vec<usize>> = HashMap::new();
    for &(id, line) in &calls {
        if !crate::devin::valid_tool_id(id) {
            findings.push(error(
                Some(line),
                "invalid_tool_id",
                "tool call has an invalid identifier",
            ));
            continue;
        }
        let entries = by_id.entry(id.to_string()).or_default();
        if !entries.is_empty() {
            findings.push(error(
                Some(line),
                "duplicate_tool_call_id",
                "tool call ID is duplicated in effective context",
            ));
        }
        entries.push(line);
    }
    let mut answered = HashSet::new();
    for &(id, line) in &results {
        let resolved = crate::devin::matching_tool_call(&by_id, id)
            .and_then(|key| by_id.get(key).map(|lines| (key, lines)))
            .filter(|(_, lines)| lines.len() == 1 && lines[0] < line);
        if let Some((key, _)) = resolved {
            answered.insert(key);
        } else {
            findings.push(warning(
                Some(line),
                "orphaned_tool_result",
                "tool node does not resolve uniquely to an earlier assistant call",
            ));
        }
    }
    for &(id, line) in &calls {
        if !answered.contains(id) {
            findings.push(error(
                Some(line),
                "orphaned_tool_call",
                "tool_calls entry has no uniquely matching tool node in a later record",
            ));
        }
    }
}

/// Codex checks the latest replacement window plus its later records. Pair
/// order includes positions inside one replacement_history record; an earlier
/// output or an output in a superseded window cannot answer a live call.
fn verify_codex(records: &[Option<Value>], findings: &mut Vec<VerifyFinding>) {
    let mut last_ordinal = None;
    let mut effective = Vec::new();
    for (line, record) in records.iter().enumerate() {
        let Some(record) = record else {
            continue;
        };
        if let Some(value) = record.get("ordinal") {
            if let Some(ordinal) = value.as_u64() {
                if last_ordinal.is_some_and(|previous| ordinal < previous) {
                    findings.push(warning(
                        Some(line),
                        "non_monotonic_ordinal",
                        "ordinal is lower than the previous ordinal-bearing record",
                    ));
                }
                last_ordinal = Some(ordinal);
            } else {
                findings.push(error(
                    Some(line),
                    "invalid_ordinal",
                    "ordinal must be a nonnegative u64 integer",
                ));
            }
        }
        match record.get("type").and_then(Value::as_str) {
            Some("compacted") => {
                effective.clear();
                match record
                    .pointer("/payload/replacement_history")
                    .and_then(Value::as_array)
                {
                    Some(items) => {
                        if items.len() > gobstopper_core::validation::MAX_ITEMS {
                            findings.push(error(
                                Some(line),
                                "transcript_limit",
                                "replacement history exceeds item limit",
                            ));
                        } else {
                            effective.extend(items.iter().map(|item| (line, item)));
                        }
                    }
                    None => findings.push(error(
                        Some(line),
                        "malformed_compacted",
                        "compacted record lacks a replacement_history array",
                    )),
                }
            }
            Some("response_item") => {
                if let Some(item) = record.get("payload").filter(|v| v.is_object()) {
                    effective.push((line, item));
                } else {
                    findings.push(error(
                        Some(line),
                        "invalid_response_item",
                        "response_item payload must be an object",
                    ));
                }
            }
            _ => {}
        }
        if effective.len() > gobstopper_core::validation::MAX_ITEMS {
            findings.push(error(
                Some(line),
                "transcript_limit",
                "effective context exceeds item limit",
            ));
            return;
        }
    }
    let mut calls = std::collections::BTreeMap::new();
    let mut answered = HashSet::new();
    let mut outputs = HashSet::new();
    for (position, (line, item)) in effective.iter().enumerate() {
        let kind = item.get("type").and_then(Value::as_str);
        if !crate::codex::supported_item(item) {
            findings.push(warning(
                Some(*line),
                "unsupported_response_item",
                "response item is outside the frozen supported shape",
            ));
        }
        let Some(id) = crate::codex::record_call_id(item) else {
            continue;
        };
        match kind {
            Some("function_call" | "custom_tool_call" | "local_shell_call") => {
                answered.remove(id);
                if calls.insert(id, (position, *line)).is_some() {
                    findings.push(error(
                        Some(*line),
                        "duplicate_tool_call_id",
                        "tool call ID is duplicated in effective context",
                    ));
                }
            }
            Some("function_call_output" | "custom_tool_call_output") => {
                if calls.get(id).is_some_and(|(call, _)| *call < position) {
                    answered.insert(id);
                } else {
                    findings.push(warning(
                        Some(*line),
                        "orphaned_tool_result",
                        "tool output has no preceding call in effective context",
                    ));
                }
                if !outputs.insert(id) {
                    findings.push(warning(
                        Some(*line),
                        "duplicate_tool_result",
                        "tool result ID is duplicated in effective context",
                    ));
                }
            }
            _ => {}
        }
    }
    for (id, (_, line)) in calls {
        if !answered.contains(id) {
            findings.push(warning(
                Some(line),
                "unpaired_tool_call",
                "tool call has no matching later output in effective context",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(findings: &[VerifyFinding]) -> Vec<&'static str> {
        findings.iter().map(|f| f.code).collect()
    }

    fn has(findings: &[VerifyFinding], severity: Severity, code: &'static str) -> bool {
        findings
            .iter()
            .any(|f| f.severity == severity && f.code == code)
    }

    // ---------- generic line checks ----------

    #[test]
    fn empty_file_warns() {
        let findings = verify(Provider::ClaudeCode, b"");
        assert_eq!(findings.len(), 1);
        assert!(has(&findings, Severity::Warning, "empty"));
        assert_eq!(findings[0].line_index, None);
    }

    #[test]
    fn blank_lines_warn() {
        let raw = b"{\"type\":\"user\"}\n\n   \n{\"type\":\"user\"}\n";
        let findings = verify(Provider::ClaudeCode, raw);
        let blanks: Vec<_> = findings.iter().filter(|f| f.code == "blank_line").collect();
        assert_eq!(blanks.len(), 2);
        assert_eq!(blanks[0].line_index, Some(1));
        assert_eq!(blanks[1].line_index, Some(2));
    }

    #[test]
    fn mid_file_garbage_is_invalid_json() {
        let raw = b"{bad json\n{\"type\":\"user\"}\n";
        let findings = verify(Provider::ClaudeCode, raw);
        assert!(has(&findings, Severity::Error, "invalid_json"));
        assert!(!has(&findings, Severity::Warning, "partial_tail"));
    }

    #[test]
    fn sole_bad_tail_is_partial_not_error() {
        let raw = b"{\"type\":\"user\"}\n{\"type\":\"assist";
        let findings = verify(Provider::ClaudeCode, raw);
        assert_eq!(codes(&findings), vec!["partial_tail"]);
        assert!(has(&findings, Severity::Warning, "partial_tail"));
    }

    #[test]
    fn bad_tail_with_earlier_garbage_is_error() {
        // "only the final line fails" exception no longer applies.
        let raw = b"{bad\n{\"type\":\"user\"}\n{bad";
        let findings = verify(Provider::ClaudeCode, raw);
        let errors: Vec<_> = findings
            .iter()
            .filter(|f| f.code == "invalid_json")
            .collect();
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].line_index, Some(0));
        assert_eq!(errors[1].line_index, Some(2));
        assert!(!has(&findings, Severity::Warning, "partial_tail"));
    }

    // ---------- Claude dialect ----------

    #[test]
    fn claude_clean_file_passes() {
        let raw = concat!(
            "{\"type\":\"summary\",\"summary\":\"s\",\"leafUuid\":\"u1\"}\n",
            "{\"type\":\"user\",\"uuid\":\"u1\",\"parentUuid\":null,\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"u2\",\"parentUuid\":\"u1\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Bash\",\"input\":{}}]}}\n",
            "{\"type\":\"user\",\"uuid\":\"u3\",\"parentUuid\":\"u2\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"content\":\"ok\"}]}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"s1\",\"parentUuid\":\"u3\",\"isSidechain\":true,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"x\"}]}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"s2\",\"parentUuid\":\"s1\",\"isSidechain\":true,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"y\"}]}}\n",
            "{\"type\":\"attachment\",\"attachment\":{\"files\":1}}\n",
        );
        let findings = verify(Provider::ClaudeCode, raw.as_bytes());
        assert_eq!(findings, vec![], "expected zero findings, got {findings:?}");
    }

    #[test]
    fn claude_broken_parent_chain() {
        let raw = concat!(
            "{\"type\":\"user\",\"uuid\":\"u1\",\"parentUuid\":null,\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"u2\",\"parentUuid\":\"gone\",\"message\":{\"role\":\"assistant\",\"content\":[]}}\n",
        );
        let findings = verify(Provider::ClaudeCode, raw.as_bytes());
        assert!(has(&findings, Severity::Error, "broken_parent_chain"));
        let f = findings
            .iter()
            .find(|f| f.code == "broken_parent_chain")
            .unwrap();
        assert_eq!(f.line_index, Some(1));
    }

    #[test]
    fn claude_uuidless_records_exempt_from_chain() {
        // parentUuid present but no uuid field: exempt, no finding.
        let raw = "{\"type\":\"system\",\"parentUuid\":\"nope\",\"content\":\"x\"}\n";
        let findings = verify(Provider::ClaudeCode, raw.as_bytes());
        assert!(!has(&findings, Severity::Error, "broken_parent_chain"));
    }

    #[test]
    fn claude_orphaned_tool_use() {
        let raw = concat!(
            "{\"type\":\"user\",\"uuid\":\"u1\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"u2\",\"parentUuid\":\"u1\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Bash\",\"input\":{}}]}}\n",
            "{\"type\":\"user\",\"uuid\":\"u3\",\"parentUuid\":\"u2\",\"message\":{\"role\":\"user\",\"content\":\"next\"}}\n",
        );
        let findings = verify(Provider::ClaudeCode, raw.as_bytes());
        assert!(has(&findings, Severity::Error, "orphaned_tool_use"));
        let f = findings
            .iter()
            .find(|f| f.code == "orphaned_tool_use")
            .unwrap();
        assert_eq!(f.line_index, Some(1));
    }

    #[test]
    fn claude_orphaned_tool_result() {
        let raw = concat!(
            "{\"type\":\"user\",\"uuid\":\"u1\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t9\",\"content\":\"ok\"}]}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"u2\",\"parentUuid\":\"u1\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"x\"}]}}\n",
        );
        let findings = verify(Provider::ClaudeCode, raw.as_bytes());
        assert!(has(&findings, Severity::Warning, "orphaned_tool_result"));
    }

    #[test]
    fn claude_tool_use_pairs_only_with_later_result() {
        // Result on an *earlier* line does not rescue the call.
        let raw = concat!(
            "{\"type\":\"user\",\"uuid\":\"u1\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"content\":\"ok\"}]}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"u2\",\"parentUuid\":\"u1\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Bash\",\"input\":{}}]}}\n",
        );
        let findings = verify(Provider::ClaudeCode, raw.as_bytes());
        assert!(has(&findings, Severity::Error, "orphaned_tool_use"));
        assert!(has(&findings, Severity::Warning, "orphaned_tool_result"));
    }

    // ---------- Codex dialect ----------

    #[test]
    fn codex_clean_file_passes() {
        let raw = concat!(
            "{\"timestamp\":\"t\",\"ordinal\":1,\"type\":\"session_meta\",\"payload\":{\"id\":\"s\"}}\n",
            "{\"timestamp\":\"t\",\"ordinal\":2,\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[]}}\n",
            "{\"timestamp\":\"t\",\"ordinal\":3,\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"shell\",\"call_id\":\"c1\",\"arguments\":\"{}\"}}\n",
            "{\"timestamp\":\"t\",\"ordinal\":4,\"type\":\"response_item\",\"payload\":{\"type\":\"function_call_output\",\"call_id\":\"c1\",\"output\":\"ok\"}}\n",
            "{\"timestamp\":\"t\",\"ordinal\":5,\"type\":\"token_usage_record\",\"payload\":{\"usage\":{\"input_tokens\":10}}}\n",
            "{\"timestamp\":\"t\",\"ordinal\":6,\"type\":\"compacted\",\"payload\":{\"type\":\"compaction\",\"replacement_history\":[{\"type\":\"function_call\",\"call_id\":\"c9\"},{\"type\":\"function_call_output\",\"call_id\":\"c9\",\"output\":\"x\"}]}}\n",
        );
        let findings = verify(Provider::Codex, raw.as_bytes());
        assert_eq!(findings, vec![], "expected zero findings, got {findings:?}");
    }

    #[test]
    fn codex_malformed_compacted() {
        let raw = "{\"type\":\"compacted\",\"payload\":{\"type\":\"compaction\"}}\n";
        let findings = verify(Provider::Codex, raw.as_bytes());
        assert!(has(&findings, Severity::Error, "malformed_compacted"));
    }

    #[test]
    fn codex_compacted_history_wrong_type() {
        let raw = "{\"type\":\"compacted\",\"payload\":{\"replacement_history\":\"nope\"}}\n";
        let findings = verify(Provider::Codex, raw.as_bytes());
        assert!(has(&findings, Severity::Error, "malformed_compacted"));
    }

    #[test]
    fn codex_non_monotonic_ordinal() {
        let raw = concat!(
            "{\"ordinal\":1,\"type\":\"response_item\",\"payload\":{\"type\":\"message\"}}\n",
            "{\"ordinal\":3,\"type\":\"response_item\",\"payload\":{\"type\":\"message\"}}\n",
            "{\"ordinal\":2,\"type\":\"response_item\",\"payload\":{\"type\":\"message\"}}\n",
        );
        let findings = verify(Provider::Codex, raw.as_bytes());
        assert!(has(&findings, Severity::Warning, "non_monotonic_ordinal"));
        let f = findings
            .iter()
            .find(|f| f.code == "non_monotonic_ordinal")
            .unwrap();
        assert_eq!(f.line_index, Some(2));
    }

    #[test]
    fn codex_unpaired_tool_call() {
        let raw = concat!(
            "{\"ordinal\":1,\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"arguments\":\"{}\"}}\n",
            "{\"ordinal\":2,\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\"}}\n",
        );
        let findings = verify(Provider::Codex, raw.as_bytes());
        assert!(has(&findings, Severity::Warning, "unpaired_tool_call"));
        let f = findings
            .iter()
            .find(|f| f.code == "unpaired_tool_call")
            .unwrap();
        assert_eq!(f.line_index, Some(0));
    }

    #[test]
    fn codex_compacted_history_unpaired_call_flagged() {
        let raw = "{\"type\":\"compacted\",\"payload\":{\"replacement_history\":[{\"type\":\"function_call\",\"call_id\":\"c7\"}]}}\n";
        let findings = verify(Provider::Codex, raw.as_bytes());
        assert!(has(&findings, Severity::Warning, "unpaired_tool_call"));
        assert!(!has(&findings, Severity::Error, "malformed_compacted"));
    }
}
