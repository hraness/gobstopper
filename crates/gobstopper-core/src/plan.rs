use serde::{Deserialize, Serialize};

/// One edit against a transcript. The IR is deliberately small; every
/// strategy lowers to these primitives and every adapter knows how to
/// execute them against its own store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    /// Replace the payload of the given items with a short stub.
    /// Executed in place; the record's structural linkage is preserved.
    Elide {
        /// Backing-record indexes (see `TranscriptItem::line_index`).
        line_indexes: Vec<usize>,
        /// Stub template. `{bytes}` and `{kind}` are substituted.
        stub_template: String,
        /// Per-record stub text overriding the template for specific line
        /// indexes (e.g. a model-written one-line digest). Values are
        /// complete stub text — no `{bytes}`/`{kind}` substitution applies.
        #[serde(default)]
        per_item_stubs: std::collections::BTreeMap<usize, String>,
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
    /// Claude-only: remove tool results by `tool_use_id` without
    /// invalidating the cached prefix. File-surgery adapters leave this
    /// as metadata; the caller must pass the ids to the Anthropic API.
    CacheEdit {
        /// `tool_use_id`s to drop at the next request.
        tool_use_ids: Vec<String>,
    },
}

/// Structured extraction written into the context after a transcript-path
/// compaction. Field-oriented rather than prose so the resumed agent can
/// trust it as state, not narrative.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DigestBlock {
    /// The user's current goal / intent.
    pub goal: Option<String>,
    /// One-line summary of what the digest represents.
    pub summary: Option<String>,
    /// Key concepts / tools / operations the elided work produced.
    pub concepts: Vec<String>,
    /// Files, paths, or URLs touched by the elided tool results.
    pub files_touched: Vec<String>,
    /// Concrete decisions or takeaways from the elided work.
    pub decisions: Vec<String>,
    /// Errors, failures, or warnings the elided region contained.
    pub errors: Vec<String>,
    /// Open tasks / todos still pending.
    pub open_tasks: Vec<String>,
    /// What the assistant was most recently working on.
    pub current_work: Option<String>,
    /// Provider and working context (no absolute paths).
    pub context: Option<String>,
    /// Number of transcript items the digest replaces.
    pub covers_items: usize,
}

impl DigestBlock {
    /// Marker that identifies a gobstopper state card in a transcript
    /// record's text content.
    pub const MARKER: &'static str = "[gobstopper state card]";

    /// Parse the prose form produced by the adapters back into a
    /// `DigestBlock`. Returns `None` if the text does not carry the marker.
    pub fn parse(text: &str) -> Option<Self> {
        if !text.starts_with(Self::MARKER) {
            return None;
        }
        let mut goal = None;
        let mut summary = None;
        let mut concepts = Vec::new();
        let mut files_touched = Vec::new();
        let mut decisions = Vec::new();
        let mut errors = Vec::new();
        let mut open_tasks = Vec::new();
        let mut current_work = None;
        let mut context = None;
        let mut covers_items = 0;
        for line in text.lines().skip(1) {
            if let Some(g) = line.strip_prefix("goal: ") {
                goal = Some(g.to_string());
            } else if let Some(s) = line.strip_prefix("summary: ") {
                summary = Some(s.to_string());
            } else if let Some(c) = line.strip_prefix("concept: ") {
                concepts.push(c.to_string());
            } else if let Some(f) = line.strip_prefix("file: ") {
                files_touched.push(f.to_string());
            } else if let Some(d) = line.strip_prefix("decision: ") {
                decisions.push(d.to_string());
            } else if let Some(e) = line.strip_prefix("error: ") {
                errors.push(e.to_string());
            } else if let Some(t) = line.strip_prefix("todo: ") {
                open_tasks.push(t.to_string());
            } else if let Some(w) = line.strip_prefix("current: ") {
                current_work = Some(w.to_string());
            } else if let Some(x) = line.strip_prefix("context: ") {
                context = Some(x.to_string());
            } else if let Some(rest) = line.strip_prefix("(covers ") {
                if let Some(n) = rest
                    .strip_suffix(" earlier records)")
                    .and_then(|s| s.parse::<usize>().ok())
                {
                    covers_items = n;
                }
            }
        }
        Some(DigestBlock {
            goal,
            summary,
            concepts,
            files_touched,
            decisions,
            errors,
            open_tasks,
            current_work,
            context,
            covers_items,
        })
    }

    /// Estimated token overhead of rendering this digest as prose. This
    /// keeps every strategy's `context_tokens_after` honest without
    /// duplicating the char-count math in each strategy module.
    pub fn estimate_overhead(&self) -> u64 {
        let chars = self.goal.as_ref().map(|s| s.len()).unwrap_or(0)
            + self.summary.as_ref().map(|s| s.len()).unwrap_or(0)
            + self.concepts.iter().map(|s| s.len()).sum::<usize>()
            + self.files_touched.iter().map(|s| s.len()).sum::<usize>()
            + self.decisions.iter().map(|s| s.len()).sum::<usize>()
            + self.errors.iter().map(|s| s.len()).sum::<usize>()
            + self.open_tasks.iter().map(|s| s.len()).sum::<usize>()
            + self.current_work.as_ref().map(|s| s.len()).unwrap_or(0)
            + self.context.as_ref().map(|s| s.len()).unwrap_or(0)
            + 64; // state-card framing
        crate::estimate::estimate_tokens(chars)
    }
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
