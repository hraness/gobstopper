use serde::{Deserialize, Serialize};

/// One edit against a transcript. The IR is deliberately small; every
/// strategy lowers to these primitives and every adapter knows how to
/// execute them against its own store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Edit {
    /// Replace the payload of the given items with a short stub.
    /// Executed in place; the record's structural linkage is preserved.
    Elide {
        /// Backing-record indexes (see `TranscriptItem::line_index`).
        line_indexes: Vec<usize>,
        /// Stub template. `{bytes}` and `{kind}` are substituted.
        stub_template: String,
    },
    /// Insert a digest line at the tail of the transcript. The adapter
    /// renders it in the provider's own "context seed" shape.
    InjectDigest {
        /// The digest text: extracted state, not freeform summary.
        digest: DigestBlock,
    },
    /// Delegate to the provider's own compaction machinery.
    /// The transcript itself is not modified; the plan describes which
    /// provider-native control to invoke.
    ProviderCompact {
        /// Human description of the control to invoke, e.g.
        /// `codex thread/compact/start` or `claude --autocompact 250000`.
        control: String,
    },
}

/// Structured extraction written into the context after a transcript-path
/// compaction. Field-oriented rather than prose so the resumed agent can
/// trust it as state, not narrative.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DigestBlock {
    pub goal: Option<String>,
    pub decisions: Vec<String>,
    pub files_touched: Vec<String>,
    pub open_tasks: Vec<String>,
    /// Number of transcript items the digest replaces.
    pub covers_items: usize,
}

/// The output of evaluating a strategy against a transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionPlan {
    /// Id of the strategy that produced this plan.
    pub strategy: String,
    /// Why the plan fired (human-readable, no transcript content).
    pub rationale: String,
    pub edits: Vec<Edit>,
    /// Context estimate before applying the plan.
    pub context_tokens_before: u64,
    /// Context estimate after applying the plan.
    pub context_tokens_after: u64,
}

impl CompactionPlan {
    pub fn est_savings(&self) -> u64 {
        self.context_tokens_before
            .saturating_sub(self.context_tokens_after)
    }
}
