//! Resume-validity checker for provider transcript files.
//!
//! `verify`/`verify_path` scan a raw JSONL transcript and report structural
//! findings that would break or degrade a provider resume — without ever
//! surfacing transcript content. Finding messages carry only counts and
//! line indexes: no payload text, paths, or uuids.
//!
//! Dialects checked (matching what `claude`/`codex` actually parse):
//!
//! Claude Code (`Provider::ClaudeCode`):
//!   - `broken_parent_chain` (Error): a uuid-bearing record's `parentUuid`
//!     string does not match any `uuid` on an earlier line. Records without
//!     a `uuid` field are exempt (attachments/system lines lack uuids).
//!     Sidechain records (`isSidechain: true`) need no special scope: a
//!     sidechain's first message legitimately parents onto any earlier
//!     uuid, and the check accepts any earlier uuid in the file.
//!   - `orphaned_tool_use` (Error): a `tool_use` block `id` inside an
//!     assistant `message.content[]` with no `tool_result` block carrying
//!     a matching `tool_use_id` in a *later* record.
//!   - `orphaned_tool_result` (Warning): a `tool_result` `tool_use_id`
//!     with no matching assistant `tool_use` on an earlier line.
//!
//! Codex (`Provider::Codex`):
//!   - `malformed_compacted` (Error): a top-level `type == "compacted"`
//!     record whose `payload.replacement_history` is missing or not an
//!     array (the adapter reads live context out of that array).
//!   - `non_monotonic_ordinal` (Warning): a top-level numeric `ordinal`
//!     lower than the previous ordinal-bearing record's.
//!   - `unpaired_tool_call` (Warning): a `response_item` payload of type
//!     `function_call`/`custom_tool_call`/`local_shell_call` whose
//!     `call_id` never appears on a `function_call_output`/
//!     `custom_tool_call_output` anywhere in the file. Items inside a
//!     compacted record's `replacement_history` participate in pairing.
//!     Item `id` fields (e.g. `fc_…`) are intentionally not used: outputs
//!     pair on `call_id` only, so calls without a `call_id` cannot be
//!     verified and are skipped rather than guessed at.
//!
//! Both dialects share the generic line checks: unparseable non-blank
//! lines are `invalid_json` errors, except when the *only* failure is the
//! final line — a torn tail write on a live session — which is downgraded
//! to the `partial_tail` warning. Whitespace-only lines are `blank_line`
//! warnings, and an empty file yields a single `empty` warning.
//!
//! Not expressible against the dialect fields, and therefore omitted:
//! Claude sidechain-internal ordering beyond the earlier-uuid rule, and
//! Codex call/output *ordering* (outputs may legitimately precede calls in
//! reconstructed `replacement_history` context).

use gobstopper_core::Provider;
use serde_json::Value;
use std::collections::HashSet;

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
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().collect();
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
        match serde_json::from_str::<Value>(line) {
            Ok(v) => records.push(Some(v)),
            Err(_) => {
                bad_lines.push(i);
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
    }

    findings.sort_by_key(|f| f.line_index.unwrap_or(usize::MAX));
    findings
}

/// Read a transcript file and verify it.
pub fn verify_path(
    provider: Provider,
    path: &std::path::Path,
) -> std::io::Result<Vec<VerifyFinding>> {
    let bytes = std::fs::read(path)?;
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
            seen_uuids.insert(uuid);
        }

        let is_assistant = record.get("type").and_then(Value::as_str) == Some("assistant");
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
                // tool_result blocks live in user lines, but accept them on
                // any record so pairing never misses a valid match.
                Some("tool_result") => {
                    if let Some(id) = block.get("tool_use_id").and_then(Value::as_str) {
                        tool_results.push((id, i));
                    }
                }
                _ => {}
            }
        }
    }

    for &(id, line) in &tool_uses {
        let answered = tool_results
            .iter()
            .any(|&(rid, rline)| rline > line && rid == id);
        if !answered {
            findings.push(error(
                Some(line),
                "orphaned_tool_use",
                "tool_use block has no matching tool_result in a later record",
            ));
        }
    }
    for &(id, line) in &tool_results {
        let preceded = tool_uses.iter().any(|&(uid, uline)| uline < line && uid == id);
        if !preceded {
            findings.push(warning(
                Some(line),
                "orphaned_tool_result",
                "tool_result block has no matching tool_use in an earlier record",
            ));
        }
    }
}

/// Collect a call/output `call_id` from one Codex response item payload —
/// either a `response_item` record's `payload` or an item inside a
/// compacted record's `replacement_history`.
fn collect_codex_item<'a>(
    payload: &'a Value,
    line: usize,
    calls: &mut Vec<(&'a str, usize)>,
    outputs: &mut HashSet<&'a str>,
) {
    match payload.get("type").and_then(Value::as_str) {
        Some("function_call") | Some("custom_tool_call") | Some("local_shell_call") => {
            if let Some(id) = payload.get("call_id").and_then(Value::as_str) {
                calls.push((id, line));
            }
        }
        Some("function_call_output") | Some("custom_tool_call_output") => {
            if let Some(id) = payload.get("call_id").and_then(Value::as_str) {
                outputs.insert(id);
            }
        }
        _ => {}
    }
}

/// Codex dialect checks: compacted-record shape, `ordinal` monotonicity,
/// and `call_id` pairing between calls and outputs.
fn verify_codex(records: &[Option<Value>], findings: &mut Vec<VerifyFinding>) {
    let mut last_ordinal: Option<i64> = None;
    let mut calls: Vec<(&str, usize)> = Vec::new();
    let mut outputs: HashSet<&str> = HashSet::new();

    for (i, record) in records.iter().enumerate() {
        let Some(record) = record else { continue };
        let rtype = record.get("type").and_then(Value::as_str).unwrap_or("");

        if let Some(ordinal) = record.get("ordinal").and_then(Value::as_i64) {
            if let Some(prev) = last_ordinal {
                if ordinal < prev {
                    findings.push(warning(
                        Some(i),
                        "non_monotonic_ordinal",
                        "ordinal is lower than the previous ordinal-bearing record",
                    ));
                }
            }
            last_ordinal = Some(ordinal);
        }

        match rtype {
            "compacted" => {
                let history = record
                    .get("payload")
                    .and_then(|p| p.get("replacement_history"));
                if !history.map(Value::is_array).unwrap_or(false) {
                    findings.push(error(
                        Some(i),
                        "malformed_compacted",
                        "compacted record lacks a replacement_history array",
                    ));
                }
                if let Some(items) = history.and_then(Value::as_array) {
                    for item in items {
                        collect_codex_item(item, i, &mut calls, &mut outputs);
                    }
                }
            }
            "response_item" => {
                if let Some(payload) = record.get("payload") {
                    collect_codex_item(payload, i, &mut calls, &mut outputs);
                }
            }
            _ => {}
        }
    }

    for &(id, line) in &calls {
        if !outputs.contains(id) {
            findings.push(warning(
                Some(line),
                "unpaired_tool_call",
                "tool call has no matching output record",
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
        let blanks: Vec<_> = findings
            .iter()
            .filter(|f| f.code == "blank_line")
            .collect();
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
        let raw =
            "{\"type\":\"compacted\",\"payload\":{\"replacement_history\":\"nope\"}}\n";
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
