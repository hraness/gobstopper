use super::{choose_with_digest, PolicyConfig, Strategy};
use crate::model::Transcript;
use crate::plan::{CompactionPlan, Edit};

/// Dedupe: collapse exact-duplicate tool outputs, keeping only the
/// newest occurrence of each identical (tool label, payload SHA-256) pair.
///
/// Research basis: repeated `ls`, `cat`, `grep`, or `read_file` of the
/// same state are common in long agent sessions; only the latest value
/// is usually needed for task continuity. Deterministic, no API call.
pub struct DedupeStrategy;

pub const DEFAULT_STUB: &str = "[output elided by gobstopper: {bytes} bytes]";

impl Strategy for DedupeStrategy {
    fn id(&self) -> &'static str {
        "dedupe"
    }

    fn evaluate(&self, transcript: &Transcript, policy: &PolicyConfig) -> Option<CompactionPlan> {
        let before = transcript.context_tokens();
        if before < policy.effective_trigger() {
            return None;
        }

        // Candidates: elidable items, newest last, except the protected tail.
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

        // Group exact duplicates by (label, payload digest) and keep the newest in
        // each group. Older duplicates in the group are elision candidates.
        let mut groups: std::collections::HashMap<(String, String), Vec<usize>> =
            std::collections::HashMap::new();
        let mut keep: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for (pos, item) in candidates.iter().enumerate() {
            if let Some(digest) = &item.payload_sha256 {
                groups
                    .entry((item.label.clone(), digest.clone()))
                    .or_default()
                    .push(pos);
            } else {
                keep.insert(pos);
            }
        }

        // Build the set of positions to keep (newest in each duplicate group
        // plus any singletons). Everything else becomes elision candidates.
        for positions in groups.values() {
            if let Some(&newest) = positions.last() {
                keep.insert(newest);
            }
        }

        let mut dedup_candidates: Vec<&crate::model::TranscriptItem> = candidates
            .iter()
            .enumerate()
            .filter(|(pos, _)| !keep.contains(pos))
            .map(|(_, item)| *item)
            .collect();
        // Oldest first so the prefix is preserved as long as possible.
        dedup_candidates.sort_by_key(|i| i.line_index);

        let (chosen, digest, context_tokens_after) =
            choose_with_digest(transcript, policy.floor_tokens, &dedup_candidates)?;

        Some(CompactionPlan {
            strategy: self.id().to_string(),
            rationale: format!(
                "context {before} tokens exceeds trigger {}; deduping {} stale tool outputs",
                policy.trigger_tokens,
                chosen.len()
            ),
            edits: vec![
                Edit::Elide {
                    line_indexes: chosen,
                    stub_template: DEFAULT_STUB.to_string(),
                },
                Edit::InjectDigest { digest },
            ],
            context_tokens_before: before,
            context_tokens_after,
        })
    }
}
