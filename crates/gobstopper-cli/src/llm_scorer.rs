//! Cheap-LLM scorer driver for the `scored` strategy.
//!
//! This calls any OpenAI-compatible chat endpoint, defaulting to Vercel
//! AI Gateway (`https://ai-gateway.vercel.sh/v1/chat/completions`).
//! With a Qwen 0.5B/1.5B/7B model it is super cheap and works today while
//! Jev is on a waitlist. Only sanitized labels and summaries are sent —
//! full tool output never leaves the machine.
//!
//! Configuration (all optional, defaults listed):
//!   AI_GATEWAY_API_KEY  - bearer token
//!   GOBSTOPPER_LLM_ENDPOINT - https://ai-gateway.vercel.sh/v1/chat/completions
//!   GOBSTOPPER_LLM_MODEL    - google/gemini-2.5-flash-lite
//!   GOBSTOPPER_LLM_TIMEOUT_MS - 20000
//!   GOBSTOPPER_LLM_MAX_CANDIDATES - 64
//!   GOBSTOPPER_LLM_BATCH_SIZE - 16
//!   GOBSTOPPER_LLM_MAX_BATCHES - 4

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use std::process::Command;

use gobstopper_core::{ScoreDriver, ScoredItem, Transcript};

#[derive(Debug, Clone, Default)]
pub struct LlmConfig {
    pub api_key: String,
    pub endpoint: String,
    pub model: String,
    pub timeout_ms: u64,
    pub max_candidates: usize,
    pub batch_size: usize,
    pub max_batches: usize,
}

impl LlmConfig {
    pub fn resolve() -> Option<Self> {
        let api_key = std::env::var("AI_GATEWAY_API_KEY")
            .ok()
            .or_else(|| std::env::var("GOBSTOPPER_LLM_API_KEY").ok())?;
        Some(Self {
            api_key,
            endpoint: std::env::var("GOBSTOPPER_LLM_ENDPOINT")
                .unwrap_or_else(|_| "https://ai-gateway.vercel.sh/v1/chat/completions".into()),
            model: std::env::var("GOBSTOPPER_LLM_MODEL")
                .unwrap_or_else(|_| "google/gemini-2.5-flash-lite".into()),
            timeout_ms: std::env::var("GOBSTOPPER_LLM_TIMEOUT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(30_000),
            max_candidates: std::env::var("GOBSTOPPER_LLM_MAX_CANDIDATES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(64),
            batch_size: std::env::var("GOBSTOPPER_LLM_BATCH_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(16),
            max_batches: std::env::var("GOBSTOPPER_LLM_MAX_BATCHES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(4),
        })
    }
}

#[derive(Debug, Clone, Serialize)]
struct ChatCompletionRequest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include_reasoning: Option<bool>,
    messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Message {
    role: &'static str,
    content: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Choice {
    message: AssistantMessage,
}

#[derive(Debug, Clone, Deserialize)]
struct AssistantMessage {
    content: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Clone, Deserialize)]
struct LlmScore {
    id: usize,
    keep_probability: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct ScoreResponse {
    scores: Vec<LlmScore>,
}

pub struct LlmScorer {
    cfg: LlmConfig,
}

impl LlmScorer {
    pub fn new(cfg: LlmConfig) -> Self {
        Self { cfg }
    }
}

impl ScoreDriver for LlmScorer {
    fn score(&self, transcript: &Transcript, candidates: &[usize]) -> Vec<ScoredItem> {
        if candidates.is_empty() {
            return Vec::new();
        }

        let scored_total = candidates.len().min(self.cfg.max_candidates);
        let mut inputs = Vec::with_capacity(scored_total);
        for (i, idx) in candidates.iter().copied().take(scored_total).enumerate() {
            if let Some(item) = transcript.items.get(idx) {
                inputs.push((
                    i,
                    idx,
                    format!(
                        "[{}] {} = {}",
                        i,
                        item.label,
                        item.summary.as_deref().unwrap_or("(no summary)")
                    ),
                ));
            }
        }
        if inputs.is_empty() {
            return candidates
                .iter()
                .map(|&idx| ScoredItem {
                    item_index: idx,
                    keep_probability: 0.5,
                })
                .collect();
        }

        let goal = transcript
            .items
            .iter()
            .rev()
            .find(|i| {
                i.kind == gobstopper_core::ItemKind::User
                    && i.summary.as_ref().is_some_and(|s| {
                        !s.starts_with('<') && !s.starts_with("[gobstopper state card]")
                    })
            })
            .and_then(|i| i.summary.clone())
            .unwrap_or_else(|| "(no explicit goal)".into());

        let tail = transcript
            .items
            .iter()
            .rev()
            .take(6)
            .filter_map(|i| i.summary.as_ref().map(|s| format!("- {}", s)))
            .collect::<Vec<_>>()
            .join("\n");

        let mut all_scores = Vec::new();
        let chunks: Vec<&[(usize, usize, String)]> = inputs
            .chunks(self.cfg.batch_size)
            .take(self.cfg.max_batches)
            .collect();
        std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(chunks.len());
            for chunk in chunks {
                let cfg = self.cfg.clone();
                let goal = goal.clone();
                let tail = tail.clone();
                let batch = chunk.to_vec();
                handles.push(s.spawn(move || score_batch(&cfg, &goal, &tail, &batch)));
            }
            for h in handles {
                match h.join().unwrap_or_else(|_| Ok(Vec::new())) {
                    Ok(scores) => all_scores.extend(scores),
                    Err(e) => eprintln!("llm scorer batch failed: {e:#}"),
                }
            }
        });

        let mut by_local: std::collections::HashMap<usize, f64> = all_scores
            .into_iter()
            .map(|s| (s.id, s.keep_probability.clamp(0.0, 1.0)))
            .collect();
        candidates
            .iter()
            .enumerate()
            .map(|(local, &idx)| ScoredItem {
                item_index: idx,
                keep_probability: by_local.remove(&local).unwrap_or(0.5),
            })
            .collect()
    }
}

fn score_batch(
    cfg: &LlmConfig,
    goal: &str,
    tail: &str,
    batch: &[(usize, usize, String)],
) -> anyhow::Result<Vec<LlmScore>> {
    let list = batch
        .iter()
        .map(|(_, _, text)| text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = format!(
        "You are scoring stale tool outputs for context compaction. The agent's current task is:\n{}\n\nRecent conversation tail:\n{}\n\nFor each candidate below, estimate the probability (0.0 to 1.0) that the tool output must remain visible for the agent to continue accurately. Return ONLY a JSON object with a `scores` array of objects containing `id` (the integer in brackets) and `keep_probability`.\n\n{}\n",
        goal, tail, list
    );

    let request = ChatCompletionRequest {
        model: cfg.model.clone(),
        response_format: None,
        max_tokens: Some(1024),
        temperature: Some(0.0),
        reasoning: None,
        include_reasoning: None,
        messages: vec![
            Message {
                role: "system",
                content: "Return only the requested JSON. Do not include markdown, prose, or explanations.".into(),
            },
            Message { role: "user", content: prompt },
        ],
    };

    call_llm(&request, cfg)
}

fn call_llm(request: &ChatCompletionRequest, cfg: &LlmConfig) -> anyhow::Result<Vec<LlmScore>> {
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
    let text = std::str::from_utf8(&raw).context("llm response is not utf8")?;
    let parsed: ChatCompletionResponse =
        serde_json::from_str(text).with_context(|| format!("parse llm response: {text}"))?;
    let content = parsed
        .choices
        .first()
        .map(|c| c.message.content.trim())
        .unwrap_or("{}");

    // Some cheap Qwen models return the JSON inside markdown fences; strip them.
    let content = content
        .strip_prefix("```json")
        .or_else(|| content.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .map(|s| s.trim())
        .unwrap_or(content);

    let wrapper: serde_json::Value = serde_json::from_str(content)
        .with_context(|| format!("parse llm content as json: {content}"))?;
    let scores = if let Ok(r) = serde_json::from_value::<ScoreResponse>(wrapper.clone()) {
        r.scores
    } else if let Ok(arr) = serde_json::from_value::<Vec<LlmScore>>(wrapper) {
        arr
    } else {
        bail!("llm response did not contain a `scores` array");
    };
    Ok(scores)
}

/// Resolve an LLM driver when `AI_GATEWAY_API_KEY` is configured. Falls back
/// to heuristic if the key is absent or the call fails.
pub fn maybe_llm_scorer() -> Option<Box<dyn ScoreDriver>> {
    LlmConfig::resolve().map(|cfg| Box::new(LlmScorer::new(cfg)) as Box<dyn ScoreDriver>)
}
