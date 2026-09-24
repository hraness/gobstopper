//! `gobstopper eval`: replay one transcript through each built-in
//! strategy and report what it would reclaim and whether the rewritten
//! file still verifies clean.
//!
//! Every file-mutating plan runs against its own detached in-memory bytes;
//! the source transcript is never written. Plans that only
//! delegate to the provider (`ProviderCompact`) rewrite nothing, so they
//! report zero findings and zero apply duration.
//!
//! Scoring has two halves: savings + safety (`est_reclaimed`, post-edit
//! `verify` findings) and quality — probe-based recall scoring checks
//! which verbatim strings extracted from the source transcript survive
//! each rewrite (see `gobstopper_core::probe`).

use anyhow::Context;
use gobstopper_core::plan::{CompactionPlan, Edit};
use gobstopper_core::probe::{
    extract_probes, score_from_probabilities, score_probes, Probe, ProbeJudge, ProbeScore,
};
use gobstopper_core::strategy::{
    builtin_strategies, strategy_by_id, PolicyConfig, ScoreDriver, ScoredStrategy, Strategy,
};
use gobstopper_core::{Provider, SessionHandle, Transcript};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use crate::verify::{self, Severity, VerifyFinding};

/// Bind token accounting to the SAME bytes retained in the vault. The handle
/// must name the exact canonical store and metadata session. Resolving the path
/// checks identity only; no transcript is reread and no provider is contacted.
pub fn token_observation(
    handle: &SessionHandle,
    bytes: &[u8],
    snapshot_manifest_sha256: Option<&str>,
) -> anyhow::Result<gobstopper_core::events::TokenObservation> {
    let canonical = std::fs::canonicalize(&handle.path)
        .map_err(|_| anyhow::anyhow!("observation source identity unavailable"))?;
    if canonical != handle.path
        || crate::fork::source_session_id(handle.provider, bytes)? != handle.session_id
        || verify::verify(handle.provider, bytes)
            .iter()
            .any(|finding| finding.severity == Severity::Error)
    {
        anyhow::bail!("observation source identity or structure mismatch");
    }
    let transcript = match handle.provider {
        Provider::Codex => crate::codex::load_bytes(handle.clone(), bytes),
        Provider::ClaudeCode => crate::claude::load_bytes(handle.clone(), bytes),
        Provider::Devin => crate::devin::load_bytes(handle.clone(), bytes),
    }?;
    let full = transcript.usage.lifetime_scope == gobstopper_core::model::LifetimeScope::Full;
    let observation = gobstopper_core::events::TokenObservation {
        source_sha256: crate::copy::sha256(bytes),
        source_identity_sha256: crate::detect::source_identity(handle)?,
        snapshot_manifest_sha256: snapshot_manifest_sha256.map(str::to_owned),
        context_state: transcript.usage.context_state,
        context_tokens: transcript.usage.reported_context(),
        estimated_context_tokens: transcript.estimated_context_tokens(),
        lifetime_scope: transcript.usage.lifetime_scope,
        lifetime_input_tokens: full.then_some(transcript.usage.lifetime_input_tokens),
        lifetime_cached_tokens: full.then_some(
            transcript
                .usage
                .lifetime_cached_tokens
                .min(transcript.usage.lifetime_input_tokens),
        ),
    };
    if !observation.is_valid() {
        anyhow::bail!("observation violates evidence bounds");
    }
    Ok(observation)
}

/// One row of eval output: what a strategy would do to this transcript,
/// what the plan claims to save, and whether the result verifies.
#[derive(Debug, serde::Serialize)]
pub struct EvalRow {
    /// Id of the strategy that produced this row.
    pub strategy: String,
    pub version: &'static str,
    pub source_sha256: String,
    pub result_sha256: Option<String>,
    pub source_bytes: usize,
    pub result_bytes: Option<usize>,
    pub execution_state: &'static str,
    pub token_basis: &'static str,
    pub charged_tokens: Option<u64>,
    pub cache_hits: Option<u64>,
    pub refetches: Option<u64>,
    pub continuation_success: Option<bool>,
    /// None when the strategy produced no plan under this trigger.
    pub plan: Option<CompactionPlan>,
    /// Tokens the plan claims to reclaim.
    pub est_reclaimed: u64,
    /// Post-edit verify findings on the prepared bytes (empty when plan is
    /// None or the strategy is provider-delegating — no file rewrite).
    pub findings: Vec<VerifyFinding>,
    /// Error-severity findings — rollup of `findings` for sorting.
    pub verify_errors: usize,
    /// Warning-severity findings — rollup of `findings`.
    pub verify_warnings: usize,
    /// Probe-based quality score on the prepared bytes: which
    /// verbatim probes extracted from the source survived. `None` when
    /// no rewrite ran — no plan, provider-delegated, or apply failure.
    pub probe_score: Option<ProbeScore>,
    /// Semantic probe score from an injected [`ProbeJudge`]: facts a
    /// model still finds in the rewritten text even when the verbatim
    /// string is gone (state cards, per-item stubs). `None` when no
    /// judge was configured or the judge call failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_score: Option<ProbeScore>,
    /// Estimated tokens left byte-identical before the first in-place edit.
    /// A byte-prefix estimate only; cache hits and charged usage are unmeasured.
    pub prefix_tokens: u64,
    /// In-memory transformation duration.
    pub duration_ms: u64,
    /// Per-strategy transformation or decoding failure. One bad
    /// strategy never fails the whole eval.
    pub error: Option<String>,
}

/// Parse `src` into a transcript using the given provider's dialect.
/// The handle carries the file's real mtime age so `auto`'s live-session
/// routing reports what it would actually do.
fn load(provider: Provider, src: &Path, original: &[u8]) -> anyhow::Result<Transcript> {
    let session_id = crate::fork::source_session_id(provider, original).ok();
    let cwd = None;
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
        Provider::Codex => crate::codex::load_bytes(handle, original),
        Provider::ClaudeCode => crate::claude::load_bytes(handle, original),
        Provider::Devin => crate::devin::load_bytes(handle, original),
    }
    .with_context(|| format!("loading {}", src.display()))?;
    Ok(transcript)
}

fn transform(
    provider: Provider,
    original: &[u8],
    edits: &[Edit],
) -> Result<Vec<u8>, crate::AdapterError> {
    match provider {
        Provider::Codex => crate::codex::transform(original, edits),
        Provider::ClaudeCode => crate::claude::transform(original, edits),
        Provider::Devin => crate::devin::transform(original, edits),
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

/// Project only context-carrying records before probe extraction and
/// matching. Blank dead lines keep source line indexes stable for tail
/// scoring without letting historical branches consume the probe budget.
/// A Codex compaction record carries its live context in replacement_history;
/// the surrounding record (including an obsolete summary) is not that context.
fn live_context_text(transcript: &Transcript, raw: &str) -> String {
    let live_lines: std::collections::HashSet<usize> = transcript
        .items
        .iter()
        .filter(|item| item.est_tokens > 0)
        .map(|item| item.line_index)
        .collect();
    let mut projected = String::new();
    for (line_index, line) in raw.lines().enumerate() {
        if live_lines.contains(&line_index) {
            let compacted = (transcript.session.provider == Provider::Codex)
                .then(|| serde_json::from_str::<serde_json::Value>(line).ok())
                .flatten()
                .filter(|record| record["type"].as_str() == Some("compacted"));
            if let Some(record) = compacted {
                if let Some(history) = record["payload"]["replacement_history"].as_array() {
                    // Serialization stays on one physical line, preserving the
                    // original line anchor for every nested replacement item.
                    projected.push_str(&serde_json::to_string(history).unwrap_or_default());
                }
                // Invalid/unknown compacted shapes remain verifier findings;
                // do not treat their wrapper text as successful live recall.
            } else {
                projected.push_str(line);
            }
        }
        projected.push('\n');
    }
    projected
}

/// Estimated tokens that remain byte-identical before the first in-place
/// edit in `plan`. Provider-compact plans touch no local file, so the
/// whole transcript is considered preserved. This estimates byte-prefix
/// retention; it cannot establish cache hits on a provider resume.
pub fn prefix_tokens(transcript: &Transcript, plan: &CompactionPlan) -> u64 {
    if plan
        .edits
        .iter()
        .any(|e| matches!(e, Edit::ProviderCompact { .. } | Edit::CacheEdit { .. }))
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

/// Optional seams the CLI injects into eval. `scorer` drives the
/// `scored` strategy's ranking (jev/apple drivers via env); without it
/// scored falls back to the built-in heuristic. `probe_judge` adds a
/// semantic probe pass over each rewritten copy.
#[derive(Default)]
pub struct EvalHooks<'a> {
    /// Driver for the `scored` strategy row. `None` keeps the
    /// deterministic heuristic path — eval matches what `plan` would
    /// produce with no scorer env configured. Only used in the
    /// sequential planning phase, so no `Sync` bound is needed.
    pub scorer: Option<&'a dyn ScoreDriver>,
    /// Semantic probe judge; scores each prepared transcript beyond
    /// verbatim matching. `None` skips the pass entirely. Shared across
    /// the parallel rewrite worker pool, so it must be `Sync`.
    pub probe_judge: Option<&'a (dyn ProbeJudge + Sync)>,
}

/// Run the plan's edits against detached source bytes,
/// verify the result, and score probe recall. Returns (apply duration
/// ms, findings, probe score, semantic probe score).
struct PreparedOutcome {
    duration_ms: u64,
    findings: Vec<VerifyFinding>,
    score: ProbeScore,
    semantic: Option<ProbeScore>,
    sha256: String,
    bytes: usize,
}

fn run_on_copy(
    transcript: &Transcript,
    original: &[u8],
    plan: &CompactionPlan,
    probes: &[Probe],
    tail_start_line: usize,
    judge: Option<&(dyn ProbeJudge + Sync)>,
) -> anyhow::Result<PreparedOutcome> {
    let provider = transcript.session.provider;
    let started = Instant::now();
    let bytes = transform(provider, original, &plan.edits)?;
    let duration_ms = started.elapsed().as_millis() as u64;
    let findings = verify::verify(provider, &bytes);
    let handle = transcript.session.clone();
    let post_transcript = match provider {
        Provider::Codex => crate::codex::load_bytes(handle, &bytes),
        Provider::ClaudeCode => crate::claude::load_bytes(handle, &bytes),
        Provider::Devin => crate::devin::load_bytes(handle, &bytes),
    }
    .context("loading rewritten live context for probe scoring")?;
    let post_text = live_context_text(&post_transcript, &String::from_utf8_lossy(&bytes));
    let score = score_probes(probes, &post_text, tail_start_line);
    let semantic = judge.and_then(|j| {
        let missed: Vec<(usize, Probe)> = probes
            .iter()
            .enumerate()
            .filter(|(_, probe)| !post_text.contains(&probe.text))
            .map(|(index, probe)| (index, probe.clone()))
            .collect();
        if missed.is_empty() {
            return Some(score.clone());
        }
        let missed_probes: Vec<Probe> = missed.iter().map(|(_, probe)| probe.clone()).collect();
        let judged = j.score(&missed_probes, &post_text)?;
        let mut probabilities: Vec<f64> = probes
            .iter()
            .map(|probe| {
                if post_text.contains(&probe.text) {
                    1.0
                } else {
                    f64::NAN // unanswered probes remain unavailable
                }
            })
            .collect();
        for ((index, _), probability) in missed.iter().zip(judged) {
            probabilities[*index] = probability;
        }
        let mut result = score_from_probabilities(probes, &probabilities, tail_start_line);
        result.basis = gobstopper_core::probe::ScoreBasis::LiteralAndModelJudgment;
        Some(result)
    });
    Ok(PreparedOutcome {
        duration_ms,
        findings,
        score,
        semantic,
        sha256: crate::copy::sha256(&bytes),
        bytes: bytes.len(),
    })
}

/// `GOBSTOPPER_EVAL_PARALLEL`: worker-pool width for the per-strategy
/// in-memory transformation phase. The (often remote) judge call dominates
/// eval wall time, so rows fan out; `1` restores sequential behavior.
const DEFAULT_EVAL_PARALLEL: usize = 4;
const MAX_EVAL_PARALLEL: usize = 8;

fn parse_parallelism(raw: Option<String>) -> usize {
    raw.and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_EVAL_PARALLEL)
        .clamp(1, MAX_EVAL_PARALLEL)
}

fn eval_parallelism() -> usize {
    parse_parallelism(std::env::var("GOBSTOPPER_EVAL_PARALLEL").ok())
}

/// Bounded worker pool: `parallelism` scoped threads pull task indices
/// from a shared counter, so a slow row never stalls the next wave the
/// way a join-per-wave barrier would. Panics are caught per task so one
/// bad rewrite neither kills the worker nor forfeits the rest of its
/// items. Outcomes come back in index order regardless of completion
/// order, keeping the row merge deterministic.
fn run_indexed<T, F>(count: usize, parallelism: usize, task: F) -> Vec<std::thread::Result<T>>
where
    T: Send,
    F: Fn(usize) -> T + Sync,
{
    if count == 0 {
        return Vec::new();
    }
    let workers = parallelism.max(1).min(count);
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<std::thread::Result<T>>>> =
        (0..count).map(|_| Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let next = &next;
            let slots = &slots;
            let task = &task;
            scope.spawn(move || loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                if index >= count {
                    break;
                }
                let outcome =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| task(index)));
                if let Ok(mut slot) = slots[index].lock() {
                    *slot = Some(outcome);
                }
            });
        }
    });
    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .unwrap_or_else(|e| e.into_inner())
                .unwrap_or_else(|| {
                    Err(Box::new("eval worker stopped before claiming task")
                        as Box<dyn std::any::Any + Send>)
                })
        })
        .collect()
}

/// Evaluate every built-in strategy (or `only` when set) against one
/// transcript file. Each file-mutating strategy transforms detached bytes;
/// the source file is never modified.
pub fn eval_transcript(
    provider: Provider,
    src: &Path,
    policy: &PolicyConfig,
    only: Option<&str>,
) -> anyhow::Result<Vec<EvalRow>> {
    eval_transcript_with_hooks(provider, src, policy, only, &EvalHooks::default())
}

/// [`eval_transcript`] with optional scorer and semantic-judge seams.
pub fn eval_transcript_with_hooks(
    provider: Provider,
    src: &Path,
    policy: &PolicyConfig,
    only: Option<&str>,
    hooks: &EvalHooks,
) -> anyhow::Result<Vec<EvalRow>> {
    eval_transcript_inner(provider, src, policy, only, hooks, eval_parallelism())
}

/// Evaluate an exact discovered session. Devin stores are exported for this
/// session once; database bytes are never interpreted as transcript JSONL.
pub fn eval_session_with_hooks(
    handle: &SessionHandle,
    policy: &PolicyConfig,
    only: Option<&str>,
    hooks: &EvalHooks,
) -> anyhow::Result<Vec<EvalRow>> {
    let bytes = if handle.provider == Provider::Devin && crate::devin::is_store_path(&handle.path) {
        crate::devin::export_bytes(&handle.path, &handle.session_id)?
    } else {
        crate::transaction::read(&handle.path)?
    };
    anyhow::ensure!(
        crate::fork::source_session_id(handle.provider, &bytes)? == handle.session_id,
        "evaluation source identity mismatch"
    );
    let transcript = match handle.provider {
        Provider::Codex => crate::codex::load_bytes(handle.clone(), &bytes)?,
        Provider::ClaudeCode => crate::claude::load_bytes(handle.clone(), &bytes)?,
        Provider::Devin => crate::devin::load_bytes(handle.clone(), &bytes)?,
    };
    eval_frozen(transcript, bytes, policy, only, hooks, eval_parallelism())
}

/// [`eval_transcript_with_hooks`] with an explicit rewrite-pool width,
/// so tests can pin concurrency without touching the process env.
fn eval_transcript_inner(
    provider: Provider,
    src: &Path,
    policy: &PolicyConfig,
    only: Option<&str>,
    hooks: &EvalHooks,
    parallelism: usize,
) -> anyhow::Result<Vec<EvalRow>> {
    let source_bytes = crate::transaction::read(src)
        .with_context(|| format!("reading {} for probe extraction", src.display()))?;
    // Plans and replay share the same captured bytes even if the source appends.
    let transcript = load(provider, src, &source_bytes)?;
    eval_frozen(transcript, source_bytes, policy, only, hooks, parallelism)
}

fn eval_frozen(
    transcript: Transcript,
    source_bytes: Vec<u8>,
    policy: &PolicyConfig,
    only: Option<&str>,
    hooks: &EvalHooks,
    parallelism: usize,
) -> anyhow::Result<Vec<EvalRow>> {
    // Restrict extraction before the global/per-kind caps and deduplication:
    // old history must neither starve live probes nor satisfy their recall.
    gobstopper_core::policy::validate_policy(policy).map_err(anyhow::Error::msg)?;
    let source_context = live_context_text(&transcript, &String::from_utf8_lossy(&source_bytes));
    let probes = extract_probes(&source_context);
    let tail_start = protected_tail_start(&transcript, policy);

    let strategies: Vec<Box<dyn Strategy>> = match only {
        Some(id) => {
            vec![strategy_by_id(id).ok_or_else(|| anyhow::anyhow!("unknown strategy '{id}'"))?]
        }
        None => builtin_strategies(),
    };

    // Phase 1 (sequential): build each row skeleton and compute its
    // plan. `hooks.scorer` is only ever used here, so it carries no
    // `Sync` bound. Plans that rewrite the file become phase-2 work
    // items using the same captured source bytes.
    let mut items: Vec<(EvalRow, Option<CompactionPlan>)> = Vec::with_capacity(strategies.len());
    for strat in &strategies {
        let mut row = EvalRow {
            strategy: strat.id().to_string(),
            version: env!("CARGO_PKG_VERSION"),
            source_sha256: crate::copy::sha256(&source_bytes),
            result_sha256: None,
            source_bytes: source_bytes.len(),
            result_bytes: None,
            execution_state: "not_planned",
            token_basis: "projected_strategy_estimate_not_provider_usage_or_billing",
            charged_tokens: None,
            cache_hits: None,
            refetches: None,
            continuation_success: None,
            plan: None,
            est_reclaimed: 0,
            findings: Vec::new(),
            verify_errors: 0,
            verify_warnings: 0,
            probe_score: None,
            semantic_score: None,
            prefix_tokens: 0,
            duration_ms: 0,
            error: None,
        };
        // The `scored` row uses the injected driver when one is
        // configured — matching what `plan`/`apply` would produce —
        // and falls back to the built-in heuristic otherwise.
        let plan_opt = match hooks.scorer.filter(|_| strat.id() == "scored") {
            Some(scorer) => {
                let candidates = ScoredStrategy::candidates(&transcript, policy);
                let scores = scorer.score(&transcript, &candidates);
                ScoredStrategy::scores_to_plan(&transcript, policy, &scores)
            }
            None => strat.evaluate(&transcript, policy),
        };
        let mut work = None;
        if let Some(plan) = plan_opt.filter(|plan| {
            policy.accepts_savings(plan.context_tokens_before, plan.context_tokens_after)
        }) {
            row.est_reclaimed = plan.est_savings();
            row.prefix_tokens = prefix_tokens(&transcript, &plan);
            let needs_rewrite = plan
                .edits
                .iter()
                .any(|e| !matches!(e, Edit::ProviderCompact { .. } | Edit::CacheEdit { .. }));
            if needs_rewrite {
                work = Some(plan);
            } else {
                row.plan = Some(plan);
                row.execution_state = "provider_not_executed";
            }
        }
        items.push((row, work));
    }

    // Phase 2 (parallel): every file-rewriting plan runs against its
    // own in-memory copy through the bounded pool. Results merge back in
    // strategy order; one row's failure or panic never touches another.
    let work_indices: Vec<usize> = items
        .iter()
        .enumerate()
        .filter_map(|(i, (_, work))| work.is_some().then_some(i))
        .collect();
    let judge = hooks.probe_judge;
    let outcomes = run_indexed(work_indices.len(), parallelism, |k| {
        let plan = items[work_indices[k]]
            .1
            .as_ref()
            .expect("work item recorded for this row");
        run_on_copy(&transcript, &source_bytes, plan, &probes, tail_start, judge)
    });
    for (k, outcome) in outcomes.into_iter().enumerate() {
        let (row, work) = &mut items[work_indices[k]];
        let plan = work.take().expect("work item still present");
        row.plan = Some(plan);
        match outcome {
            Ok(Ok(PreparedOutcome {
                duration_ms,
                findings,
                score,
                semantic,
                sha256,
                bytes,
            })) => {
                row.execution_state = "detached_transform_complete";
                row.result_sha256 = Some(sha256);
                row.result_bytes = Some(bytes);
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
                row.semantic_score = semantic;
            }
            Ok(Err(_)) => {
                row.execution_state = "failed";
                row.error = Some("transformation_failed".into());
            }
            Err(_) => {
                row.execution_state = "failed";
                row.error = Some("worker_panicked".into());
            }
        }
    }
    Ok(items.into_iter().map(|(row, _)| row).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::Severity;
    use gobstopper_core::QuotaPressure;
    use std::fs;
    use std::path::PathBuf;

    static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

    /// Keep shared test observations stable while exercising parallel eval.
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
            min_savings_tokens: 0,
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
    fn native_compaction_probes_use_replacement_context_before_capping() {
        let _guard = EVAL_LOCK.lock().unwrap();
        let dir = TestDir::new();
        let mut records = Vec::new();
        // Dead history exceeds every per-kind probe cap and also retains
        // the exact live output we will remove. Neither may affect recall.
        for i in 0..80 {
            records.push(serde_json::json!({
                "type": "response_item", "payload": {"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": format!(
                        "/dead/file_{i}.rs cargo dead_{i} decided dead_choice_{i} error: dead_failure_{i} /live/removed.rs"
                    )}]}
            }));
        }
        records.push(serde_json::json!({"type": "compacted", "payload": {
            "message": "/not-in-replacement/phantom.rs",
            "replacement_history": [
                {"type": "function_call", "call_id": "old", "name": "read", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "old",
                    "output": format!("/live/removed.rs {}", "x".repeat(3000))}
            ]
        }}));
        records.push(serde_json::json!({"type": "response_item", "payload": {
            "type": "function_call", "call_id": "tail", "name": "read", "arguments": "{}"
        }}));
        records.push(serde_json::json!({"type": "response_item", "payload": {
            "type": "function_call_output", "call_id": "tail",
            "output": format!("/live/tail_keeper.rs {}", "y".repeat(3000))
        }}));
        let src = dir.0.join("rollout.jsonl");
        let original = records
            .iter()
            .map(|r| r.to_string() + "\n")
            .collect::<String>();
        fs::write(&src, &original).unwrap();
        let rows = eval_transcript(Provider::Codex, &src, &low_policy(), Some("elide")).unwrap();
        let evaluated = &rows[0];
        assert!(evaluated.error.is_none(), "{:?}", evaluated.error);
        assert_eq!(evaluated.verify_errors, 0);
        let score = evaluated.probe_score.as_ref().unwrap();
        assert!(
            score.probes_total > 0,
            "dead history must not exhaust the live probe budget"
        );
        assert!(
            score.missed_probes.iter().any(|p| p == "/live/removed.rs"),
            "dead history must not satisfy a removed live probe: {score:?}"
        );
        assert!(
            score.tail_probes_total > 0,
            "original source line indexes must survive projection"
        );
        assert!(score.tail_intact);
        let paths = score
            .by_kind
            .iter()
            .find(|kind| kind.kind == gobstopper_core::probe::ProbeKind::Path)
            .unwrap();
        assert_eq!(
            paths.total, 2,
            "only replacement and tail paths are live; wrapper summary is not"
        );
        assert_eq!(paths.recalled, 1);
        assert_eq!(fs::read_to_string(&src).unwrap(), original);
    }

    #[test]
    fn dead_claude_branch_does_not_satisfy_removed_live_probes() {
        let _guard = EVAL_LOCK.lock().unwrap();
        let dir = TestDir::new();
        let src = write_probe_transcript(&dir.0);
        let live = fs::read_to_string(&src).unwrap();
        let dead = serde_json::json!({
            "type": "user", "uuid": "dead-root",
            "message": {"role": "user", "content":
                "error[E0308]: mismatched types in /project/crates/core/src/lib.rs FAILED"}
        });
        fs::write(&src, format!("{dead}\n{live}")).unwrap();
        let rows =
            eval_transcript(Provider::ClaudeCode, &src, &low_policy(), Some("elide")).unwrap();
        let evaluated = &rows[0];
        assert!(evaluated.error.is_none(), "{:?}", evaluated.error);
        assert_eq!(evaluated.verify_errors, 0);
        let score = evaluated.probe_score.as_ref().unwrap();
        assert!(
            score
                .missed_probes
                .iter()
                .any(|p| p == "/project/crates/core/src/lib.rs"),
            "the dead branch must neither consume nor satisfy live probes: {score:?}"
        );
        assert!(score.tail_probes_total > 0);
        assert!(score.tail_intact);
    }

    #[test]
    fn eval_reports_all_strategies_and_leaves_source_untouched() {
        let _guard = EVAL_LOCK.lock().unwrap();
        let dir = TestDir::new();
        let src = write_claude_transcript(&dir.0);
        let before = fs::read(&src).unwrap();

        let rows = eval_transcript_with_hooks(
            Provider::ClaudeCode,
            &src,
            &low_policy(),
            None,
            &EvalHooks::default(),
        )
        .unwrap();

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

        // Elide transforms detached bytes: real savings, still verifies clean.
        let elide = row(&rows, "elide");
        assert!(elide.plan.is_some(), "elide should plan: {:?}", elide.error);
        assert!(elide.est_reclaimed > 0);
        assert!(elide.error.is_none());
        assert!(
            elide.findings.iter().all(|f| f.severity != Severity::Error),
            "elide candidate should have no error findings: {:?}",
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
                    .all(|e| matches!(e, Edit::ProviderCompact { .. } | Edit::CacheEdit { .. })),
                "{id} should delegate on a hot transcript: {:?}",
                plan.edits
            );
        }

        // Source file byte-identical.
        assert_eq!(fs::read(&src).unwrap(), before);

        // Replay needs no durable output beside the fixture source.
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn only_filters_to_one_strategy_and_rejects_unknown_ids() {
        let _guard = EVAL_LOCK.lock().unwrap();
        let dir = TestDir::new();
        let src = write_claude_transcript(&dir.0);

        let rows = eval_transcript_with_hooks(
            Provider::ClaudeCode,
            &src,
            &low_policy(),
            Some("elide"),
            &EvalHooks::default(),
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].strategy, "elide");
        assert!(rows[0].est_reclaimed > 0);

        assert!(eval_transcript_with_hooks(
            Provider::ClaudeCode,
            &src,
            &low_policy(),
            Some("nope"),
            &EvalHooks::default()
        )
        .is_err());
    }

    #[test]
    fn elide_row_carries_probe_score_and_verify_counts() {
        let _guard = EVAL_LOCK.lock().unwrap();
        let dir = TestDir::new();
        let src = write_probe_transcript(&dir.0);

        let rows = eval_transcript_with_hooks(
            Provider::ClaudeCode,
            &src,
            &low_policy(),
            None,
            &EvalHooks::default(),
        )
        .unwrap();
        let elide = row(&rows, "elide");
        assert!(elide.error.is_none());

        let score = elide
            .probe_score
            .as_ref()
            .expect("elide transforms detached bytes and scores them");
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
        policy.trigger_tokens = gobstopper_core::admission::MAX_POLICY_TOKENS;

        let rows = eval_transcript_with_hooks(
            Provider::ClaudeCode,
            &src,
            &policy,
            None,
            &EvalHooks::default(),
        )
        .unwrap();
        assert_eq!(rows.len(), 12);
        for r in &rows {
            assert!(r.plan.is_none(), "{} should not fire", r.strategy);
            assert_eq!(r.est_reclaimed, 0);
            assert!(r.findings.is_empty());
            assert!(r.error.is_none());
        }
    }

    /// Driver that scores every candidate maximally keepable — the
    /// scored row should then elide nothing the window doesn't force,
    /// and the counter proves the driver ran (not the heuristic).
    struct KeepAllDriver {
        calls: AtomicUsize,
    }

    impl ScoreDriver for KeepAllDriver {
        fn score(
            &self,
            _t: &Transcript,
            candidates: &[usize],
        ) -> Vec<gobstopper_core::strategy::ScoredItem> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            candidates
                .iter()
                .map(|&item_index| gobstopper_core::strategy::ScoredItem {
                    item_index,
                    keep_probability: 1.0,
                })
                .collect()
        }
    }

    /// Judge that reports every probe as surviving.
    struct AllSurviveJudge {
        calls: AtomicUsize,
        probes_seen: AtomicUsize,
    }

    impl ProbeJudge for AllSurviveJudge {
        fn score(&self, probes: &[Probe], _post_text: &str) -> Option<Vec<f64>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.probes_seen.store(probes.len(), Ordering::Relaxed);
            Some(vec![1.0; probes.len()])
        }
    }

    #[test]
    fn hooks_drive_scored_row_and_semantic_score() {
        let _guard = EVAL_LOCK.lock().unwrap();
        let dir = TestDir::new();
        let src = write_probe_transcript(&dir.0);
        let driver = KeepAllDriver {
            calls: AtomicUsize::new(0),
        };
        let judge = AllSurviveJudge {
            calls: AtomicUsize::new(0),
            probes_seen: AtomicUsize::new(0),
        };
        let hooks = EvalHooks {
            scorer: Some(&driver),
            probe_judge: Some(&judge),
        };

        let rows = eval_transcript_with_hooks(
            Provider::ClaudeCode,
            &src,
            &low_policy(),
            Some("scored"),
            &hooks,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        let scored = &rows[0];
        assert_eq!(driver.calls.load(Ordering::Relaxed), 1);
        // All-1.0 keep probabilities still elide what the savings floor
        // demands, but nothing more: the driver produced the plan.
        let plan = scored.plan.as_ref().expect("scored produces a plan");
        assert!(plan.edits.iter().any(|e| matches!(e, Edit::Elide { .. })));
        // The judge ran on the rewritten copy: every probe "survives"
        // semantically even where the verbatim string was elided.
        let semantic = scored
            .semantic_score
            .as_ref()
            .expect("judge produced a semantic score");
        assert_eq!(semantic.recall, 1.0);
        let verbatim = scored.probe_score.as_ref().unwrap();
        assert_eq!(semantic.probes_total, verbatim.probes_total);
        assert_eq!(judge.calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            judge.probes_seen.load(Ordering::Relaxed),
            verbatim.probes_total - verbatim.probes_recalled
        );
        // And the verbatim score still shows real losses — the two
        // scores measure different things.
        assert!(verbatim.recall < 1.0);
        assert!(semantic.recall >= verbatim.recall);
    }

    #[test]
    fn partial_judge_responses_retain_missingness_and_result_identity() {
        struct NoAnswers;
        impl ProbeJudge for NoAnswers {
            fn score(&self, _: &[Probe], _: &str) -> Option<Vec<f64>> {
                Some(Vec::new())
            }
        }
        let _guard = EVAL_LOCK.lock().unwrap();
        let dir = TestDir::new();
        let src = write_probe_transcript(&dir.0);
        let hooks = EvalHooks {
            scorer: None,
            probe_judge: Some(&NoAnswers),
        };
        let rows = eval_transcript_with_hooks(
            Provider::ClaudeCode,
            &src,
            &low_policy(),
            Some("elide"),
            &hooks,
        )
        .unwrap();
        let row = &rows[0];
        assert_eq!(row.execution_state, "detached_transform_complete");
        assert_eq!(
            row.source_sha256,
            crate::copy::sha256(&fs::read(&src).unwrap())
        );
        assert!(row.result_sha256.is_some());
        assert!(row.charged_tokens.is_none() && row.continuation_success.is_none());
        let literal = row.probe_score.as_ref().unwrap();
        let judged = row.semantic_score.as_ref().unwrap();
        assert!(literal.probes_recalled < literal.probes_total);
        assert_eq!(judged.probes_requested, literal.probes_total);
        assert_eq!(judged.probes_total, literal.probes_recalled);
        assert!(!judged.complete);
    }

    #[test]
    fn semantic_judge_skips_verbatim_survivors() {
        let _guard = EVAL_LOCK.lock().unwrap();
        let dir = TestDir::new();
        let src = write_probe_transcript(&dir.0);
        let source = fs::read_to_string(&src).unwrap();
        let probes = extract_probes(&source);
        let plan = CompactionPlan {
            strategy: "test".into(),
            rationale: "test".into(),
            edits: vec![Edit::InjectDigest {
                digest: gobstopper_core::plan::DigestBlock::default(),
            }],
            context_tokens_before: 1,
            context_tokens_after: 1,
        };
        let judge = AllSurviveJudge {
            calls: AtomicUsize::new(0),
            probes_seen: AtomicUsize::new(0),
        };
        let transcript = load(Provider::ClaudeCode, &src, &fs::read(&src).unwrap()).unwrap();
        let PreparedOutcome {
            score: verbatim,
            semantic,
            ..
        } = run_on_copy(
            &transcript,
            &fs::read(&src).unwrap(),
            &plan,
            &probes,
            usize::MAX,
            Some(&judge),
        )
        .unwrap();
        assert_eq!(verbatim.recall, 1.0);
        assert_eq!(semantic.unwrap().recall, 1.0);
        assert_eq!(judge.calls.load(Ordering::Relaxed), 0);
        assert_eq!(judge.probes_seen.load(Ordering::Relaxed), 0);
    }

    /// Rows whose verbatim probe pass missed at least one probe —
    /// exactly the rows that invoked the judge. (A fully recalled row
    /// still gets a `semantic_score` clone without a judge call.)
    fn judged_rows(rows: &[EvalRow]) -> usize {
        rows.iter()
            .filter(|r| {
                r.probe_score
                    .as_ref()
                    .is_some_and(|s| !s.missed_probes.is_empty())
            })
            .count()
    }

    /// Sync judge that tracks in-flight calls so tests can observe the
    /// pool's peak concurrency. Each call sleeps `sleep_ms` to widen the
    /// overlap window; the earliest-claimed call sleeps `first_ms`
    /// instead, so one row can be made to finish after all the others.
    struct SlowJudge {
        active: AtomicUsize,
        peak: AtomicUsize,
        calls: AtomicUsize,
        sleep_ms: u64,
        first_ms: u64,
    }

    impl SlowJudge {
        fn new(sleep_ms: u64, first_ms: u64) -> Self {
            Self {
                active: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
                calls: AtomicUsize::new(0),
                sleep_ms,
                first_ms,
            }
        }
    }

    impl ProbeJudge for SlowJudge {
        fn score(&self, probes: &[Probe], _post_text: &str) -> Option<Vec<f64>> {
            let seq = self.calls.fetch_add(1, Ordering::SeqCst);
            let in_flight = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(in_flight, Ordering::SeqCst);
            let ms = if seq == 0 {
                self.first_ms.max(self.sleep_ms)
            } else {
                self.sleep_ms
            };
            std::thread::sleep(std::time::Duration::from_millis(ms));
            self.active.fetch_sub(1, Ordering::SeqCst);
            Some(vec![1.0; probes.len()])
        }
    }

    #[test]
    fn rewrite_phase_bounds_concurrency_and_judges_every_row() {
        let _guard = EVAL_LOCK.lock().unwrap();
        let dir = TestDir::new();
        let src = write_probe_transcript(&dir.0);
        let judge = SlowJudge::new(20, 20);
        let hooks = EvalHooks {
            scorer: None,
            probe_judge: Some(&judge),
        };

        let rows =
            eval_transcript_inner(Provider::ClaudeCode, &src, &low_policy(), None, &hooks, 2)
                .unwrap();

        let expected = judged_rows(&rows);
        assert!(
            expected >= 2,
            "test needs multiple judged rows; got {:?}",
            rows.iter().map(|r| r.strategy.as_str()).collect::<Vec<_>>()
        );
        // Every row that needed the judge got it — none lost in the pool.
        assert_eq!(judge.calls.load(Ordering::SeqCst), expected);
        let peak = judge.peak.load(Ordering::SeqCst);
        assert!(peak <= 2, "peak concurrency {peak} exceeded bound 2");
        assert!(
            peak >= 2,
            "judge calls never overlapped — pool ran serially"
        );
        // Row order is still strategy order.
        let ids: Vec<&str> = rows.iter().map(|r| r.strategy.as_str()).collect();
        let expected_order: Vec<&str> = builtin_strategies().iter().map(|s| s.id()).collect();
        assert_eq!(ids, expected_order);
    }

    #[test]
    fn rewrite_phase_preserves_row_order_when_rows_finish_out_of_order() {
        let _guard = EVAL_LOCK.lock().unwrap();
        let dir = TestDir::new();
        let src = write_probe_transcript(&dir.0);
        // The earliest-claimed row sleeps far longer than the rest, so
        // under a wide pool at least one later row completes first.
        let judge = SlowJudge::new(5, 120);
        let hooks = EvalHooks {
            scorer: None,
            probe_judge: Some(&judge),
        };

        let rows = eval_transcript_inner(
            Provider::ClaudeCode,
            &src,
            &low_policy(),
            None,
            &hooks,
            MAX_EVAL_PARALLEL,
        )
        .unwrap();

        assert!(
            judged_rows(&rows) >= 2,
            "test needs multiple judged rows to observe reordering"
        );
        let ids: Vec<&str> = rows.iter().map(|r| r.strategy.as_str()).collect();
        let expected_order: Vec<&str> = builtin_strategies().iter().map(|s| s.id()).collect();
        assert_eq!(ids, expected_order);
        for r in &rows {
            assert!(r.error.is_none(), "{} unexpectedly errored", r.strategy);
        }
    }

    #[test]
    fn eval_parallel_env_parse_defaults_and_clamps() {
        assert_eq!(parse_parallelism(None), DEFAULT_EVAL_PARALLEL);
        assert_eq!(parse_parallelism(Some("2".into())), 2);
        assert_eq!(parse_parallelism(Some("0".into())), 1);
        assert_eq!(parse_parallelism(Some("64".into())), MAX_EVAL_PARALLEL);
        assert_eq!(
            parse_parallelism(Some("junk".into())),
            DEFAULT_EVAL_PARALLEL
        );
    }
}
