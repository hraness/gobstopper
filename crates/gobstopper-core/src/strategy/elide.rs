use super::{PolicyConfig, Strategy};
use crate::model::Transcript;
use crate::plan::{CompactionPlan, Edit};

/// Elide: replace old tool outputs with stubs, newest outputs preserved.
///
/// Research basis: observation masking — stale command output and file
/// dumps dominate agent transcripts and are almost never re-read with
/// precision; the model needs their *conclusions*, which live in the
/// surrounding assistant text. No LLM call required; fully deterministic.
pub struct ElideStrategy;

pub const DEFAULT_STUB: &str = "[output elided by gobstopper: {bytes} bytes]";

impl Strategy for ElideStrategy {
    fn id(&self) -> &'static str {
        "elide"
    }

    fn evaluate(
        &self,
        transcript: &Transcript,
        policy: &PolicyConfig,
    ) -> Option<CompactionPlan> {
        let before = transcript.context_tokens();
        if before < policy.trigger_tokens {
            return None;
        }

        // Candidates: elidable items, oldest first, except the tail
        // `keep_recent_tool_outputs` which stay verbatim.
        let elidable: Vec<&crate::model::TranscriptItem> = transcript
            .items
            .iter()
            .filter(|i| i.elidable_bytes.is_some())
            .collect();
        let keep_from = elidable
            .len()
            .saturating_sub(policy.keep_recent_tool_outputs);
        let candidates = &elidable[..keep_from.min(elidable.len())];

        // Walk oldest -> newest until the projected estimate reaches floor.
        let mut projected = before;
        let mut chosen = Vec::new();
        for item in candidates.iter() {
            if projected <= policy.floor_tokens {
                break;
            }
            chosen.push(item.line_index);
            projected = projected.saturating_sub(item.est_tokens);
        }
        if chosen.is_empty() {
            return None;
        }

        Some(CompactionPlan {
            strategy: self.id().to_string(),
            rationale: format!(
                "context {before} tokens exceeds trigger {}; eliding {} of {} stale tool outputs",
                policy.trigger_tokens,
                chosen.len(),
                elidable.len()
            ),
            edits: vec![Edit::Elide {
                line_indexes: chosen,
                stub_template: DEFAULT_STUB.to_string(),
            }],
            context_tokens_before: before,
            context_tokens_after: projected,
        })
    }
}
