mod agentic;
mod auto;
mod cache_aware;
mod cache_edits;
mod compacted;
mod dedupe;
mod elide;
mod micro;
mod middle;
mod sawtooth;
mod scored;
mod structured;

pub use agentic::{AgenticStrategy, EditorCall, EditorDriver};
pub use auto::{cache_preservation_score, AutoStrategy};
pub use cache_aware::CacheAwareStrategy;
pub use cache_edits::CacheEditsStrategy;
pub use compacted::CompactedStrategy;
pub use dedupe::DedupeStrategy;
pub use elide::ElideStrategy;
pub use micro::MicroStrategy;
pub use middle::MiddleStrategy;
pub use sawtooth::SawtoothStrategy;
pub use scored::{HeuristicScorer, ScoreDriver, ScoredItem, ScoredStrategy};
pub use structured::StructuredStrategy;

use crate::model::{ItemKind, Transcript};
use crate::plan::{CompactionPlan, DigestBlock};
use serde::{Deserialize, Serialize};

/// How close the session is to the provider's quota ceiling. Scales the
/// configured trigger so gobstopper compacts earlier under pressure and
/// later when there is headroom to spare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaPressure {
    /// Plenty of quota headroom — compact slightly later than configured.
    Low,
    /// No adjustment; the configured trigger applies as written.
    #[default]
    Normal,
    /// Near the quota ceiling — compact earlier to stay under it.
    High,
}

/// Resolved policy for one session evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyConfig {
    /// Fire when the session's context estimate exceeds this many tokens.
    pub trigger_tokens: u64,
    /// Post-compaction occupancy target for transcript-path strategies.
    pub floor_tokens: u64,
    /// Tool outputs newer than this many from the tail are never elided.
    pub keep_recent_tool_outputs: usize,
    /// Optional retention cutoff for `scored`: candidates at or above this
    /// keep score are preserved even if the size target cannot be reached.
    /// Missing, ambiguous or invalid scores are also preserved when enabled.
    /// Heuristic scores are ranking signals, not calibrated probabilities.
    #[serde(default)]
    pub keep_score_threshold: Option<f64>,
    /// Minimum seconds between compactions of one session.
    pub min_interval_secs: u64,
    /// Minimum seconds between in-place mutations of one session.
    /// Longer than `min_interval_secs`: a session that bounces back
    /// over trigger right after an apply is appending churn, not
    /// accumulated context — re-writing it every interval only grows
    /// the vault and burns rewrite I/O. Applies to `watch`'s guarded
    /// store/in-place writes; read-only fork preparation is unaffected.
    #[serde(default = "default_apply_hold_secs")]
    pub apply_hold_secs: u64,
    #[serde(default = "default_min_savings_tokens")]
    pub min_savings_tokens: u64,
    /// Scales `trigger_tokens`; see [`PolicyConfig::effective_trigger`].
    #[serde(default)]
    pub quota_pressure: QuotaPressure,
    /// Derive trigger/floor per session from the provider-advertised
    /// window, elidable share, and past compaction yields. See
    /// [`crate::policy::adapt`].
    #[serde(default)]
    pub adaptive: bool,
}

const fn default_min_savings_tokens() -> u64 {
    4_096
}

const fn default_apply_hold_secs() -> u64 {
    1_800
}

impl Default for PolicyConfig {
    /// Research-backed default: fire well below the provider's own
    /// threshold (~60% of a ~400k effective window, ~25% of a 1M window).
    fn default() -> Self {
        Self {
            trigger_tokens: 250_000,
            floor_tokens: 40_000,
            keep_recent_tool_outputs: 8,
            keep_score_threshold: None,
            min_interval_secs: 300,
            apply_hold_secs: default_apply_hold_secs(),
            min_savings_tokens: default_min_savings_tokens(),
            quota_pressure: QuotaPressure::Normal,
            adaptive: false,
        }
    }
}

impl PolicyConfig {
    /// `trigger_tokens` adjusted for quota pressure: `High` fires at 70%
    /// of the configured trigger (compact earlier), `Normal` at 100%,
    /// `Low` at 115%. Result is floored at 1 so a nonzero context can
    /// always trigger when a trigger is configured at all.
    pub fn effective_trigger(&self) -> u64 {
        let factor = match self.quota_pressure {
            QuotaPressure::High => 0.7,
            QuotaPressure::Normal => 1.0,
            QuotaPressure::Low => 1.15,
        };
        // f64 -> u64 casts saturate on overflow; max(1) is the floor.
        ((self.trigger_tokens as f64) * factor).round().max(1.0) as u64
    }

    pub fn accepts_savings(&self, before: u64, after: u64) -> bool {
        before.saturating_sub(after) >= self.min_savings_tokens
    }
}

/// A compaction strategy. Pure: inspects the transcript, returns a plan
/// or `None` when the session is under threshold.
pub trait Strategy {
    fn id(&self) -> &'static str;
    fn evaluate(&self, transcript: &Transcript, policy: &PolicyConfig) -> Option<CompactionPlan>;
}

/// Every built-in strategy, in registry order.
pub fn builtin_strategies() -> Vec<Box<dyn Strategy>> {
    vec![
        Box::new(AutoStrategy),
        Box::new(ScoredStrategy),
        Box::new(CacheEditsStrategy),
        Box::new(CacheAwareStrategy),
        Box::new(SawtoothStrategy),
        Box::new(ElideStrategy),
        Box::new(DedupeStrategy),
        Box::new(MicroStrategy),
        Box::new(MiddleStrategy),
        Box::new(CompactedStrategy),
        Box::new(StructuredStrategy),
        Box::new(AgenticStrategy),
    ]
}

/// Look up a built-in strategy by id.
pub fn strategy_by_id(id: &str) -> Option<Box<dyn Strategy>> {
    builtin_strategies().into_iter().find(|s| s.id() == id)
}

pub(crate) const STATE_CARD_RESERVE_TOKENS: u64 = 4_096;

/// Build a bounded state-card digest from a set of chosen item
/// `line_index`es. Sanitized labels/summaries only — never payload text.
pub(crate) fn state_card_digest(transcript: &Transcript, chosen: &[usize]) -> DigestBlock {
    const MAX: usize = 8;

    let mut concepts = std::collections::HashSet::new();
    let mut files_touched = Vec::new();
    let mut decisions = Vec::new();
    let mut errors = Vec::new();
    let mut open_tasks = Vec::new();
    let by_line: std::collections::HashMap<usize, &crate::model::TranscriptItem> = transcript
        .items
        .iter()
        .map(|item| (item.line_index, item))
        .collect();

    for idx in chosen {
        if let Some(item) = by_line.get(idx).copied() {
            // Concepts: distinct tool names (without args) from chosen items.
            if let Some(tool) = item.label.split(|c: char| ['(', ' '].contains(&c)).next() {
                if !tool.is_empty() {
                    concepts.insert(tool.to_lowercase());
                }
            }

            let text = item.summary.as_deref().unwrap_or(&item.label).to_string();
            let is_error = is_error_marker(&text);
            let is_path = text.contains('/');
            let is_open_task = is_open_task_marker(&text);

            if is_error {
                errors.push(text);
            } else if is_open_task {
                open_tasks.push(text);
            } else if is_path || item.kind == ItemKind::ToolResult {
                // Prefer the summary for file references, fall back to label.
                files_touched.push(item.summary.clone().unwrap_or_else(|| item.label.clone()));
            } else {
                decisions.push(text);
            }
        }
    }

    let mut concepts: Vec<String> = concepts.into_iter().collect();
    concepts.sort();
    concepts.truncate(MAX);
    files_touched.truncate(MAX);
    decisions.truncate(MAX);
    errors.truncate(MAX);
    open_tasks.truncate(MAX);

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
        .and_then(|i| i.summary.clone());

    let summary = goal
        .as_ref()
        .map(|g| {
            let mut s = g.trim().to_string();
            if s.len() > 120 {
                let mut end = 117;
                while !s.is_char_boundary(end) {
                    end -= 1;
                }
                s.truncate(end);
                s.push_str("...");
            }
            s
        })
        .or_else(|| {
            Some(format!(
                "gobstopper state card covering {} elided items",
                chosen.len()
            ))
        });

    let current_work = transcript
        .items
        .iter()
        .rev()
        .find(|i| i.kind == ItemKind::Assistant)
        .and_then(|i| i.summary.clone())
        .filter(|s| !s.starts_with("[gobstopper state card]"));

    let context = Some(format!(
        "provider: {}",
        transcript.session.provider.as_str()
    ));

    DigestBlock {
        goal: goal.clone(),
        summary,
        concepts,
        files_touched,
        decisions,
        errors,
        open_tasks,
        current_work,
        context,
        covers_items: chosen.len(),
    }
}

pub(crate) fn choose_with_digest(
    transcript: &Transcript,
    floor_tokens: u64,
    candidates: &[&crate::model::TranscriptItem],
) -> Option<(Vec<usize>, DigestBlock, u64)> {
    let mut projected = transcript.context_tokens();
    let mut cursor = 0usize;
    let mut chosen = Vec::new();
    while projected > floor_tokens && cursor < candidates.len() {
        let item = candidates[cursor];
        cursor += 1;
        chosen.push(item.line_index);
        projected = projected.saturating_sub(item.estimated_elision_savings());
    }
    if chosen.is_empty() {
        return None;
    }
    loop {
        chosen.sort_unstable();
        let digest = state_card_digest(transcript, &chosen);
        let overhead = digest.estimate_overhead();
        let after = projected.saturating_add(overhead);
        if after <= floor_tokens || cursor == candidates.len() {
            return Some((chosen, digest, after));
        }
        let target = floor_tokens.saturating_sub(overhead);
        let mut pushed = false;
        while projected > target && cursor < candidates.len() {
            let item = candidates[cursor];
            cursor += 1;
            chosen.push(item.line_index);
            projected = projected.saturating_sub(item.estimated_elision_savings());
            pushed = true;
        }
        // Nothing was added and the digest overhead alone still puts
        // `after` over the floor — the loop would re-check an identical
        // state forever. Return the best-effort plan; the savings gate
        // downstream decides whether it is acceptable.
        if !pushed {
            return Some((chosen, digest, after));
        }
    }
}

fn is_error_marker(text: &str) -> bool {
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
        "command failed",
        "permission denied",
        "no such file",
        "not found",
        "timed out",
        "killed",
        "abort",
    ]
    .iter()
    .any(|m| lower.contains(m))
}

fn is_open_task_marker(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "todo",
        "fixme",
        "pending",
        "open:",
        "follow up",
        "follow-up",
    ]
    .iter()
    .any(|m| lower.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest_for_goal(goal: &str) -> DigestBlock {
        let transcript = Transcript {
            session: crate::SessionHandle {
                provider: crate::Provider::Codex,
                session_id: "summary-fixture".into(),
                path: "/unused/synthetic.jsonl".into(),
                cwd: None,
                age_secs: u64::MAX,
            },
            items: vec![crate::TranscriptItem {
                line_index: 0,
                kind: ItemKind::User,
                est_tokens: 40,
                elidable_bytes: None,
                elidable_parts: 0,
                label: "user".into(),
                summary: Some(goal.into()),
                uuid: None,
                parent_uuid: None,
                tool_use_ids: Vec::new(),
                payload_sha256: None,
            }],
            usage: Default::default(),
        };
        state_card_digest(&transcript, &[])
    }

    #[test]
    fn unicode_state_card_summary_truncates_at_safe_byte_boundaries() {
        for character in ['é', '界', '🙂'] {
            // Exercise every possible split within 2-, 3-, and 4-byte UTF-8.
            for prefix_length in (118 - character.len_utf8())..117 {
                let prefix = "a".repeat(prefix_length);
                let goal = format!("{prefix}{character}tail");
                assert!(goal.len() > 120);
                assert!(!goal.is_char_boundary(117));
                let digest = digest_for_goal(&goal);
                assert_eq!(digest.goal.as_deref(), Some(goal.as_str()));
                assert_eq!(digest.summary, Some(format!("{prefix}...")));
                assert!(digest.summary.unwrap().len() <= 120);
            }
        }
    }

    #[test]
    fn unicode_state_card_summary_preserves_short_text_and_ascii_behavior() {
        for goal in [
            String::new(),
            "short goal".into(),
            "a".repeat(120),
            "é".repeat(60),
            "界".repeat(40),
            "🙂".repeat(30),
        ] {
            let digest = digest_for_goal(&goal);
            assert_eq!(digest.goal.as_deref(), Some(goal.as_str()));
            assert_eq!(digest.summary.as_deref(), Some(goal.as_str()));
        }
        let goal = "a".repeat(121);
        let digest = digest_for_goal(&goal);
        assert_eq!(digest.goal.as_deref(), Some(goal.as_str()));
        assert_eq!(digest.summary, Some(format!("{}...", "a".repeat(117))));
    }

    fn policy_with(pressure: QuotaPressure) -> PolicyConfig {
        PolicyConfig {
            trigger_tokens: 1_000,
            quota_pressure: pressure,
            ..Default::default()
        }
    }

    #[test]
    fn effective_trigger_scales_with_quota_pressure() {
        assert_eq!(
            policy_with(QuotaPressure::Normal).effective_trigger(),
            1_000
        );
        // High compacts earlier: 70% of the configured trigger.
        assert_eq!(policy_with(QuotaPressure::High).effective_trigger(), 700);
        // Low waits for more headroom: 115%.
        assert_eq!(policy_with(QuotaPressure::Low).effective_trigger(), 1_150);
    }

    #[test]
    fn effective_trigger_floors_at_one() {
        let mut p = policy_with(QuotaPressure::High);
        p.trigger_tokens = 0;
        assert_eq!(p.effective_trigger(), 1);
        p.trigger_tokens = 1;
        assert_eq!(p.effective_trigger(), 1); // 0.7 rounds to 1, floored
    }

    #[test]
    fn minimum_savings_is_configurable() {
        let mut policy = PolicyConfig::default();
        assert!(policy.accepts_savings(100_000, 95_000));
        assert!(!policy.accepts_savings(100_000, 96_000));
        policy.min_savings_tokens = 0;
        assert!(policy.accepts_savings(100_000, 100_000));
    }

    #[test]
    fn choose_with_digest_terminates_when_overhead_beats_floor() {
        // Digest overhead exceeds the floor and candidate savings zero
        // `projected` before the cursor exhausts: the adjust loop must
        // return the best-effort plan, not spin on unchangeable state.
        use crate::model::{ItemKind, SessionHandle, Transcript, TranscriptItem, UsageSample};
        use std::path::PathBuf;
        let item = |line: usize, bytes: u64| TranscriptItem {
            line_index: line,
            kind: ItemKind::ToolResult,
            est_tokens: crate::estimate::estimate_tokens(bytes as usize),
            elidable_bytes: Some(bytes),
            elidable_parts: 1,
            label: "tool(out)".into(),
            summary: None,
            uuid: None,
            parent_uuid: None,
            tool_use_ids: Vec::new(),
            payload_sha256: None,
        };
        let items = vec![item(0, 4_000), item(1, 4_000), item(2, 4_000)];
        let transcript = Transcript {
            session: SessionHandle {
                provider: crate::model::Provider::Codex,
                session_id: "s".into(),
                path: PathBuf::from("/tmp/s.jsonl"),
                cwd: None,
                age_secs: 0,
            },
            items,
            usage: UsageSample {
                context_tokens: 100,
                ..Default::default()
            },
        };
        let candidates: Vec<&TranscriptItem> = transcript.items.iter().collect();
        // floor_tokens = 1: any digest's overhead already exceeds it.
        let outcome = choose_with_digest(&transcript, 1, &candidates);
        let (chosen, _digest, after) = outcome.expect("returns a best-effort plan");
        assert!(!chosen.is_empty());
        assert!(after > 1, "after {after} honestly reports digest overhead");
    }

    #[test]
    fn quota_pressure_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&QuotaPressure::High).unwrap(),
            "\"high\""
        );
        let back: QuotaPressure = serde_json::from_str("\"low\"").unwrap();
        assert_eq!(back, QuotaPressure::Low);
        // PolicyConfig without the field still deserializes (additive).
        let p: PolicyConfig = serde_json::from_str(
            r#"{"trigger_tokens":1000,"floor_tokens":300,"keep_recent_tool_outputs":2,"min_interval_secs":0}"#,
        )
        .unwrap();
        assert_eq!(p.quota_pressure, QuotaPressure::Normal);
    }
}
