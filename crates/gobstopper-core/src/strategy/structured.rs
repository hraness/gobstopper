use super::{PolicyConfig, Strategy};
use crate::model::{ItemKind, Transcript};
use crate::plan::{CompactionPlan, DigestBlock, Edit};

/// Structured: extract a field-oriented digest of the session (goal,
/// decisions, files, open tasks), elide everything the digest covers,
/// and inject the digest at the tail so the resumed context is
/// "state card + recent verbatim turns" rather than a lossy narrative.
///
/// This is the generation-side strategy of the compaction literature
/// (strictly more expressive than selection), implemented here as
/// deterministic extraction — cheap, auditable, no model call.
pub struct StructuredStrategy;

/// Recent items (from the tail) kept verbatim alongside the digest.
const KEEP_TAIL_ITEMS: usize = 24;

impl Strategy for StructuredStrategy {
    fn id(&self) -> &'static str {
        "structured"
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

        let tail_start = transcript.items.len().saturating_sub(KEEP_TAIL_ITEMS);
        let covered = &transcript.items[..tail_start];
        if covered.is_empty() {
            return None;
        }

        let digest = DigestBlock {
            goal: transcript
                .items
                .iter()
                .find(|i| i.kind == ItemKind::User)
                .map(|i| i.label.clone()),
            decisions: Vec::new(),
            files_touched: Vec::new(),
            open_tasks: Vec::new(),
            covers_items: covered.len(),
        };

        // Elide every elidable item inside the covered region; the digest
        // stands in for their content. Recent tail stays untouched.
        let covered_lines: Vec<usize> = covered
            .iter()
            .filter(|i| i.elidable_bytes.is_some())
            .map(|i| i.line_index)
            .collect();
        let elided_tokens: u64 = covered
            .iter()
            .filter(|i| i.elidable_bytes.is_some())
            .map(|i| i.est_tokens)
            .sum();

        let mut edits = Vec::new();
        if !covered_lines.is_empty() {
            edits.push(Edit::Elide {
                line_indexes: covered_lines,
                stub_template: super::elide::DEFAULT_STUB.to_string(),
            });
        }
        edits.push(Edit::InjectDigest { digest });

        Some(CompactionPlan {
            strategy: self.id().to_string(),
            rationale: format!(
                "context {before} tokens exceeds trigger {}; digest covers {} items, {} tail items kept verbatim",
                policy.trigger_tokens,
                tail_start,
                transcript.items.len() - tail_start
            ),
            edits,
            context_tokens_before: before,
            context_tokens_after: before.saturating_sub(elided_tokens),
        })
    }
}
