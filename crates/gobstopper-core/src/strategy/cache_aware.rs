use super::elide::DEFAULT_STUB;
use super::{choose_with_digest, PolicyConfig, Strategy};
use crate::model::Transcript;
use crate::plan::{CompactionPlan, Edit};

/// Cache-aware: compact by eliding the *latest* stale tool outputs before
/// the protected tail, then inject a state-card digest. Eliding a suffix
/// (rather than the oldest prefix) keeps the earliest conversation records
/// byte-identical, which preserves the provider's prompt-cache prefix.
///
/// The digest uses the same bounded, field-oriented shape as the
/// `compacted` strategy; on Codex it can be lowered to a provider-native
/// `compacted` record by the CLI.
pub struct CacheAwareStrategy;

impl Strategy for CacheAwareStrategy {
    fn id(&self) -> &'static str {
        "cache_aware"
    }

    fn evaluate(&self, transcript: &Transcript, policy: &PolicyConfig) -> Option<CompactionPlan> {
        let before = transcript.context_tokens();
        if before < policy.effective_trigger() {
            return None;
        }

        let elidable: Vec<&crate::model::TranscriptItem> = transcript
            .items
            .iter()
            .filter(|i| i.is_elidable())
            .collect();
        let keep_from =
            crate::admission::unprotected_len(elidable.len(), policy.keep_recent_tool_outputs);
        let candidates = &elidable[..keep_from.min(elidable.len())];
        if candidates.is_empty() {
            return None;
        }

        // Elide from the newest candidate backward until we are at or below
        // the floor. This leaves the conversation prefix untouched for as
        // long as possible, preserving prompt-cache hits on the next resume.
        let priority: Vec<_> = candidates.iter().rev().copied().collect();
        let (chosen, digest, context_tokens_after) =
            choose_with_digest(transcript, policy.floor_tokens, &priority)?;

        let first_elided = chosen.first().copied().unwrap_or(0);
        let prefix_items = transcript
            .items
            .iter()
            .filter(|i| i.line_index < first_elided)
            .count();

        Some(CompactionPlan {
            strategy: self.id().to_string(),
            rationale: format!(
                "context {before} tokens exceeds trigger {}; eliding {} latest stale outputs before tail to keep {} prefix records in cache",
                policy.trigger_tokens,
                chosen.len(),
                prefix_items
            ),
            edits: vec![
                Edit::Elide {
                    line_indexes: chosen,
                    stub_template: DEFAULT_STUB.to_string(),
                    per_item_stubs: Default::default(),
                },
                Edit::InjectDigest { digest },
            ],
            context_tokens_before: before,
            context_tokens_after,
        })
    }
}
