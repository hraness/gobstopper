use super::{PolicyConfig, Strategy};
use crate::model::{Provider, Transcript};
use crate::plan::{CompactionPlan, Edit};

/// Sawtooth: let the provider's own compaction machinery do the rewrite,
/// just fire it far earlier than the provider default. Context rides up
/// to `trigger_tokens`, the provider summarizes down to its floor, and
/// the cycle repeats — a sawtooth instead of a plateau at the ceiling.
///
/// This is the safest strategy: no transcript surgery, and the summary
/// quality is whatever the provider ships. It exists because both
/// providers expose the trigger:
///   - Codex: `thread/compact/start` on the app-server protocol
///   - Claude Code: `--autocompact <tokens>` at launch, `/compact` mid-session
pub struct SawtoothStrategy;

impl Strategy for SawtoothStrategy {
    fn id(&self) -> &'static str {
        "sawtooth"
    }

    fn evaluate(
        &self,
        transcript: &Transcript,
        policy: &PolicyConfig,
    ) -> Option<CompactionPlan> {
        let before = transcript.context_tokens();
        if before < policy.effective_trigger() {
            return None;
        }
        let control = match transcript.session.provider {
            Provider::Codex => "codex app-server: thread/compact/start",
            Provider::ClaudeCode => "claude: /compact (or --autocompact at launch)",
        };
        Some(CompactionPlan {
            strategy: self.id().to_string(),
            rationale: format!(
                "context {before} tokens exceeds trigger {}",
                policy.trigger_tokens
            ),
            edits: vec![Edit::ProviderCompact {
                control: control.to_string(),
            }],
            context_tokens_before: before,
            // Provider decides its own floor; estimate policy floor.
            context_tokens_after: policy.floor_tokens,
        })
    }
}
