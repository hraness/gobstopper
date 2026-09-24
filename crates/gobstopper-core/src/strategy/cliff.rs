use super::{PolicyConfig, Strategy};
use crate::model::{ItemKind, Transcript, TranscriptItem};
use crate::plan::{CompactionPlan, Edit};

/// Cliff: keep the head and the newest assistant steps byte-for-byte, and
/// drop every older tool result larger than `result_max_bytes`. Nothing is
/// summarized, nothing is truncated, and no state card is added, so the
/// result is a pure function of the source records that survive.
///
/// Research basis: CliffCompaction (Nguyen, Cho, Chen and Dettmers,
/// [arXiv:2609.26779](https://arxiv.org/abs/2609.26779), 2026), an API
/// proxy that keeps the system prompt, the task and the last `K` turns
/// verbatim, keeps tool results of at most 500 characters, drops longer
/// ones outright because the files behind them remain readable, and
/// rebuilds every compaction from the original history so a compaction is
/// never compacted again. Gobstopper's transcript model exposes tool
/// results as its only elidable payloads, so this strategy implements the
/// drop rule and the protected head and tail; tool-call signatures and
/// reasoning caps are not part of the file transform.
///
/// Because the strategy only removes payloads by class and never
/// generates text, compacting a copy again at a later cut yields the same
/// records a single compaction from the source would: the file-side
/// equivalent of never compacting a compaction. The original bytes stay
/// in the vault, not in the copy.
pub struct CliffStrategy;

pub const DEFAULT_STUB: &str = "[tool result dropped by gobstopper cliff: {bytes} bytes]";

/// Which side of the conversation a live record belongs to. A turn starts
/// where the assistant side resumes after a user prompt or a tool result,
/// so an assistant text record, its reasoning and its tool call form one
/// step together with the tool results that answer it.
fn assistant_side(item: &TranscriptItem) -> bool {
    matches!(
        item.kind,
        ItemKind::Assistant | ItemKind::ToolCall | ItemKind::Reasoning
    )
}

/// Positions (into `items`) where a live assistant step begins.
fn turn_starts(items: &[TranscriptItem]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut previous_assistant = false;
    for (position, item) in items.iter().enumerate() {
        if item.est_tokens == 0 {
            // Dead branches and records behind a provider compaction carry
            // no live context and cannot start or end a live step.
            continue;
        }
        let current = assistant_side(item);
        if current && !previous_assistant {
            starts.push(position);
        }
        previous_assistant = current;
    }
    starts
}

/// The half-open range of item positions the strategy may drop from: after
/// the head (everything before the first assistant step) and before the
/// newest `keep_recent_turns` steps. `None` when no step is old enough.
fn compactable_range(
    items: &[TranscriptItem],
    keep_recent_turns: usize,
) -> Option<std::ops::Range<usize>> {
    let starts = turn_starts(items);
    let older = crate::admission::unprotected_len(starts.len(), keep_recent_turns);
    if older == 0 {
        return None;
    }
    let head_end = starts[0];
    let tail_start = if older < starts.len() {
        starts[older]
    } else {
        items.len()
    };
    (head_end < tail_start).then_some(head_end..tail_start)
}

impl Strategy for CliffStrategy {
    fn id(&self) -> &'static str {
        "cliff"
    }

    fn evaluate(&self, transcript: &Transcript, policy: &PolicyConfig) -> Option<CompactionPlan> {
        let before = transcript.context_tokens();
        if before < policy.effective_trigger() {
            return None;
        }
        let range = compactable_range(&transcript.items, policy.keep_recent_turns)?;

        // The validator protects the newest `keep_recent_tool_outputs`
        // elidable records regardless of turn structure; respect the same
        // set so every emitted edit is admissible.
        let protected: std::collections::HashSet<usize> = transcript
            .items
            .iter()
            .rev()
            .filter(|i| i.is_elidable())
            .take(policy.keep_recent_tool_outputs)
            .map(|i| i.line_index)
            .collect();

        let mut chosen = Vec::new();
        let mut kept_small = 0usize;
        let mut projected = before;
        for item in &transcript.items[range.clone()] {
            if !item.is_elidable() || protected.contains(&item.line_index) {
                continue;
            }
            if item
                .elidable_bytes
                .is_some_and(|bytes| bytes > policy.result_max_bytes)
            {
                chosen.push(item.line_index);
                projected = projected.saturating_sub(item.estimated_elision_savings());
            } else {
                kept_small += 1;
            }
        }
        if chosen.is_empty() {
            return None;
        }
        chosen.sort_unstable();

        let steps = turn_starts(&transcript.items).len();
        Some(CompactionPlan {
            strategy: self.id().to_string(),
            rationale: format!(
                "context {before} tokens exceeds trigger {}; head and newest {} of {} assistant steps kept verbatim; dropping {} older tool results over {} bytes, keeping {} smaller ones",
                policy.trigger_tokens,
                policy.keep_recent_turns.min(steps),
                steps,
                chosen.len(),
                policy.result_max_bytes,
                kept_small
            ),
            edits: vec![Edit::Elide {
                line_indexes: chosen,
                stub_template: DEFAULT_STUB.to_string(),
                per_item_stubs: Default::default(),
            }],
            context_tokens_before: before,
            context_tokens_after: projected,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ContextState, Provider, SessionHandle, UsageSample};
    use std::path::PathBuf;

    fn item(line: usize, kind: ItemKind, bytes: u64) -> TranscriptItem {
        let elidable = kind == ItemKind::ToolResult && bytes > 256;
        TranscriptItem {
            line_index: line,
            kind,
            est_tokens: crate::estimate::estimate_tokens(bytes as usize),
            elidable_bytes: elidable.then_some(bytes),
            elidable_parts: u32::from(elidable),
            label: format!("{kind:?}"),
            summary: None,
            uuid: None,
            parent_uuid: None,
            tool_use_ids: Vec::new(),
            payload_sha256: None,
        }
    }

    /// One assistant step: reasoning, a tool call and its result of `bytes`.
    fn step(line: &mut usize, bytes: u64) -> Vec<TranscriptItem> {
        let mut items = Vec::new();
        for (kind, size) in [
            (ItemKind::Reasoning, 200),
            (ItemKind::ToolCall, 120),
            (ItemKind::ToolResult, bytes),
        ] {
            items.push(item(*line, kind, size));
            *line += 1;
        }
        items
    }

    fn transcript(items: Vec<TranscriptItem>) -> Transcript {
        let context_tokens = items.iter().map(|i| i.est_tokens).sum();
        Transcript {
            session: SessionHandle {
                provider: Provider::Codex,
                session_id: "s".into(),
                path: PathBuf::from("/tmp/s.jsonl"),
                cwd: None,
                age_secs: u64::MAX,
            },
            items,
            usage: UsageSample {
                context_tokens,
                context_state: ContextState::Reported,
                ..Default::default()
            },
        }
    }

    /// System prompt, task, then `steps` assistant steps with the given
    /// tool-result sizes.
    fn session(sizes: &[u64]) -> Transcript {
        let mut line = 0;
        let mut items = vec![item(0, ItemKind::System, 900), item(1, ItemKind::User, 400)];
        line += 2;
        for size in sizes {
            items.extend(step(&mut line, *size));
        }
        transcript(items)
    }

    fn policy() -> PolicyConfig {
        PolicyConfig {
            trigger_tokens: 1,
            floor_tokens: 0,
            keep_recent_tool_outputs: 0,
            keep_recent_turns: 3,
            result_max_bytes: 500,
            min_interval_secs: 0,
            ..Default::default()
        }
    }

    fn elided(plan: &CompactionPlan) -> Vec<usize> {
        plan.edits
            .iter()
            .flat_map(|e| match e {
                Edit::Elide { line_indexes, .. } => line_indexes.clone(),
                _ => Vec::new(),
            })
            .collect()
    }

    /// Simulate applying the plan to a copy: dropped payloads become short
    /// stubs, which the adapters then no longer report as elidable.
    fn apply(transcript: &Transcript, plan: &CompactionPlan) -> Transcript {
        let dropped: std::collections::HashSet<usize> = elided(plan).into_iter().collect();
        let mut copy = transcript.clone();
        for item in &mut copy.items {
            if dropped.contains(&item.line_index) {
                item.est_tokens = crate::estimate::estimate_tokens(DEFAULT_STUB.len());
                item.elidable_bytes = None;
                item.elidable_parts = 0;
            }
        }
        copy.usage.context_tokens = copy.items.iter().map(|i| i.est_tokens).sum();
        copy
    }

    #[test]
    fn turns_start_where_the_assistant_side_resumes() {
        let t = session(&[1_000, 1_000, 1_000]);
        // Head is items 0..2; each step starts at its reasoning record.
        assert_eq!(turn_starts(&t.items), vec![2, 5, 8]);
        assert_eq!(compactable_range(&t.items, 1), Some(2..8));
        assert_eq!(compactable_range(&t.items, 3), None);
        assert_eq!(compactable_range(&t.items, 0), Some(2..11));
        assert_eq!(compactable_range(&t.items, usize::MAX), None);
    }

    #[test]
    fn dead_records_never_start_a_step() {
        let mut t = session(&[1_000, 1_000]);
        for item in &mut t.items[..5] {
            item.est_tokens = 0;
            item.elidable_bytes = None;
        }
        assert_eq!(turn_starts(&t.items), vec![5]);
    }

    #[test]
    fn keeps_head_and_newest_steps_and_small_results() {
        // Six steps: sizes alternate large / small. With three recent steps
        // protected, only large results in steps 1-3 are dropped.
        let t = session(&[2_000, 300, 4_000, 2_000, 8_000, 300]);
        let plan = CliffStrategy.evaluate(&t, &policy()).unwrap();
        // Step 1 result is line 4, step 3 result is line 10. Step 2 (line 7)
        // is 300 bytes: elidable, but at or below result_max_bytes.
        assert_eq!(elided(&plan), vec![4, 10]);
        assert!(plan.rationale.contains("keeping 1 smaller"));
        assert!(plan.context_tokens_after < plan.context_tokens_before);
        // Every edit lands inside the validator's admissible set.
        assert!(crate::validation::validate_edits(&t, &policy(), &plan.edits).is_ok());
    }

    #[test]
    fn respects_the_protected_recent_tool_outputs() {
        let t = session(&[2_000, 2_000, 2_000, 2_000, 2_000]);
        let mut p = policy();
        p.keep_recent_turns = 1;
        p.keep_recent_tool_outputs = 3;
        let plan = CliffStrategy.evaluate(&t, &p).unwrap();
        // Five results at lines 4, 7, 10, 13, 16; the newest three are
        // protected by policy, the newest step by the turn rule.
        assert_eq!(elided(&plan), vec![4, 7]);
        assert!(crate::validation::validate_edits(&t, &p, &plan.edits).is_ok());
    }

    #[test]
    fn under_trigger_or_without_old_steps_produces_no_plan() {
        let t = session(&[2_000, 2_000, 2_000]);
        let mut p = policy();
        p.trigger_tokens = 1_000_000;
        assert!(CliffStrategy.evaluate(&t, &p).is_none());
        // Three steps, three protected: nothing is old enough.
        assert!(CliffStrategy.evaluate(&t, &policy()).is_none());
        // Old steps whose results are all small: nothing to drop.
        let small = session(&[300, 300, 300, 300, 300]);
        assert!(CliffStrategy.evaluate(&small, &policy()).is_none());
        assert!(CliffStrategy
            .evaluate(&transcript(Vec::new()), &policy())
            .is_none());
    }

    #[test]
    fn result_size_rule_is_a_strict_threshold() {
        let t = session(&[500, 501, 2_000, 2_000, 2_000]);
        let plan = CliffStrategy.evaluate(&t, &policy()).unwrap();
        // 500 bytes stays; 501 bytes goes.
        assert_eq!(elided(&plan), vec![7]);
        let mut everything = policy();
        everything.result_max_bytes = 0;
        let plan = CliffStrategy.evaluate(&t, &everything).unwrap();
        assert_eq!(elided(&plan), vec![4, 7]);
    }

    #[test]
    fn compacting_a_compaction_equals_compacting_the_source() {
        // Never compact a compaction: dropping by class from a copy that
        // was already compacted at an earlier cut selects exactly the
        // records a single compaction from the source would select.
        let early = session(&[2_000, 300, 4_000, 2_000, 8_000, 300]);
        let first = CliffStrategy.evaluate(&early, &policy()).unwrap();
        let mut copy = apply(&early, &first);
        let mut full = early.clone();
        let mut line = full.items.len();
        let mut copy_line = line;
        for size in [3_000u64, 300, 5_000, 2_000] {
            full.items.extend(step(&mut line, size));
            copy.items.extend(step(&mut copy_line, size));
        }
        full.usage.context_tokens = full.items.iter().map(|i| i.est_tokens).sum();
        copy.usage.context_tokens = copy.items.iter().map(|i| i.est_tokens).sum();

        let again = CliffStrategy.evaluate(&copy, &policy()).unwrap();
        let once = CliffStrategy.evaluate(&full, &policy()).unwrap();
        let mut composed = elided(&first);
        composed.extend(elided(&again));
        composed.sort_unstable();
        assert_eq!(composed, elided(&once));
        // And the copy's surviving records are byte-identical in kind: the
        // second pass never touches what the first pass left in place.
        assert!(elided(&again).iter().all(|l| !elided(&first).contains(l)));
        let twice = apply(&copy, &again);
        let direct = apply(&full, &once);
        assert_eq!(
            twice
                .items
                .iter()
                .map(|i| (i.line_index, i.est_tokens, i.elidable_bytes))
                .collect::<Vec<_>>(),
            direct
                .items
                .iter()
                .map(|i| (i.line_index, i.est_tokens, i.elidable_bytes))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_pass_is_idempotent_at_the_same_cut() {
        let t = session(&[2_000, 300, 4_000, 2_000, 8_000, 300]);
        let plan = CliffStrategy.evaluate(&t, &policy()).unwrap();
        let copy = apply(&t, &plan);
        assert!(CliffStrategy.evaluate(&copy, &policy()).is_none());
    }
}
