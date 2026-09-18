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
//!   GOBSTOPPER_APPLE_BATCH_SIZE  - 32
//!   GOBSTOPPER_APPLE_MAX_BATCHES - 4

use std::path::PathBuf;
use std::sync::OnceLock;

use gobstopper_core::{ScoreDriver, ScoredItem, Transcript};
use serde_json::Value;

use crate::llm_scorer;

#[derive(Debug, Clone)]
pub struct AppleConfig {
    pub bridge: PathBuf,
    pub timeout_ms: u64,
    pub max_candidates: usize,
    pub batch_size: usize,
    pub max_batches: usize,
}

fn share_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/gobstopper"))
}

fn resolve_bridge() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("GOBSTOPPER_APPLE_BRIDGE") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.parent()?.join("apple-bridge");
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    // The managed install path rebuilds automatically when the embedded
    // bridge source changes; env/sibling paths are user-managed.
    let installed = share_dir()?.join("apple-bridge");
    apple_foundation::ensure_bridge(&installed).ok()
}

impl AppleConfig {
    pub fn resolve() -> Option<Self> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let bridge = resolve_bridge()?;
        let availability =
            apple_foundation::check(&[bridge.to_string_lossy().into_owned()]).ok()?;
        if !availability.available {
            eprintln!(
                "apple scorer: model unavailable ({})",
                availability.reason.as_deref().unwrap_or("unknown")
            );
            return None;
        }
        Some(Self {
            bridge,
            timeout_ms: std::env::var("GOBSTOPPER_APPLE_TIMEOUT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(180_000),
            max_candidates: std::env::var("GOBSTOPPER_APPLE_MAX_CANDIDATES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(64),
            batch_size: std::env::var("GOBSTOPPER_APPLE_BATCH_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(32),
            max_batches: std::env::var("GOBSTOPPER_APPLE_MAX_BATCHES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(4),
        })
    }
}

fn shared_bridge(cfg: &AppleConfig) -> Option<&'static apple_foundation::Bridge> {
    static BRIDGE: OnceLock<Option<apple_foundation::Bridge>> = OnceLock::new();
    BRIDGE
        .get_or_init(|| {
            apple_foundation::Bridge::with_options(
                &[cfg.bridge.to_string_lossy().into_owned()],
                apple_foundation::Options {
                    request_timeout: std::time::Duration::from_millis(cfg.timeout_ms),
                    max_pending: 8,
                },
            )
            .ok()
        })
        .as_ref()
}

const SCORES_SCHEMA: &str = r#"{"type":"object","properties":{"scores":{"type":"array","items":{"type":"object","properties":{"id":{"type":"integer"},"keep_probability":{"type":"number"}},"required":["id","keep_probability"]}}},"required":["scores"]}"#;

pub struct AppleScorer {
    cfg: AppleConfig,
}

impl AppleScorer {
    pub fn new(cfg: AppleConfig) -> Self {
        Self { cfg }
    }
}

impl ScoreDriver for AppleScorer {
    fn score(&self, transcript: &Transcript, candidates: &[usize]) -> Vec<ScoredItem> {
        let Some(bridge) = shared_bridge(&self.cfg) else {
            return neutral(candidates);
        };
        let ctx = llm_scorer::scoring_context(transcript, candidates, self.cfg.max_candidates);
        if ctx.inputs.is_empty() {
            return neutral(candidates);
        }
        let schema: Value = serde_json::from_str(SCORES_SCHEMA).unwrap();
        let mut by_local: std::collections::HashMap<usize, f64> = Default::default();
        for chunk in ctx
            .inputs
            .chunks(self.cfg.batch_size)
            .take(self.cfg.max_batches)
        {
            match score_batch(bridge, &ctx.goal, &ctx.tail, chunk, &schema) {
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
) -> anyhow::Result<Vec<(usize, f64)>> {
    let list = batch
        .iter()
        .map(|(_, _, text)| text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = format!(
        "You are scoring stale tool outputs for context compaction. The agent's current task is:\n{goal}\n\nRecent conversation tail:\n{tail}\n\nFor each candidate below, estimate the probability (0.0 to 1.0) that the tool output must remain visible for the agent to continue accurately. Score every candidate id.\n\n{list}\n"
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

/// Returns an Apple on-device driver when the bridge and model are available.
/// Falls back to the heuristic scorer when unavailable or the build fails.
pub fn maybe_apple_scorer() -> Option<Box<dyn ScoreDriver>> {
    AppleConfig::resolve().map(|cfg| Box::new(AppleScorer::new(cfg)) as Box<dyn ScoreDriver>)
}
