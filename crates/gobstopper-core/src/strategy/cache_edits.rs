use super::{HeuristicScorer, PolicyConfig, ScoredItem, ScoredStrategy, Strategy};
use crate::model::Transcript;
use crate::plan::{CompactionPlan, Edit};

/// Claude `cache_edits`: remove stale tool results by `tool_use_id` at the
/// Anthropic request layer. This keeps the on-disk conversation byte-identical,
/// preserving the prompt cache prefix, while dropping the selected tool outputs
/// from the next prompt. No transcript file is modified.
pub struct CacheEditsStrategy;

impl Strategy for CacheEditsStrategy {
    fn id(&self) -> &'static str {
        "cache_edits"
    }

    fn evaluate(&self, transcript: &Transcript, policy: &PolicyConfig) -> Option<CompactionPlan> {
        let before = transcript.context_tokens();
        if before < policy.effective_trigger() {
            return None;
        }

        let eligible = ScoredStrategy::candidates(transcript, policy);
        if eligible.is_empty() {
            return None;
        }

        let scores = HeuristicScorer.score(transcript, &eligible);
        let mut scored: Vec<ScoredItem> = scores
            .into_iter()
            .filter(|s| eligible.contains(&s.item_index))
            .collect();
        // Drop the lowest-keep-probability candidates first; break ties by
        // eliding the larger token savers first.
        scored.sort_by(|a, b| {
            a.keep_probability
                .partial_cmp(&b.keep_probability)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    let at = transcript.items[a.item_index].estimated_elision_savings();
                    let bt = transcript.items[b.item_index].estimated_elision_savings();
                    bt.cmp(&at)
                })
        });

        let mut projected = before;
        let mut chosen = 0usize;
        let mut tool_use_ids: Vec<String> = Vec::new();
        for scored in &scored {
            if projected <= policy.floor_tokens {
                break;
            }
            let item = &transcript.items[scored.item_index];
            if let Some(id) = item.parent_uuid.as_deref() {
                tool_use_ids.push(id.to_string());
            }
            chosen += 1;
            projected = projected.saturating_sub(item.estimated_elision_savings());
        }
        if chosen == 0 {
            return None;
        }

        let rationale = format!(
            "context {before} tokens exceeds trigger {}; emitting {} scored cache_edits to drop stale tool outputs",
            policy.trigger_tokens,
            tool_use_ids.len()
        );

        Some(CompactionPlan {
            strategy: self.id().to_string(),
            rationale,
            edits: vec![Edit::CacheEdit { tool_use_ids }],
            context_tokens_before: before,
            context_tokens_after: projected,
        })
    }
}
