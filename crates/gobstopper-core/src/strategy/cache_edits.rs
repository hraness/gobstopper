use super::{PolicyConfig, Strategy};
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

        let elidable: Vec<&crate::model::TranscriptItem> = transcript
            .items
            .iter()
            .filter(|i| i.elidable_bytes.is_some())
            .collect();
        let keep_from = elidable
            .len()
            .saturating_sub(policy.keep_recent_tool_outputs);
        let candidates = &elidable[..keep_from.min(elidable.len())];
        if candidates.is_empty() {
            return None;
        }

        let mut projected = before;
        let mut chosen = 0usize;
        let mut tool_use_ids: Vec<String> = Vec::new();
        for item in candidates.iter().rev() {
            if projected <= policy.floor_tokens {
                break;
            }
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
            "context {before} tokens exceeds trigger {}; emitting {} cache_edits to drop stale tool outputs",
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
