use super::{choose_with_digest, PolicyConfig, Strategy};
use crate::model::Transcript;
use crate::plan::{CompactionPlan, Edit};

/// Middle: keep the first and last `keep_recent_tool_outputs` tool
/// results and stub the middle. Mitigates "lost in the middle" by
/// preserving both the early setup context and the recent tail while
/// compressing the high-noise interior.
pub struct MiddleStrategy;

pub const DEFAULT_STUB: &str = "[output elided by gobstopper: {bytes} bytes]";

impl Strategy for MiddleStrategy {
    fn id(&self) -> &'static str {
        "middle"
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
        if elidable.is_empty() {
            return None;
        }

        let keep_each = policy.keep_recent_tool_outputs;
        let head = keep_each.min(elidable.len());
        let tail = keep_each.min(elidable.len().saturating_sub(head));
        let head_set: std::collections::HashSet<usize> = (0..head).collect();
        let tail_set: std::collections::HashSet<usize> =
            (elidable.len() - tail..elidable.len()).collect();

        let mut candidates: Vec<&crate::model::TranscriptItem> = elidable
            .iter()
            .enumerate()
            .filter(|(pos, _)| !head_set.contains(pos) && !tail_set.contains(pos))
            .map(|(_, item)| *item)
            .collect();
        candidates.sort_by_key(|i| i.line_index);

        let (chosen, digest, context_tokens_after) =
            choose_with_digest(transcript, policy.floor_tokens, &candidates)?;

        Some(CompactionPlan {
            strategy: self.id().to_string(),
            rationale: format!(
                "context {before} tokens exceeds trigger {}; keeping {head} head + {tail} tail, eliding {} middle outputs",
                policy.trigger_tokens,
                chosen.len()
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
