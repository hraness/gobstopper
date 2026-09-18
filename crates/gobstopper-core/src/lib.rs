//! gobstopper-core: provider-neutral transcript model and the compaction
//! strategy engine.
//!
//! The strategy engine is intentionally I/O-free: adapters parse provider
//! stores into [`model::Transcript`], strategies lower to [`plan::Edit`]
//! primitives, and adapters execute the edits. Nothing here knows about
//! JSONL dialects, file watching, or provider CLIs. [`events`] is the one
//! exception: it appends numeric-only compaction telemetry records.

pub mod estimate;
pub mod events;
pub mod model;
pub mod plan;
pub mod policy;
pub mod probe;
pub mod strategy;
pub mod validation;

pub use events::{append_event, default_log_path, read_events, CompactionEvent};
pub use model::{ItemKind, Provider, SessionHandle, Transcript, TranscriptItem, UsageSample};
pub use plan::{CompactionPlan, DigestBlock, Edit};
pub use policy::{adapt, AdaptiveOutcome, AdaptiveSample};
pub use probe::{KindTally, Probe, ProbeKind, ProbeScore};
pub use strategy::{
    builtin_strategies, strategy_by_id, HeuristicScorer, PolicyConfig, QuotaPressure, ScoreDriver,
    ScoredItem, ScoredStrategy, Strategy,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::{AutoStrategy, ElideStrategy, SawtoothStrategy, StructuredStrategy};
    use std::path::PathBuf;

    fn item(line: usize, kind: ItemKind, est_tokens: u64, elidable: bool) -> TranscriptItem {
        TranscriptItem {
            line_index: line,
            kind,
            est_tokens,
            elidable_bytes: elidable.then_some(est_tokens * 4),
            elidable_parts: 1,
            label: format!("item-{line}"),
            summary: None,
            uuid: None,
            parent_uuid: None,
            tool_use_ids: Vec::new(),
            payload_sha256: None,
        }
    }

    fn transcript(items: Vec<TranscriptItem>, context_tokens: u64) -> Transcript {
        Transcript {
            session: SessionHandle {
                provider: Provider::Codex,
                session_id: "s".into(),
                path: PathBuf::from("/tmp/s.jsonl"),
                cwd: None,
                age_secs: u64::MAX, // cold: not active
            },
            items,
            usage: UsageSample {
                context_tokens,
                ..Default::default()
            },
        }
    }

    fn policy() -> PolicyConfig {
        PolicyConfig {
            trigger_tokens: 1_000,
            floor_tokens: 300,
            keep_recent_tool_outputs: 2,
            min_interval_secs: 0,
            quota_pressure: QuotaPressure::Normal,
            ..Default::default()
        }
    }

    #[test]
    fn under_threshold_produces_no_plan() {
        let t = transcript(vec![item(0, ItemKind::User, 100, false)], 500);
        assert!(SawtoothStrategy.evaluate(&t, &policy()).is_none());
        assert!(ElideStrategy.evaluate(&t, &policy()).is_none());
        assert!(StructuredStrategy.evaluate(&t, &policy()).is_none());
    }

    #[test]
    fn sawtooth_delegates_to_provider() {
        let t = transcript(vec![item(0, ItemKind::User, 100, false)], 5_000);
        let plan = SawtoothStrategy.evaluate(&t, &policy()).unwrap();
        assert!(matches!(plan.edits[0], Edit::ProviderCompact { .. }));
    }

    #[test]
    fn high_quota_pressure_fires_below_configured_trigger() {
        // Context 750 is under the configured 1_000 trigger but over the
        // High-pressure effective trigger of 700.
        let t = transcript(vec![item(0, ItemKind::User, 100, false)], 750);
        assert!(SawtoothStrategy.evaluate(&t, &policy()).is_none());

        let mut pressured = policy();
        pressured.quota_pressure = QuotaPressure::High;
        assert_eq!(pressured.effective_trigger(), 700);
        assert!(SawtoothStrategy.evaluate(&t, &pressured).is_some());

        // Same for a transcript-path strategy: enough elidable items to
        // clear the keep_recent_tool_outputs tail guard.
        let tool_items: Vec<TranscriptItem> = (0..5)
            .map(|i| item(i, ItemKind::ToolResult, 400, true))
            .collect();
        let tool_heavy = transcript(tool_items, 750);
        assert!(ElideStrategy.evaluate(&tool_heavy, &policy()).is_none());
        assert!(ElideStrategy.evaluate(&tool_heavy, &pressured).is_some());
    }

    #[test]
    fn elide_walks_oldest_first_and_keeps_tail() {
        let items: Vec<TranscriptItem> = (0..10)
            .map(|i| item(i, ItemKind::ToolResult, 400, true))
            .collect();
        let t = transcript(items, 4_000);
        let plan = ElideStrategy.evaluate(&t, &policy()).unwrap();
        let Edit::Elide { line_indexes, .. } = &plan.edits[0] else {
            panic!("expected elide edit")
        };
        // tail two (8,9) kept; 0..=7 elidable but only enough to reach floor
        assert!(!line_indexes.contains(&8) && !line_indexes.contains(&9));
        assert_eq!(line_indexes.first(), Some(&0));
        assert!(plan.context_tokens_after <= 4_000);
    }

    #[test]
    fn auto_picks_elide_for_tool_heavy() {
        let mut items = vec![item(0, ItemKind::User, 100, false)];
        for i in 1..20 {
            items.push(item(i, ItemKind::ToolResult, 500, true));
        }
        let t = transcript(items, 9_500);
        assert_eq!(AutoStrategy::select(&t), "cache_aware");
        assert!(AutoStrategy.evaluate(&t, &policy()).is_some());
    }

    #[test]
    fn auto_picks_sawtooth_for_live_session() {
        let mut t = transcript(vec![item(0, ItemKind::User, 100, false)], 5_000);
        t.session.age_secs = 5; // hot
        assert_eq!(AutoStrategy::select(&t), "sawtooth");
    }

    #[test]
    fn auto_picks_structured_for_chatty() {
        let items = (0..60)
            .map(|i| {
                item(
                    i,
                    if i % 2 == 0 {
                        ItemKind::User
                    } else {
                        ItemKind::Assistant
                    },
                    100,
                    false,
                )
            })
            .collect();
        let t = transcript(items, 6_000);
        assert_eq!(AutoStrategy::select(&t), "structured");
    }
}
