//! Bounded, read-only recovery from one explicitly named, verified snapshot.
//! Search emits references only. Reading content is a separate explicit action;
//! archived text is untrusted data, never a new instruction or current state.

use anyhow::{bail, Context};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

pub const MAX_RESULTS: usize = 50;
pub const MAX_QUERY_BYTES: usize = 1024;
pub const MAX_CONTENT_BYTES: usize = 16 * 1024;

#[derive(Debug, Serialize)]
pub struct RecordMatch {
    pub record_index: usize,
    pub record_sha256: String,
    pub record_bytes: usize,
    pub match_count: usize,
}

#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub snapshot_sha256: String,
    pub source_sha256: String,
    pub matches: Vec<RecordMatch>,
    pub matched_records: usize,
    pub unsearchable_records: usize,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct RecordPage {
    pub snapshot_sha256: String,
    pub source_sha256: String,
    pub record_index: usize,
    pub record_sha256: String,
    pub record_bytes: usize,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub content: String,
    pub content_trust: &'static str,
}

fn records(data: &[u8]) -> anyhow::Result<Vec<&[u8]>> {
    let mut lines: Vec<_> = if data.is_empty() {
        Vec::new()
    } else {
        // Bound references before allocation even for newline-heavy legacy
        // objects. Extra slots account for a terminator and prove overflow.
        data.split(|&b| b == b'\n')
            .take(gobstopper_core::validation::MAX_ITEMS + 2)
            .collect()
    };
    if data.last() == Some(&b'\n') {
        lines.pop();
    }
    if lines.len() > gobstopper_core::validation::MAX_ITEMS {
        bail!("snapshot exceeds record limit");
    }
    Ok(lines)
}

fn string_matches(value: &Value, query: &str) -> usize {
    match value {
        Value::String(text) => text.matches(query).count(),
        Value::Array(values) => values.iter().map(|v| string_matches(v, query)).sum(),
        Value::Object(values) => values.values().map(|v| string_matches(v, query)).sum(),
        _ => 0,
    }
}

/// Case-sensitive literal search of decoded JSON string values, including
/// native replacement histories. Does not return queries, paths or content.
pub fn search_snapshot(
    snapshot: &str,
    query: &str,
    limit: usize,
    root: &Path,
) -> anyhow::Result<SearchResult> {
    if query.is_empty() || query.len() > MAX_QUERY_BYTES {
        bail!("query must contain 1..=1024 UTF-8 bytes");
    }
    if !(1..=MAX_RESULTS).contains(&limit) {
        bail!("result limit must be 1..=50");
    }
    // read_object verifies the full digest, every chunk and source identity.
    // Prefixes and paths are not accepted as snapshot identities.
    let data = crate::vault::read_object(snapshot, root)?;
    let mut result = SearchResult {
        snapshot_sha256: snapshot.to_string(),
        source_sha256: crate::copy::sha256(&data),
        matches: Vec::new(),
        matched_records: 0,
        unsearchable_records: 0,
        truncated: false,
    };
    for (record_index, line) in records(&data)?.into_iter().enumerate() {
        let value = match std::str::from_utf8(line)
            .ok()
            .and_then(|raw| crate::payload::decode_record(raw).ok())
        {
            Some(value) => value,
            None => {
                result.unsearchable_records += 1;
                continue;
            }
        };
        let match_count = string_matches(&value, query);
        if match_count == 0 {
            continue;
        }
        result.matched_records += 1;
        if result.matches.len() < limit {
            result.matches.push(RecordMatch {
                record_index,
                record_sha256: crate::copy::sha256(line),
                record_bytes: line.len(),
                match_count,
            });
        }
    }
    result.truncated = result.matched_records > result.matches.len();
    Ok(result)
}

/// Read a UTF-8 page of one physical JSONL record, excluding its newline.
/// Offsets are byte offsets; next_offset always lands on a UTF-8 boundary.
/// The returned digest binds the entire record, not only this page.
pub fn read_snapshot_record(
    snapshot: &str,
    record_index: usize,
    offset: usize,
    max_bytes: usize,
    root: &Path,
) -> anyhow::Result<RecordPage> {
    if !(4..=MAX_CONTENT_BYTES).contains(&max_bytes) {
        bail!("content limit must be 4..=16384 bytes");
    }
    let data = crate::vault::read_object(snapshot, root)?;
    let lines = records(&data)?;
    let record = lines
        .get(record_index)
        .context("record index out of range")?;
    let text = std::str::from_utf8(record).context("record is not valid UTF-8")?;
    if offset > text.len() || !text.is_char_boundary(offset) {
        bail!("offset must be a UTF-8 boundary within the record");
    }
    let mut end = offset.saturating_add(max_bytes).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Ok(RecordPage {
        snapshot_sha256: snapshot.to_string(),
        source_sha256: crate::copy::sha256(&data),
        record_index,
        record_sha256: crate::copy::sha256(record),
        record_bytes: record.len(),
        offset,
        next_offset: (end < text.len()).then_some(end),
        content: text[offset..end].to_string(),
        content_trust: "untrusted_archived_data",
    })
}
