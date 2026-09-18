use super::{PolicyConfig, Strategy};
use crate::model::Transcript;
use crate::plan::{CompactionPlan, Edit};
use serde::{Deserialize, Serialize};

/// Agentic: hand the transcript to a small editor model that emits edits
/// through a fixed tool schema. The strategy owns the schema and the
/// validation; the model driver is pluggable (`EditorDriver` below).
///
/// Research basis: SelfCompact (arXiv:2606.23525) shows a model choosing
/// *when* and *how* to compact beats fixed-interval triggers at 30-70%
/// lower cost, provided the scaffold supplies both the tool and a rubric.
/// `agentic` supplies the tool; `auto`'s selection rules act as the rubric
/// when no driver is configured.
pub struct AgenticStrategy;

/// Tools the editor model may call. This schema is the contract a driver
/// presents to the model; emitted calls are validated back into `Edit`s.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum EditorCall {
    /// Keep a range verbatim.
    Keep { from_item: usize, to_item: usize },
    /// Replace item payloads with stubs.
    Elide { items: Vec<usize> },
    /// Replace a range with a structured digest the model writes itself.
    Summarize {
        from_item: usize,
        to_item: usize,
        digest: String,
    },
    /// Take no action; the session should not compact now.
    Defer { reason: String },
}

/// A model backend that can drive the editor schema. Implemented by
/// userspace drivers (CLI preset commands, future embedded runtimes);
/// never required by the deterministic strategies.
#[allow(dead_code)]
pub trait EditorDriver {
    /// Present the transcript and tool schema; return the model's calls.
    fn edit(&self, transcript: &Transcript, budget_tokens: u64) -> Vec<EditorCall>;
}

impl AgenticStrategy {
    /// Validate raw model calls into a bounded edit plan. Calls that drop
    /// the protected tail or exceed the item space are discarded — the
    /// model proposes, the strategy disposes.
    pub fn calls_to_plan(
        transcript: &Transcript,
        policy: &PolicyConfig,
        calls: &[EditorCall],
    ) -> Option<CompactionPlan> {
        let before = transcript.context_tokens();
        let mut edits = Vec::new();
        let mut projected = before;
        if calls.len() > crate::validation::MAX_EDITS {
            return None;
        }
        let mut kept = std::collections::HashSet::new();
        for call in calls {
            if let EditorCall::Keep { from_item, to_item } = call {
                if from_item >= to_item || *to_item > transcript.items.len() {
                    return None;
                }
                kept.extend(*from_item..*to_item);
            }
        }

        for call in calls {
            match call {
                EditorCall::Defer { .. } => return None,
                EditorCall::Keep { .. } => {}
                EditorCall::Elide { items } => {
                    if items
                        .iter()
                        .any(|i| kept.contains(i) || *i >= transcript.items.len())
                    {
                        return None;
                    }
                    let valid: Vec<usize> = items
                        .iter()
                        .filter_map(|&i| transcript.items.get(i))
                        .filter(|item| item.elidable_bytes.is_some())
                        .map(|item| item.line_index)
                        .collect();
                    for &line in &valid {
                        if let Some(item) = transcript.items.iter().find(|i| i.line_index == line) {
                            projected = projected.saturating_sub(item.estimated_elision_savings());
                        }
                    }
                    if !valid.is_empty() {
                        edits.push(Edit::Elide {
                            line_indexes: valid,
                            stub_template: super::elide::DEFAULT_STUB.to_string(),
                            per_item_stubs: Default::default(),
                        });
                    }
                }
                EditorCall::Summarize {
                    from_item,
                    to_item,
                    digest,
                } => {
                    if to_item <= from_item || *to_item > transcript.items.len() {
                        return None;
                    }
                    projected =
                        projected.saturating_add(crate::estimate::estimate_tokens(digest.len()));
                    edits.push(Edit::InjectDigest {
                        digest: crate::plan::DigestBlock {
                            goal: Some(digest.clone()),
                            summary: Some(format!(
                                "agentic digest covering {} items",
                                to_item - from_item
                            )),
                            decisions: Vec::new(),
                            files_touched: Vec::new(),
                            open_tasks: Vec::new(),
                            covers_items: to_item - from_item,
                            ..Default::default()
                        },
                    });
                }
            }
        }

        if edits.is_empty()
            || crate::validation::validate_edits(transcript, policy, &edits).is_err()
        {
            return None;
        }
        Some(CompactionPlan {
            strategy: "agentic".to_string(),
            rationale: format!(
                "editor model emitted {} calls; projected context {projected} (floor {})",
                calls.len(),
                policy.floor_tokens
            ),
            edits,
            context_tokens_before: before,
            context_tokens_after: projected,
        })
    }
}

impl Strategy for AgenticStrategy {
    fn id(&self) -> &'static str {
        "agentic"
    }

    fn evaluate(&self, transcript: &Transcript, policy: &PolicyConfig) -> Option<CompactionPlan> {
        // Without a configured driver, `agentic` defers to `auto`'s rubric
        // rather than guessing — matching the SelfCompact finding that the
        // tool alone is unreliable without the scaffold deciding for it.
        super::auto::AutoStrategy
            .evaluate(transcript, policy)
            .map(|mut plan| {
                plan.strategy = self.id().to_string();
                plan.rationale = format!(
                    "agentic (no driver configured, rubric fallback): {}",
                    plan.rationale
                );
                plan
            })
    }
}
