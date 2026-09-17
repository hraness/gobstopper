use super::{ElideStrategy, PolicyConfig, Strategy};
use crate::model::Transcript;
use crate::plan::{CompactionPlan, DigestBlock, Edit};

/// Compacted: observation masking plus a Codex `compacted` record.
///
/// This strategy first applies the same elision logic as `elide`, then appends
/// a provider-specific `compacted` record that carries a digest of the elided
/// work plus a verbatim window of the most recent response items. The record
/// is understood by Codex as a native context-swap signal; other providers
/// should fall back to the plain `elide` behavior.
pub struct CompactedStrategy;

impl Strategy for CompactedStrategy {
    fn id(&self) -> &'static str {
        "compacted"
    }

    fn evaluate(
        &self,
        transcript: &Transcript,
        policy: &PolicyConfig,
    ) -> Option<CompactionPlan> {
        let mut plan = ElideStrategy.evaluate(transcript, policy)?;
        let elided_count = plan
            .edits
            .iter()
            .filter_map(|e| match e {
                Edit::Elide { line_indexes, .. } => Some(line_indexes.len()),
                _ => None,
            })
            .sum::<usize>();
        if elided_count == 0 {
            return None;
        }

        let digest = DigestBlock {
            goal: None,
            decisions: Vec::new(),
            files_touched: Vec::new(),
            open_tasks: Vec::new(),
            covers_items: elided_count,
        };

        plan.edits.push(Edit::InjectDigest { digest });
        plan.strategy = self.id().to_string();
        plan.rationale = format!(
            "context {} tokens exceeds trigger {}; emitting a compacted record for {} elided items",
            plan.context_tokens_before,
            policy.trigger_tokens,
            elided_count
        );
        // Rough token budget for the compacted record's replacement_history.
        // The exact size is calculated by the adapter; this is a planning estimate.
        plan.context_tokens_after = plan.context_tokens_after.saturating_add(256);
        Some(plan)
    }
}
