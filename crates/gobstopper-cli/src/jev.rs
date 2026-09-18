//! Jev (typesafe.ai `systemone`) scorer driver for the `scored` strategy.
//!
//! Jev is a System One scorer: no prose generation, just typed
//! question/answer pairs. It is fast (~100ms/call), cheap
//! ($0.042/M input tokens, output free), and structured. For compaction
//! we ask one `noul` (yes/no probability) question per eligible item:
//! "Does the output of `{label}` need to stay visible for the agent to
//! continue?". The conversation is stripped of full tool payloads before
//! it reaches the request — only sanitized labels and summaries are sent.
//!
//! Calls are made via `curl` (spawning the system binary) so gobstopper
//! needs no HTTP dependency. Batching keeps the request under Jev's 32k
//! context window and a bounded runtime.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::process::Command;

use gobstopper_core::{ScoreDriver, ScoredItem, Transcript};

/// Runtime configuration for the Jev scorer. Lives in gobstopper.toml as
/// `[scorer]` or `[scorer.jev]` depending on which design we ship.
#[derive(Debug, Clone, Default)]
pub struct JevConfig {
    pub api_key: String,
    pub endpoint: String,
    pub max_questions_per_call: usize,
    pub max_state_items: usize,
    pub timeout_ms: u64,
}

impl JevConfig {
    /// Load from env (default) or config. Returns `None` if no key.
    pub fn resolve() -> Option<Self> {
        let api_key = std::env::var("TYPESAFE_API_KEY")
            .ok()
            .or_else(|| std::env::var("GOBSTOPPER_JEV_API_KEY").ok())?;
        Some(Self {
            api_key,
            endpoint: std::env::var("GOBSTOPPER_JEV_ENDPOINT")
                .unwrap_or_else(|_| "https://api.typesafe.ai/v1/systemone".into()),
            max_questions_per_call: std::env::var("GOBSTOPPER_JEV_MAX_Q")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(64),
            max_state_items: std::env::var("GOBSTOPPER_JEV_MAX_STATE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(40),
            timeout_ms: std::env::var("GOBSTOPPER_JEV_TIMEOUT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(8_000),
        })
    }
}

/// System One `noul` question: a yes/no probability. The `instructions`
/// reference the item by sanitized label and summary.
#[derive(Debug, Clone, Serialize)]
struct NoulQuestion {
    #[serde(rename = "type")]
    qtype: &'static str,
    instructions: String,
}

#[derive(Debug, Clone, Serialize)]
struct JevRequest {
    model: &'static str,
    state: serde_json::Value,
    questions: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct JevAnswer {
    /// Primary answer probability; accepted forms: a number 0..1, a
    /// boolean, or a nested `probability` field.
    #[serde(default)]
    probability: Option<f64>,
    #[serde(default)]
    answer: Option<bool>,
    #[serde(default)]
    score: Option<f64>,
    #[serde(default)]
    p: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
struct JevAnswers {
    #[serde(default)]
    answers: Option<serde_json::Map<String, serde_json::Value>>,
}

pub struct JevScorer {
    cfg: JevConfig,
}

impl JevScorer {
    pub fn new(cfg: JevConfig) -> Self {
        Self { cfg }
    }
}

impl ScoreDriver for JevScorer {
    fn score(&self, transcript: &Transcript, candidates: &[usize]) -> Vec<ScoredItem> {
        if candidates.is_empty() {
            return Vec::new();
        }
        let state = build_state(transcript, self.cfg.max_state_items);
        let chunks = candidates
            .chunks(self.cfg.max_questions_per_call)
            .collect::<Vec<_>>();

        let mut results: Vec<ScoredItem> = Vec::with_capacity(candidates.len());
        for (chunk_idx, chunk) in chunks.iter().enumerate() {
            let request = build_request(&state, chunk, transcript);
            match call_jev(&request, &self.cfg) {
                Ok(probs) => {
                    for (i, &idx) in chunk.iter().enumerate() {
                        let qid = format!("q_{}_{}", chunk_idx, i);
                        let prob = probs.get(&qid).copied().unwrap_or(0.5);
                        results.push(ScoredItem {
                            item_index: idx,
                            keep_probability: prob.clamp(0.0, 1.0),
                        });
                    }
                }
                Err(e) => {
                    eprintln!("jev scorer call failed for chunk {chunk_idx}: {e:#}");
                    // Treat failures as neutral 0.5 so the run can fall
                    // back to position-based ordering rather than abort.
                    for &idx in chunk.iter() {
                        results.push(ScoredItem {
                            item_index: idx,
                            keep_probability: 0.5,
                        });
                    }
                }
            }
        }
        results
    }
}

/// Build a small state object from recent non-elided context. No full
/// tool output is included — only sanitized labels/summaries.
fn build_state(transcript: &Transcript, max_items: usize) -> serde_json::Value {
    let tail = transcript
        .items
        .iter()
        .rev()
        .filter(|i| i.elidable_bytes.is_none() || i.summary.is_some())
        .take(max_items)
        .map(|i| {
            json!({
                "kind": i.kind,
                "label": i.label,
                "summary": i.summary.as_deref().unwrap_or(""),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "session_provider": transcript.session.provider.as_str(),
        "total_items": transcript.items.len(),
        "context_tokens": transcript.context_tokens(),
        "tail_summary": tail,
    })
}

fn build_request(
    state: &serde_json::Value,
    chunk: &[usize],
    transcript: &Transcript,
) -> JevRequest {
    let mut questions = serde_json::Map::new();
    for (i, &idx) in chunk.iter().enumerate() {
        if let Some(item) = transcript.items.get(idx) {
            let desc = if let Some(summary) = &item.summary {
                format!("{} = {}", item.label, summary)
            } else {
                item.label.clone()
            };
            questions.insert(
                format!("q_0_{i}"),
                serde_json::to_value(NoulQuestion {
                    qtype: "noul",
                    instructions: format!(
                        "Does the output of `{desc}` need to stay visible for the agent to continue its current task?"
                    ),
                })
                .unwrap(),
            );
        }
    }
    JevRequest {
        model: "jev-latest",
        state: state.clone(),
        questions,
    }
}

fn call_jev(
    request: &JevRequest,
    cfg: &JevConfig,
) -> anyhow::Result<std::collections::HashMap<String, f64>> {
    let body = serde_json::to_vec(request)?;
    let mut cmd = Command::new("curl");
    cmd.arg("-sS")
        .arg("-X")
        .arg("POST")
        .arg("-H")
        .arg("Content-Type: application/json")
        .arg("-H")
        .arg(format!("Authorization: Bearer {}", cfg.api_key))
        .arg("-d")
        .arg("@-")
        .arg(&cfg.endpoint);

    let raw = gobstopper_adapters::plugins::run_bounded(cmd, body, cfg.timeout_ms, 1024 * 1024)?;
    let text = std::str::from_utf8(&raw).context("jev response is not utf8")?;
    let parsed: JevAnswers =
        serde_json::from_str(text).with_context(|| format!("parse jev response: {text}"))?;
    let answers = parsed.answers.unwrap_or_default();
    Ok(answers
        .into_iter()
        .map(|(k, v)| {
            let prob = if let Ok(a) = serde_json::from_value::<JevAnswer>(v.clone()) {
                a.probability
                    .or(a.p)
                    .or(a.score)
                    .or_else(|| a.answer.map(|b| if b { 1.0 } else { 0.0 }))
                    .unwrap_or(0.5)
            } else if let Some(b) = v.as_bool() {
                if b {
                    1.0
                } else {
                    0.0
                }
            } else {
                v.as_f64().unwrap_or(0.5)
            };
            (k, prob.clamp(0.0, 1.0))
        })
        .collect())
}

/// Convenience: resolve a driver when the `scored` strategy is selected
/// and a key is configured. Returns `None` if the user wants the default
/// heuristic scorer (no key), and never fails the compaction.
pub fn maybe_jev_scorer() -> Option<Box<dyn ScoreDriver>> {
    JevConfig::resolve().map(|cfg| Box::new(JevScorer::new(cfg)) as Box<dyn ScoreDriver>)
}
