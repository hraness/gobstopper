use super::{state_card_digest, PolicyConfig, Strategy};
use crate::model::Transcript;
use crate::plan::{CompactionPlan, Edit};

/// Claude `cache_edits`: remove stale tool results by `tool_use_id` at the
/// Anthropic request layer rather than rewriting the transcript file. This
/// keeps the on-disk conversation byte-identical, preserving the prompt cache
/// prefix, while still dropping the selected tool outputs from the prompt.
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
        let mut chosen: Vec<usize> = Vec::new();
        let mut tool_use_ids: Vec<String> = Vec::new();
        for item in candidates.iter().rev() {
            if projected <= policy.floor_tokens {
                break;
            }
            if let Some(id) = item.parent_uuid.as_deref() {
                tool_use_ids.push(id.to_string());
            }
            chosen.push(item.line_index);
            projected = projected.saturating_sub(item.estimated_elision_savings());
        }
        if chosen.is_empty() {
            return None;
        }
        chosen.sort();

        let digest = state_card_digest(transcript, &chosen);
        let digest_overhead = digest.estimate_overhead();

        let rationale = format!(
            "context {before} tokens exceeds trigger {}; emitting {} cache_edits (and digest) to drop stale tool outputs",
            policy.trigger_tokens,
            tool_use_ids.len()
        );

        Some(CompactionPlan {
            strategy: self.id().to_string(),
            rationale,
            edits: vec![
                Edit::CacheEdit { tool_use_ids },
                Edit::InjectDigest { digest },
            ],
            context_tokens_before: before,
            context_tokens_after: projected.saturating_add(digest_overhead),
        })
    }
}
