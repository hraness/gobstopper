//! Pure preparation of caller-owned Codex context. No discovery, filesystem
//! writes, runtime attachment, or provider calls occur here. An owner must
//! separately establish a completed-turn boundary and adopt a new continuation.

use crate::{codex, copy, transaction};
use anyhow::{bail, ensure, Result};
use gobstopper_core::model::UsageSample;
use gobstopper_core::strategy::{CacheAwareStrategy, Strategy};
use gobstopper_core::{PolicyConfig, Provider, SessionHandle};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;

/// Private content supplied by the owner, with exact rollout-byte provenance.
#[derive(Clone, Serialize, Deserialize)]
pub struct OwnedHistory {
    pub items: Vec<Value>,
    pub usage: UsageSample,
    pub source_sha256: String,
}

/// Numeric preparation evidence. Estimates are not observed usage or savings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparationMetrics {
    /// Hashes of canonical serialized raw-item vectors, not rollout files.
    pub source_sha256: String,
    pub output_sha256: String,
    pub context_tokens_before: u64,
    pub projected_context_tokens_after: u64,
    pub projected_reclaimed_tokens: u64,
    pub preserved_prefix_items: usize,
    /// Serialized item bytes, excluding vector framing and separators.
    pub preserved_prefix_bytes: u64,
    pub preserved_prefix_estimated_tokens: u64,
    pub modified_tool_outputs: usize,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PreparedHistory {
    pub items: Vec<Value>,
    pub metrics: PreparationMetrics,
}

fn handle(id: &str) -> SessionHandle {
    SessionHandle {
        provider: Provider::Codex,
        session_id: id.into(),
        path: PathBuf::from("owned-history"),
        cwd: None,
        // This is a supplied immutable snapshot, never discovered live state.
        age_secs: u64::MAX,
    }
}

/// Capture the effective current window from an explicitly owned rollout.
/// Unknown metadata records are retained by the source hash but not interpreted.
/// Malformed records fail closed; no torn-tail recovery is attempted at idle.
pub fn capture_rollout(bytes: &[u8], expected_thread_id: &str) -> Result<OwnedHistory> {
    ensure!(
        !expected_thread_id.is_empty(),
        "missing expected thread identity"
    );
    ensure!(
        bytes.len() as u64 <= transaction::MAX_TRANSCRIPT_BYTES,
        "rollout exceeds byte limit"
    );
    let text = std::str::from_utf8(bytes).map_err(|_| anyhow::anyhow!("rollout is not UTF-8"))?;
    let mut matched = false;
    let mut items = Vec::new();
    for (index, line) in text.lines().enumerate() {
        ensure!(
            index < gobstopper_core::validation::MAX_ITEMS,
            "rollout exceeds record limit"
        );
        let record: Value =
            serde_json::from_str(line).map_err(|_| anyhow::anyhow!("malformed rollout record"))?;
        let kind = record
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("invalid rollout envelope"))?;
        match kind {
            "session_meta" => {
                ensure!(!matched, "duplicate session identity");
                ensure!(
                    record.pointer("/payload/id").and_then(Value::as_str)
                        == Some(expected_thread_id),
                    "rollout identity mismatch"
                );
                matched = true;
            }
            "response_item" => {
                items.push(
                    record
                        .get("payload")
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("missing response item"))?,
                );
            }
            "compacted" => {
                items = record
                    .pointer("/payload/replacement_history")
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("invalid compacted history"))?;
            }
            _ => {}
        }
    }
    ensure!(matched, "missing session identity");
    validate_items(&items)?;
    let usage = codex::load_bytes(handle(expected_thread_id), bytes)?.usage;
    Ok(OwnedHistory {
        items,
        usage,
        source_sha256: copy::sha256(bytes),
    })
}

fn nonempty<'a>(item: &'a Value, field: &str) -> Result<&'a str> {
    item.get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing or invalid history string"))
}

fn content(value: &Value, allow_string: bool) -> Result<()> {
    if allow_string && value.is_string() {
        return Ok(());
    }
    let blocks = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid content shape"))?;
    for block in blocks {
        match nonempty(block, "type")? {
            "input_text" | "output_text" | "text" | "reasoning_text" | "summary_text" => {
                ensure!(
                    block.get("text").is_some_and(Value::is_string),
                    "invalid textual content"
                );
            }
            "input_image" => {
                nonempty(block, "image_url")?;
            }
            "input_audio" => {
                nonempty(block, "audio_url")?;
            }
            "encrypted_content" => {
                nonempty(block, "encrypted_content")?;
            }
            _ => bail!("unsupported content kind"),
        }
    }
    Ok(())
}

/// Validate complete raw history at an idle boundary. Opaque supported items
/// remain unchanged. This is not authorization to inject arbitrary roles or
/// controls; a caller offering synthetic injection needs its own allowlist.
pub fn validate_items(items: &[Value]) -> Result<()> {
    ensure!(!items.is_empty(), "empty history");
    ensure!(
        items.len() <= gobstopper_core::validation::MAX_ITEMS,
        "history exceeds item limit"
    );
    let mut size = 0usize;
    let mut calls: HashMap<&str, (&str, bool)> = HashMap::new();
    for item in items {
        size = size
            .checked_add(serde_json::to_vec(item)?.len())
            .ok_or_else(|| anyhow::anyhow!("history size overflow"))?;
        ensure!(
            size as u64 <= transaction::MAX_TRANSCRIPT_BYTES,
            "history exceeds byte limit"
        );
        let kind = nonempty(item, "type")?;
        match kind {
            "message" => {
                ensure!(
                    matches!(
                        nonempty(item, "role")?,
                        "user" | "assistant" | "system" | "developer"
                    ),
                    "unsupported message role"
                );
                content(&item["content"], false)?;
            }
            "agent_message" => {
                nonempty(item, "author")?;
                nonempty(item, "recipient")?;
                content(&item["content"], false)?;
            }
            "function_call" | "custom_tool_call" | "local_shell_call" => {
                let id = nonempty(item, "call_id")?;
                ensure!(!calls.contains_key(id), "duplicate tool call identity");
                match kind {
                    "function_call" => {
                        nonempty(item, "name")?;
                        ensure!(
                            item.get("arguments").is_some_and(Value::is_string),
                            "invalid call arguments"
                        );
                    }
                    "custom_tool_call" => {
                        nonempty(item, "name")?;
                        ensure!(
                            item.get("input").is_some_and(Value::is_string),
                            "invalid custom call input"
                        );
                    }
                    _ => ensure!(
                        item.get("action").is_some_and(Value::is_object),
                        "invalid shell action"
                    ),
                }
                calls.insert(id, (kind, false));
            }
            "function_call_output" | "custom_tool_call_output" => {
                let id = nonempty(item, "call_id")?;
                let (call_kind, complete) = calls
                    .get_mut(id)
                    .ok_or_else(|| anyhow::anyhow!("unpaired or out-of-order tool output"))?;
                ensure!(!*complete, "duplicate tool output");
                ensure!(
                    (kind == "custom_tool_call_output") == (*call_kind == "custom_tool_call"),
                    "mismatched tool output kind"
                );
                content(&item["output"], true)?;
                *complete = true;
            }
            "reasoning" => {
                if let Some(value) = item.get("content").filter(|v| !v.is_null()) {
                    content(value, false)?;
                }
                if let Some(value) = item.get("summary").filter(|v| !v.is_null()) {
                    content(value, false)?;
                }
                ensure!(
                    item.get("encrypted_content")
                        .is_none_or(|v| v.is_null() || v.is_string()),
                    "invalid reasoning envelope"
                );
            }
            "compaction" => {
                nonempty(item, "encrypted_content")?;
            }
            "context_compaction" => ensure!(
                item.get("encrypted_content")
                    .is_none_or(|v| v.is_null() || v.is_string()),
                "invalid compaction envelope"
            ),
            "configuration_update" => ensure!(
                item.get("reasoning").is_some_and(Value::is_object),
                "invalid configuration envelope"
            ),
            "compaction_trigger" => {}
            "web_search_call" => {}
            "image_generation_call" => {
                nonempty(item, "status")?;
                ensure!(
                    item.get("result").is_some_and(Value::is_string),
                    "invalid image result"
                );
            }
            // These kinds have runtime-controlled effects/definitions not
            // currently represented in the pairing adapter. Defer them.
            _ => bail!("unsupported history item kind"),
        }
    }
    ensure!(
        calls.values().all(|(_, complete)| *complete),
        "unresolved tool call"
    );
    Ok(())
}

fn encode(items: &[Value]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for item in items {
        serde_json::to_writer(&mut bytes, &json!({"type":"response_item", "payload":item}))?;
        bytes.push(b'\n');
    }
    ensure!(
        bytes.len() as u64 <= transaction::MAX_TRANSCRIPT_BYTES,
        "encoded history exceeds byte limit"
    );
    Ok(bytes)
}

fn text_only_change(before: &Value, after: &Value) -> bool {
    match (before, after) {
        (Value::String(_), Value::String(_)) => true,
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            a.iter().zip(b).all(|(old, new)| {
                if matches!(
                    old["type"].as_str(),
                    Some("input_text" | "output_text" | "text")
                ) {
                    let mut permitted = old.clone();
                    permitted["text"] = new["text"].clone();
                    new["text"].is_string() && permitted == *new
                } else {
                    old == new
                }
            })
        }
        _ => false,
    }
}

/// Prepare stale-output elision for a separately owned continuation. Policy
/// estimates may differ from the provider's next request; the owner supplies
/// cooldown, cache economics, identity, snapshot and adoption guards.
pub fn prepare(
    items: &[Value],
    usage: UsageSample,
    policy: &PolicyConfig,
    min_prefix_tokens: u64,
) -> Result<Option<PreparedHistory>> {
    validate_items(items)?;
    ensure!(
        policy.trigger_tokens > 0
            && policy.floor_tokens < policy.trigger_tokens
            && policy.trigger_tokens <= 10_000_000
            && policy.keep_recent_tool_outputs <= 100_000
            && policy.min_savings_tokens <= 10_000_000,
        "invalid preparation policy"
    );
    ensure!(usage.context_tokens > 0, "missing measured context usage");
    let original = encode(items)?;
    let mut transcript = codex::load_bytes(handle("owned-history"), &original)?;
    transcript.usage = usage;
    let Some(plan) = CacheAwareStrategy.evaluate(&transcript, policy) else {
        return Ok(None);
    };
    if plan.context_tokens_after >= plan.context_tokens_before
        || !policy.accepts_savings(plan.context_tokens_before, plan.context_tokens_after)
    {
        return Ok(None);
    }
    let transformed =
        codex::transform_bytes(handle("owned-history"), &original, policy, &plan.edits)?;
    let replacement: Vec<Value> = std::str::from_utf8(&transformed)?
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).map(|record| record["payload"].clone()))
        .collect::<std::result::Result<_, _>>()?;
    validate_items(&replacement)?;
    ensure!(
        replacement.len() == items.len() + 1,
        "unexpected preparation item count"
    );
    let prefix = items
        .iter()
        .zip(&replacement)
        .take_while(|(a, b)| a == b)
        .count();
    let prefix_bytes = items[..prefix]
        .iter()
        .map(|item| item.to_string().len() as u64)
        .sum();
    let prefix_tokens = transcript.items[..prefix]
        .iter()
        .map(|item| item.est_tokens)
        .sum();
    if prefix_tokens < min_prefix_tokens {
        return Ok(None);
    }
    let mut modified = 0;
    for (before, after) in items.iter().zip(&replacement) {
        if before == after {
            continue;
        }
        ensure!(
            matches!(
                before["type"].as_str(),
                Some("function_call_output" | "custom_tool_call_output")
            ),
            "unexpected non-output change"
        );
        ensure!(
            text_only_change(&before["output"], &after["output"]),
            "nontext output changed"
        );
        let mut permitted = before.clone();
        permitted["output"] = after["output"].clone();
        ensure!(&permitted == after, "tool output metadata changed");
        modified += 1;
    }
    ensure!(modified > 0, "preparation has no changed output");
    let metrics = PreparationMetrics {
        source_sha256: copy::sha256(&serde_json::to_vec(items)?),
        output_sha256: copy::sha256(&serde_json::to_vec(&replacement)?),
        context_tokens_before: plan.context_tokens_before,
        projected_context_tokens_after: plan.context_tokens_after,
        projected_reclaimed_tokens: plan.est_savings(),
        preserved_prefix_items: prefix,
        preserved_prefix_bytes: prefix_bytes,
        preserved_prefix_estimated_tokens: prefix_tokens,
        modified_tool_outputs: modified,
    };
    Ok(Some(PreparedHistory {
        items: replacement,
        metrics,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(text: &str) -> Value {
        json!({"type":"message","role":"user","content":[{"type":"input_text","text":text}]})
    }

    fn fixture() -> Vec<Value> {
        let mut items = vec![
            message("Keep the release gate at 0.60. 界é😀"),
            json!({"type":"reasoning","encrypted_content":"opaque-preserved","summary":[]}),
        ];
        for n in 0..4 {
            items.push(json!({"type":"function_call","call_id":format!("c{n}"),"name":"read_file","arguments":"{}"}));
            items.push(json!({"type":"function_call_output","call_id":format!("c{n}"),"output":"archived result 界😀 ".repeat(800)}));
        }
        items.push(message("Continue the bounded task."));
        items
    }

    fn policy() -> PolicyConfig {
        PolicyConfig {
            trigger_tokens: 10_000,
            floor_tokens: 12_000 / 2,
            keep_recent_tool_outputs: 1,
            min_savings_tokens: 100,
            ..Default::default()
        }
    }

    fn usage() -> UsageSample {
        UsageSample {
            context_tokens: 20_000,
            ..Default::default()
        }
    }

    fn rollout(items: &[Value]) -> Vec<u8> {
        let mut bytes =
            serde_json::to_vec(&json!({"type":"session_meta","payload":{"id":"owned"}})).unwrap();
        bytes.push(b'\n');
        bytes.extend(encode(items).unwrap());
        bytes.extend_from_slice(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{\"input_tokens\":19000,\"output_tokens\":1000},\"total_token_usage\":{\"input_tokens\":90000,\"cached_input_tokens\":80000},\"model_context_window\":200000}}}\n");
        bytes
    }

    #[test]
    fn capture_is_bound_and_folds_only_the_latest_window() {
        let old = fixture();
        let mut bytes = rollout(&old);
        let active = vec![
            message("new window"),
            json!({"type":"reasoning","encrypted_content":"opaque"}),
        ];
        bytes.extend(
            serde_json::to_vec(
                &json!({"type":"compacted","payload":{"replacement_history":active}}),
            )
            .unwrap(),
        );
        bytes.push(b'\n');
        bytes.extend(encode(&[message("later")]).unwrap());
        let before = bytes.clone();
        let captured = capture_rollout(&bytes, "owned").unwrap();
        assert_eq!(captured.items.len(), 3);
        assert_eq!(captured.items[0], active[0]);
        assert_eq!(
            captured.usage.context_tokens, 0,
            "old occupancy must not survive compaction"
        );
        assert_eq!(captured.usage.lifetime_input_tokens, 90_000);
        assert_eq!(captured.source_sha256, copy::sha256(&bytes));
        assert_eq!(bytes, before);
        assert!(capture_rollout(&bytes, "other").is_err());
    }

    #[test]
    fn capture_rejects_malformed_empty_missing_or_duplicate_identity() {
        for bytes in [
            b"".as_slice(),
            b"{}\n",
            b"{\"type\":\"response_item\",\"payload\":null}\n",
            b"{",
            b"\xff",
        ] {
            assert!(capture_rollout(bytes, "owned").is_err());
        }
        let mut bytes = rollout(&fixture());
        bytes.extend_from_slice(b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"owned\"}}\n");
        assert!(capture_rollout(&bytes, "owned").is_err());
        let mut bytes = rollout(&fixture());
        bytes.extend_from_slice(
            b"{\"type\":\"compacted\",\"payload\":{\"replacement_history\":null}}\n",
        );
        assert!(capture_rollout(&bytes, "owned").is_err());
    }

    #[test]
    fn preparation_changes_only_stale_text_preserves_prefix_tail_and_source() {
        let items = fixture();
        let before = serde_json::to_vec(&items).unwrap();
        let prepared = prepare(&items, usage(), &policy(), 1).unwrap().unwrap();
        assert_eq!(serde_json::to_vec(&items).unwrap(), before);
        assert_eq!(&prepared.items[..3], &items[..3]);
        assert_eq!(&prepared.items[8..11], &items[8..11]);
        assert_eq!(prepared.items.len(), items.len() + 1);
        assert!(prepared.items[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("0.60"));
        assert!(prepared.items.last().unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with(gobstopper_core::DigestBlock::MARKER));
        assert!(prepared.metrics.modified_tool_outputs > 0);
        assert_eq!(prepared.metrics.preserved_prefix_items, 3);
        assert!(prepared.metrics.projected_context_tokens_after < 20_000);
        assert_ne!(
            prepared.metrics.source_sha256,
            prepared.metrics.output_sha256
        );
        validate_items(&prepared.items).unwrap();
        let again = prepare(&items, usage(), &policy(), 1).unwrap().unwrap();
        assert_eq!(again.metrics.output_sha256, prepared.metrics.output_sha256);
    }

    #[test]
    fn preparation_obeys_threshold_prefix_minimum_recent_tail_and_gain() {
        let items = fixture();
        let mut low = usage();
        low.context_tokens = 9_999;
        assert!(prepare(&items, low, &policy(), 0).unwrap().is_none());
        assert!(prepare(&items, usage(), &policy(), u64::MAX)
            .unwrap()
            .is_none());
        let mut p = policy();
        p.keep_recent_tool_outputs = 4;
        assert!(prepare(&items, usage(), &p, 0).unwrap().is_none());
        let mut p = policy();
        p.min_savings_tokens = 1_000_000;
        assert!(prepare(&items, usage(), &p, 0).unwrap().is_none());
        assert!(prepare(&items, UsageSample::default(), &policy(), 0).is_err());
        let mut p = policy();
        p.floor_tokens = p.trigger_tokens;
        assert!(prepare(&items, usage(), &p, 0).is_err());
    }

    #[test]
    fn mixed_output_preserves_images_encryption_and_block_metadata() {
        let mut items = fixture();
        items[2] = json!({"type":"custom_tool_call","call_id":"c0","name":"custom","input":"{}"});
        items[3] = json!({"type":"custom_tool_call_output","call_id":"c0","output":[
            {"type":"input_text","text":"界é😀".repeat(3000),"custom_marker":7},
            {"type":"input_image","image_url":"data:image/png;base64,AAAA"},
            {"type":"encrypted_content","encrypted_content":"opaque"}]});
        let out = prepare(&items, usage(), &policy(), 0).unwrap().unwrap();
        assert_ne!(
            out.items[3]["output"][0]["text"],
            items[3]["output"][0]["text"]
        );
        assert_eq!(out.items[3]["output"][0]["custom_marker"], 7);
        assert_eq!(out.items[3]["output"][1], items[3]["output"][1]);
        assert_eq!(out.items[3]["output"][2], items[3]["output"][2]);
    }

    #[test]
    fn incomplete_duplicate_reversed_and_wrong_kind_pairs_fail_closed() {
        let base = fixture();
        let mut bad = base.clone();
        bad.remove(3);
        assert!(validate_items(&bad).is_err());
        let mut bad = base.clone();
        bad.swap(2, 3);
        assert!(validate_items(&bad).is_err());
        let mut bad = base.clone();
        bad.insert(4, base[3].clone());
        assert!(validate_items(&bad).is_err());
        let mut bad = base.clone();
        bad.insert(4, base[2].clone());
        assert!(validate_items(&bad).is_err());
        let mut bad = base.clone();
        bad[3]["type"] = json!("custom_tool_call_output");
        assert!(validate_items(&bad).is_err());
        let mut bad = base;
        bad[3]["call_id"] = json!("");
        assert!(validate_items(&bad).is_err());
    }

    #[test]
    fn unknown_malformed_and_nontext_content_fail_without_silently_dropping() {
        for item in [
            json!(null),
            json!({"type":"future_type"}),
            json!({"type":"message","role":"user","content":null}),
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":4}]}),
            json!({"type":"message","role":"user","content":[{"type":"unknown_block"}]}),
            json!({"type":"reasoning","encrypted_content":4}),
        ] {
            assert!(validate_items(&[item]).is_err());
        }
        assert!(validate_items(&[]).is_err());
        let bytes = rollout(&[json!({"type":"future_type"})]);
        assert!(capture_rollout(&bytes, "owned").is_err());
    }

    #[test]
    fn unknown_metadata_is_ignored_but_active_history_limits_are_enforced() {
        let mut bytes = rollout(&fixture());
        bytes.extend_from_slice(b"{\"type\":\"future_metadata\",\"payload\":{\"number\":9}}\n");
        assert_eq!(capture_rollout(&bytes, "owned").unwrap().items, fixture());
        let too_many =
            vec![json!({"type":"compaction_trigger"}); gobstopper_core::validation::MAX_ITEMS + 1];
        assert!(validate_items(&too_many).is_err());
    }

    #[test]
    fn pure_transform_rejects_provider_controls_and_protected_targets() {
        let items = fixture();
        let bytes = encode(&items).unwrap();
        let provider = gobstopper_core::Edit::ProviderCompact {
            control: "thread/compact/start".into(),
        };
        assert!(codex::transform_bytes(handle("owned"), &bytes, &policy(), &[provider]).is_err());
        let edit = gobstopper_core::Edit::Elide {
            line_indexes: vec![9],
            stub_template: "[elided]".into(),
            per_item_stubs: Default::default(),
        };
        assert!(codex::transform_bytes(handle("owned"), &bytes, &policy(), &[edit]).is_err());
    }
}
