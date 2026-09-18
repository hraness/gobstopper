//! Apple on-device state-card writer.
//!
//! Strategies inject a mechanically-extracted `DigestBlock` (keyword
//! heuristics over sanitized labels). With `GOBSTOPPER_DIGEST=apple` the
//! plan's digest is rewritten by the local model instead: it reads bounded
//! excerpts of the records being elided and emits the same structured
//! fields through guided generation. On-device means the excerpts never
//! leave the machine — the privacy boundary that keeps the remote llm
//! scorer on labels-only does not apply here.
//!
//! The upgrade is strictly additive: the model digest is overlaid field by
//! field onto the mechanical one, capped at a fixed token overhead (the
//! plan's `context_tokens_after` is restated honestly against the card
//! actually injected), and any failure leaves the mechanical digest in
//! place.
//!
//! Configuration (all optional, defaults listed):
//!   GOBSTOPPER_DIGEST                - "apple" enables; unset = mechanical
//!   GOBSTOPPER_APPLE_DIGEST_ITEMS    - 8 (max elided records excerpted)
//!   GOBSTOPPER_APPLE_DIGEST_ITEM_BYTES  - 500 (per-record excerpt cap)
//!   GOBSTOPPER_APPLE_DIGEST_TOTAL_BYTES - 4000 (prompt excerpt budget;
//!     raw JSONL tokenizes at ~2-3 chars/token and the on-device context
//!     window is ~4k tokens, so excerpts, goal/tail, instructions, and the
//!     guided schema together stay near ~2.5k tokens)

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};

use anyhow::Context;
use gobstopper_core::plan::{CompactionPlan, DigestBlock, Edit};
use gobstopper_core::Transcript;
use serde::Deserialize;
use serde_json::Value;

use crate::{apple, llm_scorer};

const MAX_FIELD_ITEMS: usize = 8;
const MAX_FIELD_CHARS: usize = 300;

const DIGEST_SCHEMA: &str = r#"{"type":"object","properties":{"digest":{"type":"object","properties":{"summary":{"type":"string"},"concepts":{"type":"array","items":{"type":"string"}},"files_touched":{"type":"array","items":{"type":"string"}},"decisions":{"type":"array","items":{"type":"string"}},"errors":{"type":"array","items":{"type":"string"}},"open_tasks":{"type":"array","items":{"type":"string"}},"current_work":{"type":"string"}}}},"required":["digest"]}"#;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ModelFields {
    summary: Option<String>,
    concepts: Option<Vec<String>>,
    files_touched: Option<Vec<String>>,
    decisions: Option<Vec<String>>,
    errors: Option<Vec<String>>,
    open_tasks: Option<Vec<String>>,
    current_work: Option<String>,
}

/// Rewrite `plan`'s `InjectDigest` digest with model-extracted fields when
/// `GOBSTOPPER_DIGEST=apple` and the bridge is available. No-op otherwise.
pub fn maybe_upgrade(plan: &mut CompactionPlan, transcript: &Transcript) {
    if std::env::var("GOBSTOPPER_DIGEST").as_deref() != Ok("apple") || !cfg!(target_os = "macos") {
        return;
    }
    let elided: HashSet<usize> = plan
        .edits
        .iter()
        .flat_map(|e| match e {
            Edit::Elide { line_indexes, .. } => line_indexes.clone(),
            _ => Vec::new(),
        })
        .collect();
    let Some(digest_slot) = plan.edits.iter_mut().find_map(|e| match e {
        Edit::InjectDigest { digest } => Some(digest),
        _ => None,
    }) else {
        return;
    };
    if elided.is_empty() {
        return;
    }
    let Some(bridge_path) = apple::resolve_bridge() else {
        return;
    };
    if !apple::available(&bridge_path) {
        return;
    }
    let Some(bridge) = apple::shared_bridge(&bridge_path, apple::timeout_ms()) else {
        return;
    };
    let before_overhead = digest_slot.estimate_overhead();
    match write_digest(bridge, transcript, digest_slot, &elided) {
        // The strategy priced the mechanical digest into
        // `context_tokens_after`; restate it against the card actually
        // being injected so the savings gate below sees honest numbers.
        Ok(true) => {
            plan.context_tokens_after = plan
                .context_tokens_after
                .saturating_sub(before_overhead)
                .saturating_add(digest_slot.estimate_overhead());
        }
        Ok(false) => {}
        Err(e) => eprintln!("apple digest: keeping mechanical state card ({e:#})"),
    }
}

fn write_digest(
    bridge: &apple_foundation::Bridge,
    transcript: &Transcript,
    digest: &mut DigestBlock,
    elided: &HashSet<usize>,
) -> anyhow::Result<bool> {
    let max_items = env_usize("GOBSTOPPER_APPLE_DIGEST_ITEMS", 8);
    let item_bytes = env_usize("GOBSTOPPER_APPLE_DIGEST_ITEM_BYTES", 500);
    let total_bytes = env_usize("GOBSTOPPER_APPLE_DIGEST_TOTAL_BYTES", 4_000);

    let by_line: HashMap<usize, &gobstopper_core::model::TranscriptItem> =
        transcript.items.iter().map(|i| (i.line_index, i)).collect();
    // Prefer the largest elided records — they carry the most content to
    // lose — then present them in transcript order for a readable prompt.
    let mut picked: Vec<usize> = elided.iter().copied().collect();
    picked.sort_by_key(|l| {
        std::cmp::Reverse(by_line.get(l).and_then(|i| i.elidable_bytes).unwrap_or(0))
    });
    picked.truncate(max_items);
    picked.sort_unstable();

    let excerpts = read_excerpts(&transcript.session.path, &picked, item_bytes, total_bytes)?;
    if excerpts.is_empty() {
        return Ok(false);
    }

    let ctx = llm_scorer::scoring_context(transcript, &[], 0);
    let mut records = String::new();
    for (line_index, excerpt) in &excerpts {
        let item = by_line.get(line_index);
        let label = item.map(|i| i.label.as_str()).unwrap_or("record");
        records.push_str(&format!(
            "=== record {line_index} ({label}) ===\n{excerpt}\n"
        ));
    }
    let prompt = format!(
        "These transcript records are about to be deleted from a coding agent's context. The state card you produce is all the agent will see of them.\n\nAgent's current task:\n{}\n\nRecent conversation tail:\n{}\n\nRecords being removed (truncated):\n{}",
        ctx.goal, ctx.tail, records
    );
    let schema: Value = serde_json::from_str(DIGEST_SCHEMA).unwrap();
    let value = bridge.request(&apple_foundation::Request {
        prompt,
        instructions: Some(
            "Fill the digest object with short factual strings. files_touched: file paths or URLs. errors: failing commands, exit codes, exceptions. decisions: concrete findings or choices made. open_tasks: unfinished work mentioned. concepts: tool and library names. current_work: what the agent was doing most recently. summary: one line covering what the removed records contained. Omit a field the records give no evidence for. Never include credentials, tokens, or code blocks.".into(),
        ),
        schema: Some(schema),
        expect_json: false,
        max_output_bytes: Some(4096),
    })?;
    let fields: ModelFields = serde_json::from_value(
        value
            .get("digest")
            .cloned()
            .context("apple response missing `digest` object")?,
    )
    .context("apple response `digest` had invalid shape")?;

    Ok(overlay(digest, fields))
}

/// Replace each digest field with the model's version when it produced
/// one, bounded to the mechanical field sizes, then shrink or reject the
/// whole card until it fits the token overhead the plan already priced.
fn overlay(mechanical: &mut DigestBlock, model: ModelFields) -> bool {
    let mut d = mechanical.clone();
    if let Some(s) = bounded_string(model.summary) {
        d.summary = Some(s);
    }
    if let Some(v) = bounded_list(model.concepts) {
        d.concepts = v;
    }
    if let Some(v) = bounded_list(model.files_touched) {
        d.files_touched = v;
    }
    if let Some(v) = bounded_list(model.decisions) {
        d.decisions = v;
    }
    if let Some(v) = bounded_list(model.errors) {
        d.errors = v;
    }
    if let Some(v) = bounded_list(model.open_tasks) {
        d.open_tasks = v;
    }
    if let Some(s) = bounded_string(model.current_work) {
        d.current_work = Some(s);
    }
    if d == *mechanical {
        return false;
    }
    // Cap the card's token overhead well under the 4096-token state-card
    // reserve the strategies budget for; shed model overrides
    // least-valuable-first by reverting fields to the mechanical values.
    const MAX_OVERHEAD_TOKENS: u64 = 512;
    while d.estimate_overhead() > MAX_OVERHEAD_TOKENS {
        if d.concepts != mechanical.concepts {
            d.concepts = mechanical.concepts.clone();
        } else if d.open_tasks != mechanical.open_tasks {
            d.open_tasks = mechanical.open_tasks.clone();
        } else if d.current_work != mechanical.current_work {
            d.current_work = mechanical.current_work.clone();
        } else if d.files_touched != mechanical.files_touched {
            d.files_touched = mechanical.files_touched.clone();
        } else if d.decisions != mechanical.decisions {
            d.decisions = mechanical.decisions.clone();
        } else if d.errors != mechanical.errors {
            d.errors = mechanical.errors.clone();
        } else if d.summary != mechanical.summary {
            d.summary = mechanical.summary.clone();
        } else {
            return false;
        }
    }
    if d == *mechanical {
        return false;
    }
    *mechanical = d;
    true
}

fn bounded_string(s: Option<String>) -> Option<String> {
    let s = s?.trim().to_string();
    if s.is_empty() {
        return None;
    }
    Some(if s.chars().count() > MAX_FIELD_CHARS {
        s.chars().take(MAX_FIELD_CHARS - 1).collect::<String>() + "…"
    } else {
        s
    })
}

fn bounded_list(v: Option<Vec<String>>) -> Option<Vec<String>> {
    let v: Vec<String> = v?
        .into_iter()
        .filter_map(|s| bounded_string(Some(s)))
        .take(MAX_FIELD_ITEMS)
        .collect();
    (!v.is_empty()).then_some(v)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// Head+tail window of a record: errors tend to sit at the end of tool
/// output, so the tail is kept alongside the opening context.
fn excerpt(line: &str, max_bytes: usize) -> String {
    if line.len() <= max_bytes {
        return line.to_string();
    }
    let head = (max_bytes * 3) / 4;
    let tail = max_bytes - head;
    format!(
        "{} …[{} bytes elided]… {}",
        head_bytes(line, head),
        line.len(),
        tail_bytes(line, tail)
    )
}

fn head_bytes(s: &str, n: usize) -> &str {
    let mut end = s.len().min(n);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn tail_bytes(s: &str, n: usize) -> &str {
    let mut start = s.len().saturating_sub(n);
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// Pull the given JSONL line numbers out of the transcript file, each
/// bounded to `item_bytes`, until the excerpt budget is spent.
fn read_excerpts(
    path: &std::path::Path,
    wanted: &[usize],
    item_bytes: usize,
    total_bytes: usize,
) -> anyhow::Result<Vec<(usize, String)>> {
    let file =
        std::fs::File::open(path).with_context(|| format!("open transcript {}", path.display()))?;
    let wanted: HashSet<usize> = wanted.iter().copied().collect();
    let last = wanted.iter().copied().max().unwrap_or(0);
    let mut out = Vec::new();
    let mut budget = total_bytes;
    let mut line = String::new();
    let mut reader = BufReader::new(file);
    let mut idx = 0usize;
    while idx <= last && reader.read_line(&mut line)? > 0 {
        if wanted.contains(&idx) && budget > 256 {
            let e = excerpt(line.trim_end(), item_bytes.min(budget));
            budget = budget.saturating_sub(e.len());
            out.push((idx, e));
        }
        line.clear();
        idx += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_windows_head_and_tail() {
        let line = format!("{}TAIL-ERROR{}", "x".repeat(2000), "z".repeat(50));
        let e = excerpt(&line, 400);
        assert!(e.len() < 500);
        assert!(e.contains("TAIL-ERROR"));
        assert!(e.contains("bytes elided"));
    }

    #[test]
    fn excerpt_char_boundary_safe() {
        let line = "é".repeat(500); // 2 bytes per char
        let e = excerpt(&line, 101);
        assert!(e.len() <= 140);
    }

    #[test]
    fn overlay_replaces_and_bounds_fields() {
        let mut mech = DigestBlock {
            summary: Some("mech summary".into()),
            concepts: vec!["bash".into()],
            files_touched: vec![],
            decisions: vec!["mech decision".into()],
            errors: vec!["exit code 1".into()],
            open_tasks: vec![],
            current_work: None,
            goal: Some("goal".into()),
            context: Some("provider: codex".into()),
            covers_items: 3,
        };
        let model = ModelFields {
            summary: Some("  model summary  ".into()),
            files_touched: Some(vec!["src/a.rs".into(), " ".into(), "src/b.rs".into()]),
            errors: Some(vec!["x".repeat(400)]),
            ..Default::default()
        };
        assert!(overlay(&mut mech, model));
        assert_eq!(mech.summary.as_deref(), Some("model summary"));
        assert_eq!(mech.files_touched, vec!["src/a.rs", "src/b.rs"]);
        assert_eq!(mech.errors[0].chars().count(), MAX_FIELD_CHARS);
        assert_eq!(mech.decisions, vec!["mech decision"]); // untouched field kept
        assert_eq!(mech.concepts, vec!["bash"]);
        assert!(mech.estimate_overhead() > 0);
    }

    #[test]
    fn overlay_sheds_fields_to_fit_cap() {
        let mut mech = DigestBlock {
            errors: vec!["tiny".into()],
            covers_items: 1,
            ..Default::default()
        };
        let model = ModelFields {
            // Every field far beyond what fits; shedding must revert the
            // least valuable until the card is under the overhead cap.
            summary: Some("s".repeat(300)),
            concepts: Some(vec!["c".repeat(300); 8]),
            files_touched: Some(vec!["f".repeat(300); 8]),
            decisions: Some(vec!["d".repeat(300); 8]),
            errors: Some(vec!["e".repeat(300); 8]),
            open_tasks: Some(vec!["o".repeat(300); 8]),
            current_work: Some("w".repeat(300)),
        };
        assert!(overlay(&mut mech, model));
        assert!(mech.estimate_overhead() <= 512);
        assert_eq!(mech.errors, vec!["tiny"]); // mechanical field preserved
        assert_eq!(mech.summary.as_ref().map(|s| s.chars().count()), Some(300)); // model's kept
        assert!(mech.concepts.is_empty() && mech.decisions.is_empty());
    }
}
