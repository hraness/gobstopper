use super::{ElideStrategy, PolicyConfig, Strategy};
use crate::estimate::estimate_tokens;
use crate::model::{ItemKind, Transcript};
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

    fn evaluate(&self, transcript: &Transcript, policy: &PolicyConfig) -> Option<CompactionPlan> {
        let mut plan = ElideStrategy.evaluate(transcript, policy)?;
        let elided_indexes: Vec<usize> = plan
            .edits
            .iter()
            .filter_map(|e| match e {
                Edit::Elide { line_indexes, .. } => Some(line_indexes.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        if elided_indexes.is_empty() {
            return None;
        }

        let mut decisions = Vec::new();
        let mut files_touched = Vec::new();
        for idx in &elided_indexes {
            if let Some(item) = transcript.items.iter().find(|i| i.line_index == *idx) {
                if let Some(summary) = &item.summary {
                    decisions.push(format!("{}: {}", item.label, summary));
                } else {
                    decisions.push(format!(
                        "{} elided ({} bytes)",
                        item.label,
                        item.elidable_bytes.unwrap_or(0)
                    ));
                }
                if item.kind == ItemKind::ToolResult {
                    files_touched.push(item.label.clone());
                }
            }
        }

        // Keep the digest itself bounded so the compacted record does not
        // re-expand the context window it is trying to shrink.
        const MAX_DECISIONS: usize = 8;
        decisions.truncate(MAX_DECISIONS);
        files_touched.truncate(MAX_DECISIONS);

        let goal = transcript
            .items
            .iter()
            .rev()
            .find(|i| i.kind == ItemKind::User)
            .map(|i| i.summary.clone().unwrap_or_else(|| i.label.clone()));

        let digest = DigestBlock {
            goal: goal.clone(),
            decisions,
            files_touched,
            open_tasks: Vec::new(),
            covers_items: elided_indexes.len(),
        };

        // Estimate the digest text size that the adapter will render, so the
        // `context_tokens_after` projection is honest and the no-op guard can
        // reject plans that would not actually shrink the provider context.
        let digest_chars: usize = goal.as_ref().map(|g| g.len()).unwrap_or(0)
            + digest.decisions.iter().map(|d| d.len()).sum::<usize>()
            + digest.files_touched.iter().map(|f| f.len()).sum::<usize>()
            + 64; // state-card framing
        let digest_overhead = estimate_tokens(digest_chars);

        plan.edits.push(Edit::InjectDigest { digest });
        plan.strategy = self.id().to_string();
        plan.rationale = format!(
            "context {} tokens exceeds trigger {}; emitting a compacted record for {} elided items",
            plan.context_tokens_before,
            policy.trigger_tokens,
            elided_indexes.len()
        );
        plan.context_tokens_after = plan.context_tokens_after.saturating_add(digest_overhead);
        Some(plan)
    }
}
