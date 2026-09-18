mod agentic;
mod auto;
mod cache_aware;
mod compacted;
mod elide;
mod sawtooth;
mod scored;
mod structured;

pub use agentic::{AgenticStrategy, EditorCall, EditorDriver};
pub use auto::AutoStrategy;
pub use cache_aware::CacheAwareStrategy;
pub use compacted::CompactedStrategy;
pub use elide::ElideStrategy;
pub use sawtooth::SawtoothStrategy;
pub use scored::{HeuristicScorer, ScoreDriver, ScoredItem, ScoredStrategy};
pub use structured::StructuredStrategy;

use crate::model::{ItemKind, Transcript};
use crate::plan::{CompactionPlan, DigestBlock};
use serde::{Deserialize, Serialize};

/// How close the session is to the provider's quota ceiling. Scales the
/// configured trigger so gobstopper compacts earlier under pressure and
/// later when there is headroom to spare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaPressure {
    /// Plenty of quota headroom — compact slightly later than configured.
    Low,
    /// No adjustment; the configured trigger applies as written.
    #[default]
    Normal,
    /// Near the quota ceiling — compact earlier to stay under it.
    High,
}

/// Resolved policy for one session evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyConfig {
    /// Fire when the session's context estimate exceeds this many tokens.
    pub trigger_tokens: u64,
    /// Post-compaction occupancy target for transcript-path strategies.
    pub floor_tokens: u64,
    /// Tool outputs newer than this many from the tail are never elided.
    pub keep_recent_tool_outputs: usize,
    /// Minimum seconds between compactions of one session.
    pub min_interval_secs: u64,
    /// Scales `trigger_tokens`; see [`PolicyConfig::effective_trigger`].
    #[serde(default)]
    pub quota_pressure: QuotaPressure,
    /// Derive trigger/floor per session from the provider-advertised
    /// window, elidable share, and past compaction yields. See
    /// [`crate::policy::adapt`].
    #[serde(default)]
    pub adaptive: bool,
}

impl Default for PolicyConfig {
    /// Research-backed default: fire well below the provider's own
    /// threshold (~60% of a ~400k effective window, ~25% of a 1M window).
    fn default() -> Self {
        Self {
            trigger_tokens: 250_000,
            floor_tokens: 40_000,
            keep_recent_tool_outputs: 8,
            min_interval_secs: 300,
            quota_pressure: QuotaPressure::Normal,
            adaptive: false,
        }
    }
}

impl PolicyConfig {
    /// `trigger_tokens` adjusted for quota pressure: `High` fires at 70%
    /// of the configured trigger (compact earlier), `Normal` at 100%,
    /// `Low` at 115%. Result is floored at 1 so a nonzero context can
    /// always trigger when a trigger is configured at all.
    pub fn effective_trigger(&self) -> u64 {
        let factor = match self.quota_pressure {
            QuotaPressure::High => 0.7,
            QuotaPressure::Normal => 1.0,
            QuotaPressure::Low => 1.15,
        };
        // f64 -> u64 casts saturate on overflow; max(1) is the floor.
        ((self.trigger_tokens as f64) * factor).round().max(1.0) as u64
    }
}

/// A compaction strategy. Pure: inspects the transcript, returns a plan
/// or `None` when the session is under threshold.
pub trait Strategy {
    fn id(&self) -> &'static str;
    fn evaluate(&self, transcript: &Transcript, policy: &PolicyConfig) -> Option<CompactionPlan>;
}

/// Every built-in strategy, in registry order.
pub fn builtin_strategies() -> Vec<Box<dyn Strategy>> {
    vec![
        Box::new(AutoStrategy),
        Box::new(ScoredStrategy),
        Box::new(CacheAwareStrategy),
        Box::new(SawtoothStrategy),
        Box::new(ElideStrategy),
        Box::new(CompactedStrategy),
        Box::new(StructuredStrategy),
        Box::new(AgenticStrategy),
    ]
}

/// Look up a built-in strategy by id.
pub fn strategy_by_id(id: &str) -> Option<Box<dyn Strategy>> {
    builtin_strategies().into_iter().find(|s| s.id() == id)
}

/// Build a bounded state-card digest from a set of chosen item
/// `line_index`es. Sanitized labels/summaries only — never payload text.
pub(crate) fn state_card_digest(transcript: &Transcript, chosen: &[usize]) -> DigestBlock {
    let mut decisions = Vec::new();
    let mut files_touched = Vec::new();
    for idx in chosen {
        if let Some(item) = transcript.items.iter().find(|i| i.line_index == *idx) {
            if let Some(summary) = &item.summary {
                decisions.push(summary.clone());
            } else {
                decisions.push(format!(
                    "{} elided ({} bytes)",
                    item.label,
                    item.elidable_bytes.unwrap_or(0)
                ));
            }
            if item.kind == ItemKind::ToolResult {
                files_touched.push(item.summary.clone().unwrap_or_else(|| item.label.clone()));
            }
        }
    }
    const MAX_DECISIONS: usize = 8;
    decisions.truncate(MAX_DECISIONS);
    files_touched.truncate(MAX_DECISIONS);

    let goal = transcript
        .items
        .iter()
        .rev()
        .find(|i| {
            i.kind == ItemKind::User
                && i.summary.as_ref().is_some_and(|s| {
                    !s.starts_with('<') && !s.starts_with("[gobstopper state card]")
                })
        })
        .and_then(|i| i.summary.clone());

    DigestBlock {
        goal: goal.clone(),
        decisions,
        files_touched,
        open_tasks: Vec::new(),
        covers_items: chosen.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_with(pressure: QuotaPressure) -> PolicyConfig {
        PolicyConfig {
            trigger_tokens: 1_000,
            quota_pressure: pressure,
            ..Default::default()
        }
    }

    #[test]
    fn effective_trigger_scales_with_quota_pressure() {
        assert_eq!(
            policy_with(QuotaPressure::Normal).effective_trigger(),
            1_000
        );
        // High compacts earlier: 70% of the configured trigger.
        assert_eq!(policy_with(QuotaPressure::High).effective_trigger(), 700);
        // Low waits for more headroom: 115%.
        assert_eq!(policy_with(QuotaPressure::Low).effective_trigger(), 1_150);
    }

    #[test]
    fn effective_trigger_floors_at_one() {
        let mut p = policy_with(QuotaPressure::High);
        p.trigger_tokens = 0;
        assert_eq!(p.effective_trigger(), 1);
        p.trigger_tokens = 1;
        assert_eq!(p.effective_trigger(), 1); // 0.7 rounds to 1, floored
    }

    #[test]
    fn quota_pressure_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&QuotaPressure::High).unwrap(),
            "\"high\""
        );
        let back: QuotaPressure = serde_json::from_str("\"low\"").unwrap();
        assert_eq!(back, QuotaPressure::Low);
        // PolicyConfig without the field still deserializes (additive).
        let p: PolicyConfig = serde_json::from_str(
            r#"{"trigger_tokens":1000,"floor_tokens":300,"keep_recent_tool_outputs":2,"min_interval_secs":0}"#,
        )
        .unwrap();
        assert_eq!(p.quota_pressure, QuotaPressure::Normal);
    }
}
