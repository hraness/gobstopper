mod agentic;
mod auto;
mod elide;
mod sawtooth;
mod structured;

pub use agentic::AgenticStrategy;
pub use auto::AutoStrategy;
pub use elide::ElideStrategy;
pub use sawtooth::SawtoothStrategy;
pub use structured::StructuredStrategy;

use crate::model::Transcript;
use crate::plan::CompactionPlan;
use serde::{Deserialize, Serialize};

/// Resolved policy for one session evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyConfig {
    /// Fire when the session's context estimate exceeds this many tokens.
    pub trigger_tokens: u64,
    /// Post-compaction occupancy target for transcript-path strategies.
    pub floor_tokens: u64,
    /// Tool outputs newer than this many from the tail are never elided.
    pub keep_recent_tool_outputs: usize,
    /// Minimum seconds between compactions of one session.
    pub min_interval_secs: u64,
}

impl Default for PolicyConfig {
    /// Research-backed default: fire well below the provider's own
    /// threshold (~60% of a ~400k effective window, ~25% of a 1M window).
    fn default() -> Self {
        Self {
            trigger_tokens: 250_000,
            floor_tokens: 40_000,
            keep_recent_tool_outputs: 8,
            min_interval_secs: 300,
        }
    }
}

/// A compaction strategy. Pure: inspects the transcript, returns a plan
/// or `None` when the session is under threshold.
pub trait Strategy {
    fn id(&self) -> &'static str;
    fn evaluate(&self, transcript: &Transcript, policy: &PolicyConfig)
        -> Option<CompactionPlan>;
}

/// Every built-in strategy, in registry order.
pub fn builtin_strategies() -> Vec<Box<dyn Strategy>> {
    vec![
        Box::new(AutoStrategy),
        Box::new(SawtoothStrategy),
        Box::new(ElideStrategy),
        Box::new(StructuredStrategy),
        Box::new(AgenticStrategy),
    ]
}

/// Look up a built-in strategy by id.
pub fn strategy_by_id(id: &str) -> Option<Box<dyn Strategy>> {
    builtin_strategies()
        .into_iter()
        .find(|s| s.id() == id)
}
