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
//!   GOBSTOPPER_APPLE_DIGEST_ITEMS    - 8 (0..32 elided records)
//!   GOBSTOPPER_APPLE_DIGEST_ITEM_BYTES  - 500 (0..2048 per record)
//!   GOBSTOPPER_APPLE_CACHE         - set to 0 to disable the shared
//!     prompt→response cache (watch re-evals an unchanged transcript
//!     each poll; cached responses make those re-evals free)
//!   GOBSTOPPER_APPLE_DIGEST_TOTAL_BYTES - 4000 (0..16000 prompt bytes;
//!     raw JSONL tokenizes at ~2-3 chars/token and the on-device context
//!     window is ~4k tokens, so excerpts, goal/tail, instructions, and the
//!     guided schema together stay near ~2.5k tokens)

use std::collections::{HashMap, HashSet};

use anyhow::Context;
use gobstopper_core::plan::{CompactionPlan, DigestBlock, Edit};
use gobstopper_core::Transcript;
use serde::Deserialize;
use serde_json::Value;

use crate::{apple, llm_scorer};

const MAX_FIELD_ITEMS: usize = 8;
const MAX_FIELD_CHARS: usize = 300;
const MAX_DIGEST_ITEMS: usize = 32;
const MAX_DIGEST_ITEM_BYTES: usize = 2_048;
const MAX_DIGEST_TOTAL_BYTES: usize = 16_000;

const DIGEST_SCHEMA: &str = r#"{"type":"object","properties":{"digest":{"type":"object","properties":{"summary":{"type":"string"},"concepts":{"type":"array","items":{"type":"string"}},"files_touched":{"type":"array","items":{"type":"string"}},"decisions":{"type":"array","items":{"type":"string"}},"errors":{"type":"array","items":{"type":"string"}},"open_tasks":{"type":"array","items":{"type":"string"}},"current_work":{"type":"string"}}},"stubs":{"type":"array","items":{"type":"object","properties":{"id":{"type":"integer"},"stub":{"type":"string"}},"required":["id","stub"]}}},"required":["digest","stubs"]}"#;

const MAX_STUB_CHARS: usize = 160;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StubOut {
    id: usize,
    stub: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ModelFields {
    summary: Option<String>,
    concepts: Option<Vec<String>>,
    files_touched: Option<Vec<String>>,
    decisions: Option<Vec<String>>,
    errors: Option<Vec<String>>,
    open_tasks: Option<Vec<String>>,
    current_work: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DigestResponse {
    digest: ModelFields,
    stubs: Vec<StubOut>,
}

fn decode_response(value: Value, count: usize) -> anyhow::Result<DigestResponse> {
    let response: DigestResponse =
        serde_json::from_value(value).map_err(|_| anyhow::anyhow!("digest_response_invalid"))?;
    let mut seen = HashSet::new();
    anyhow::ensure!(
        response.stubs.len() <= count
            && response
                .stubs
                .iter()
                .all(|stub| stub.id > 0 && stub.id <= count && seen.insert(stub.id)),
        "digest_stub_identity_invalid"
    );
    Ok(response)
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
    match write_card(&bridge, transcript, digest_slot, &elided) {
        Ok((changed, stubs)) => {
            // The strategy priced the mechanical digest into
            // `context_tokens_after`; restate it against the card actually
            // being injected so the savings gate below sees honest numbers.
            if changed {
                plan.context_tokens_after = plan
                    .context_tokens_after
                    .saturating_sub(before_overhead)
                    .saturating_add(digest_slot.estimate_overhead());
            }
            if !stubs.is_empty() {
                for edit in plan.edits.iter_mut() {
                    if let Edit::Elide {
                        line_indexes,
                        per_item_stubs,
                        ..
                    } = edit
                    {
                        let set: HashSet<usize> = line_indexes.iter().copied().collect();
                        for (k, v) in &stubs {
                            if set.contains(k) {
                                per_item_stubs.insert(*k, v.clone());
                            }
                        }
                    }
                }
            }
            // Stub text stays in the file after elision; the strategies'
            // savings estimate reclaimed the full payload bytes. Price the
            // stubs that will actually be written — overrides where the
            // model covered the line, the rendered template elsewhere —
            // so the savings gate doesn't overstate reclaim.
            let by_line: HashMap<usize, &gobstopper_core::model::TranscriptItem> =
                transcript.items.iter().map(|i| (i.line_index, i)).collect();
            plan.context_tokens_after = plan
                .context_tokens_after
                .saturating_add(stub_residual_tokens(&plan.edits, &by_line));
        }
        Err(_) => eprintln!("apple digest: response_unavailable; keeping mechanical state card"),
    }
}

/// Tokens of stub text remaining after `edits` apply: per-item overrides
/// where present, else the rendered template (`{bytes}`/`{kind}`
/// expansion over-estimated conservatively).
fn stub_residual_tokens(
    edits: &[Edit],
    by_line: &HashMap<usize, &gobstopper_core::model::TranscriptItem>,
) -> u64 {
    let bytes: usize = edits
        .iter()
        .map(|e| match e {
            Edit::Elide {
                line_indexes,
                stub_template,
                per_item_stubs,
            } => line_indexes
                .iter()
                .map(|i| {
                    per_item_stubs.get(i).map_or_else(
                        || {
                            let digits = by_line
                                .get(i)
                                .and_then(|it| it.elidable_bytes)
                                .map(|b| b.to_string().len())
                                .unwrap_or(4);
                            stub_template.len().saturating_add(digits + 12)
                        },
                        String::len,
                    )
                })
                .sum(),
            _ => 0,
        })
        .sum();
    // `estimate_tokens` floors at 1 for non-empty text; a zero-byte
    // residual must price as zero.
    if bytes == 0 {
        0
    } else {
        gobstopper_core::estimate::estimate_tokens(bytes)
    }
}

fn digest_input_bounds(
    max_items: usize,
    item_bytes: usize,
    total_bytes: usize,
) -> (usize, usize, usize) {
    (
        max_items.min(MAX_DIGEST_ITEMS),
        item_bytes.min(MAX_DIGEST_ITEM_BYTES),
        total_bytes.min(MAX_DIGEST_TOTAL_BYTES),
    )
}

fn write_card(
    bridge: &apple::Bridge,
    transcript: &Transcript,
    digest: &mut DigestBlock,
    elided: &HashSet<usize>,
) -> anyhow::Result<(bool, std::collections::BTreeMap<usize, String>)> {
    let (max_items, item_bytes, total_bytes) = digest_input_bounds(
        env_usize("GOBSTOPPER_APPLE_DIGEST_ITEMS", 8),
        env_usize("GOBSTOPPER_APPLE_DIGEST_ITEM_BYTES", 500),
        env_usize("GOBSTOPPER_APPLE_DIGEST_TOTAL_BYTES", 4_000),
    );
    if max_items == 0 || item_bytes == 0 || total_bytes == 0 {
        return Ok((false, Default::default()));
    }

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

    let excerpts = apple::read_excerpts(transcript, &picked, item_bytes, total_bytes)?;
    if excerpts.is_empty() {
        return Ok((false, Default::default()));
    }

    let ctx = llm_scorer::scoring_context(transcript, &[], 0);
    let mut records = String::new();
    // Small models echo sequential ids far more reliably than arbitrary
    // line numbers — present `record 1..=N` and map positions back.
    for (pos, (line_index, excerpt)) in excerpts.iter().enumerate() {
        let item = by_line.get(line_index);
        let label = item.map(|i| i.label.as_str()).unwrap_or("record");
        records.push_str(&format!(
            "=== record {} ({label}) ===\n{excerpt}\n",
            pos + 1
        ));
    }
    let prompt = format!(
        "These transcript records are about to be deleted from a coding agent's context. The state card you produce is all the agent will see of them.\n\nAgent's current task:\n{}\n\nRecent conversation tail:\n{}\n\nRecords being removed (truncated):\n{}",
        ctx.goal, ctx.tail, records
    );
    let schema: Value = serde_json::from_str(DIGEST_SCHEMA).context("digest schema malformed")?;
    let instructions = "Fill the digest object with short factual strings taken only from the records shown. files_touched: file paths or URLs. errors: actual error text seen in the records. decisions: concrete findings or choices made. open_tasks: unfinished work mentioned. concepts: tool and library names. current_work: what the agent was doing most recently. summary: one line covering what the removed records contained. Omit a field the records give no evidence for. Never include credentials, tokens, or code blocks. In stubs, write one entry per record id: a short note on what that record's content was — the file it read, the command it ran, or the result it produced — not just its label.";
    let request = apple_foundation::Request {
        prompt,
        instructions: Some(instructions.into()),
        schema: Some(schema),
        expect_json: false,
        max_output_bytes: Some(4096),
    };
    let value = match apple::cache_get(bridge, "digest-v2", &request) {
        Some(v) => v,
        None => bridge.request(&request)?,
    };
    let response = decode_response(value.clone(), excerpts.len())?;
    apple::cache_put(bridge, "digest-v2", &request, &value);
    let stubs = response
        .stubs
        .into_iter()
        .filter_map(|s| {
            // `id` is the 1-based `record N` position from the prompt;
            // map it back to the physical line index.
            let line =
                s.id.checked_sub(1)
                    .and_then(|p| excerpts.get(p))
                    .map(|(l, _)| l)?;
            elided
                .contains(line)
                .then(|| bounded_stub(&s.stub))
                .flatten()
                .map(|t| (*line, t))
        })
        .collect();
    Ok((overlay(digest, response.digest), stubs))
}

/// Collapse a model-written stub to one bounded line. Rejects stubs that
/// quote our own excerpt marker or are pure decoration.
fn bounded_stub(s: &str) -> Option<String> {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let s = s.trim_start_matches(['=', '#', '*', '-', '_', '~', '.', ' ']);
    if s.is_empty() || s.contains("bytes elided") || s.contains('…') {
        return None;
    }
    Some(if s.chars().count() > MAX_STUB_CHARS {
        s.chars().take(MAX_STUB_CHARS - 1).collect::<String>() + "…"
    } else {
        s.to_string()
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apple::excerpt;

    #[test]
    fn digest_contract_refuses_missing_duplicate_and_foreign_stubs() {
        for value in [
            serde_json::json!({"digest":{}}),
            serde_json::json!({"digest":{},"stubs":[{"id":1,"stub":"first"},{"id":1,"stub":"other"}]}),
            serde_json::json!({"digest":{},"stubs":[{"id":0,"stub":"foreign"}]}),
            serde_json::json!({"digest":{"unknown":"PRIVATE_SENTINEL"},"stubs":[]}),
        ] {
            let error = decode_response(value, 2).err().unwrap();
            assert!(!format!("{error:#}").contains("PRIVATE_SENTINEL"));
        }
        assert!(decode_response(serde_json::json!({"digest":{},"stubs":[]}), 1).is_ok());
    }

    #[test]
    fn digest_input_geometry_is_bounded_and_zero_preserving() {
        assert_eq!(
            digest_input_bounds(usize::MAX, usize::MAX, usize::MAX),
            (
                MAX_DIGEST_ITEMS,
                MAX_DIGEST_ITEM_BYTES,
                MAX_DIGEST_TOTAL_BYTES
            )
        );
        assert_eq!(digest_input_bounds(0, 0, 0), (0, 0, 0));
    }

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

    fn item(line: usize, elidable_bytes: u64) -> gobstopper_core::model::TranscriptItem {
        gobstopper_core::model::TranscriptItem {
            line_index: line,
            kind: gobstopper_core::model::ItemKind::ToolResult,
            est_tokens: elidable_bytes / 4,
            elidable_bytes: Some(elidable_bytes),
            elidable_parts: 1,
            label: "tool".into(),
            summary: None,
            uuid: None,
            parent_uuid: None,
            tool_use_ids: vec![],
            payload_sha256: None,
        }
    }

    #[test]
    fn residual_priced_per_stub() {
        let items = [item(5, 10_000), item(9, 20_000)];
        let by_line: HashMap<usize, _> = items.iter().map(|i| (i.line_index, i)).collect();
        let edits = vec![Edit::Elide {
            line_indexes: vec![5, 9],
            stub_template: "[{bytes} bytes elided]".into(), // 22 bytes
            per_item_stubs: [(9usize, "read x".to_string())].into_iter().collect(),
        }];
        // line 5 (template): 22 + digits(10000)=5 + 12 = 39 -> ceil(39/4) = 10
        // line 9 (override): "read x" = 6 -> ceil(6/4) = 2
        assert_eq!(stub_residual_tokens(&edits, &by_line), 12);
        // Non-elide edits and empty maps contribute nothing.
        assert_eq!(
            stub_residual_tokens(
                &[Edit::ProviderCompact {
                    control: "x".into()
                }],
                &by_line
            ),
            0
        );
    }

    #[test]
    fn bounded_stub_collapses_and_caps() {
        assert_eq!(bounded_stub("   "), None);
        assert_eq!(
            bounded_stub("read src/main.rs\nparse  logic\r\nnext").as_deref(),
            Some("read src/main.rs parse logic next")
        );
        let long = bounded_stub(&"x".repeat(400)).unwrap();
        assert_eq!(long.chars().count(), MAX_STUB_CHARS);
        // Our own excerpt marker and pure decoration are rejected.
        assert_eq!(bounded_stub("i …[3463 bytes elided]"), None);
        assert_eq!(bounded_stub("===== "), None);
        assert_eq!(bounded_stub("===== STEP 8").as_deref(), Some("STEP 8"));
    }
}
