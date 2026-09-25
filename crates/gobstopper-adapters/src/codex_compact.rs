//! Codex `compacted`-record byte transformation (experimental).
//!
//! This module prepares detached candidates; it grants no authority to rewrite
//! a provider path, and provider resume compatibility needs separate qualification.
//!
//! Codex persists provider-native compaction as a tail record of
//! `type == "compacted"` whose `payload.replacement_history` is the
//! context the provider swaps in on resume — it is the provider's own
//! resume-time context swap, not a convention of ours. Emitting such a
//! record with a custom `replacement_history` lets transcript-path
//! strategies produce a real Codex compaction: on the next resume the
//! provider rebuilds context from `replacement_history` plus the records
//! appended after it.
//!
//! Observed envelope (verified against live `~/.codex/sessions` rollouts):
//!
//! ```json
//! {"timestamp": "…", "ordinal": <line index>, "type": "compacted",
//!  "payload": {"message": "", "replacement_history": […],
//!              "guardian_history": […],            // optional
//!              "retained_context": {…},            // optional
//!              "window_number": n,
//!              "first_window_id": "…",
//!              "previous_window_id": "…",
//!              "window_id": "…",
//!              "compaction_response_id": "resp_…",
//!              "latest_token_usage_record": {…} | null}}
//! ```
//!
//! Window-chain semantics observed across consecutive compactions of one
//! session: `first_window_id` is the original window and stays fixed;
//! `previous_window_id` is the window the new record replaces (the prior
//! record's `window_id`); `window_id` is a fresh uuidv7; `window_number`
//! increments from 1. On the first compaction `previous_window_id` equals
//! `first_window_id` — the window being replaced IS the original window.
//! This module replays that chain: envelope fields are inherited from the
//! most recent existing `compacted` record when one exists, with the
//! window chain advanced by one step, not duplicated — every real record
//! has a unique `window_id`, and resume keys on `window_number`.
//!
//! Deliberate deviations from a verbatim provider record, all forced:
//!   - `replacement_history` carries no trailing
//!     `{"type": "compaction", "encrypted_content": …}` item — that blob
//!     is provider-encrypted and cannot be forged. The plaintext digest
//!     user-message takes its place as the summary carrier.
//!   - `compaction_response_id` is synthesized (`resp_<48 hex>`) when no
//!     prior record supplies one: it references the provider's compaction
//!     API response, which a file-side write never has.
//!   - `latest_token_usage_record` prefers the file's freshest
//!     `token_usage_record` payload over the stale copy embedded in the
//!     prior `compacted` record — the field is defined as "latest".

use anyhow::{bail, Context};
use gobstopper_core::plan::DigestBlock;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Process-unique counter mixed into synthesized ids so same-instant
/// records still diverge.
static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The payload keys this module knows how to set, in the exact order the
/// provider writes them. Anything else on a cloned envelope is preserved
/// verbatim through `CompactedPayload::extra`.
const KNOWN_PAYLOAD_KEYS: &[&str] = &[
    "message",
    "replacement_history",
    "guardian_history",
    "retained_context",
    "window_number",
    "first_window_id",
    "previous_window_id",
    "window_id",
    "compaction_response_id",
    "latest_token_usage_record",
];

/// serde_json's `Map` sorts keys alphabetically, so the envelope is
/// serialized through structs: serde emits fields in declaration order,
/// reproducing the provider's key order exactly.
#[derive(Serialize)]
struct CompactedRecord {
    timestamp: String,
    ordinal: u64,
    #[serde(rename = "type")]
    kind: &'static str,
    payload: CompactedPayload,
}

#[derive(Serialize)]
struct CompactedPayload {
    message: Value,
    replacement_history: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    guardian_history: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retained_context: Option<Value>,
    window_number: u64,
    first_window_id: String,
    previous_window_id: String,
    window_id: String,
    compaction_response_id: Value,
    latest_token_usage_record: Value,
    /// Envelope fields a future provider version adds pass through
    /// untouched rather than being dropped by the clone.
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

/// SHA-256-derived uuidv7-shaped id (48-bit unix-millis prefix, version
/// and variant bits forced), matching the `window_id`/`msg_` id shape in
/// real rollouts. `seed` scopes the hash; time/pid/counter supply the
/// entropy — no uuid crate needed.
fn synth_uuid_v7(seed: &[u8]) -> String {
    use std::fmt::Write as _;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(seed);
    hasher.update(now.as_nanos().to_be_bytes());
    hasher.update(std::process::id().to_be_bytes());
    hasher.update(ID_COUNTER.fetch_add(1, Ordering::Relaxed).to_be_bytes());
    let digest = hasher.finalize();

    let mut b = [0u8; 16];
    let millis = (now.as_millis() as u64).to_be_bytes();
    b[..6].copy_from_slice(&millis[2..]); // 48-bit epoch milliseconds
    b[6..].copy_from_slice(&digest[..10]);
    b[6] = (b[6] & 0x0f) | 0x70; // version 7
    b[8] = (b[8] & 0x3f) | 0x80; // variant 10

    let mut s = String::with_capacity(36);
    for (i, byte) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        let _ = write!(s, "{byte:02x}");
    }
    s
}

/// `resp_<48 lowercase hex>` shaped like the provider's compaction
/// response ids. Purely synthetic — no provider response backs it.
fn synth_resp_id(seed: &[u8]) -> String {
    use std::fmt::Write as _;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(b"gobstopper-compaction-response");
    hasher.update(seed);
    hasher.update(now.as_nanos().to_be_bytes());
    hasher.update(ID_COUNTER.fetch_add(1, Ordering::Relaxed).to_be_bytes());
    let digest = hasher.finalize();
    let mut s = String::with_capacity(5 + 48);
    s.push_str("resp_");
    for byte in &digest[..24] {
        let _ = write!(s, "{byte:02x}");
    }
    s
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` — the timestamp format observed on rollout
/// records, produced without a chrono dependency.
pub fn rfc3339_now() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    // Civil date from days since epoch (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}.{millis:03}Z")
}

/// Read a rollout file into lines (trailing newline not included), the
/// `src_lines` input shape [`build_compacted_record`] expects.
pub fn read_rollout_lines(path: &Path) -> anyhow::Result<Vec<String>> {
    let bytes = crate::transaction::read(path)?;
    let raw = std::str::from_utf8(&bytes).context("rollout is not UTF-8")?;
    Ok(raw.lines().map(str::to_string).collect())
}

/// The payloads of the last `n` `response_item` records, in file order —
/// the "keep tail" half of a custom replacement history.
pub fn tail_response_items(src_lines: &[String], n: usize) -> Vec<Value> {
    if src_lines.len() > gobstopper_core::validation::MAX_ITEMS {
        return Vec::new();
    }
    let mut items = Vec::new();
    for line in src_lines.iter().rev() {
        if items.len() == n {
            break;
        }
        let Ok(record) = crate::payload::decode_record(line) else {
            break;
        };
        if record.get("type").and_then(Value::as_str) == Some("compacted") {
            break;
        }
        if record.get("type").and_then(Value::as_str) == Some("response_item") {
            if let Some(payload) = record
                .get("payload")
                .filter(|p| crate::codex::supported_item(p))
            {
                items.push(payload.clone());
            }
        }
    }
    items.reverse();
    let mut calls = std::collections::HashMap::new();
    let mut results = std::collections::HashMap::new();
    let mut duplicates = std::collections::HashSet::new();
    for (position, item) in items.iter().enumerate() {
        let Some(id) = crate::codex::record_call_id(item) else {
            continue;
        };
        let slots = match item.get("type").and_then(Value::as_str) {
            Some("function_call" | "custom_tool_call" | "local_shell_call") => &mut calls,
            Some("function_call_output" | "custom_tool_call_output") => &mut results,
            _ => continue,
        };
        if slots.insert(id.to_string(), position).is_some() {
            duplicates.insert(id.to_string());
        }
    }
    items
        .into_iter()
        .filter(|item| match item.get("type").and_then(Value::as_str) {
            Some(
                "function_call"
                | "custom_tool_call"
                | "local_shell_call"
                | "function_call_output"
                | "custom_tool_call_output",
            ) => crate::codex::record_call_id(item).is_some_and(|id| {
                !duplicates.contains(id)
                    && calls
                        .get(id)
                        .zip(results.get(id))
                        .is_some_and(|(call, result)| call < result)
            }),
            _ => true,
        })
        .collect()
}

/// The digest as the first `replacement_history` entry, in the same
/// flat response-item shape real compacted records use for user
/// messages: `{"type": "message", "id": "msg_…", "role": "user",
/// "content": [{"type": "input_text", …}],
/// "internal_chat_message_metadata_passthrough": {…}}`.
///
/// `turn_id` is inherited from the newest tail item that carries one so
/// the synthesized message belongs to a turn the file already knows;
/// `content_item_kinds: ["user.text"]` matches real user messages.
fn digest_item(digest: &DigestBlock, tail_items: &[Value]) -> Value {
    let text = crate::codex::digest_text(digest);
    let turn_id = tail_items
        .iter()
        .rev()
        .find_map(|it| {
            it.get("internal_chat_message_metadata_passthrough")
                .and_then(|m| m.get("turn_id"))
                .and_then(Value::as_str)
        })
        .map(str::to_string)
        .unwrap_or_else(|| synth_uuid_v7(b"gobstopper-digest-turn"));
    let create_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    serde_json::json!({
        "type": "message",
        "id": format!("msg_{}", synth_uuid_v7(text.as_bytes())),
        "role": "user",
        "content": [{"type": "input_text", "text": text}],
        "internal_chat_message_metadata_passthrough": {
            "turn_id": turn_id,
            "create_time": create_time,
            "content_item_kinds": ["user.text"],
        },
    })
}

/// Lower an [`Edit::InjectDigest`](gobstopper_core::plan::Edit) to a
/// `replacement_history`: the digest as a leading user message followed
/// by the last-N verbatim tail items the caller wants to keep live.
pub fn digest_to_replacement_history(digest: &DigestBlock, tail_items: &[Value]) -> Vec<Value> {
    let mut history = Vec::with_capacity(tail_items.len() + 1);
    history.push(digest_item(digest, tail_items));
    history.extend(tail_items.iter().cloned());
    history
}

/// Build a `compacted` record line that asks Codex resume to swap in
/// `replacement_history`. Returns the serialized JSON object (no
/// trailing newline).
///
/// Envelope fields are inherited from the most recent existing
/// `compacted` record's payload when one exists, with the window chain
/// advanced (`window_number` +1, `previous_window_id` ← prior
/// `window_id`, fresh `window_id`); otherwise they are synthesized
/// minimally in the provider's own shape. `latest_token_usage_record`
/// always prefers the file's freshest `token_usage_record` payload.
///
/// `ordinal` is the appended record's own line index — in real rollouts
/// `ordinal` equals the 0-based line number, so it is `src_lines.len()`.
/// Callers therefore pass the lines of the file this record will be
/// appended to, before appending.
pub fn build_compacted_record(
    src_lines: &[String],
    replacement_history: Vec<Value>,
) -> anyhow::Result<String> {
    if replacement_history.is_empty() {
        bail!("compacted record needs a non-empty replacement_history");
    }
    if src_lines.len() >= gobstopper_core::validation::MAX_ITEMS
        || replacement_history.len() > gobstopper_core::validation::MAX_ITEMS
        || !replacement_history.iter().all(crate::codex::supported_item)
    {
        bail!("compacted record exceeds supported dialect bounds");
    }

    // The last compacted record's payload is the envelope template; the
    // last token_usage_record payload is the freshest usage accounting.
    let mut prev: Option<Value> = None;
    let mut last_usage: Option<Value> = None;
    for line in src_lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let rec = crate::payload::decode_record(line)
            .context("compaction source contains invalid or ambiguous JSON")?;
        match rec.get("type").and_then(Value::as_str) {
            Some("compacted") => prev = rec.get("payload").cloned(),
            Some("token_usage_record") => last_usage = rec.get("payload").cloned(),
            _ => {}
        }
    }

    let seed = src_lines
        .last()
        .map(|s| s.as_bytes())
        .unwrap_or(b"gobstopper-empty");
    let (window_number, first_window_id, previous_window_id) = match &prev {
        Some(p) => {
            let number = match p.get("window_number") {
                None => 1,
                Some(value) => value
                    .as_u64()
                    .and_then(|n| n.checked_add(1))
                    .context("compaction window number is invalid or exhausted")?,
            };
            let first = p
                .get("first_window_id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| synth_uuid_v7(seed));
            // The window this record replaces is the prior record's
            // window; a malformed prior record degrades to "the first
            // window", matching the window-1 invariant.
            let previous = p
                .get("window_id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| first.clone());
            (number, first, previous)
        }
        None => {
            // First compaction: the window being replaced IS the
            // original window — observed as first == previous.
            let first = synth_uuid_v7(seed);
            (1, first.clone(), first)
        }
    };

    let payload = CompactedPayload {
        message: prev
            .as_ref()
            .and_then(|p| p.get("message"))
            .cloned()
            .unwrap_or_else(|| Value::String(String::new())),
        replacement_history,
        guardian_history: prev
            .as_ref()
            .and_then(|p| p.get("guardian_history"))
            .cloned(),
        retained_context: prev
            .as_ref()
            .and_then(|p| p.get("retained_context"))
            .cloned(),
        window_number,
        first_window_id,
        previous_window_id,
        window_id: synth_uuid_v7(seed),
        compaction_response_id: prev
            .as_ref()
            .and_then(|p| p.get("compaction_response_id"))
            .cloned()
            .unwrap_or_else(|| Value::String(synth_resp_id(seed))),
        latest_token_usage_record: last_usage
            .or_else(|| {
                prev.as_ref()
                    .and_then(|p| p.get("latest_token_usage_record"))
                    .cloned()
            })
            .unwrap_or(Value::Null),
        extra: prev
            .as_ref()
            .and_then(Value::as_object)
            .map(|obj| {
                obj.iter()
                    .filter(|(k, _)| !KNOWN_PAYLOAD_KEYS.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default(),
    };
    let record = CompactedRecord {
        timestamp: rfc3339_now(),
        ordinal: src_lines.len() as u64,
        kind: "compacted",
        payload,
    };
    serde_json::to_string(&record).context("serialize compacted record")
}

/// Append a validated compacted record to detached bytes in memory. The caller
/// must separately admit any publication; this function performs no file I/O.
pub fn append_compacted_bytes(original: &[u8], record_line: &str) -> anyhow::Result<Vec<u8>> {
    let separators = 1 + usize::from(!original.is_empty() && original.last() != Some(&b'\n'));
    if !original
        .len()
        .checked_add(record_line.len())
        .and_then(|n| n.checked_add(separators))
        .is_some_and(|n| (n as u64) <= crate::transaction::max_transcript_bytes())
    {
        bail!("compacted rollout exceeds transcript byte limit");
    }
    let rec = crate::payload::decode_record(record_line)
        .context("compacted line is not unambiguous JSON")?;
    if rec.get("type").and_then(Value::as_str) != Some("compacted") {
        bail!("refusing to append a record that is not type=compacted");
    }
    if !rec
        .get("payload")
        .and_then(|p| p.get("replacement_history"))
        .is_some_and(Value::is_array)
    {
        bail!("compacted record lacks a replacement_history array");
    }
    Ok(crate::transaction::prepare(
        gobstopper_core::Provider::Codex,
        original,
        |raw| {
            let mut candidate = raw.to_string();
            if !candidate.is_empty() && !candidate.ends_with('\n') {
                candidate.push('\n');
            }
            candidate.push_str(record_line);
            candidate.push('\n');
            Ok(candidate)
        },
    )?)
}

/// Prepare an experimental compacted record without changing any file.
/// Returns the candidate bytes and appended record ordinal. Synthesized
/// identities use the clock/counter; preserve the returned bytes for retries.
pub fn transform_with_digest(
    original: &[u8],
    digest: &DigestBlock,
    keep_tail: usize,
) -> anyhow::Result<(Vec<u8>, u64)> {
    if original.len() as u64 > crate::transaction::max_transcript_bytes() {
        bail!("rollout exceeds transcript byte limit");
    }
    crate::payload::check_digest_bounds(digest)?;
    let raw = std::str::from_utf8(original).context("rollout is not UTF-8")?;
    let lines: Vec<String> = raw.lines().map(str::to_string).collect();
    let tail = tail_response_items(&lines, keep_tail);
    let history = digest_to_replacement_history(digest, &tail);
    let line = build_compacted_record(&lines, history)?;
    let ordinal = serde_json::from_str::<Value>(&line)
        .ok()
        .and_then(|v| v.get("ordinal").and_then(Value::as_u64))
        .unwrap_or(lines.len() as u64);
    Ok((append_compacted_bytes(original, &line)?, ordinal))
}

/// Direct provider-path mutation is disabled before any file access. Use
/// [`transform_with_digest`] and an admitted no-clobber copy publisher instead.
pub fn compact_with_digest(
    _path: &Path,
    _digest: &DigestBlock,
    _keep_tail: usize,
) -> anyhow::Result<u64> {
    Err(crate::AdapterError::DirectMutationDisabled.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::{verify, Severity};
    use gobstopper_core::model::SessionHandle;
    use gobstopper_core::Provider;
    use std::path::PathBuf;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "gob-compact-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A small but shape-faithful rollout: meta, a user turn with a
    /// paired tool call, an assistant message, and usage accounting.
    fn fixture_lines() -> Vec<String> {
        [
            r#"{"timestamp":"t","ordinal":0,"type":"session_meta","payload":{"id":"s1","cwd":"/w"}}"#,
            r#"{"timestamp":"t","ordinal":1,"type":"response_item","payload":{"type":"message","id":"msg_u1","role":"user","content":[{"type":"input_text","text":"hi"}],"internal_chat_message_metadata_passthrough":{"turn_id":"turn-1","create_time":1.0,"content_item_kinds":["user.text"]}}}"#,
            r#"{"timestamp":"t","ordinal":2,"type":"response_item","payload":{"type":"function_call","id":"fc_1","name":"shell","call_id":"c1","arguments":"{}"}}"#,
            r#"{"timestamp":"t","ordinal":3,"type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"ok"}}"#,
            r#"{"timestamp":"t","ordinal":4,"type":"response_item","payload":{"type":"message","id":"msg_a1","role":"assistant","content":[{"type":"output_text","text":"done"}],"internal_chat_message_metadata_passthrough":{"turn_id":"turn-1","create_time":1.5,"content_item_kinds":["assistant.text"]}}}"#,
            r#"{"timestamp":"t","ordinal":5,"type":"token_usage_record","payload":{"thread_id":"s1","turn_id":"turn-1","usage":{"input_tokens":100,"output_tokens":5},"thread_token_usage":{"input_tokens":500}}}"#,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    fn digest() -> DigestBlock {
        DigestBlock {
            goal: Some("ship it".to_string()),
            decisions: vec!["use structs".to_string()],
            files_touched: vec!["codex_compact.rs".to_string()],
            open_tasks: vec!["test".to_string()],
            covers_items: 4,
            ..Default::default()
        }
    }

    fn payload_of(line: &str) -> Value {
        let rec: Value = serde_json::from_str(line).unwrap();
        assert_eq!(rec.get("type").and_then(Value::as_str), Some("compacted"));
        rec.get("payload").cloned().unwrap()
    }

    /// Ordered positions of envelope keys inside the serialized payload
    /// — the provider writes them in this exact sequence.
    fn key_order(line: &str, keys: &[&str]) -> Vec<usize> {
        keys.iter()
            .map(|k| {
                line.find(&format!("\"{k}\":"))
                    .unwrap_or_else(|| panic!("missing key {k} in {line}"))
            })
            .collect()
    }

    #[test]
    fn build_minimal_envelope_matches_provider_shape() {
        let lines = fixture_lines();
        let tail = tail_response_items(&lines, 2);
        let history = digest_to_replacement_history(&digest(), &tail);
        let line = build_compacted_record(&lines, history).unwrap();
        let rec: Value = serde_json::from_str(&line).unwrap();

        // Envelope: ordinal is the appended record's own line index.
        assert_eq!(rec["ordinal"], lines.len() as u64);
        assert_eq!(rec["type"], "compacted");
        assert!(rec["timestamp"].as_str().unwrap().ends_with('Z'));

        // Payload key order matches the provider's observed sequence.
        let pos = key_order(
            &line,
            &[
                "message",
                "replacement_history",
                "window_number",
                "first_window_id",
                "previous_window_id",
                "window_id",
                "compaction_response_id",
                "latest_token_usage_record",
            ],
        );
        assert!(pos.windows(2).all(|w| w[0] < w[1]), "order: {pos:?}");

        let p = rec["payload"].clone();
        assert_eq!(p["message"], "");
        assert_eq!(p["window_number"], 1);
        // First compaction: the replaced window IS the original window.
        assert_eq!(p["first_window_id"], p["previous_window_id"]);
        assert_ne!(p["window_id"], p["first_window_id"]);
        for k in ["first_window_id", "previous_window_id", "window_id"] {
            let id = p[k].as_str().unwrap();
            assert_eq!(id.len(), 36, "{k} not uuid-shaped");
            assert_eq!(id.as_bytes()[14], b'7', "{k} not v7");
        }
        assert!(p["compaction_response_id"]
            .as_str()
            .unwrap()
            .starts_with("resp_"));
        // The file's freshest usage record is embedded, not a stale one.
        let src_usage: Value = serde_json::from_str::<Value>(&lines[5]).unwrap()["payload"].clone();
        assert_eq!(p["latest_token_usage_record"], src_usage);
        // Minimal path omits the optional envelope fields.
        assert!(p.get("guardian_history").is_none());
        assert!(p.get("retained_context").is_none());
    }

    #[test]
    fn tail_never_splits_tool_pairs() {
        let lines = vec![
            serde_json::json!({
                "type": "response_item",
                "payload": {"type": "function_call", "call_id": "paired"}
            })
            .to_string(),
            serde_json::json!({
                "type": "response_item",
                "payload": {"type": "function_call_output", "call_id": "paired", "output": "ok"}
            })
            .to_string(),
            serde_json::json!({
                "type": "response_item",
                "payload": {"type": "function_call", "call_id": "open"}
            })
            .to_string(),
        ];
        let tail = tail_response_items(&lines, 3);
        assert_eq!(tail.len(), 2);
        assert!(tail.iter().all(|item| item["call_id"] == "paired"));
        assert!(tail_response_items(&lines, 1).is_empty());
    }

    #[test]
    fn replacement_history_is_digest_then_verbatim_tail() {
        let lines = fixture_lines();
        let tail = tail_response_items(&lines, 3);
        let history = digest_to_replacement_history(&digest(), &tail);
        assert_eq!(history.len(), 4);

        // First entry: user message in the real item shape carrying the
        // digest text.
        let first = &history[0];
        assert_eq!(first["type"], "message");
        assert_eq!(first["role"], "user");
        assert!(first["id"].as_str().unwrap().starts_with("msg_"));
        let blocks = first["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "input_text");
        assert!(blocks[0]["text"]
            .as_str()
            .unwrap()
            .contains("[gobstopper state card]"));
        let meta = &first["internal_chat_message_metadata_passthrough"];
        assert_eq!(meta["content_item_kinds"][0], "user.text");
        // turn_id inherited from the newest tail item that has one.
        assert_eq!(meta["turn_id"], "turn-1");

        // Tail items are verbatim payload objects.
        assert_eq!(&history[1..], &tail[..]);
    }

    #[test]
    fn chained_record_advances_window_and_clones_envelope() {
        let mut lines = fixture_lines();
        // A prior provider compaction with the full observed envelope.
        lines.push(
            r#"{"timestamp":"t","ordinal":6,"type":"compacted","payload":{"message":"","replacement_history":[{"type":"message","role":"user","content":[{"type":"input_text","text":"x"}]}],"guardian_history":[{"type":"message","role":"user","content":[]}],"retained_context":{"verified_answers":[],"incomplete":false,"user_messages":[],"user_messages_incomplete":true,"next_order":0},"window_number":3,"first_window_id":"11111111-1111-7111-8111-111111111111","previous_window_id":"22222222-2222-7222-8222-222222222222","window_id":"33333333-3333-7333-8333-333333333333","compaction_response_id":"resp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","latest_token_usage_record":null,"future_field":{"a":1}}}"#.to_string(),
        );
        let history = digest_to_replacement_history(&digest(), &[]);
        let line = build_compacted_record(&lines, history).unwrap();
        let p = payload_of(&line);

        // Window chain advanced one step, never duplicated.
        assert_eq!(p["window_number"], 4);
        assert_eq!(p["first_window_id"], "11111111-1111-7111-8111-111111111111");
        assert_eq!(
            p["previous_window_id"],
            "33333333-3333-7333-8333-333333333333"
        );
        let wid = p["window_id"].as_str().unwrap();
        assert_ne!(wid, "33333333-3333-7333-8333-333333333333");
        assert_eq!(wid.len(), 36);

        // Envelope fields cloned verbatim, including unknown extras.
        assert_eq!(p["guardian_history"][0]["type"], "message");
        assert_eq!(p["retained_context"]["next_order"], 0);
        assert_eq!(
            p["compaction_response_id"],
            "resp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(p["future_field"]["a"], 1);
        // Fresh usage from the file wins over the prior record's null.
        assert!(p["latest_token_usage_record"].is_object());
        // Key order preserved when optional fields are present.
        let pos = key_order(
            &line,
            &[
                "message",
                "replacement_history",
                "guardian_history",
                "retained_context",
                "window_number",
                "first_window_id",
                "previous_window_id",
                "window_id",
                "compaction_response_id",
                "latest_token_usage_record",
            ],
        );
        assert!(pos.windows(2).all(|w| w[0] < w[1]), "order: {pos:?}");
    }

    #[test]
    fn empty_replacement_history_rejected() {
        assert!(build_compacted_record(&fixture_lines(), vec![]).is_err());
    }

    #[test]
    fn appended_record_verifies_and_reloads() {
        let dir = TestDir::new();
        let path = dir.0.join("rollout-test.jsonl");
        let lines = fixture_lines();
        fs::write(&path, lines.join("\n") + "\n").unwrap();

        // Lower an InjectDigest edit and append as a compacted record.
        fs::write(
            &path,
            transform_with_digest(&fs::read(&path).unwrap(), &digest(), 3)
                .unwrap()
                .0,
        )
        .unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        let out_lines: Vec<&str> = raw.lines().collect();
        assert_eq!(out_lines.len(), lines.len() + 1);
        let last: Value = serde_json::from_str(out_lines.last().unwrap()).unwrap();
        assert_eq!(last["type"], "compacted");
        assert_eq!(last["ordinal"], lines.len() as u64);
        assert!(last["payload"]["replacement_history"].is_array());
        // Deliberately no forged encrypted compaction item.
        assert!(!last["payload"]["replacement_history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i.get("type").and_then(Value::as_str) == Some("compaction")));

        // verify: zero findings — no malformed_compacted, no unpaired
        // call (the c1 call+output pair travels together in the tail).
        let findings = verify(Provider::Codex, raw.as_bytes());
        assert_eq!(findings, vec![], "expected zero findings, got {findings:?}");

        // The dialect loader still enumerates items; the compacted
        // record is a ToolResult entry and usage comes from the file's
        // token_usage_record.
        let transcript = crate::codex::load(SessionHandle {
            provider: Provider::Codex,
            session_id: "s1".to_string(),
            path: path.clone(),
            cwd: None,
            age_secs: 0,
        })
        .unwrap();
        let kinds: Vec<_> = transcript.items.iter().map(|i| i.kind).collect();
        assert_eq!(kinds.len(), 5);
        assert_eq!(
            kinds.last(),
            Some(&gobstopper_core::model::ItemKind::ToolResult)
        );
        assert_eq!(transcript.items.last().unwrap().line_index, lines.len());
        assert_eq!(transcript.usage.context_tokens, 0);
        assert!(transcript.items[..4]
            .iter()
            .all(|i| i.est_tokens == 0 && i.elidable_bytes.is_none()));
        assert!(transcript.estimated_context_tokens() > 0);
    }

    #[test]
    fn append_terminates_torn_tail_and_rejects_garbage() {
        let dir = TestDir::new();
        let path = dir.0.join("rollout-torn.jsonl");
        // A file whose last line lacks a newline (torn tail write).
        fs::write(
            &path,
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\"}}",
        )
        .unwrap();

        let line = build_compacted_record(
            &read_rollout_lines(&path).unwrap(),
            digest_to_replacement_history(&digest(), &[]),
        )
        .unwrap();
        fs::write(
            &path,
            append_compacted_bytes(&fs::read(&path).unwrap(), &line).unwrap(),
        )
        .unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let out: Vec<&str> = raw.lines().collect();
        assert_eq!(out.len(), 2, "record must start on its own line");
        assert!(out.iter().all(|l| serde_json::from_str::<Value>(l).is_ok()));

        // Guard rails: refuse non-compacted or malformed lines.
        assert!(
            append_compacted_bytes(&fs::read(&path).unwrap(), "{\"type\":\"response_item\"}")
                .is_err()
        );
        assert!(append_compacted_bytes(&fs::read(&path).unwrap(), "not json").is_err());
        assert!(append_compacted_bytes(
            &fs::read(&path).unwrap(),
            "{\"type\":\"compacted\",\"payload\":{\"type\":\"compaction\"}}"
        )
        .is_err());
    }

    #[test]
    fn second_append_repeats_chain_like_provider() {
        let dir = TestDir::new();
        let path = dir.0.join("rollout-chain.jsonl");
        fs::write(&path, fixture_lines().join("\n") + "\n").unwrap();

        fs::write(
            &path,
            transform_with_digest(&fs::read(&path).unwrap(), &digest(), 2)
                .unwrap()
                .0,
        )
        .unwrap();
        fs::write(
            &path,
            transform_with_digest(&fs::read(&path).unwrap(), &digest(), 1)
                .unwrap()
                .0,
        )
        .unwrap();

        let lines = read_rollout_lines(&path).unwrap();
        let findings = verify(Provider::Codex, lines.join("\n").as_bytes());
        assert!(
            !findings.iter().any(|f| f.severity == Severity::Error),
            "errors: {findings:?}"
        );
        let p1 = payload_of(&lines[lines.len() - 2]);
        let p2 = payload_of(&lines[lines.len() - 1]);
        assert_eq!(p1["window_number"], 1);
        assert_eq!(p2["window_number"], 2);
        assert_eq!(p2["previous_window_id"], p1["window_id"]);
        assert_eq!(p2["first_window_id"], p1["first_window_id"]);
        assert_ne!(p2["window_id"], p1["window_id"]);
    }
}
