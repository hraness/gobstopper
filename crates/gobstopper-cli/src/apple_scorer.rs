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
//! warm-up paid once), killed and respawned on timeout, and any failure
//! falls back to neutral 0.5 scores or the heuristic scorer.
//!
//! Configuration (all optional, defaults listed):
//!   GOBSTOPPER_APPLE_BRIDGE      - env → sibling of the gobstopper binary →
//!                                  ~/.local/share/gobstopper/apple-bridge
//!                                  (auto-built via swiftc when absent)
//!   GOBSTOPPER_APPLE_TIMEOUT_MS  - 180000 (first request pays model warm-up)
//!   GOBSTOPPER_APPLE_MAX_CANDIDATES - 64
//!   GOBSTOPPER_APPLE_BATCH_SIZE  - 32 (8 when content excerpts are on)
//!   GOBSTOPPER_APPLE_MAX_BATCHES - 4 (8 when content excerpts are on)
//!   GOBSTOPPER_APPLE_CONTENT_BYTES - 400 per candidate (0 = labels only;
//!     on-device inference lifts the labels-only boundary remote scorers
//!     need, so candidates include bounded payload excerpts by default)

use std::path::PathBuf;

use gobstopper_core::{ScoreDriver, ScoredItem, Transcript};
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
        let Some(bridge) = apple::shared_bridge(&self.cfg.bridge, self.cfg.timeout_ms) else {
            return neutral(candidates);
        };
        let ctx = llm_scorer::scoring_context(transcript, candidates, self.cfg.max_candidates);
        if ctx.inputs.is_empty() {
            return neutral(candidates);
        }
        let inputs = self.with_content(transcript, ctx.inputs);
        let schema: Value = serde_json::from_str(SCORES_SCHEMA).unwrap();
        let mut by_local: std::collections::HashMap<usize, f64> = Default::default();
        for chunk in inputs
            .chunks(self.cfg.batch_size)
            .take(self.cfg.max_batches)
        {
            match score_batch(
                bridge,
                &ctx.goal,
                &ctx.tail,
                chunk,
                &schema,
                self.cfg.content_bytes > 0,
            ) {
                Ok(scores) => by_local.extend(scores),
                Err(e) => eprintln!("apple scorer batch failed: {e:#}"),
            }
        }
        candidates
            .iter()
            .enumerate()
            .map(|(local, &idx)| ScoredItem {
                item_index: idx,
                keep_probability: by_local.remove(&local).unwrap_or(0.5).clamp(0.0, 1.0),
            })
            .collect()
    }
}

fn neutral(candidates: &[usize]) -> Vec<ScoredItem> {
    candidates
        .iter()
        .map(|&idx| ScoredItem {
            item_index: idx,
            keep_probability: 0.5,
        })
        .collect()
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
    let value = bridge.request(&apple_foundation::Request {
        prompt,
        instructions: Some(
            "Score every listed candidate id. Probabilities are numbers from 0.0 to 1.0.".into(),
        ),
        schema: Some(schema.clone()),
        expect_json: false,
        max_output_bytes: Some(8192),
    })?;
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
