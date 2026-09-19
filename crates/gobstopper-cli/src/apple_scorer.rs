//! Apple on-device scorer driver for the `scored` strategy.
//!
//! Uses the shared `apple-foundation` bridge (Apple Intelligence Foundation
//! Models) for free, private, local inference — cheaper than any hosted
//! endpoint because it never leaves the device. Guided generation constrains
//! output to `{scores:[{id,keep_probability}]}`, which is far more robust
//! than parsing free text from a small model. Only sanitized labels and
//! summaries are sent — full tool output never reaches the model.
//!
//! Requests are serialized through one persistent bridge process: the
//! on-device model is serial anyway, so batches queue in-process rather
//! than fanning out. The bridge is reused across scoring rounds (model
//! warm-up paid once), killed and respawned on timeout, and items the
//! model leaves unanswered keep their deterministic heuristic score.
//!
//! Request economy: identical candidate lines in one pass are scored
//! once and the answer fans out to every item that produced the line,
//! so repeated tool outputs cost one generation instead of one each.
//!
//! Configuration (all optional, defaults listed):
//!   GOBSTOPPER_APPLE_BRIDGE      - env → sibling of the gobstopper binary →
//!                                  ~/.local/share/gobstopper/apple-bridge
//!                                  (auto-built via swiftc when absent)
//!   GOBSTOPPER_APPLE_TIMEOUT_MS  - 180000 (first request pays model warm-up)
//!   GOBSTOPPER_APPLE_MAX_CANDIDATES - 64
//!   GOBSTOPPER_APPLE_BATCH_SIZE  - 32 (8 when content excerpts are on)
//!   GOBSTOPPER_APPLE_MAX_BATCHES - 4 (8 when content excerpts are on)
//!   GOBSTOPPER_APPLE_CACHE         - set to 0 to disable the shared
//!     prompt→response cache (identical batches under watch re-evals
//!     cost zero model calls)
//!   GOBSTOPPER_APPLE_CONTENT_BYTES - 400 per candidate (0 = labels only;
//!     on-device inference lifts the labels-only boundary remote scorers
//!     need, so candidates include bounded payload excerpts by default)

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use gobstopper_core::{HeuristicScorer, ScoreDriver, ScoredItem, Transcript};
use serde_json::Value;

use crate::{apple, llm_scorer};

#[derive(Debug, Clone)]
pub struct AppleConfig {
    pub bridge: PathBuf,
    pub timeout_ms: u64,
    pub max_candidates: usize,
    pub batch_size: usize,
    pub max_batches: usize,
    pub content_bytes: usize,
}

impl AppleConfig {
    pub fn resolve() -> Option<Self> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let bridge = apple::resolve_bridge()?;
        if !apple::available(&bridge) {
            return None;
        }
        let content_bytes = env_usize("GOBSTOPPER_APPLE_CONTENT_BYTES", 400);
        Some(Self {
            bridge,
            timeout_ms: apple::timeout_ms(),
            max_candidates: env_usize("GOBSTOPPER_APPLE_MAX_CANDIDATES", 64),
            batch_size: env_usize(
                "GOBSTOPPER_APPLE_BATCH_SIZE",
                if content_bytes > 0 { 8 } else { 32 },
            ),
            max_batches: env_usize(
                "GOBSTOPPER_APPLE_MAX_BATCHES",
                if content_bytes > 0 { 8 } else { 4 },
            ),
            content_bytes,
        })
    }
}

const SCORES_SCHEMA: &str = r#"{"type":"object","properties":{"scores":{"type":"array","items":{"type":"object","properties":{"id":{"type":"integer"},"keep_probability":{"type":"number"}},"required":["id","keep_probability"]}}},"required":["scores"]}"#;

pub struct AppleScorer {
    cfg: AppleConfig,
}

impl AppleScorer {
    pub fn new(cfg: AppleConfig) -> Self {
        Self { cfg }
    }

    /// Append a bounded excerpt of each candidate's backing record to its
    /// scoring line (` :: ` separator). Excerpt failures degrade to
    /// labels-only for that item — scoring never fails on content reads.
    fn with_content(
        &self,
        transcript: &Transcript,
        inputs: Vec<(usize, usize, String)>,
    ) -> Vec<(usize, usize, String)> {
        if self.cfg.content_bytes == 0 {
            return inputs;
        }
        let line_indexes: Vec<usize> = inputs
            .iter()
            .filter_map(|(_, idx, _)| transcript.items.get(*idx).map(|i| i.line_index))
            .collect();
        let total = self.cfg.content_bytes.saturating_mul(inputs.len());
        let Ok(excerpts) = apple::read_excerpts(
            &transcript.session.path,
            &line_indexes,
            self.cfg.content_bytes,
            total,
        ) else {
            return inputs;
        };
        let by_line: std::collections::HashMap<usize, String> = excerpts.into_iter().collect();
        inputs
            .into_iter()
            .map(|(local, idx, text)| {
                let line = transcript.items.get(idx).map(|i| i.line_index);
                match line.and_then(|l| by_line.get(&l)) {
                    Some(e) => (local, idx, format!("{text} :: {e}")),
                    None => (local, idx, text),
                }
            })
            .collect()
    }
}

impl ScoreDriver for AppleScorer {
    fn score(&self, transcript: &Transcript, candidates: &[usize]) -> Vec<ScoredItem> {
        if candidates.is_empty() {
            return Vec::new();
        }
        // The deterministic heuristic pass seeds every item; model
        // answers only overwrite the locals they actually scored, so a
        // missing bridge, a failed batch, or a skipped id never loses
        // the quality signal.
        let mut results = HeuristicScorer.score(transcript, candidates);
        let started = Instant::now();
        let ctx = llm_scorer::scoring_context(transcript, candidates, self.cfg.max_candidates);
        let lines = ctx.inputs.len();
        let mut unique = 0usize;
        let mut calls = 0usize;
        let mut overlaid = 0usize;
        let mut failed = 0usize;

        if let Some(bridge) = apple::shared_bridge(&self.cfg.bridge, self.cfg.timeout_ms) {
            let inputs = self.with_content(transcript, ctx.inputs);
            // Repeated tool outputs produce identical lines: score each
            // unique line once and fan the answer out to every member
            // local. `max_batches` caps unique-line batches.
            let uniques = unique_lines(&inputs);
            unique = uniques.len();
            let schema: Value = serde_json::from_str(SCORES_SCHEMA).unwrap();
            let mut answers: Vec<(usize, f64)> = Vec::new();
            for chunk in uniques
                .chunks(self.cfg.batch_size.max(1))
                .take(self.cfg.max_batches)
            {
                calls += 1;
                let batch: Vec<(usize, usize, String)> = chunk
                    .iter()
                    .map(|&(position, _)| inputs[position].clone())
                    .collect();
                match score_batch(
                    bridge,
                    &ctx.goal,
                    &ctx.tail,
                    &batch,
                    &schema,
                    self.cfg.content_bytes > 0,
                ) {
                    Ok(scores) => answers.extend(fan_out_answers(&inputs, &uniques, &scores)),
                    Err(e) => {
                        failed += 1;
                        eprintln!("apple scorer batch failed: {e:#}");
                    }
                }
            }
            overlaid = llm_scorer::overlay_answers(&mut results, candidates, &answers);
        }
        eprintln!(
            "apple: {lines} candidates → {unique} unique lines in {calls} batch call(s), {overlaid} items overlaid, {failed} failed, {}ms",
            started.elapsed().as_millis()
        );
        results
    }
}

/// The line content minus the `[local] ` bracket the prompt embeds as
/// the answer id. The bracket is the only difference between lines from
/// repeated identical tool outputs, so the body is the dedup key.
fn line_body(local: usize, text: &str) -> &str {
    text.strip_prefix(&format!("[{local}] ")).unwrap_or(text)
}

/// Group scoring inputs by identical line content, keeping
/// first-occurrence order so chunking stays deterministic. Each entry
/// is `(representative_position, member_locals)`: only the
/// representative's line — carrying its own bracketed local id — goes
/// into the prompt, and its score fans out to every member local.
fn unique_lines(inputs: &[(usize, usize, String)]) -> Vec<(usize, Vec<usize>)> {
    let mut uniques: Vec<(usize, Vec<usize>)> = Vec::new();
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for (position, (local, _, text)) in inputs.iter().enumerate() {
        let key = line_body(*local, text);
        if let Some(&u) = seen.get(key) {
            uniques[u].1.push(*local);
        } else {
            seen.insert(key, uniques.len());
            uniques.push((position, vec![*local]));
        }
    }
    uniques
}

/// Fan a batch's answers out to member locals. The model's `id` is the
/// representative's bracketed local; every candidate that produced an
/// identical line receives the same probability. Ids matching no
/// representative (an id the prompt never listed) are dropped.
fn fan_out_answers(
    inputs: &[(usize, usize, String)],
    uniques: &[(usize, Vec<usize>)],
    answered: &[(usize, f64)],
) -> Vec<(usize, f64)> {
    let representative: HashMap<usize, usize> = uniques
        .iter()
        .enumerate()
        .map(|(u, &(position, _))| (inputs[position].0, u))
        .collect();
    let mut out = Vec::new();
    for &(id, probability) in answered {
        if let Some(&u) = representative.get(&id) {
            out.extend(uniques[u].1.iter().map(|&local| (local, probability)));
        }
    }
    out
}

fn score_batch(
    bridge: &apple_foundation::Bridge,
    goal: &str,
    tail: &str,
    batch: &[(usize, usize, String)],
    schema: &Value,
    content_on: bool,
) -> anyhow::Result<Vec<(usize, f64)>> {
    let list = batch
        .iter()
        .map(|(_, _, text)| text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let note = if content_on {
        " Each candidate line ends with ` :: ` followed by a bounded excerpt of the actual output."
    } else {
        ""
    };
    let prompt = format!(
        "You are scoring stale tool outputs for context compaction. The agent's current task is:\n{goal}\n\nRecent conversation tail:\n{tail}\n\nFor each candidate below, estimate the probability (0.0 to 1.0) that the tool output must remain visible for the agent to continue accurately. Score every candidate id.{note}\n\n{list}\n"
    );
    let value = match apple::cache_get(&prompt, schema) {
        Some(v) => v,
        None => {
            let v = bridge.request(&apple_foundation::Request {
                prompt: prompt.clone(),
                instructions: Some(
                    "Score every listed candidate id. Probabilities are numbers from 0.0 to 1.0."
                        .into(),
                ),
                schema: Some(schema.clone()),
                expect_json: false,
                max_output_bytes: Some(8192),
            })?;
            apple::cache_put(&prompt, schema, &v);
            v
        }
    };
    let scores = value
        .get("scores")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("apple response missing `scores` array"))?;
    Ok(scores
        .iter()
        .filter_map(|s| {
            Some((
                s.get("id")?.as_u64()? as usize,
                s.get("keep_probability")?.as_f64()?,
            ))
        })
        .collect())
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// Returns an Apple on-device driver when the bridge and model are available.
/// Falls back to the heuristic scorer when unavailable or the build fails.
pub fn maybe_apple_scorer() -> Option<Box<dyn ScoreDriver>> {
    AppleConfig::resolve().map(|cfg| Box::new(AppleScorer::new(cfg)) as Box<dyn ScoreDriver>)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_item(line_index: usize, label: &str, summary: &str) -> gobstopper_core::TranscriptItem {
        gobstopper_core::TranscriptItem {
            line_index,
            kind: gobstopper_core::ItemKind::ToolResult,
            est_tokens: 10,
            elidable_bytes: Some(40),
            elidable_parts: 1,
            label: label.into(),
            summary: Some(summary.into()),
            uuid: None,
            parent_uuid: None,
            tool_use_ids: Vec::new(),
            payload_sha256: None,
        }
    }

    fn test_transcript(items: Vec<gobstopper_core::TranscriptItem>) -> Transcript {
        Transcript {
            session: gobstopper_core::SessionHandle {
                provider: gobstopper_core::Provider::Codex,
                session_id: "s".into(),
                path: std::path::PathBuf::from("/tmp/s.jsonl"),
                cwd: None,
                age_secs: 0,
            },
            items,
            usage: Default::default(),
        }
    }

    fn input(local: usize, item_index: usize, text: &str) -> (usize, usize, String) {
        (local, item_index, text.to_string())
    }

    #[test]
    fn line_body_strips_only_the_own_bracket_prefix() {
        assert_eq!(line_body(2, "[2] exec = ok"), "exec = ok");
        // A bracket belonging to another local is content, not a prefix.
        assert_eq!(line_body(1, "[0] exec = ok"), "[0] exec = ok");
        assert_eq!(line_body(0, "no bracket"), "no bracket");
    }

    #[test]
    fn unique_lines_groups_identical_bodies_in_first_occurrence_order() {
        let inputs = vec![
            input(0, 10, "[0] exec = cargo test: pass"),
            input(1, 11, "[1] exec = cargo build: ok"),
            input(2, 12, "[2] exec = cargo test: pass"),
            input(3, 13, "[3] exec = cargo test: pass"),
        ];
        let uniques = unique_lines(&inputs);
        // One unique line per distinct text body; the bracketed local id
        // differs per line and never prevents the dedup.
        assert_eq!(uniques.len(), 2);
        assert_eq!(uniques[0], (0, vec![0, 2, 3]));
        assert_eq!(uniques[1], (1, vec![1]));
        // The representative's line keeps its own bracketed id.
        assert_eq!(inputs[uniques[0].0].2, "[0] exec = cargo test: pass");
    }

    #[test]
    fn fan_out_spreads_representative_answer_to_member_locals() {
        let inputs = vec![
            input(0, 10, "[0] exec = cargo test: pass"),
            input(1, 11, "[1] exec = cargo build: ok"),
            input(2, 12, "[2] exec = cargo test: pass"),
            input(3, 13, "[3] exec = cargo test: pass"),
        ];
        let uniques = unique_lines(&inputs);
        let out = fan_out_answers(&inputs, &uniques, &[(0, 0.9), (1, 0.2), (99, 0.5)]);
        // The rep's score reaches every member; the unknown id 99 drops.
        assert_eq!(out, vec![(0, 0.9), (2, 0.9), (3, 0.9), (1, 0.2)]);
    }

    #[test]
    fn dedup_then_overlay_preserves_heuristic_and_local_mapping() {
        let transcript = test_transcript(vec![
            test_item(0, "exec", "cargo test: pass"),
            test_item(1, "exec", "cargo build: ok"),
            test_item(2, "exec", "cargo test: pass"),
        ]);
        let candidates = [0, 1, 2];
        let mut results = HeuristicScorer.score(&transcript, &candidates);
        let baseline1 = results[1].keep_probability;

        let inputs = vec![
            input(0, 0, "[0] exec = cargo test: pass"),
            input(1, 1, "[1] exec = cargo build: ok"),
            input(2, 2, "[2] exec = cargo test: pass"),
        ];
        let uniques = unique_lines(&inputs);
        assert_eq!(uniques.len(), 2);

        // The model answered only the first unique line (rep local 0):
        // members 0 and 2 overlay, local 1 keeps its heuristic score.
        let answers = fan_out_answers(&inputs, &uniques, &[(0, 0.9)]);
        let overlaid = llm_scorer::overlay_answers(&mut results, &candidates, &answers);
        assert_eq!(overlaid, 2);
        assert_eq!(results[0].item_index, 0);
        assert_eq!(results[0].keep_probability, 0.9);
        assert_eq!(results[1].keep_probability, baseline1);
        assert_eq!(results[2].item_index, 2);
        assert_eq!(results[2].keep_probability, 0.9);
    }
}
