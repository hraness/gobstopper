//! `gobstopper eval`: replay one transcript through each built-in
//! strategy and report what it would reclaim and whether the rewritten
//! file still verifies clean.
//!
//! Every file-mutating plan runs against its own throwaway copy in
//! `temp_dir`; the source transcript is never touched. Plans that only
//! delegate to the provider (`ProviderCompact`) rewrite nothing, so they
//! report zero findings and zero apply duration.
//!
//! Scoring has two halves: savings + safety (`est_reclaimed`, post-edit
//! `verify` findings) and quality — probe-based recall scoring checks
//! which verbatim strings extracted from the source transcript survive
//! each rewrite (see `gobstopper_core::probe`).

use anyhow::Context;
use gobstopper_core::plan::{CompactionPlan, Edit};
use gobstopper_core::probe::{extract_probes, score_probes, Probe, ProbeScore};
use gobstopper_core::strategy::{builtin_strategies, strategy_by_id, PolicyConfig, Strategy};
use gobstopper_core::{Provider, SessionHandle, Transcript};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use crate::verify::{self, Severity, VerifyFinding};

/// Temp-copy suffix counter, process-wide so concurrent evals and tests
/// never collide on a scratch path.
static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

/// One row of eval output: what a strategy would do to this transcript,
/// what the plan claims to save, and whether the result verifies.
#[derive(Debug, serde::Serialize)]
pub struct EvalRow {
    /// Id of the strategy that produced this row.
    pub strategy: String,
    /// None when the strategy produced no plan under this trigger.
    pub plan: Option<CompactionPlan>,
    /// Tokens the plan claims to reclaim.
    pub est_reclaimed: u64,
    /// Post-edit verify findings on the temp copy (empty when plan is
    /// None or the strategy is provider-delegating — no file rewrite).
    pub findings: Vec<VerifyFinding>,
    /// Error-severity findings — rollup of `findings` for sorting.
    pub verify_errors: usize,
    /// Warning-severity findings — rollup of `findings`.
    pub verify_warnings: usize,
    /// Probe-based quality score on the rewritten temp copy: which
    /// verbatim probes extracted from the source survived. `None` when
    /// no rewrite ran — no plan, provider-delegated, or apply failure.
    pub probe_score: Option<ProbeScore>,
    /// Estimated tokens left byte-identical before the first in-place edit.
    /// A larger number means more of the provider's prompt cache prefix
    /// is preserved on the next resume.
    pub prefix_tokens: u64,
    /// Apply duration on the temp copy.
    pub duration_ms: u64,
    /// Per-strategy failure (temp copy, apply, or read-back). One bad
    /// strategy never fails the whole eval.
    pub error: Option<String>,
}

/// Parse `src` into a transcript using the given provider's dialect.
/// The handle carries the file's real mtime age so `auto`'s live-session
/// routing reports what it would actually do.
fn load(provider: Provider, src: &Path) -> anyhow::Result<Transcript> {
    let (session_id, cwd) = match provider {
        Provider::Codex => crate::codex::scan_meta(src),
        Provider::ClaudeCode => crate::claude::scan_meta(src),
    };
    let age_secs = std::fs::metadata(src)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs())
        .unwrap_or(u64::MAX);
    let handle = SessionHandle {
        provider,
        session_id: session_id.unwrap_or_else(|| {
            src.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "eval".into())
        }),
        path: src.to_path_buf(),
        cwd,
        age_secs,
    };
    let transcript = match provider {
        Provider::Codex => crate::codex::load(handle),
        Provider::ClaudeCode => crate::claude::load(handle),
    }
    .with_context(|| format!("loading {}", src.display()))?;
    Ok(transcript)
}

fn apply(provider: Provider, path: &Path, edits: &[Edit]) -> Result<u64, crate::AdapterError> {
    match provider {
        Provider::Codex => crate::codex::apply(path, edits),
        Provider::ClaudeCode => crate::claude::apply(path, edits),
    }
}

/// First raw line of the protected tail: from the oldest still-kept
/// recent tool output to EOF. Mirrors what transcript-rewriting
/// strategies promise verbatim — the `keep_recent_tool_outputs` newest
/// elidable items plus everything after them. Falls back to the last
/// item's line, and to `usize::MAX` (nothing is tail) for an empty
/// transcript.
fn protected_tail_start(transcript: &Transcript, policy: &PolicyConfig) -> usize {
    let elidable: Vec<usize> = transcript
        .items
        .iter()
        .filter(|i| i.elidable_bytes.is_some())
        .map(|i| i.line_index)
        .collect();
    elidable
        .get(
            elidable
                .len()
                .saturating_sub(policy.keep_recent_tool_outputs),
        )
        .copied()
        .unwrap_or_else(|| {
            transcript
                .items
                .last()
                .map(|i| i.line_index)
                .unwrap_or(usize::MAX)
        })
}

/// Estimated tokens that remain byte-identical before the first in-place
/// edit in `plan`. Provider-compact plans touch no local file, so the
/// whole transcript is considered preserved. A larger number means more
/// of the provider's prefix cache survives the rewrite.
pub fn prefix_tokens(transcript: &Transcript, plan: &CompactionPlan) -> u64 {
    if plan
        .edits
        .iter()
        .any(|e| matches!(e, Edit::ProviderCompact { .. }))
    {
        return plan.context_tokens_before;
    }
    let first_changed = plan
        .edits
        .iter()
        .filter_map(|e| match e {
            Edit::Elide { line_indexes, .. } => line_indexes.iter().min().copied(),
            _ => None,
        })
        .min()
        .unwrap_or(usize::MAX);
    transcript
        .items
        .iter()
        .filter(|i| i.line_index < first_changed)
        .map(|i| i.est_tokens)
        .fold(0u64, u64::saturating_add)
}

/// Copy `src` to `tmp`, run the plan's file edits against the copy,
/// verify the result, and score probe recall. Returns (apply duration
/// ms, findings, probe score).
fn run_on_copy(
    provider: Provider,
    src: &Path,
    tmp: &Path,
    plan: &CompactionPlan,
    probes: &[Probe],
    tail_start_line: usize,
) -> anyhow::Result<(u64, Vec<VerifyFinding>, ProbeScore)> {
    std::fs::copy(src, tmp).with_context(|| format!("copy {} to temp eval file", src.display()))?;
    let started = Instant::now();
    apply(provider, tmp, &plan.edits).map_err(|e| anyhow::anyhow!(e))?;
    let duration_ms = started.elapsed().as_millis() as u64;
    let bytes = std::fs::read(tmp).with_context(|| "reading back temp eval file")?;
    let findings = verify::verify(provider, &bytes);
    let score = score_probes(probes, &String::from_utf8_lossy(&bytes), tail_start_line);
    Ok((duration_ms, findings, score))
}

/// Evaluate every built-in strategy (or `only` when set) against one
/// transcript file. Each file-mutating strategy runs on its own temp
/// copy; the source file is never modified.
pub fn eval_transcript(
    provider: Provider,
    src: &Path,
    policy: &PolicyConfig,
    only: Option<&str>,
) -> anyhow::Result<Vec<EvalRow>> {
    let transcript = load(provider, src)?;
    let source_bytes = std::fs::read(src)
        .with_context(|| format!("reading {} for probe extraction", src.display()))?;

    // One probe set shared by every strategy keeps scores comparable.
    // Probes come only from context-carrying lines: `est_tokens > 0`
    // excludes provably dead branches and zero-cost bookkeeping.
    let live_lines: std::collections::HashSet<usize> = transcript
        .items
        .iter()
        .filter(|i| i.est_tokens > 0)
        .map(|i| i.line_index)
        .collect();
    let mut probes = extract_probes(&String::from_utf8_lossy(&source_bytes));
    probes.retain(|p| live_lines.contains(&p.line_index));
    let tail_start = protected_tail_start(&transcript, policy);

    let strategies: Vec<Box<dyn Strategy>> = match only {
        Some(id) => {
            vec![strategy_by_id(id).ok_or_else(|| anyhow::anyhow!("unknown strategy '{id}'"))?]
        }
        None => builtin_strategies(),
    };

    let mut rows = Vec::with_capacity(strategies.len());
    for strat in &strategies {
        let mut row = EvalRow {
            strategy: strat.id().to_string(),
            plan: None,
            est_reclaimed: 0,
            findings: Vec::new(),
            verify_errors: 0,
            verify_warnings: 0,
            probe_score: None,
            prefix_tokens: 0,
            duration_ms: 0,
            error: None,
        };
        if let Some(plan) = strat.evaluate(&transcript, policy) {
            row.est_reclaimed = plan.est_savings();
            row.prefix_tokens = prefix_tokens(&transcript, &plan);
            let needs_rewrite = plan
                .edits
                .iter()
                .any(|e| !matches!(e, Edit::ProviderCompact { .. }));
            if needs_rewrite {
                let tmp = std::env::temp_dir().join(format!(
                    "gob-eval-{}-{}",
                    std::process::id(),
                    NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
                ));
                let outcome = run_on_copy(provider, src, &tmp, &plan, &probes, tail_start);
                // Always clean up: the temp copy itself, plus the
                // intermediate an adapter may have written before a
                // failed rename.
                let _ = std::fs::remove_file(&tmp);
                let _ = std::fs::remove_file(tmp.with_extension("jsonl.gobstopper-tmp"));
                match outcome {
                    Ok((duration_ms, findings, score)) => {
                        row.duration_ms = duration_ms;
                        row.verify_errors = findings
                            .iter()
                            .filter(|f| f.severity == Severity::Error)
                            .count();
                        row.verify_warnings = findings
                            .iter()
                            .filter(|f| f.severity == Severity::Warning)
                            .count();
                        row.findings = findings;
                        row.probe_score = Some(score);
                    }
                    Err(e) => row.error = Some(e.to_string()),
                }
            }
            row.plan = Some(plan);
        }
        rows.push(row);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::Severity;
    use gobstopper_core::QuotaPressure;
    use std::fs;
    use std::path::PathBuf;

    /// Serializes tests that inspect `gob-eval-*` temp names: NEXT_TEMP is
    /// process-global, so a concurrent eval could draw a name inside the
    /// range another test is asserting on.
    static EVAL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "gob-eval-test-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Small Claude Code transcript: a user prompt, three assistant
    /// tool_use / user tool_result rounds with fat payloads, and a final
    /// text reply. uuid chain is linear so every line is live.
    fn write_claude_transcript(dir: &Path) -> PathBuf {
        let mut lines = vec![serde_json::json!({
            "type": "user", "uuid": "u1",
            "message": {"role": "user", "content": "please run the test suite"}
        })];
        for i in 0..3 {
            let call = format!("u{}", 2 + i * 2);
            let result = format!("u{}", 3 + i * 2);
            lines.push(serde_json::json!({
                "type": "assistant", "uuid": call, "parentUuid": format!("u{}", 1 + i * 2),
                "message": {"role": "assistant", "content": [
                    {"type": "tool_use", "id": format!("t{i}"), "name": "Bash",
                     "input": {"command": "cargo test"}}
                ]}
            }));
            lines.push(serde_json::json!({
                "type": "user", "uuid": result, "parentUuid": call,
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": format!("t{i}"),
                     "content": "x".repeat(3_000)}
                ]}
            }));
        }
        lines.push(serde_json::json!({
            "type": "assistant", "uuid": "u8", "parentUuid": "u7",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "done"}]}
        }));
        let path = dir.join("session.jsonl");
        let mut text = String::new();
        for line in &lines {
            text.push_str(&line.to_string());
            text.push('\n');
        }
        fs::write(&path, text).unwrap();
        path
    }

    /// Probe-bearing variant: each tool_result payload carries distinct
    /// probe text (error signature, command, path, decision), so eliding
    /// early results loses their probes while the kept tail result —
    /// `keep_recent_tool_outputs: 1` — keeps its own.
    fn write_probe_transcript(dir: &Path) -> PathBuf {
        let mut lines = vec![serde_json::json!({
            "type": "user", "uuid": "u1",
            "message": {"role": "user", "content": "please fix the build in /project/src/main.rs"}
        })];
        let bodies = [
            "error[E0308]: mismatched types in /project/crates/core/src/lib.rs FAILED",
            "ran git status; decided to keep /project/docs/design.md",
            "cargo build --release finished for /project/src/tail_keeper.rs",
        ];
        for (i, body) in bodies.iter().enumerate() {
            let call = format!("u{}", 2 + i * 2);
            let result = format!("u{}", 3 + i * 2);
            lines.push(serde_json::json!({
                "type": "assistant", "uuid": call, "parentUuid": format!("u{}", 1 + i * 2),
                "message": {"role": "assistant", "content": [
                    {"type": "tool_use", "id": format!("t{i}"), "name": "Bash",
                     "input": {"command": "cargo test"}}
                ]}
            }));
            lines.push(serde_json::json!({
                "type": "user", "uuid": result, "parentUuid": call,
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": format!("t{i}"),
                     "content": format!("{body}\n{}", "x".repeat(3_000))}
                ]}
            }));
        }
        lines.push(serde_json::json!({
            "type": "assistant", "uuid": "u8", "parentUuid": "u7",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "done"}]}
        }));
        let path = dir.join("session.jsonl");
        let mut text = String::new();
        for line in &lines {
            text.push_str(&line.to_string());
            text.push('\n');
        }
        fs::write(&path, text).unwrap();
        path
    }

    /// Low trigger so every strategy fires; one recent tool output kept.
    fn low_policy() -> PolicyConfig {
        PolicyConfig {
            trigger_tokens: 100,
            floor_tokens: 10,
            keep_recent_tool_outputs: 1,
            min_interval_secs: 0,
            quota_pressure: QuotaPressure::Normal,
            ..Default::default()
        }
    }

    fn row<'a>(rows: &'a [EvalRow], id: &str) -> &'a EvalRow {
        rows.iter()
            .find(|r| r.strategy == id)
            .unwrap_or_else(|| panic!("missing eval row for '{id}'"))
    }

    #[test]
    fn eval_reports_all_strategies_and_leaves_source_untouched() {
        let _guard = EVAL_LOCK.lock().unwrap();
        let dir = TestDir::new();
        let src = write_claude_transcript(&dir.0);
        let before = fs::read(&src).unwrap();

        // Snapshot the temp-name counter so the leftover check below only
        // inspects names this eval call could have used — sibling tests
        // running concurrently in this process draw other values.
        let counter_start = NEXT_TEMP.load(Ordering::Relaxed);
        let rows = eval_transcript(Provider::ClaudeCode, &src, &low_policy(), None).unwrap();
        let counter_end = NEXT_TEMP.load(Ordering::Relaxed);

        for id in ["auto", "sawtooth", "elide", "structured", "agentic"] {
            row(&rows, id);
        }

        // Sawtooth delegates: a ProviderCompact plan, no file rewrite.
        let sawtooth = row(&rows, "sawtooth");
        let plan = sawtooth.plan.as_ref().expect("sawtooth should plan");
        assert!(
            plan.edits
                .iter()
                .all(|e| matches!(e, Edit::ProviderCompact { .. })),
            "sawtooth edits should be provider-only: {:?}",
            plan.edits
        );
        assert!(sawtooth.findings.is_empty());
        assert_eq!(sawtooth.duration_ms, 0);
        assert!(sawtooth.error.is_none());

        // Elide rewrites a temp copy: real savings, still verifies clean.
        let elide = row(&rows, "elide");
        assert!(elide.plan.is_some(), "elide should plan: {:?}", elide.error);
        assert!(elide.est_reclaimed > 0);
        assert!(elide.error.is_none());
        assert!(
            elide.findings.iter().all(|f| f.severity != Severity::Error),
            "elide temp copy should have no error findings: {:?}",
            elide.findings
        );

        // Structured declines (fewer items than its keep-tail window) but
        // the row exists and carries no error.
        let structured = row(&rows, "structured");
        assert!(structured.error.is_none());

        // Fresh file reads as a live session, so auto/agentic delegate.
        for id in ["auto", "agentic"] {
            let r = row(&rows, id);
            assert!(r.error.is_none());
            let plan = r
                .plan
                .as_ref()
                .unwrap_or_else(|| panic!("{id} should plan"));
            assert!(
                plan.edits
                    .iter()
                    .all(|e| matches!(e, Edit::ProviderCompact { .. })),
                "{id} should delegate on a hot transcript: {:?}",
                plan.edits
            );
        }

        // Source file byte-identical.
        assert_eq!(fs::read(&src).unwrap(), before);

        // Every temp name this call could have drawn is gone — both the
        // copy and the adapter's pre-rename intermediate.
        for n in counter_start..counter_end {
            let base = format!("gob-eval-{}-{n}", std::process::id());
            assert!(
                !std::env::temp_dir().join(&base).exists(),
                "leftover temp copy {base}"
            );
            assert!(
                !std::env::temp_dir()
                    .join(format!("{base}.jsonl.gobstopper-tmp"))
                    .exists(),
                "leftover intermediate {base}.jsonl.gobstopper-tmp"
            );
        }
        assert!(
            counter_end > counter_start,
            "elide should have run on a temp copy"
        );
    }

    #[test]
    fn only_filters_to_one_strategy_and_rejects_unknown_ids() {
        let _guard = EVAL_LOCK.lock().unwrap();
        let dir = TestDir::new();
        let src = write_claude_transcript(&dir.0);

        let rows =
            eval_transcript(Provider::ClaudeCode, &src, &low_policy(), Some("elide")).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].strategy, "elide");
        assert!(rows[0].est_reclaimed > 0);

        assert!(eval_transcript(Provider::ClaudeCode, &src, &low_policy(), Some("nope")).is_err());
    }

    #[test]
    fn elide_row_carries_probe_score_and_verify_counts() {
        let _guard = EVAL_LOCK.lock().unwrap();
        let dir = TestDir::new();
        let src = write_probe_transcript(&dir.0);

        let rows = eval_transcript(Provider::ClaudeCode, &src, &low_policy(), None).unwrap();
        let elide = row(&rows, "elide");
        assert!(elide.error.is_none());

        let score = elide
            .probe_score
            .as_ref()
            .expect("elide rewrites a temp copy and scores it");
        assert!(score.probes_total > 0);
        // Early tool payloads were elided: their probes are gone.
        assert!(score.probes_recalled < score.probes_total);
        assert!(score.recall < 1.0 && score.recall > 0.0);
        // The newest tool output is the protected tail: intact.
        assert!(
            score.tail_intact,
            "tail probes should survive elision: {:?}",
            score.missed_probes
        );
        assert!(score.tail_probes_total > 0);
        assert!(score.missed_probes.len() <= 8);
        assert!(score
            .missed_probes
            .iter()
            .any(|m| m.contains("error[E0308]")));
        assert!(score.by_kind.iter().any(|k| k.total > 0));

        // Verify rollups are consistent with the findings vec.
        assert_eq!(elide.verify_errors, 0);
        assert_eq!(
            elide.verify_errors + elide.verify_warnings,
            elide.findings.len()
        );

        // Delegating strategies never rewrite the file: no score.
        assert!(row(&rows, "sawtooth").probe_score.is_none());
    }

    #[test]
    fn under_trigger_strategies_report_no_plan() {
        let dir = TestDir::new();
        let src = write_claude_transcript(&dir.0);
        let mut policy = low_policy();
        policy.trigger_tokens = u64::MAX;

        let rows = eval_transcript(Provider::ClaudeCode, &src, &policy, None).unwrap();
        assert_eq!(rows.len(), 11);
        for r in &rows {
            assert!(r.plan.is_none(), "{} should not fire", r.strategy);
            assert_eq!(r.est_reclaimed, 0);
            assert!(r.findings.is_empty());
            assert!(r.error.is_none());
        }
    }
}
