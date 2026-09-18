use super::{choose_with_digest, PolicyConfig, Strategy};
use crate::model::Transcript;
use crate::plan::{CompactionPlan, Edit};

/// Micro: keep only the last `k` outputs per tool label and stub the
/// rest. Inspired by Claude Code's tier-1 native compaction, but
/// deterministic and auditable in the vault.
pub struct MicroStrategy;

/// Number of most-recent occurrences of each tool label to retain.
const MICRO_KEEP_PER_LABEL: usize = 2;
pub const DEFAULT_STUB: &str = "[output elided by gobstopper: {bytes} bytes]";

impl Strategy for MicroStrategy {
    fn id(&self) -> &'static str {
        "micro"
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

        // For each tool *type* (ignoring tool arguments), keep the newest
        // MICRO_KEEP_PER_LABEL occurrences. Older instances are elision
        // candidates. Walk oldest->newest until the floor.
        let mut by_tool: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        for (pos, item) in elidable.iter().enumerate() {
            let tool = item
                .label
                .split(|c: char| ['(', ' '].contains(&c))
                .next()
                .unwrap_or("")
                .to_lowercase();
            by_tool.entry(tool).or_default().push(pos);
        }

        let mut keep: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for positions in by_tool.values() {
            for &p in positions.iter().rev().take(MICRO_KEEP_PER_LABEL) {
                keep.insert(p);
            }
        }

        let mut candidates: Vec<&crate::model::TranscriptItem> = elidable
            .iter()
            .enumerate()
            .filter(|(pos, _)| !keep.contains(pos))
            .map(|(_, item)| *item)
            .collect();
        candidates.sort_by_key(|i| i.line_index);

        let (chosen, digest, context_tokens_after) =
            choose_with_digest(transcript, policy.floor_tokens, &candidates)?;

        Some(CompactionPlan {
            strategy: self.id().to_string(),
            rationale: format!(
                "context {before} tokens exceeds trigger {}; micro-stubbing {} stale tool outputs",
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
