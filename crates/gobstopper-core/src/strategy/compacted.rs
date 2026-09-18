use super::{choose_with_digest, PolicyConfig, Strategy};
use crate::model::Transcript;
use crate::plan::{CompactionPlan, Edit};

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

    fn evaluate(&self, transcript: &Transcript, policy: &PolicyConfig) -> Option<CompactionPlan> {
        let before = transcript.context_tokens();
        if before < policy.effective_trigger() {
            return None;
        }
        let elidable: Vec<_> = transcript
            .items
            .iter()
            .filter(|item| item.elidable_bytes.is_some())
            .collect();
        let keep_from = elidable
            .len()
            .saturating_sub(policy.keep_recent_tool_outputs);
        let candidates = &elidable[..keep_from.min(elidable.len())];
        let (elided_indexes, digest, context_tokens_after) =
            choose_with_digest(transcript, policy.floor_tokens, candidates)?;
        Some(CompactionPlan {
            strategy: self.id().to_string(),
            rationale: format!(
                "context {before} tokens exceeds trigger {}; emitting a compacted record for {} elided items",
                policy.trigger_tokens,
                elided_indexes.len()
            ),
            edits: vec![
                Edit::Elide {
                    line_indexes: elided_indexes,
                    stub_template: super::elide::DEFAULT_STUB.to_string(),
                },
                Edit::InjectDigest { digest },
            ],
            context_tokens_before: before,
            context_tokens_after,
        })
    }
}
