use super::{
    CacheAwareStrategy, CacheEditsStrategy, CompactedStrategy, DedupeStrategy, ElideStrategy,
    MicroStrategy, MiddleStrategy, PolicyConfig, SawtoothStrategy, ScoredStrategy, Strategy,
    StructuredStrategy,
};
use crate::model::Transcript;
use crate::plan::{CompactionPlan, Edit};
use crate::Provider;

/// Auto: the default. Selects the concrete strategy per evaluation from
/// transcript composition rather than holding a fixed policy — the
/// deterministic precursor of the fully agentic strategy.
///
/// Selection rules (each grounded in the compaction literature):
///   - tool-result-dominated transcripts lose almost nothing to
///     observation masking; keep the conversation prefix in cache by
///     eliding the latest stale outputs and carrying a state-card digest
///     -> `cache_aware`
///   - mixed/chatty transcripts need state carried forward -> `structured`
///   - transcripts without parseable items (or on live sessions where
///     transcript surgery is unsafe) delegate to the provider -> `sawtooth`
pub struct AutoStrategy;

/// Fraction of context attributed to tool results above which elision is
/// considered sufficient on its own.
const TOOL_DOMINANCE: f64 = 0.55;

/// How strongly we reward preserving the conversation prefix. A higher
/// exponent makes `auto` more reluctant to swap a long prefix for a
/// slightly larger token saving.
const PREFIX_EXP: i32 = 3;

/// Concrete file-surgery strategies that `auto` compares for idle
/// sessions. The best-scoring plan wins; `sawtooth` is reserved for live
/// sessions because it is a provider control, not a transcript rewrite.
fn file_strategies() -> Vec<Box<dyn Strategy>> {
    vec![
        Box::new(CacheAwareStrategy),
        Box::new(ScoredStrategy),
        Box::new(ElideStrategy),
        Box::new(CompactedStrategy),
        Box::new(DedupeStrategy),
        Box::new(MicroStrategy),
        Box::new(MiddleStrategy),
        Box::new(StructuredStrategy),
    ]
}

/// The first line any `Elide` edit touches, or `None` if the plan does not
/// elide in place.
fn first_elided_line(plan: &CompactionPlan) -> Option<usize> {
    plan.edits
        .iter()
        .filter_map(|e| match e {
            Edit::Elide { line_indexes, .. } => line_indexes.iter().min().copied(),
            _ => None,
        })
        .min()
}

/// Tokens of the conversation that sit before the first in-place edit.
/// The larger this is, the more of the provider prompt cache is likely to
/// remain valid.
fn prefix_tokens(transcript: &Transcript, plan: &CompactionPlan) -> u64 {
    let first = first_elided_line(plan).unwrap_or(usize::MAX);
    transcript
        .items
        .iter()
        .filter(|i| i.line_index < first)
        .map(|i| i.est_tokens)
        .sum()
}

/// Quality score for a file-surgery plan. It rewards token savings but
/// quadratically rewards preserving the prefix, so `auto` prefers plans
/// that shrink the context without busting the prompt cache.
fn plan_score(transcript: &Transcript, plan: &CompactionPlan) -> f64 {
    let before = plan.context_tokens_before.max(1) as f64;
    let saved = plan.est_savings() as f64;
    let prefix = prefix_tokens(transcript, plan) as f64;
    let prefix_ratio = prefix / before;
    saved * prefix_ratio.powi(PREFIX_EXP)
}

impl AutoStrategy {
    /// Which concrete strategy `auto` would select. Exposed so `plan`
    /// output and the oompa seam can report the decision, not just the plan.
    pub fn select<'a>(transcript: &Transcript) -> &'a str {
        let total = transcript.context_tokens().max(1);
        let tool_tokens = transcript.elidable_tokens();
        if transcript.items.is_empty() || transcript.session.is_active() {
            if transcript.session.provider == Provider::ClaudeCode {
                "cache_edits"
            } else {
                "sawtooth"
            }
        } else if tool_tokens as f64 / total as f64 >= TOOL_DOMINANCE {
            "cache_aware"
        } else {
            "structured"
        }
    }
}

impl Strategy for AutoStrategy {
    fn id(&self) -> &'static str {
        "auto"
    }

    fn evaluate(&self, transcript: &Transcript, policy: &PolicyConfig) -> Option<CompactionPlan> {
        // Live sessions are unsafe to edit in place; delegate to a
        // provider-native control. Claude can drop tool results by
        // `tool_use_id` without invalidating the cache; Codex falls back
        // to the generic sawtooth provider control.
        if transcript.items.is_empty() || transcript.session.is_active() {
            let (selected, mut plan) = if transcript.session.provider == Provider::ClaudeCode {
                match CacheEditsStrategy.evaluate(transcript, policy) {
                    Some(plan) => ("cache_edits", plan),
                    None => ("sawtooth", SawtoothStrategy.evaluate(transcript, policy)?),
                }
            } else {
                ("sawtooth", SawtoothStrategy.evaluate(transcript, policy)?)
            };
            plan.rationale = format!(
                "auto (live session): delegated to {}; {}",
                selected, plan.rationale
            );
            plan.strategy = self.id().to_string();
            return Some(plan);
        }

        // Otherwise, run every file-surgery strategy and pick the plan that
        // maximizes token savings while preserving the conversation prefix.
        // This is robust to mixed transcripts because it scores each plan
        // empirically instead of committing to a single heuristic rule.
        let mut best: Option<CompactionPlan> = None;
        let mut best_score = -1.0;
        for s in file_strategies() {
            if let Some(plan) = s.evaluate(transcript, policy) {
                let score = plan_score(transcript, &plan);
                if score > best_score {
                    best_score = score;
                    best = Some(plan);
                }
            }
        }

        let mut plan = best?;
        let selected = plan.strategy.clone();
        plan.rationale = format!(
            "auto: selected {} (score {:.0}); {}",
            selected, best_score, plan.rationale
        );
        plan.strategy = self.id().to_string();
        Some(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ItemKind, Provider, SessionHandle, Transcript, TranscriptItem};
    use crate::strategy::QuotaPressure;
    use std::path::PathBuf;

    fn item(
        line: usize,
        kind: ItemKind,
        est_tokens: u64,
        elidable: bool,
        label: &str,
        summary: Option<&str>,
    ) -> TranscriptItem {
        TranscriptItem {
            line_index: line,
            kind,
            est_tokens,
            elidable_bytes: elidable.then_some(est_tokens * 4),
            elidable_parts: 1,
            label: label.into(),
            summary: summary.map(String::from),
            uuid: None,
            parent_uuid: None,
            tool_use_ids: Vec::new(),
            payload_sha256: None,
        }
    }

    fn transcript(items: Vec<TranscriptItem>, context_tokens: u64) -> Transcript {
        Transcript {
            session: SessionHandle {
                provider: Provider::Codex,
                session_id: "s".into(),
                path: PathBuf::from("/tmp/s.jsonl"),
                cwd: None,
                age_secs: u64::MAX,
            },
            items,
            usage: crate::model::UsageSample {
                context_tokens,
                ..Default::default()
            },
        }
    }

    fn policy() -> PolicyConfig {
        PolicyConfig {
            trigger_tokens: 100,
            floor_tokens: 50,
            keep_recent_tool_outputs: 1,
            min_interval_secs: 0,
            quota_pressure: QuotaPressure::Normal,
            ..Default::default()
        }
    }

    #[test]
    fn best_of_prefers_high_prefix() {
        // Tail-heavy: newest tool result is the only protected item. Cache
        // aware will leave the long prefix untouched while `elide` rewrites
        // from the front.
        let items = vec![
            item(0, ItemKind::User, 10, false, "user", Some("goal")),
            item(
                1,
                ItemKind::ToolResult,
                100,
                true,
                "read_file a",
                Some("alpha"),
            ),
            item(
                2,
                ItemKind::ToolResult,
                100,
                true,
                "read_file b",
                Some("beta"),
            ),
            item(
                3,
                ItemKind::ToolResult,
                100,
                true,
                "read_file c",
                Some("gamma"),
            ),
            item(
                4,
                ItemKind::ToolResult,
                50,
                true,
                "read_file d",
                Some("delta"),
            ),
        ];
        let t = transcript(items, 360);
        let plan = AutoStrategy.evaluate(&t, &policy()).unwrap();
        // `auto` must pick a concrete file-surgery strategy (not sawtooth)
        // and report its choice in the rationale.
        assert_eq!(plan.strategy, "auto");
        assert!(plan.rationale.starts_with("auto: selected"));
        assert!(!plan.rationale.contains("sawtooth"));
    }

    #[test]
    fn live_session_delegates_to_provider() {
        let mut t = transcript(
            vec![
                item(0, ItemKind::User, 10, false, "user", Some("goal")),
                item(
                    1,
                    ItemKind::ToolResult,
                    200,
                    true,
                    "read_file a",
                    Some("alpha"),
                ),
            ],
            210,
        );
        t.session.age_secs = 0;
        let plan = AutoStrategy.evaluate(&t, &policy()).unwrap();
        assert_eq!(plan.strategy, "auto");
        assert!(plan.rationale.contains("sawtooth"));
    }
}
