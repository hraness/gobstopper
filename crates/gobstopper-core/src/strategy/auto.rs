use super::{ElideStrategy, PolicyConfig, SawtoothStrategy, Strategy, StructuredStrategy};
use crate::model::Transcript;
use crate::plan::CompactionPlan;

/// Auto: the default. Selects the concrete strategy per evaluation from
/// transcript composition rather than holding a fixed policy — the
/// deterministic precursor of the fully agentic strategy.
///
/// Selection rules (each grounded in the compaction literature):
///   - tool-result-dominated transcripts lose almost nothing to
///     observation masking -> `elide`
///   - mixed/chatty transcripts need state carried forward -> `structured`
///   - transcripts without parseable items (or on live sessions where
///     transcript surgery is unsafe) delegate to the provider -> `sawtooth`
pub struct AutoStrategy;

/// Fraction of context attributed to tool results above which elision is
/// considered sufficient on its own.
const TOOL_DOMINANCE: f64 = 0.55;

impl AutoStrategy {
    /// Which concrete strategy `auto` would select. Exposed so `plan`
    /// output and the oompa seam can report the decision, not just the plan.
    pub fn select<'a>(transcript: &Transcript) -> &'a str {
        let total = transcript.context_tokens().max(1);
        let tool_tokens = transcript.elidable_tokens();
        if transcript.items.is_empty() || transcript.session.is_active() {
            "sawtooth"
        } else if tool_tokens as f64 / total as f64 >= TOOL_DOMINANCE {
            "elide"
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
        let mut plan = match Self::select(transcript) {
            "elide" => ElideStrategy.evaluate(transcript, policy),
            "structured" => StructuredStrategy.evaluate(transcript, policy),
            _ => SawtoothStrategy.evaluate(transcript, policy),
        }?;
        plan.rationale = format!("auto -> {}: {}", plan.strategy, plan.rationale);
        plan.strategy = self.id().to_string();
        Some(plan)
    }
}
