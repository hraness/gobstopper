use super::{state_card_digest, PolicyConfig, Strategy};
use crate::estimate::estimate_tokens;
use crate::model::Transcript;
use crate::plan::{CompactionPlan, Edit};

/// Dedupe: collapse exact-duplicate tool outputs, keeping only the
/// newest occurrence of each identical (label, summary) pair.
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

        // Group exact duplicates by (label, summary) and keep the newest in
        // each group. Older duplicates in the group are elision candidates.
        let mut groups: std::collections::HashMap<(String, String), Vec<usize>> =
            std::collections::HashMap::new();
        for (pos, item) in candidates.iter().enumerate() {
            let key = (item.label.clone(), item.summary.clone().unwrap_or_default());
            groups.entry(key).or_default().push(pos);
        }

        // Build the set of positions to keep (newest in each duplicate group
        // plus any singletons). Everything else becomes elision candidates.
        let mut keep: std::collections::HashSet<usize> = std::collections::HashSet::new();
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

        let mut projected = before;
        let mut chosen = Vec::new();
        for item in dedup_candidates {
            if projected <= policy.floor_tokens {
                break;
            }
            chosen.push(item.line_index);
            projected = projected.saturating_sub(item.estimated_elision_savings());
        }
        if chosen.is_empty() {
            return None;
        }

        let digest = state_card_digest(transcript, &chosen);
        let digest_chars: usize = digest.goal.as_ref().map(|g| g.len()).unwrap_or(0)
            + digest.decisions.iter().map(|d| d.len()).sum::<usize>()
            + digest.files_touched.iter().map(|f| f.len()).sum::<usize>()
            + 64;
        let digest_overhead = estimate_tokens(digest_chars);

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
            context_tokens_after: projected.saturating_add(digest_overhead),
        })
    }
}
