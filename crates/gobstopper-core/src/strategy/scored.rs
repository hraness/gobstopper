//! Scored: a relevance scorer ranks each elidable item by keep
//! probability; the lowest-scoring items are elided until the floor.
//!
//! Unlike position-ordered strategies, this can keep an important older
//! result while dropping a more recent but irrelevant one — at the cost
//! of giving up on prefix preservation (different items may be elided
//! from anywhere in the eligible range). A built-in deterministic
//! heuristic scorer makes this strategy useful with no external API; the
//! `ScoreDriver` trait lets a CLI-side driver plug in a model-based
//! scorer such as Jev.

use super::elide::DEFAULT_STUB;
use super::{state_card_digest, PolicyConfig, Strategy};
use crate::estimate::estimate_tokens;
use crate::model::{ItemKind, Transcript};
use crate::plan::{CompactionPlan, Edit};

/// One scored candidate: an index into `transcript.items` plus a keep
/// probability (0..1; higher = more worth preserving).
#[derive(Debug, Clone)]
pub struct ScoredItem {
    pub item_index: usize,
    pub keep_probability: f64,
}

/// Driver interface for model-based scorers. Implemented in the CLI or
/// userspace; the core strategy only sees `ScoredItem`s.
pub trait ScoreDriver {
    /// Score every candidate item index. The `transcript` is provided
    /// for context (tail item labels, current task), but drivers must
    /// avoid consuming full payload text — only `item.label` and
    /// `item.summary` are safe.
    fn score(&self, transcript: &Transcript, candidates: &[usize]) -> Vec<ScoredItem>;
}

/// Deterministic built-in scorer. It reads item metadata only and makes
/// the `scored` strategy usable without an API key.
pub struct HeuristicScorer;

impl HeuristicScorer {
    pub fn score(&self, transcript: &Transcript, candidates: &[usize]) -> Vec<ScoredItem> {
        let goal = transcript
            .items
            .iter()
            .rev()
            .find(|i| {
                i.kind == ItemKind::User
                    && i.summary.as_ref().is_some_and(|s| {
                        !s.starts_with('<') && !s.starts_with("[gobstopper state card]")
                    })
            })
            .map(|i| tokenize(i.summary.as_deref().unwrap_or(&i.label)));
        let goal_tokens = goal.unwrap_or_default();

        // Build an index of every tool label that appears after each candidate.
        let mut label_occurrences: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();

        for &idx in candidates {
            if let Some(item) = transcript.items.get(idx) {
                label_occurrences
                    .entry(item.label.clone())
                    .or_default()
                    .push(idx);
            }
        }

        // Corpus-wide token statistics for IDF and future-reference counting.
        let n_docs = transcript.items.len().max(1);
        let mut token_df: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut token_occurrences: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        for (idx, item) in transcript.items.iter().enumerate() {
            let text = format!("{} {}", item.label, item.summary.as_deref().unwrap_or(""));
            for token in tokenize(&text) {
                *token_df.entry(token.clone()).or_insert(0) += 1;
                token_occurrences.entry(token).or_default().push(idx);
            }
        }
        let max_idf = (n_docs as f64).ln().max(1.0);

        // Latest occurrence tokens per tool label, for near-duplicate detection.
        let mut label_latest_tokens: std::collections::HashMap<
            String,
            std::collections::HashSet<String>,
        > = std::collections::HashMap::new();
        for item in transcript.items.iter() {
            let text = format!("{} {}", item.label, item.summary.as_deref().unwrap_or(""));
            label_latest_tokens.insert(item.label.clone(), tokenize(&text));
        }

        let max_line = transcript.items.last().map(|i| i.line_index).unwrap_or(0);
        let n = candidates.len().max(1) as f64;

        candidates
            .iter()
            .enumerate()
            .map(|(pos, &idx)| {
                let item = transcript.items.get(idx).unwrap();

                // 1. Recency: newer candidates are more likely still relevant.
                let recency = (pos as f64) / n;

                // 2. Tool importance: some tools produce durable state,
                //    others are transient (ls, echo, pwd).
                let tool = item
                    .label
                    .split(|c: char| ['(', ' '].contains(&c))
                    .next()
                    .unwrap_or("");
                let tool_importance = tool_importance(tool);

                // 3. Error/failure markers are almost always worth keeping.
                let item_text = format!("{} {}", item.label, item.summary.as_deref().unwrap_or(""));
                let error_marker = has_error_marker(&item_text);

                let item_tokens = tokenize(&item_text);

                // 4. Informativeness (IDF): rare, distinctive tokens are more
                //    likely to carry state the model cannot reconstruct.
                let idf: f64 = item_tokens
                    .iter()
                    .map(|t| (n_docs as f64 / (*token_df.get(t).unwrap_or(&1) as f64)).ln())
                    .sum();
                let idf_score = if item_tokens.is_empty() {
                    0.0
                } else {
                    (idf / (item_tokens.len() as f64 * max_idf)).clamp(0.0, 1.0)
                };

                // 5. Future reuse: count how often this item's tokens are
                //    referenced in later turns, bounded to avoid noise.
                let future_ref: f64 = item_tokens
                    .iter()
                    .map(|t| {
                        token_occurrences
                            .get(t)
                            .map(|occ| {
                                let start = occ.partition_point(|&i| i <= idx);
                                occ[start..].iter().take(12).count() as f64
                            })
                            .unwrap_or(0.0)
                    })
                    .sum::<f64>()
                    / (item_tokens.len().max(1) as f64);
                let future_ref_score = (future_ref / 12.0).clamp(0.0, 1.0);

                // 6. Goal overlap: files/paths mentioned in the current user
                //    goal are likely still being worked on.
                let goal_overlap = if item_tokens.is_empty() {
                    0.0
                } else {
                    (item_tokens.intersection(&goal_tokens).count() as f64)
                        / (item_tokens.len() as f64)
                };

                // 7. Superseded: if the same tool+label appears later, the
                //    older run is less valuable unless it has error markers.
                let occurrences = label_occurrences
                    .get(&item.label)
                    .cloned()
                    .unwrap_or_default();
                let newest = *occurrences.last().unwrap_or(&idx);
                let superseded = newest != idx && occurrences.len() > 1;

                // 8. Near-duplicate: similar output from the same tool (even
                //    with different args) is usually not worth keeping twice.
                let latest_tokens = label_latest_tokens
                    .get(&item.label)
                    .cloned()
                    .unwrap_or_default();
                let near_duplicate = if item_tokens.is_empty() {
                    false
                } else {
                    let newest_idx = *occurrences.last().unwrap_or(&idx);
                    newest_idx != idx && jaccard_similarity(&item_tokens, &latest_tokens) >= 0.85
                };

                // 9. Spread over the conversation: older items in the middle
                //    of a long transcript are less likely to matter.
                let position_ratio = if max_line == 0 {
                    0.0
                } else {
                    1.0 - (item.line_index as f64 / max_line as f64)
                };

                let score = 0.18 * recency
                    + 0.12 * tool_importance
                    + (if error_marker { 0.25 } else { 0.0 })
                    + 0.12 * future_ref_score
                    + 0.12 * goal_overlap
                    + 0.10 * position_ratio
                    + 0.12 * idf_score
                    - (if superseded { 0.15 } else { 0.0 })
                    - (if near_duplicate { 0.12 } else { 0.0 });

                ScoredItem {
                    item_index: idx,
                    keep_probability: score.clamp(0.0, 1.0),
                }
            })
            .collect()
    }
}

fn tokenize(text: &str) -> std::collections::HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '/' && c != '.' && c != '_')
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .map(|s| s.to_lowercase())
        .collect()
}

fn has_error_marker(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "error",
        "failed",
        "failure",
        "panic",
        "exception",
        "traceback",
        "enoent",
        "eacces",
        "non-zero",
        "nonzero",
        "exit code",
        "stderr:",
    ]
    .iter()
    .any(|m| lower.contains(m))
}

fn tool_importance(tool: &str) -> f64 {
    match tool.to_lowercase().as_str() {
        "read_file" | "view" | "view_range" | "search_files" | "glob" | "grep" | "apply"
        | "replace" | "edit" => 1.0,
        "bash" | "ls" | "cat" | "echo" | "pwd" | "which" | "find" | "wc" | "head" | "tail"
        | "sort" | "uniq" => 0.0,
        _ => 0.5,
    }
}

fn jaccard_similarity(
    a: &std::collections::HashSet<String>,
    b: &std::collections::HashSet<String>,
) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    inter / union
}

pub struct ScoredStrategy;

impl ScoredStrategy {
    /// Eligible item indexes: elidable items outside the protected tail.
    pub fn candidates(transcript: &Transcript, policy: &PolicyConfig) -> Vec<usize> {
        let elidable: Vec<usize> = transcript
            .items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.elidable_bytes.is_some())
            .map(|(idx, _)| idx)
            .collect();
        let keep_from = elidable
            .len()
            .saturating_sub(policy.keep_recent_tool_outputs);
        elidable[..keep_from.min(elidable.len())].to_vec()
    }

    /// Build a plan from driver-provided or heuristic scores. The
    /// `scores` are re-intersected with the transcript's eligible
    /// candidates. The strategy expands a tailward window of candidates
    /// and elides the lowest-scoring items *within* that window, so the
    /// conversation prefix stays unchanged until the first elided item.
    /// Unknown item indexes are ignored.
    pub fn scores_to_plan(
        transcript: &Transcript,
        policy: &PolicyConfig,
        scores: &[ScoredItem],
    ) -> Option<CompactionPlan> {
        let before = transcript.context_tokens();
        if before < policy.effective_trigger() {
            return None;
        }
        let eligible = Self::candidates(transcript, policy);
        if eligible.is_empty() {
            return None;
        }
        let score_by_index: std::collections::HashMap<usize, f64> = scores
            .iter()
            .map(|s| (s.item_index, s.keep_probability))
            .collect();

        let target_savings = before.saturating_sub(policy.floor_tokens);

        // Min-heap by keep-probability (lower score = higher elision priority)
        // using a fixed-point u64 key so it is `Ord`.
        let mut heap = std::collections::BinaryHeap::new();
        let mut total_savings = 0u64;

        // Walk from newest candidate to oldest, growing the window. The
        // first window that can reach target_savings is the smallest window;
        // we then elide the lowest-scored items in it.
        for k in (0..eligible.len()).rev() {
            let idx = eligible[k];
            let item = &transcript.items[idx];
            let prob = score_by_index.get(&idx).copied().unwrap_or(0.5);
            let key = (prob.clamp(0.0, 1.0) * 1_000_000.0) as u64;
            heap.push(std::cmp::Reverse((
                key,
                item.line_index,
                item.estimated_elision_savings(),
                idx,
            )));
            total_savings += item.estimated_elision_savings();

            if total_savings >= target_savings {
                let mut chosen = Vec::new();
                let mut accumulated = 0u64;
                while accumulated < target_savings && !heap.is_empty() {
                    let std::cmp::Reverse((_, _, savings, idx)) = heap.pop().unwrap();
                    chosen.push(transcript.items[idx].line_index);
                    accumulated += savings;
                }
                if chosen.is_empty() {
                    return None;
                }
                chosen.sort();
                let projected = before.saturating_sub(accumulated);

                let digest = state_card_digest(transcript, &chosen);
                let digest_chars: usize = digest.goal.as_ref().map(|g| g.len()).unwrap_or(0)
                    + digest.decisions.iter().map(|d| d.len()).sum::<usize>()
                    + digest.files_touched.iter().map(|f| f.len()).sum::<usize>()
                    + 64;
                let digest_overhead = estimate_tokens(digest_chars);

                let first_elided = chosen.first().copied().unwrap_or(0);
                let prefix_items = transcript
                    .items
                    .iter()
                    .filter(|i| i.line_index < first_elided)
                    .count();

                return Some(CompactionPlan {
                    strategy: "scored".to_string(),
                    rationale: format!(
                        "context {before} tokens exceeds trigger {}; eliding {} scored stale outputs, {} prefix records unchanged",
                        policy.trigger_tokens,
                        chosen.len(),
                        prefix_items
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
                });
            }
        }

        // Even the full set of candidates can't reach the floor; elide as
        // many low-scored items as possible (heuristic / max-effort).
        let mut chosen = Vec::new();
        let mut accumulated = 0u64;
        while !heap.is_empty() {
            let std::cmp::Reverse((_, _, savings, idx)) = heap.pop().unwrap();
            chosen.push(transcript.items[idx].line_index);
            accumulated += savings;
        }
        if chosen.is_empty() {
            return None;
        }
        chosen.sort();
        let projected = before.saturating_sub(accumulated);

        let digest = state_card_digest(transcript, &chosen);
        let digest_chars: usize = digest.goal.as_ref().map(|g| g.len()).unwrap_or(0)
            + digest.decisions.iter().map(|d| d.len()).sum::<usize>()
            + digest.files_touched.iter().map(|f| f.len()).sum::<usize>()
            + 64;
        let digest_overhead = estimate_tokens(digest_chars);

        let first_elided = chosen.first().copied().unwrap_or(0);
        let prefix_items = transcript
            .items
            .iter()
            .filter(|i| i.line_index < first_elided)
            .count();

        Some(CompactionPlan {
            strategy: "scored".to_string(),
            rationale: format!(
                "context {before} tokens exceeds trigger {}; eliding {} scored stale outputs, {} prefix records unchanged",
                policy.trigger_tokens,
                chosen.len(),
                prefix_items
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

impl Strategy for ScoredStrategy {
    fn id(&self) -> &'static str {
        "scored"
    }

    fn evaluate(&self, transcript: &Transcript, policy: &PolicyConfig) -> Option<CompactionPlan> {
        let scores = HeuristicScorer.score(transcript, &Self::candidates(transcript, policy));
        Self::scores_to_plan(transcript, policy, &scores)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ItemKind, Provider, SessionHandle, Transcript, TranscriptItem};
    use crate::strategy::{PolicyConfig, QuotaPressure};
    use std::path::PathBuf;

    fn item(
        line: usize,
        kind: ItemKind,
        est_tokens: u64,
        elidable: bool,
        label: &str,
        summary: Option<&str>,
    ) -> TranscriptItem {
        TranscriptItem {
            line_index: line,
            kind,
            est_tokens,
            elidable_bytes: elidable.then_some(est_tokens * 4),
            label: label.into(),
            summary: summary.map(String::from),
        }
    }

    fn transcript(items: Vec<TranscriptItem>, context_tokens: u64) -> Transcript {
        Transcript {
            session: SessionHandle {
                provider: Provider::Codex,
                session_id: "s".into(),
                path: PathBuf::from("/tmp/s.jsonl"),
                cwd: None,
                age_secs: u64::MAX,
            },
            items,
            usage: crate::model::UsageSample {
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
    fn heuristic_prefers_errors_and_recent() {
        let items = vec![
            item(
                0,
                ItemKind::User,
                100,
                false,
                "user",
                Some("implement thing"),
            ),
            item(
                1,
                ItemKind::ToolResult,
                400,
                true,
                "bash ls",
                Some("main.rs src/"),
            ),
            item(
                2,
                ItemKind::ToolResult,
                400,
                true,
                "bash ls",
                Some("main.rs src/"),
            ), // dup
            item(
                3,
                ItemKind::ToolResult,
                400,
                true,
                "bash gcc",
                Some("error: missing header"),
            ),
            item(4, ItemKind::User, 100, false, "user", Some("fix header")),
            item(
                5,
                ItemKind::ToolResult,
                400,
                true,
                "bash read",
                Some("main.rs"),
            ), // tail protected
            item(
                6,
                ItemKind::ToolResult,
                400,
                true,
                "bash cat",
                Some("main.rs"),
            ), // tail protected
        ];
        let t = transcript(items, 2_500);
        let candidates = ScoredStrategy::candidates(&t, &policy());
        // 5 tool results, minus 2 tail = 3 candidates (items 1,2,3)
        assert_eq!(candidates.len(), 3);
        let scores = HeuristicScorer.score(&t, &candidates);
        // item 3 (error, later) should have highest keep prob; item 2 (older dup) lowest.
        let by_idx: std::collections::HashMap<usize, f64> = scores
            .iter()
            .map(|s| (s.item_index, s.keep_probability))
            .collect();
        assert!(by_idx[&3] > by_idx[&1], "error marker should raise score");
        assert!(
            by_idx[&2] > by_idx[&1],
            "newer duplicate should outrank older"
        );
    }

    #[test]
    fn score_driver_interface_and_plan() {
        struct StubDriver;
        impl ScoreDriver for StubDriver {
            fn score(&self, _t: &Transcript, candidates: &[usize]) -> Vec<ScoredItem> {
                candidates
                    .iter()
                    .enumerate()
                    .map(|(i, &idx)| ScoredItem {
                        item_index: idx,
                        keep_probability: if i == 0 { 0.0 } else { 1.0 },
                    })
                    .collect()
            }
        }

        let items: Vec<TranscriptItem> = (0..5)
            .map(|i| {
                item(
                    i,
                    ItemKind::ToolResult,
                    400,
                    true,
                    &format!("bash-{i}"),
                    None,
                )
            })
            .collect();
        let t = transcript(items, 2_500);
        let candidates = ScoredStrategy::candidates(&t, &policy());
        let scores = StubDriver.score(&t, &candidates);
        let plan = ScoredStrategy::scores_to_plan(&t, &policy(), &scores).unwrap();
        // item with 0.0 score elided; others kept.
        assert_eq!(plan.edits.len(), 2);
    }
}
