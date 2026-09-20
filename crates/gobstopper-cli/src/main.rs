//! gobstopper: automatic context compaction for Codex and Claude Code.

mod apple;
mod apple_digest;
mod apple_scorer;
mod config;
mod hooks;
mod jev;
mod llm_scorer;
mod mcp;
mod report;
mod secrets;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use gobstopper_adapters::detect::{self, Discovered, Roots};
use gobstopper_adapters::{
    codex, copy, eval, fork, plugins, recovery, vault, verify, AdapterError,
};
use gobstopper_core::events::{append_event, default_log_path, CompactionEvent};
use gobstopper_core::plan::{CompactionPlan, Edit};
use gobstopper_core::strategy::{self, HeuristicScorer, QuotaPressure, ScoredStrategy};
use gobstopper_core::Provider;
use std::io::{BufRead as _, IsTerminal as _, Read as _, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Parser)]
#[command(
    name = "gobstopper",
    version,
    about = "Automatic context compaction for coding-agent sessions"
)]
struct Cli {
    /// Codex state root (default: $CODEX_HOME or ~/.codex).
    #[arg(long, global = true)]
    codex_home: Option<PathBuf>,
    /// Claude state root (default: $CLAUDE_CONFIG_DIR or ~/.claude).
    #[arg(long, global = true)]
    claude_home: Option<PathBuf>,
    /// Codex CLI binary for provider controls (default: $GOBSTOPPER_CODEX_BIN or `codex` on PATH).
    #[arg(long, global = true)]
    codex_bin: Option<PathBuf>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List detected sessions with context occupancy.
    Detect {
        /// Include sessions of any age (default: last 7 days).
        #[arg(long)]
        all: bool,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show the compaction plan a strategy would produce for a session.
    Plan {
        /// Session id prefix, or path to a transcript file.
        session: String,
        #[arg(long)]
        strategy: Option<String>,
        #[arg(long)]
        preset: Option<String>,
        /// Override the trigger threshold (tokens) for this evaluation.
        #[arg(long)]
        trigger: Option<u64>,
        /// Override the post-compaction floor (tokens) for this evaluation.
        #[arg(long)]
        floor: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    /// Apply a compaction plan to a session transcript.
    Apply {
        session: String,
        #[arg(long)]
        strategy: Option<String>,
        #[arg(long)]
        preset: Option<String>,
        /// Override the trigger threshold (tokens) for this evaluation.
        #[arg(long)]
        trigger: Option<u64>,
        /// Override the post-compaction floor (tokens) for this evaluation.
        #[arg(long)]
        floor: Option<u64>,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
        /// Retired compatibility flag; standalone compaction is copy-only.
        #[arg(long)]
        in_place: bool,
        /// Retired compatibility flag; snapshots are mandatory.
        #[arg(long)]
        no_backup: bool,
        /// Emit an experimental synthetic Codex `compacted` record instead
        /// of the portable forked digest representation.
        #[arg(long)]
        experimental_compacted: bool,
    },
    /// Check a transcript for resume-breaking defects (broken parent
    /// chains, orphaned tool calls, malformed compaction records).
    Verify {
        /// Session id prefix, or path to a transcript file.
        session: String,
        /// Emit JSON findings.
        #[arg(long)]
        json: bool,
    },
    /// Restore a transcript from the content-addressed snapshot vault.
    Undo {
        /// Session id prefix, or path to a transcript file.
        session: String,
        /// Snapshot sha256 prefix (default: latest snapshot for the file).
        #[arg(long)]
        sha: Option<String>,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
        /// Retired compatibility flag; standalone restore is copy-only.
        #[arg(long)]
        in_place: bool,
    },
    /// Clone a session transcript under a fresh session id (fork-on-write)
    /// and print the provider resume command.
    Fork {
        /// Session id prefix, or path to a transcript file.
        session: String,
    },
    /// Replay a transcript through every strategy against temp copies:
    /// report savings and post-edit verify findings without touching the
    /// source file.
    Eval {
        /// Session id prefix, or path to a transcript file.
        session: String,
        /// Evaluate only this strategy id.
        #[arg(long)]
        strategy: Option<String>,
        /// Override the trigger threshold (tokens).
        #[arg(long)]
        trigger: Option<u64>,
        /// Override the post-compaction floor (tokens).
        #[arg(long)]
        floor: Option<u64>,
        /// Emit JSON rows.
        #[arg(long)]
        json: bool,
    },
    /// Emit the Claude `cache_edits` tool_use_ids for a session as JSON.
    /// Does not modify the transcript; the caller dispatches the ids to
    /// the Anthropic API.
    CacheEdits {
        /// Session id prefix, or path to a transcript file.
        session: String,
        /// Override the trigger threshold (tokens).
        #[arg(long)]
        trigger: Option<u64>,
        /// Override the post-compaction floor (tokens).
        #[arg(long)]
        floor: Option<u64>,
        /// Emit the full plan (including the digest) as JSON.
        #[arg(long)]
        plan: bool,
    },
    /// Show the trigger/floor the adaptive tuner derives for a session
    /// from its provider window, elidable share, and past compaction
    /// yields — and the TOML to pin them.
    Tune {
        /// Session id prefix, or path to a transcript file.
        session: String,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// Install provider hook entries (Claude settings.json, Codex
    /// hooks.json) that call back into `gobstopper hook <event>` at
    /// compaction lifecycle points. Additive merge; never removes
    /// existing hooks.
    InstallHooks,
    /// Remove gobstopper hook entries from provider config.
    UninstallHooks,
    /// Handle a provider hook callback (reads hook JSON on stdin).
    /// Invoked by provider hook configs, not by users.
    #[command(hide = true)]
    Hook {
        /// "precompact" | "session-start"
        event: String,
    },
    /// Emit a session-observations-v1 report (aicharts schema) joining
    /// detected sessions with compaction telemetry. JSON on stdout.
    Report {
        /// Strip the `gobstopper` extension key so the output parses
        /// strictly against aicharts' session-observations-v1 schema.
        #[arg(long)]
        strict: bool,
        /// Report only files updated in the last 180 seconds.
        #[arg(long)]
        active_only: bool,
    },
    /// Show compaction telemetry: recent events and cumulative savings.
    Events {
        /// Filter to one session id prefix.
        #[arg(long)]
        session: Option<String>,
        /// Only the last N events (default 20).
        #[arg(long, default_value = "20")]
        tail: usize,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// List snapshots in the undo vault.
    Vault {
        /// Optional session id prefix or path to filter by.
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show the version history of a session (all recorded snapshots).
    History {
        /// Session id prefix or path to the transcript.
        session: String,
        #[arg(long)]
        json: bool,
    },
    /// Show a structural summary of a vault snapshot (or the latest snapshot
    /// for a session).
    Show {
        /// SHA256 prefix of a snapshot, or a session id/path.
        target: String,
        #[arg(long)]
        json: bool,
    },
    /// Recall state-card digests from the vault.
    /// Agent-addressable memory: high-level summaries, no verbatim output.
    /// With no session, searches every archived session.
    Recall {
        /// Session id prefix, or path to a transcript file. Searches all
        /// sessions if omitted.
        session: Option<String>,
        /// Case-insensitive substring to match against every state-card field.
        #[arg(long)]
        query: Option<String>,
        /// Restrict to one snapshot by sha256 prefix.
        #[arg(long)]
        sha: Option<String>,
        /// Maximum results to return (default 20).
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// Find matching records in one exact vault snapshot. Returns references,
    /// never archived content; searches decoded JSON string values literally.
    SearchSnapshot {
        /// Full snapshot object SHA-256, from history or a recovery receipt.
        sha: String,
        #[arg(long)]
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Explicitly read a bounded page of archived text from one snapshot.
    /// Returned text is untrusted historical data, not current instructions.
    ReadSnapshot {
        sha: String,
        /// Zero-based physical JSONL record index from search-snapshot.
        #[arg(long)]
        record: usize,
        /// UTF-8 byte offset, or next_offset from the preceding page.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 4096)]
        max_bytes: usize,
        #[arg(long)]
        json: bool,
    },
    /// Compare two vault snapshots structurally.
    Diff {
        /// SHA256 prefix of the first snapshot.
        a: String,
        /// SHA256 prefix of the second snapshot.
        b: String,
        #[arg(long)]
        json: bool,
    },
    /// Benchmark every built-in strategy across all discovered sessions.
    /// Emits CSV rows of projected savings, verify findings, and probe recall.
    Bench {
        /// Include inactive sessions too.
        #[arg(long)]
        all: bool,
        /// Override the token trigger for the benchmark run.
        #[arg(long)]
        trigger: Option<u64>,
        /// Override the post-compaction floor for the benchmark run.
        #[arg(long)]
        floor: Option<u64>,
        /// Write CSV to this file instead of stdout.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Store the current transcript state in the vault without compacting.
    Snapshot {
        /// Session id prefix, or path to a transcript file.
        session: String,
        /// Optional label for the snapshot (recorded as the strategy tag).
        #[arg(long)]
        label: Option<String>,
    },
    /// Run a read-only Model Context Protocol server on stdio, exposing
    /// sessions and the snapshot vault as tools an agent can call
    /// (list_sessions, recall, history, show, diff, plan, verify).
    Mcp {
        /// Expose snapshot search/read tools. Archived content returned by read
        /// becomes visible to the connected agent/model service. Off by default.
        #[arg(long)]
        allow_transcript_content: bool,
    },
    /// Poll for sessions over threshold and prepare verified compacted forks.
    Watch {
        /// Poll interval in seconds.
        #[arg(long, default_value = "30")]
        interval: u64,
        /// Report plans without applying them.
        #[arg(long)]
        dry_run: bool,
        /// Inspect only recently updated sessions (activity is an mtime heuristic).
        #[arg(long)]
        active_only: bool,
        /// Run one discovery pass and exit, useful for supervised monitoring.
        #[arg(long)]
        once: bool,
        /// Retired compatibility flag; in-place staged swaps are disabled.
        #[arg(long)]
        double_buffer: bool,
    },
    /// Manage vaulted provider credentials (OS keychain).
    Auth {
        /// Provider to configure (currently only `jev`).
        provider: String,
        /// Show where the key comes from and run a live check.
        #[arg(long)]
        status: bool,
        /// Remove the stored key.
        #[arg(long)]
        delete: bool,
    },
    /// Pure policy check for integrators (oompa): give the numbers, get
    /// the action. Reads no transcript files.
    PolicyCheck {
        #[arg(long)]
        provider: String,
        #[arg(long)]
        context_tokens: u64,
        #[arg(long, default_value = "false")]
        session_active: bool,
        /// Provider quota pressure; scales the effective trigger.
        #[arg(long, value_enum)]
        quota_pressure: Option<QuotaArg>,
        #[arg(long)]
        preset: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List configured presets.
    Presets,
    /// Print the economics model behind gobstopper's defaults.
    Explain,
    Plugin {
        #[command(subcommand)]
        command: PluginCmd,
    },
}

#[derive(Subcommand)]
enum PluginCmd {
    Check {
        manifest: PathBuf,
    },
    Inspect {
        manifest: PathBuf,
        #[arg(long)]
        trusted_sha256: String,
        #[arg(long)]
        provider: String,
        #[arg(long)]
        source: PathBuf,
    },
}

/// CLI mirror of `QuotaPressure` (kept separate so the flag surface stays
/// a closed vocabulary).
#[derive(Clone, Copy, ValueEnum)]
enum QuotaArg {
    Low,
    Normal,
    High,
}

impl From<QuotaArg> for QuotaPressure {
    fn from(q: QuotaArg) -> Self {
        match q {
            QuotaArg::Low => QuotaPressure::Low,
            QuotaArg::Normal => QuotaPressure::Normal,
            QuotaArg::High => QuotaPressure::High,
        }
    }
}

fn roots(cli: &Cli) -> Roots {
    let mut r = Roots::from_env();
    if let Some(p) = &cli.codex_home {
        r.codex_home = p.clone();
    }
    if let Some(p) = &cli.claude_home {
        r.claude_home = p.clone();
    }
    r
}

fn find_session(cli: &Cli, cfg: &config::Config, query: &str) -> Result<Discovered> {
    let path = PathBuf::from(query);
    if path.is_file() {
        // Direct transcript path: construct a discovered entry ad hoc.
        let provider = detect::sniff_provider(&path).unwrap_or_else(|| {
            if query.contains("rollout-") || query.contains(".codex") {
                Provider::Codex
            } else {
                Provider::ClaudeCode
            }
        });
        let meta = match provider {
            Provider::Codex => gobstopper_adapters::codex::scan_meta(&path),
            Provider::ClaudeCode => gobstopper_adapters::claude::scan_meta(&path),
        };
        let usage = match provider {
            Provider::Codex => gobstopper_adapters::codex::scan_usage(&path),
            Provider::ClaudeCode => gobstopper_adapters::claude::scan_usage(&path),
        };
        let age = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs())
            .unwrap_or(u64::MAX);
        return Ok(Discovered {
            handle: gobstopper_core::SessionHandle {
                provider,
                session_id: meta.0.unwrap_or_else(|| {
                    path.file_stem()
                        .map(|value| value.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "unknown".to_string())
                }),
                path,
                cwd: meta.1,
                age_secs: age,
            },
            usage,
        });
    }
    let _ = cfg;
    let matches = detect::find(&roots(cli), query);
    match matches.len() {
        0 => bail!("no session matching '{query}'"),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => bail!("'{query}' matches {n} sessions; use a longer prefix or a path"),
    }
}

/// Recent applied compactions for one session as
/// `(context_tokens_before, est_reclaimed_tokens)`, newest first —
/// the past-yield evidence the adaptive tuner reads.
fn recent_applied(session_id: &str, provider: Provider) -> Vec<(u64, u64)> {
    let path = gobstopper_core::events::default_log_path();
    let Ok(events) = gobstopper_core::events::read_events(&path) else {
        return Vec::new();
    };
    events
        .iter()
        .filter(|e| e.session_id == session_id && e.provider == provider && e.outcome == "applied")
        .rev()
        .take(3)
        .map(|e| (e.context_tokens_before, e.est_reclaimed_tokens))
        .collect()
}

/// Full adaptive sample from a parsed transcript.
fn adaptive_sample(transcript: &gobstopper_core::Transcript) -> gobstopper_core::AdaptiveSample {
    gobstopper_core::AdaptiveSample {
        model_context_window: transcript.usage.model_context_window,
        context_tokens: transcript.context_tokens(),
        elidable_tokens: Some(transcript.elidable_tokens()),
        recent: recent_applied(&transcript.session.session_id, transcript.session.provider),
    }
}

/// Transcript-free adaptive sample for cheap pre-checks (watch loop):
/// the elidable-share rule is skipped.
fn adaptive_sample_usage(
    provider: Provider,
    session_id: &str,
    usage: &gobstopper_core::UsageSample,
) -> gobstopper_core::AdaptiveSample {
    gobstopper_core::AdaptiveSample {
        model_context_window: usage.model_context_window,
        context_tokens: usage.context_tokens,
        elidable_tokens: None,
        recent: recent_applied(session_id, provider),
    }
}

/// The policy a decision should use right now: the resolved layers plus
/// the adaptive tuner when `policy.adaptive` is set.
fn effective_policy(
    transcript: &gobstopper_core::Transcript,
    resolved: &config::Resolved,
) -> (gobstopper_core::PolicyConfig, Vec<&'static str>) {
    if !resolved.policy.adaptive {
        return (resolved.policy.clone(), Vec::new());
    }
    let outcome = gobstopper_core::adapt(&resolved.policy, &adaptive_sample(transcript));
    (outcome.policy, outcome.reasons)
}

fn maybe_scorer() -> Option<Box<dyn gobstopper_core::ScoreDriver>> {
    // The LLM and Jev scorers are opt-in. The built-in heuristic is the
    // default because it is fast, deterministic, and private.
    let name = std::env::var("GOBSTOPPER_SCORER").ok()?;
    let driver = match name.as_str() {
        "llm" => llm_scorer::maybe_llm_scorer(),
        "jev" => jev::maybe_jev_scorer(),
        "apple" => apple_scorer::maybe_apple_scorer(),
        _ => None,
    };
    if driver.is_none() {
        // A configured scorer that resolves to nothing silently becomes
        // the heuristic — say so once per process so a missing key,
        // unavailable bridge, or typo'd name is never invisible.
        static WARN: std::sync::Once = std::sync::Once::new();
        WARN.call_once(|| {
            eprintln!("warning: GOBSTOPPER_SCORER={name} resolved no driver; using heuristic");
        });
    }
    driver
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum NoPlanReason {
    BelowTrigger,
    StrategyReturnedNoPlan,
    EmptyExternalEdits,
    MinimumSavingsNotMet,
    ExternalNonreducingPlan,
}

/// Numeric diagnostics captured during the original evaluation. A policy target
/// is not an achievable floor; projections exist only for a rejected proposal.
#[derive(Debug, serde::Serialize)]
struct NoPlanReport {
    status: &'static str,
    reason_code: NoPlanReason,
    context_tokens_before: u64,
    effective_trigger_tokens: u64,
    target_context_tokens: u64,
    min_savings_tokens: u64,
    projected_context_tokens_after: Option<u64>,
    projected_savings_tokens: Option<u64>,
    #[serde(skip)]
    adaptive_reasons: Vec<&'static str>,
}

impl NoPlanReport {
    fn new(
        before: u64,
        policy: &gobstopper_core::PolicyConfig,
        adaptive_reasons: Vec<&'static str>,
    ) -> Self {
        Self {
            status: "no_plan",
            reason_code: NoPlanReason::StrategyReturnedNoPlan,
            context_tokens_before: before,
            effective_trigger_tokens: policy.effective_trigger(),
            target_context_tokens: policy.floor_tokens,
            min_savings_tokens: policy.min_savings_tokens,
            projected_context_tokens_after: None,
            projected_savings_tokens: None,
            adaptive_reasons,
        }
    }

    fn reject(mut self, reason: NoPlanReason, projection: Option<(u64, u64)>) -> Evaluation {
        self.reason_code = reason;
        if let Some((before, after)) = projection {
            self.context_tokens_before = before;
            self.projected_context_tokens_after = Some(after);
            self.projected_savings_tokens = Some(before.saturating_sub(after));
        }
        Evaluation::NoPlan(self)
    }
}

enum Evaluation {
    Plan(CompactionPlan),
    NoPlan(NoPlanReport),
}

fn evaluate(
    transcript: &gobstopper_core::Transcript,
    resolved: &config::Resolved,
) -> Result<Option<CompactionPlan>> {
    Ok(match evaluate_detailed(transcript, resolved)? {
        Evaluation::Plan(plan) => Some(plan),
        Evaluation::NoPlan(_) => None,
    })
}

fn evaluate_detailed(
    transcript: &gobstopper_core::Transcript,
    resolved: &config::Resolved,
) -> Result<Evaluation> {
    config::validate_policy(&resolved.policy)?;
    let (policy, adaptive_reasons) = effective_policy(transcript, resolved);
    config::validate_policy(&policy)?;
    let before = transcript.context_tokens();
    let diagnostic = NoPlanReport::new(before, &policy, adaptive_reasons);
    if before < policy.effective_trigger() {
        return Ok(diagnostic.reject(NoPlanReason::BelowTrigger, None));
    }
    if let Some(selection) = &resolved.plugin {
        let checked = plugins::check(&selection.manifest)?;
        let bytes = gobstopper_adapters::transaction::read(&transcript.session.path)?;
        let content = if checked
            .manifest
            .capabilities
            .contains(&plugins::Capability::ReadContent)
        {
            if bytes.len() > checked.manifest.max_input_bytes {
                bail!("content-authorized plugin input exceeds its manifest byte limit");
            }
            Some(
                std::str::from_utf8(&bytes)?
                    .lines()
                    .map(str::to_string)
                    .collect(),
            )
        } else {
            None
        };
        let mut items = transcript.items.clone();
        if !checked
            .manifest
            .capabilities
            .contains(&plugins::Capability::ReadContent)
        {
            for item in &mut items {
                item.summary = None;
            }
        }
        let request = plugins::Request {
            protocol_version: 1,
            operation: plugins::Capability::Strategy,
            provider_id: transcript.session.provider.as_str().into(),
            source_sha256: copy::sha256(&bytes),
            items,
            usage: transcript.usage,
            policy: Some(policy.clone()),
            content,
        };
        let response = plugins::invoke(&selection.manifest, &selection.trusted_sha256, &request)?;
        return external_plan(
            transcript,
            &policy,
            &resolved.strategy,
            response.edits,
            diagnostic,
        );
    }
    // Userspace command preset: feed the normalized transcript, read edits.
    if let Some(command) = &resolved.command {
        if !resolved.trusted_legacy_command {
            bail!("legacy command execution is not explicitly trusted");
        }
        let plan_json = run_preset_command(command, transcript)?;
        let edits: Vec<Edit> = serde_json::from_value(plan_json["edits"].clone())
            .context("preset command returned invalid edits")?;
        return external_plan(transcript, &policy, &resolved.strategy, edits, diagnostic);
    }
    let mut plan = if resolved.strategy == "scored" {
        let candidates = ScoredStrategy::candidates(transcript, &policy);
        let (scores, scorer_summary) = if let Some(driver) = maybe_scorer() {
            let scores = driver.score(transcript, &candidates);
            (scores, driver.last_run_summary())
        } else {
            (HeuristicScorer.score(transcript, &candidates), None)
        };
        let mut plan = ScoredStrategy::scores_to_plan(transcript, &policy, &scores);
        if let (Some(plan), Some(summary)) = (&mut plan, scorer_summary) {
            // The scorer's request-economy line joins the rationale, so
            // it reaches compaction events and `plan` output rather than
            // staying stderr-only.
            plan.rationale = format!("{} | {summary}", plan.rationale);
        }
        plan
    } else {
        let strat = strategy::strategy_by_id(&resolved.strategy)
            .ok_or_else(|| anyhow::anyhow!("unknown strategy '{}'", resolved.strategy))?;
        strat.evaluate(transcript, &policy)
    };
    if let Some(plan) = &mut plan {
        apple_digest::maybe_upgrade(plan, transcript);
        gobstopper_core::validation::validate_edits(transcript, &policy, &plan.edits)
            .map_err(anyhow::Error::msg)?;
        if !policy.accepts_savings(plan.context_tokens_before, plan.context_tokens_after) {
            return Ok(diagnostic.reject(
                NoPlanReason::MinimumSavingsNotMet,
                Some((plan.context_tokens_before, plan.context_tokens_after)),
            ));
        }
        if !diagnostic.adaptive_reasons.is_empty() {
            plan.rationale = format!(
                "{} | adaptive: {}",
                plan.rationale,
                diagnostic.adaptive_reasons.join(", ")
            );
        }
    }
    Ok(match plan {
        Some(plan) => Evaluation::Plan(plan),
        None => diagnostic.reject(NoPlanReason::StrategyReturnedNoPlan, None),
    })
}

fn external_plan(
    transcript: &gobstopper_core::Transcript,
    policy: &gobstopper_core::PolicyConfig,
    strategy_id: &str,
    edits: Vec<Edit>,
    diagnostic: NoPlanReport,
) -> Result<Evaluation> {
    if edits.is_empty() {
        return Ok(diagnostic.reject(NoPlanReason::EmptyExternalEdits, None));
    }
    gobstopper_core::validation::validate_edits(transcript, policy, &edits)
        .map_err(anyhow::Error::msg)?;
    let before = transcript.context_tokens();
    let mut after = before;
    for edit in &edits {
        match edit {
            Edit::Elide { line_indexes, .. } => {
                let selected: std::collections::HashSet<usize> =
                    line_indexes.iter().copied().collect();
                for item in transcript
                    .items
                    .iter()
                    .filter(|item| selected.contains(&item.line_index))
                {
                    after = after.saturating_sub(item.estimated_elision_savings());
                }
            }
            Edit::InjectDigest { digest } => {
                after = after.saturating_add(gobstopper_core::estimate::estimate_tokens(
                    serde_json::to_vec(digest)?.len(),
                ))
            }
            Edit::CacheEdit { .. } => {
                bail!("external strategy plugins cannot dispatch Claude cache_edits")
            }
            Edit::ProviderCompact { .. } => {
                bail!("external strategy plugins cannot dispatch provider controls")
            }
        }
    }
    if after >= before {
        return Ok(diagnostic.reject(NoPlanReason::ExternalNonreducingPlan, Some((before, after))));
    }
    if !policy.accepts_savings(before, after) {
        return Ok(diagnostic.reject(NoPlanReason::MinimumSavingsNotMet, Some((before, after))));
    }
    Ok(Evaluation::Plan(CompactionPlan {
        strategy: format!("preset:{strategy_id}"),
        rationale: "validated userspace proposal; savings are projected".into(),
        edits,
        context_tokens_before: before,
        context_tokens_after: after,
    }))
}

/// Explain a `None` plan: under trigger vs. over trigger but nothing to cut.
fn report_no_plan(transcript: &gobstopper_core::Transcript, resolved: &config::Resolved) {
    let ctx = transcript.context_tokens();
    let (policy, reasons) = effective_policy(transcript, resolved);
    let trigger = policy.effective_trigger();
    print_no_plan_text(ctx, trigger, &resolved.strategy, &reasons);
}

fn print_no_plan_text(ctx: u64, trigger: u64, strategy: &str, reasons: &[&str]) {
    if ctx < trigger {
        println!("nothing to do: context ~{ctx} under trigger {trigger}");
    } else {
        println!(
            "context ~{ctx} exceeds trigger {trigger} but the '{}' strategy found no applicable edits",
            strategy
        );
    }
    if !reasons.is_empty() {
        println!("  adaptive policy: {}", reasons.join(", "));
    }
}

fn run_preset_command(
    command: &str,
    transcript: &gobstopper_core::Transcript,
) -> Result<serde_json::Value> {
    if command.len() > 4096 {
        bail!("legacy command exceeds byte limit");
    }
    let mut process = Command::new("sh");
    process.args(["-c", command]);
    let payload = serde_json::json!({
        "session_id": transcript.session.session_id,
        "provider": transcript.session.provider,
        "items": transcript.items,
        "usage": transcript.usage,
    });
    let output = plugins::run_bounded(process, serde_json::to_vec(&payload)?, 30_000, 1024 * 1024)?;
    serde_json::from_slice(&output)
        .map_err(|_| anyhow::anyhow!("legacy command returned invalid JSON"))
}

/// Snapshot into the content-addressed vault — the undo path.
fn snapshot_before_edit(d: &Discovered, strategy: &str) -> Result<vault::VaultEntry> {
    let entry = vault::snapshot(
        &d.handle.path,
        d.handle.provider,
        &d.handle.session_id,
        Some(strategy),
        &vault::default_root(),
    )?;
    Ok(entry)
}

/// Emit a numeric compaction telemetry record (v1 schema). Telemetry is
/// best-effort: a logging failure must never fail a compaction.
fn emit_event(
    d: &Discovered,
    plan: &CompactionPlan,
    action: &str,
    outcome: &str,
    trigger_tokens: u64,
    duration_ms: u64,
    error_code: Option<&str>,
) {
    let ev = CompactionEvent::new(
        d.handle.provider,
        &d.handle.session_id,
        &plan.strategy,
        action,
        outcome,
        trigger_tokens,
        plan.context_tokens_before,
        plan.context_tokens_after,
        plan.edits
            .iter()
            .map(|e| match e {
                Edit::Elide { line_indexes, .. } => line_indexes.len() as u64,
                Edit::InjectDigest { digest } => digest.covers_items as u64,
                Edit::CacheEdit { tool_use_ids } => tool_use_ids.len() as u64,
                Edit::ProviderCompact { .. } => 0,
            })
            .sum(),
        duration_ms,
        error_code.map(|s| s.to_string()),
    );
    if let Err(e) = append_event(&default_log_path(), &ev) {
        eprintln!("telemetry write failed (non-fatal): {e}");
    }
}

fn apply_edits(d: &Discovered, plan: &CompactionPlan) -> Result<u64> {
    match d.handle.provider {
        Provider::Codex => gobstopper_adapters::codex::apply(&d.handle.path, &plan.edits)
            .map_err(|e| anyhow::anyhow!(e)),
        Provider::ClaudeCode => gobstopper_adapters::claude::apply(&d.handle.path, &plan.edits)
            .map_err(|e| anyhow::anyhow!(e)),
    }
}

/// Route a `ProviderCompact` edit to the provider's own machinery.
fn provider_compact(
    d: &Discovered,
    codex_bin: &std::path::Path,
    codex_home: &std::path::Path,
) -> Result<()> {
    match d.handle.provider {
        Provider::Codex => codex_compact(codex_bin, &d.handle.session_id, Some(codex_home)),
        Provider::ClaudeCode => bail!(
            "claude sessions compact via /compact in-session or --autocompact at launch; \
             gobstopper cannot inject into a running TUI"
        ),
    }
}

/// Resolve the Codex CLI binary: explicit flag > $GOBSTOPPER_CODEX_BIN > PATH.
fn resolve_codex_bin(flag: Option<&std::path::Path>) -> PathBuf {
    if let Some(p) = flag {
        return p.to_path_buf();
    }
    if let Ok(env) = std::env::var("GOBSTOPPER_CODEX_BIN") {
        if !env.is_empty() {
            return PathBuf::from(env);
        }
    }
    PathBuf::from("codex")
}

/// Ask a private Codex app-server to compact a thread.
///
/// Spawns `codex app-server --listen stdio://` — a self-contained JSON-RPC
/// server, no daemon or standalone install required — resumes the thread into
/// it (`thread/resume`, `excludeTurns` per the pinned pagination contract), then
/// issues `thread/compact/start`. Request acceptance is the success boundary:
/// compaction runs as a provider-side turn whose `contextCompaction` item and
/// `turn/completed` arrive asynchronously; gobstopper reports the observed
/// outcome when it lands inside a bounded wait.
fn codex_compact(
    codex_bin: &std::path::Path,
    thread_id: &str,
    codex_home: Option<&std::path::Path>,
) -> Result<()> {
    use std::io::BufRead;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let mut child = Command::new(codex_bin)
        .args([
            "app-server",
            "--listen",
            "stdio://",
        ])
        .envs(codex_home.map(|home| ("CODEX_HOME", home)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "spawning `{} app-server --listen stdio://` (set --codex-bin or $GOBSTOPPER_CODEX_BIN)",
                codex_bin.display()
            )
        })?;
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut stderr.take(64 * 1024), &mut std::io::sink());
    });
    let (tx, rx) = mpsc::sync_channel::<serde_json::Value>(64);
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout.take(4 * 1024 * 1024)).lines() {
            match line {
                Ok(text) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                        if tx.send(v).is_err() {
                            return;
                        }
                    }
                }
                Err(_) => return,
            }
        }
    });
    let mut send = |v: serde_json::Value| -> Result<()> {
        stdin.write_all(v.to_string().as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    };
    let deadline = |ms: u64| Instant::now() + Duration::from_millis(ms);
    // Read frames until `id` answers or the deadline passes; notifications are
    // skipped here — the outcome window below reads them after acceptance.
    let await_response = |id: i64, until: Instant| -> Result<serde_json::Value> {
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("codex app-server timed out waiting for response id {id}");
            }
            match rx.recv_timeout(left.min(Duration::from_millis(500))) {
                Ok(v) if v.get("id").and_then(|i| i.as_i64()) == Some(id) => {
                    if let Some(err) = v.get("error") {
                        let msg = err
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("unknown app-server error");
                        if msg.contains("thread not found") {
                            bail!("codex app-server: {msg}");
                        }
                        let code = if msg.contains("cannot resume") {
                            "cannot resume provider thread"
                        } else if msg.contains("usage limit") {
                            "usage limit exceeded"
                        } else {
                            "provider_rejected"
                        };
                        bail!("codex app-server: {code}");
                    }
                    return Ok(v);
                }
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("codex app-server closed its stream before answering id {id}")
                }
            }
        }
    };

    let outcome = (|| -> Result<String> {
        let boot = deadline(15_000);
        send(serde_json::json!({
            "method": "initialize", "id": 0,
            "params": {"clientInfo": {"name": "gobstopper", "version": env!("CARGO_PKG_VERSION")}}
        }))?;
        await_response(0, boot)?;
        send(serde_json::json!({"method": "initialized"}))?;
        send(serde_json::json!({
            "method": "thread/resume", "id": 1,
            "params": {"threadId": thread_id, "excludeTurns": true}
        }))?;
        await_response(1, deadline(60_000))?;
        send(serde_json::json!({
            "method": "thread/compact/start", "id": 2,
            "params": {"threadId": thread_id}
        }))?;
        await_response(2, deadline(30_000))?;
        // Compaction accepted. Give the provider-side turn a bounded window to
        // report its outcome so the CLI can say what happened.
        let end = deadline(90_000);
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("provider outcome unknown at deadline; do not replay automatically");
            }
            match rx.recv_timeout(left.min(Duration::from_millis(500))) {
                Ok(v) => {
                    let is_our_turn = v.get("method").and_then(|m| m.as_str())
                        == Some("turn/completed")
                        && v.pointer("/params/threadId").and_then(|s| s.as_str())
                            == Some(thread_id);
                    if is_our_turn {
                        let status = v
                            .pointer("/params/turn/status")
                            .and_then(|s| s.as_str())
                            .unwrap_or("unknown");
                        let detail = v
                            .pointer("/params/turn/error/message")
                            .and_then(|s| s.as_str())
                            .map(|m| {
                                if m.contains("usage limit") {
                                    ": usage limit exceeded".to_string()
                                } else {
                                    ": provider_error".to_string()
                                }
                            })
                            .unwrap_or_default();
                        if status == "completed" {
                            return Ok("compaction turn completed".into());
                        }
                        let status = match status {
                            "failed" => "failed",
                            "interrupted" => "interrupted",
                            _ => "unknown",
                        };
                        bail!("codex compaction turn {status}{detail}");
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("provider outcome unknown after disconnect; do not replay automatically")
                }
            }
        }
    })();
    let _ = child.kill();
    let _ = child.wait();
    drop(stdin);
    println!("codex provider compaction: {}", outcome?);
    Ok(())
}

fn print_plan(d: &Discovered, plan: &CompactionPlan, prefix_tokens: u64, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(plan)?);
        return Ok(());
    }
    println!(
        "{} {} ({})\n  context: {} -> ~{} tokens (saves ~{})\n  prefix: {} tokens cached\n  strategy: {}\n  {}",
        d.handle.provider.as_str(),
        d.handle.session_id,
        if d.handle.is_active() {
            "active"
        } else {
            "idle"
        },
        plan.context_tokens_before,
        plan.context_tokens_after,
        plan.est_savings(),
        prefix_tokens,
        plan.strategy,
        plan.rationale,
    );
    for edit in &plan.edits {
        match edit {
            Edit::Elide { line_indexes, .. } => {
                println!("  elide {} items", line_indexes.len())
            }
            Edit::InjectDigest { digest } => {
                println!("  inject digest covering {} items", digest.covers_items)
            }
            Edit::CacheEdit { tool_use_ids } => {
                println!("  cache_edits: {} tool result ids", tool_use_ids.len())
            }
            Edit::ProviderCompact { control } => println!("  provider control: {control}"),
        }
    }
    Ok(())
}

fn session_rows(cli: &Cli, all: bool) -> Vec<serde_json::Value> {
    let max_age = if all {
        0
    } else {
        detect::default_max_age_secs()
    };
    detect::discover(&roots(cli), max_age)
        .iter()
        .map(|d| {
            serde_json::json!({
                "provider": d.handle.provider,
                "session_id": d.handle.session_id,
                "path": d.handle.path,
                "cwd": d.handle.cwd,
                "active": d.handle.is_active(),
                "context_tokens": d.usage.context_tokens,
                "lifetime_input_tokens": d.usage.lifetime_input_tokens,
                "model_context_window": d.usage.model_context_window,
            })
        })
        .collect()
}

fn cmd_detect(cli: &Cli, all: bool, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&session_rows(cli, all))?);
        return Ok(());
    }
    let max_age = if all {
        0
    } else {
        detect::default_max_age_secs()
    };
    let sessions = detect::discover(&roots(cli), max_age);
    println!(
        "{:<12} {:<38} {:<6} {:>12} {:>14}  PATH",
        "PROVIDER", "SESSION", "STATE", "CTX TOKENS", "LIFETIME IN"
    );
    for d in sessions {
        println!(
            "{:<12} {:<38} {:<6} {:>12} {:>14}  {}",
            d.handle.provider.as_str(),
            &d.handle.session_id[..d.handle.session_id.len().min(38)],
            if d.handle.is_active() { "live" } else { "idle" },
            d.usage.context_tokens,
            d.usage.lifetime_input_tokens,
            d.handle.path.display(),
        );
    }
    Ok(())
}

fn cmd_verify(cli: &Cli, cfg: &config::Config, session: &str, json: bool) -> Result<()> {
    let d = find_session(cli, cfg, session)?;
    let findings = verify::verify_path(d.handle.provider, &d.handle.path)
        .with_context(|| format!("reading {}", d.handle.path.display()))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&findings)?);
    } else if findings.is_empty() {
        println!("{}: clean", d.handle.path.display());
    } else {
        for f in &findings {
            let sev = match f.severity {
                verify::Severity::Error => "error",
                verify::Severity::Warning => "warn ",
            };
            let line = f
                .line_index
                .map(|i| format!("line {}", i + 1))
                .unwrap_or_else(|| "-".to_string());
            println!("{sev} {line:<12} [{}] {}", f.code, f.message);
        }
    }
    if findings
        .iter()
        .any(|f| f.severity == verify::Severity::Error)
    {
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_undo(
    cli: &Cli,
    cfg: &config::Config,
    session: &str,
    sha: Option<&str>,
    yes: bool,
    in_place: bool,
) -> Result<()> {
    if in_place {
        bail!("standalone in-place restore is retired because a provider may hold an open writer; omit --in-place to restore a verified fork");
    }
    let d = find_session(cli, cfg, session)?;
    let root = vault::default_root();
    let entry = match sha {
        Some(prefix) => vault::list(&root)?
            .into_iter()
            .filter(|e| e.path == d.handle.path)
            .find(|e| e.sha256.starts_with(prefix))
            .ok_or_else(|| {
                anyhow::anyhow!("no vault snapshot matching '{prefix}' for this session")
            })?,
        None => vault::latest_pre_compaction(&d.handle.path, &root)?
            .ok_or_else(|| anyhow::anyhow!("no vault snapshot for {}", d.handle.path.display()))?,
    };
    println!(
        "restore snapshot {} — {} bytes, session {}",
        &entry.sha256[..16],
        entry.bytes,
        entry.session_id
    );
    if !yes {
        print!(
            "restore snapshot into {}{}? [y/N] ",
            if in_place {
                "the original path "
            } else {
                "a separate fork of "
            },
            d.handle.path.display()
        );
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y") {
            println!("aborted");
            return Ok(());
        }
    }
    // Snapshot the current (post-compaction) state too, so undo is undoable.
    if d.handle.path.is_file() {
        vault::snapshot(
            &d.handle.path,
            d.handle.provider,
            &d.handle.session_id,
            Some("pre-undo"),
            &root,
        )?;
    }
    let restored = fork::restore_copy(d.handle.provider, &d.handle.path, &entry.sha256, &root)?;
    println!(
        "restored copy {}\n{}",
        restored.path.display(),
        restored.resume_hint
    );
    Ok(())
}

fn cmd_vault(cli: &Cli, cfg: &config::Config, session: Option<&str>, json: bool) -> Result<()> {
    let root = vault::default_root();
    let mut entries = vault::list(&root)?;
    if let Some(q) = session {
        let d = find_session(cli, cfg, q)?;
        entries.retain(|e| e.path == d.handle.path);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }
    println!(
        "{:<18} {:<12} {:>10} {:<10} {:<12} PATH",
        "SHA256", "PROVIDER", "BYTES", "STRATEGY", "SESSION"
    );
    for e in entries {
        println!(
            "{:<18} {:<12} {:>10} {:<10} {:<12} {}",
            &e.sha256[..16],
            e.provider.as_str(),
            e.bytes,
            e.strategy.as_deref().unwrap_or("-"),
            &e.session_id[..e.session_id.len().min(12)],
            e.path.display(),
        );
    }
    Ok(())
}

fn cmd_history(cli: &Cli, cfg: &config::Config, session: &str, json: bool) -> Result<()> {
    let d = find_session(cli, cfg, session)?;
    let root = vault::default_root();
    let mut entries = vault::list(&root)?;
    entries.retain(|e| e.path == d.handle.path || e.session_id.starts_with(session));
    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }
    println!(
        "{:<18} {:>10} {:>8} {:<10} {:<12} PATH",
        "SHA256", "BYTES", "RECORDS", "STRATEGY", "SESSION"
    );
    for e in entries {
        println!(
            "{:<18} {:>10} {:>8} {:<10} {:<12} {}",
            &e.sha256[..16],
            e.bytes,
            e.record_count,
            e.strategy.as_deref().unwrap_or("-"),
            &e.session_id[..e.session_id.len().min(12)],
            e.path.display(),
        );
    }
    Ok(())
}

fn show_summary(cli: &Cli, cfg: &config::Config, target: &str) -> Result<serde_json::Value> {
    let root = vault::default_root();
    let entries = vault::list(&root)?;
    let entry = if target.len() >= 16 && target.chars().all(|c| c.is_ascii_hexdigit()) {
        entries
            .into_iter()
            .find(|e| e.sha256.starts_with(target))
            .with_context(|| format!("no snapshot matches sha prefix {target:?}"))?
    } else {
        let d = find_session(cli, cfg, target)?;
        vault::latest_for(&d.handle.path, &root)?
            .with_context(|| format!("no vault snapshot for {}", d.handle.path.display()))?
    };

    let data = vault::read_object(&entry.sha256, &root)?;
    let mut type_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for line in data.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) {
            let t = v.get("type").and_then(|t| t.as_str()).unwrap_or("unknown");
            *type_counts.entry(t.to_string()).or_default() += 1;
        }
    }

    Ok(serde_json::json!({
        "sha256": entry.sha256,
        "provider": entry.provider.as_str(),
        "session_id": entry.session_id,
        "path": entry.path,
        "bytes": entry.bytes,
        "record_count": entry.record_count,
        "strategy": entry.strategy,
        "record_types": type_counts,
    }))
}

fn cmd_show(cli: &Cli, cfg: &config::Config, target: &str, json: bool) -> Result<()> {
    let summary = show_summary(cli, cfg, target)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!("sha:        {}", summary["sha256"].as_str().unwrap_or("-"));
        println!(
            "provider:   {}",
            summary["provider"].as_str().unwrap_or("-")
        );
        println!(
            "session:    {}",
            summary["session_id"].as_str().unwrap_or("-")
        );
        println!("path:       {}", summary["path"].as_str().unwrap_or("-"));
        println!("bytes:      {}", summary["bytes"].as_u64().unwrap_or(0));
        println!(
            "records:    {}",
            summary["record_count"].as_u64().unwrap_or(0)
        );
        println!(
            "strategy:   {}",
            summary["strategy"].as_str().unwrap_or("-")
        );
        println!("record types:");
        if let Some(types) = summary["record_types"].as_object() {
            for (t, n) in types {
                println!("  {:<12} {}", t, n);
            }
        }
    }
    Ok(())
}

fn recall_rows(
    cli: &Cli,
    cfg: &config::Config,
    session: Option<&str>,
    query: Option<&str>,
    sha: Option<&str>,
    limit: usize,
) -> Result<Vec<vault::RecallDigest>> {
    let root = vault::default_root();
    let session_key = match session {
        Some(s) => {
            let d = find_session(cli, cfg, s)?;
            if d.handle.session_id.starts_with(s) {
                s.to_string()
            } else {
                d.handle.session_id.clone()
            }
        }
        None => "*".to_string(),
    };
    let mut digests = vault::recall(&session_key, query, sha, &root)?;
    digests.truncate(limit);
    Ok(digests)
}

fn recall_row_json(r: &vault::RecallDigest) -> serde_json::Value {
    serde_json::json!({
        "snapshot_sha": r.snapshot_sha,
        "ts": r.ts,
        "provider": r.provider.as_str(),
        "session_id": r.session_id,
        "record_index": r.record_index,
        "score": r.score,
        "goal": r.digest.goal,
        "summary": r.digest.summary,
        "concepts": r.digest.concepts,
        "decisions": r.digest.decisions,
        "files_touched": r.digest.files_touched,
        "errors": r.digest.errors,
        "open_tasks": r.digest.open_tasks,
        "current_work": r.digest.current_work,
        "context": r.digest.context,
        "covers_items": r.digest.covers_items,
    })
}

fn cmd_recall(
    cli: &Cli,
    cfg: &config::Config,
    session: Option<&str>,
    query: Option<&str>,
    sha: Option<&str>,
    limit: usize,
    json: bool,
) -> Result<()> {
    let digests = recall_rows(cli, cfg, session, query, sha, limit)?;
    if json {
        let rows: Vec<_> = digests.iter().map(recall_row_json).collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if digests.is_empty() {
        println!("no state-card digests found");
        return Ok(());
    }
    println!("found {} state-card digest(s)", digests.len());
    for r in digests {
        println!("\n---");
        println!("snapshot:   {}", &r.snapshot_sha[..16]);
        println!("session:    {}", r.session_id);
        println!("record:     {}", r.record_index);
        println!("relevance:  {}", r.score);
        println!("covers:     {} earlier records", r.digest.covers_items);
        if let Some(g) = &r.digest.goal {
            println!("goal:\n  {}", g);
        }
        for (label, text) in [
            ("summary", &r.digest.summary),
            ("current work", &r.digest.current_work),
            ("context", &r.digest.context),
        ] {
            if let Some(text) = text {
                println!("{label}:\n  {text}");
            }
        }
        for (label, values) in [
            ("concepts", &r.digest.concepts),
            ("errors", &r.digest.errors),
        ] {
            for value in values {
                println!("{label}: {value}");
            }
        }
        if !r.digest.decisions.is_empty() {
            println!("decisions:");
            for d in &r.digest.decisions {
                println!("  - {}", d);
            }
        }
        if !r.digest.files_touched.is_empty() {
            println!("files:");
            for f in &r.digest.files_touched {
                println!("  - {}", f);
            }
        }
        if !r.digest.open_tasks.is_empty() {
            println!("todos:");
            for t in &r.digest.open_tasks {
                println!("  - {}", t);
            }
        }
    }
    Ok(())
}

fn diff_summary(a: &str, b: &str) -> Result<serde_json::Value> {
    let root = vault::default_root();
    let entries = vault::list(&root)?;
    let a_full = entries
        .iter()
        .find(|e| e.sha256.starts_with(a))
        .map(|e| e.sha256.clone())
        .with_context(|| format!("no snapshot matches sha prefix {a:?}"))?;
    let b_full = entries
        .iter()
        .find(|e| e.sha256.starts_with(b))
        .map(|e| e.sha256.clone())
        .with_context(|| format!("no snapshot matches sha prefix {b:?}"))?;
    let summary = vault::diff(&a_full, &b_full, &root)?;
    Ok(serde_json::json!({
        "sha_a": summary.sha1,
        "sha_b": summary.sha2,
        "record_count_a": summary.record_count_a,
        "record_count_b": summary.record_count_b,
        "added_count": summary.added.len(),
        "removed_count": summary.removed.len(),
        "type_summary_a": summary.type_summary_a,
        "type_summary_b": summary.type_summary_b,
    }))
}

fn cmd_diff(a: &str, b: &str, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&diff_summary(a, b)?)?);
        return Ok(());
    }
    let root = vault::default_root();
    let entries = vault::list(&root)?;
    let a_full = entries
        .iter()
        .find(|e| e.sha256.starts_with(a))
        .map(|e| e.sha256.clone())
        .with_context(|| format!("no snapshot matches sha prefix {a:?}"))?;
    let b_full = entries
        .iter()
        .find(|e| e.sha256.starts_with(b))
        .map(|e| e.sha256.clone())
        .with_context(|| format!("no snapshot matches sha prefix {b:?}"))?;
    let summary = vault::diff(&a_full, &b_full, &root)?;
    println!(
        "snapshot a: {} ({} records)",
        summary.sha1, summary.record_count_a
    );
    println!(
        "snapshot b: {} ({} records)",
        summary.sha2, summary.record_count_b
    );
    println!("added records:  {}", summary.added.len());
    println!("removed records: {}", summary.removed.len());
    println!("\ntype counts in a:");
    for (t, n) in &summary.type_summary_a {
        println!("  {:<12} {}", t, n);
    }
    println!("\ntype counts in b:");
    for (t, n) in &summary.type_summary_b {
        println!("  {:<12} {}", t, n);
    }
    Ok(())
}

fn cmd_snapshot(cli: &Cli, cfg: &config::Config, session: &str, label: Option<&str>) -> Result<()> {
    let d = find_session(cli, cfg, session)?;
    let entry = vault::snapshot(
        &d.handle.path,
        d.handle.provider,
        &d.handle.session_id,
        label,
        &vault::default_root(),
    )?;
    println!(
        "snapshotted {} -> {} ({} bytes, {} records)",
        d.handle.path.display(),
        &entry.sha256[..16],
        entry.bytes,
        entry.record_count,
    );
    Ok(())
}

fn cmd_install_hooks(uninstall: bool, roots: &Roots) -> Result<()> {
    let claude_settings = roots.claude_home.join("settings.json");
    let claude_targets = [
        hooks::HookTarget::ClaudePreCompact,
        hooks::HookTarget::ClaudeSessionStart,
    ];
    let report = if uninstall {
        hooks::uninstall(&claude_settings)?
    } else {
        hooks::install(&claude_settings, &claude_targets)?
    };
    println!(
        "{}: +{} -{}",
        report.path.display(),
        report.added.len(),
        report.skipped.len()
    );
    for a in &report.added {
        println!("  {} {a}", if uninstall { "removed" } else { "added" });
    }
    if hooks::codex_hooks_supported() {
        // Codex also accepts inline `[hooks]` tables in `config.toml`; the
        // standalone JSON file is the additive, non-destructive install point.
        let codex_hooks = roots.codex_home.join("hooks.json");
        let codex_targets = [
            hooks::HookTarget::CodexPreCompact,
            hooks::HookTarget::CodexSessionStart,
        ];
        let report = if uninstall {
            hooks::uninstall(&codex_hooks)?
        } else {
            hooks::install(&codex_hooks, &codex_targets)?
        };
        println!(
            "{}: +{} -{}",
            report.path.display(),
            report.added.len(),
            report.skipped.len()
        );
        for a in &report.added {
            println!("  {} {a}", if uninstall { "removed" } else { "added" });
        }
        if !uninstall {
            println!("note: codex requires one-time hook trust approval (/hooks in the TUI)");
        }
    } else {
        println!("codex hooks: not supported by the installed codex version — skipped");
    }
    Ok(())
}

fn cmd_hook(event: &str) -> Result<()> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    if let Some(out) = hooks::handle(event, &buf)? {
        println!("{out}");
    }
    Ok(())
}

fn cmd_report(cli: &Cli, strict: bool, active_only: bool) -> Result<()> {
    let sessions = detect::discover(
        &roots(cli),
        if active_only {
            gobstopper_core::SessionHandle::HOT_SECS
        } else {
            0
        },
    );
    let events = match gobstopper_core::events::read_events(&default_log_path()) {
        Ok(events) => events,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error).context("compaction telemetry unavailable"),
    };
    let mut report = report::build_report(&sessions, &events);
    if strict {
        if let Some(list) = report["sessions"].as_array_mut() {
            for s in list {
                s.as_object_mut().map(|o| o.remove("gobstopper"));
            }
        }
    }
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn cmd_events(session: Option<&str>, tail: usize, json: bool) -> Result<()> {
    let path = default_log_path();
    let mut events = gobstopper_core::events::read_events(&path).unwrap_or_default();
    if let Some(prefix) = session {
        events.retain(|e| e.session_id.starts_with(prefix));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&events)?);
        return Ok(());
    }
    let applied: Vec<&CompactionEvent> = events.iter().filter(|e| e.outcome == "applied").collect();
    let reclaimed: u64 = applied.iter().map(|e| e.est_reclaimed_tokens).sum();
    println!(
        "{} events ({} applied) — ~{} tokens reclaimed lifetime",
        events.len(),
        applied.len(),
        reclaimed
    );
    for e in events.iter().rev().take(tail).rev() {
        println!(
            "  {} {:<12} {:<10} {:<18} {:<8} {} -> {}",
            e.ts,
            e.provider.as_str(),
            e.strategy,
            e.action,
            e.outcome,
            e.context_tokens_before,
            e.context_tokens_after,
        );
    }
    Ok(())
}

fn cmd_fork(cli: &Cli, cfg: &config::Config, session: &str) -> Result<()> {
    let d = find_session(cli, cfg, session)?;
    let r = fork::fork(d.handle.provider, &d.handle.path, None)?;
    println!("forked {} -> {}", d.handle.session_id, r.session_id);
    println!("  {}", r.path.display());
    println!("  resume: {}", r.resume_hint);
    Ok(())
}

fn cmd_eval(
    cli: &Cli,
    cfg: &config::Config,
    session: &str,
    strategy_flag: Option<&str>,
    trigger: Option<u64>,
    floor: Option<u64>,
    json: bool,
) -> Result<()> {
    let d = find_session(cli, cfg, session)?;
    let resolved = cfg.resolve(d.handle.provider, &d.handle.session_id, None, None)?;
    let mut policy = resolved.policy;
    if let Some(t) = trigger {
        policy.trigger_tokens = t;
    }
    if let Some(f) = floor {
        policy.floor_tokens = f;
    }
    let scorer = maybe_scorer();
    let judge = jev::eval_judge();
    let hooks = eval::EvalHooks {
        scorer: scorer.as_deref(),
        probe_judge: judge.as_deref(),
    };
    let rows = eval::eval_transcript_with_hooks(
        d.handle.provider,
        &d.handle.path,
        &policy,
        strategy_flag,
        &hooks,
    )?;
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    println!(
        "{} {} — context ~{} tokens, effective trigger {}",
        d.handle.provider.as_str(),
        d.handle.session_id,
        d.usage.context_tokens,
        policy.effective_trigger(),
    );
    for row in &rows {
        match (&row.plan, &row.error) {
            (Some(plan), _) => {
                let probe = match &row.probe_score {
                    Some(s) if s.probes_total > 0 => format!(
                        "  recall {:.0}% ({}/{}){}",
                        s.recall * 100.0,
                        s.probes_recalled,
                        s.probes_total,
                        if s.tail_intact { "" } else { ", tail lost" },
                    ),
                    _ => String::new(),
                };
                let semantic = match &row.semantic_score {
                    Some(s) if s.probes_total > 0 => format!(
                        "  semantic {:.0}% ({}/{})",
                        s.recall * 100.0,
                        s.probes_recalled,
                        s.probes_total,
                    ),
                    _ => String::new(),
                };
                println!(
                    "  {:<11} {} -> ~{} (saves ~{}){}{}{}",
                    row.strategy,
                    plan.context_tokens_before,
                    plan.context_tokens_after,
                    row.est_reclaimed,
                    probe,
                    semantic,
                    if row.verify_errors > 0 {
                        format!("  ⚠ {} verify errors", row.verify_errors)
                    } else {
                        String::new()
                    },
                );
            }
            (None, Some(e)) => println!("  {:<11} error: {e}", row.strategy),
            (None, None) => println!("  {:<11} no plan", row.strategy),
        }
    }

    // Pareto summary: highest scoring plan (savings × prefix³) among the
    // concrete strategies that verified clean. This is the same metric `auto`
    // uses, exposed for inspection.
    let best = rows
        .iter()
        .filter(|r| {
            r.plan.is_some()
                && r.error.is_none()
                && r.verify_errors == 0
                && !r
                    .plan
                    .as_ref()
                    .unwrap()
                    .edits
                    .iter()
                    .any(|e| matches!(e, Edit::ProviderCompact { .. } | Edit::CacheEdit { .. }))
        })
        .map(|r| {
            let before = r
                .plan
                .as_ref()
                .map(|p| p.context_tokens_before)
                .unwrap_or(1);
            (
                r,
                strategy::cache_preservation_score(r.est_reclaimed, r.prefix_tokens, before),
            )
        })
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if let Some((row, score)) = best {
        println!(
            "pareto best: {:<11} score={:.0}  prefix={}  reclaimed={}",
            row.strategy, score, row.prefix_tokens, row.est_reclaimed
        );
    }
    Ok(())
}

fn cmd_cache_edits(
    cli: &Cli,
    cfg: &config::Config,
    session: &str,
    trigger: Option<u64>,
    floor: Option<u64>,
    plan: bool,
) -> Result<()> {
    let d = find_session(cli, cfg, session)?;
    if d.handle.provider != Provider::ClaudeCode {
        bail!("cache_edits is only available for Claude Code sessions");
    }
    let mut resolved = cfg.resolve(
        d.handle.provider,
        &d.handle.session_id,
        None,
        Some("cache_edits"),
    )?;
    if let Some(t) = trigger {
        resolved.policy.trigger_tokens = t;
    }
    if let Some(f) = floor {
        resolved.policy.floor_tokens = f;
    }
    let transcript = detect::load(&d)?;
    match evaluate(&transcript, &resolved)? {
        Some(p) => {
            let cache_edit = p.edits.iter().find_map(|e| match e {
                Edit::CacheEdit { tool_use_ids } => Some(tool_use_ids),
                _ => None,
            });
            let tool_use_ids = cache_edit.cloned().unwrap_or_default();
            if plan {
                println!("{}", serde_json::to_string_pretty(&p)?);
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "session_id": d.handle.session_id,
                        "strategy": p.strategy,
                        "context_tokens_before": p.context_tokens_before,
                        "context_tokens_after": p.context_tokens_after,
                        "tool_use_ids": tool_use_ids,
                    }))?
                );
            }
        }
        None => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "session_id": d.handle.session_id,
                    "tool_use_ids": Vec::<String>::new(),
                    "reason": "no cache_edits plan at this policy"
                }))?
            );
        }
    }
    Ok(())
}

fn cmd_bench(
    cli: &Cli,
    all: bool,
    trigger: Option<u64>,
    floor: Option<u64>,
    output: Option<&std::path::Path>,
) -> Result<()> {
    let max_age = if all {
        0
    } else {
        detect::default_max_age_secs()
    };
    let sessions = detect::discover(&roots(cli), max_age);
    let cfg = config::load()?;
    // The scorer env applies to `scored` rows here exactly as it does
    // in `plan`. The probe judge is skipped: bench emits no semantic
    // columns, and a remote call per session × strategy would spend
    // requests on data nobody reads.
    let scorer = maybe_scorer();
    let hooks = eval::EvalHooks {
        scorer: scorer.as_deref(),
        probe_judge: None,
    };
    let mut csv = String::new();
    csv.push_str(
        "provider,session,strategy,context_before,context_after,est_reclaimed,prefix_tokens,prefix_ratio,score,verify_errors,verify_warnings,probes_total,probes_recalled,recall,tail_intact,duration_ms\n",
    );
    for d in sessions {
        let mut resolved = match cfg.resolve(d.handle.provider, &d.handle.session_id, None, None) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if let Some(t) = trigger {
            resolved.policy.trigger_tokens = t;
        }
        if let Some(f) = floor {
            resolved.policy.floor_tokens = f;
        }
        let rows = match eval::eval_transcript_with_hooks(
            d.handle.provider,
            &d.handle.path,
            &resolved.policy,
            None,
            &hooks,
        ) {
            Ok(rows) => rows,
            Err(_) => continue,
        };
        for row in rows {
            let plan = row.plan.as_ref();
            let before = plan
                .map(|p| p.context_tokens_before)
                .unwrap_or(d.usage.context_tokens);
            let after = plan.map(|p| p.context_tokens_after).unwrap_or(before);
            let (probes_total, probes_recalled, recall, tail_intact) =
                row.probe_score.as_ref().map_or((0, 0, 0.0, false), |s| {
                    (s.probes_total, s.probes_recalled, s.recall, s.tail_intact)
                });
            let prefix_ratio = if before > 0 {
                (row.prefix_tokens as f64) / (before as f64)
            } else {
                0.0
            };
            let provider_compact = row
                .plan
                .as_ref()
                .map(|p| {
                    p.edits
                        .iter()
                        .any(|e| matches!(e, Edit::ProviderCompact { .. } | Edit::CacheEdit { .. }))
                })
                .unwrap_or(false);
            let score = if provider_compact {
                0.0
            } else {
                strategy::cache_preservation_score(row.est_reclaimed, row.prefix_tokens, before)
            };
            csv.push_str(&format!(
                "{},{},{},{},{},{},{},{:.4},{:.0},{},{},{},{},{:.2},{},{}\n",
                d.handle.provider.as_str(),
                d.handle.session_id,
                row.strategy,
                before,
                after,
                row.est_reclaimed,
                row.prefix_tokens,
                prefix_ratio,
                score,
                row.verify_errors,
                row.verify_warnings,
                probes_total,
                probes_recalled,
                recall,
                tail_intact,
                row.duration_ms
            ));
        }
    }
    if let Some(path) = output {
        std::fs::write(path, csv.as_bytes())?;
        println!(
            "wrote {} rows to {}",
            csv.lines().count().saturating_sub(1),
            path.display()
        );
    } else {
        print!("{}", csv);
    }
    Ok(())
}

/// `gobstopper tune` — show what the adaptive tuner derives for one
/// session. Always computes the adjustment (a preview when the policy
/// does not opt in) and suggests the TOML to pin the derived values.
fn cmd_tune(cli: &Cli, cfg: &config::Config, session: &str, json: bool) -> Result<()> {
    let d = find_session(cli, cfg, session)?;
    let resolved = cfg.resolve(d.handle.provider, &d.handle.session_id, None, None)?;
    let transcript = detect::load(&d)?;
    let sample = adaptive_sample(&transcript);
    let outcome = gobstopper_core::adapt(&resolved.policy, &sample);
    let adjusted = &outcome.policy;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "provider": d.handle.provider,
                "session_id": d.handle.session_id,
                "adaptive_enabled": resolved.policy.adaptive,
                "model_context_window": sample.model_context_window,
                "context_tokens": sample.context_tokens,
                "elidable_tokens": sample.elidable_tokens,
                "recent_applied": sample.recent.len(),
                "configured": {
                    "trigger_tokens": resolved.policy.trigger_tokens,
                    "floor_tokens": resolved.policy.floor_tokens,
                },
                "adjusted": {
                    "trigger_tokens": adjusted.trigger_tokens,
                    "floor_tokens": adjusted.floor_tokens,
                },
                "reasons": outcome.reasons,
            }))?
        );
        return Ok(());
    }
    println!(
        "{} {} — adaptive {}",
        d.handle.provider.as_str(),
        d.handle.session_id,
        if resolved.policy.adaptive {
            "on"
        } else {
            "off (preview)"
        },
    );
    if let Some(window) = sample.model_context_window {
        println!("  model context window: {window}");
    }
    println!("  context tokens:       {}", sample.context_tokens);
    if let Some(elidable) = sample.elidable_tokens {
        let share = if sample.context_tokens > 0 {
            (elidable as f64) / (sample.context_tokens as f64) * 100.0
        } else {
            0.0
        };
        println!("  elidable tokens:      {elidable} ({share:.0}% of context)");
    }
    println!("  applied compactions:  {}", sample.recent.len());
    println!(
        "  configured:           trigger {} / floor {}",
        resolved.policy.trigger_tokens, resolved.policy.floor_tokens
    );
    println!(
        "  adjusted:             trigger {} / floor {}",
        adjusted.trigger_tokens, adjusted.floor_tokens
    );
    if outcome.reasons.is_empty() {
        println!("  no adjustment — configured policy fits this session");
    } else {
        println!("  reasons:              {}", outcome.reasons.join(", "));
        if !resolved.policy.adaptive {
            println!("\n  enable with `adaptive = true` under [policy], or pin for this session:");
        } else {
            println!("\n  pin for this session:");
        }
        println!(
            "  [sessions.\"{}\"]\n  trigger_tokens = {}\n  floor_tokens = {}",
            d.handle.session_id, adjusted.trigger_tokens, adjusted.floor_tokens
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_apply(
    cli: &Cli,
    cfg: &config::Config,
    session: &str,
    strategy: Option<&str>,
    preset: Option<&str>,
    trigger: Option<u64>,
    floor: Option<u64>,
    yes: bool,
    in_place: bool,
    no_backup: bool,
    experimental_compacted: bool,
) -> Result<()> {
    if no_backup {
        bail!("snapshots are mandatory; --no-backup is no longer supported");
    }
    if in_place {
        bail!("standalone in-place compaction is retired because a provider may hold an open writer; omit --in-place to publish a verified fork");
    }
    let d = find_session(cli, cfg, session)?;
    if d.handle.provider == Provider::Codex {
        if let Some(parent) = codex::parent_thread(&d.handle.path) {
            if parent != d.handle.session_id {
                println!("warning: this Codex session is a sub-agent/fork of {parent}; resume the parent with: codex resume {parent}");
            }
        }
    }
    let mut resolved = cfg.resolve(d.handle.provider, &d.handle.session_id, preset, strategy)?;
    if let Some(t) = trigger {
        resolved.policy.trigger_tokens = t;
    }
    if let Some(f) = floor {
        resolved.policy.floor_tokens = f;
    }
    let (transcript, source_sha256) = copy::load_bound(d.handle.clone())?;
    let Some(plan) = evaluate(&transcript, &resolved)? else {
        report_no_plan(&transcript, &resolved);
        return Ok(());
    };
    let prefix = eval::prefix_tokens(&transcript, &plan);
    print_plan(&d, &plan, prefix, false)?;
    let has_provider_control = plan
        .edits
        .iter()
        .any(|e| matches!(e, Edit::ProviderCompact { .. } | Edit::CacheEdit { .. }));
    if !has_provider_control && plan.context_tokens_after >= plan.context_tokens_before {
        bail!(
            "plan has no net context benefit ({} -> {} tokens); refusing to rewrite",
            plan.context_tokens_before,
            plan.context_tokens_after
        );
    }
    if plan
        .edits
        .iter()
        .any(|e| matches!(e, Edit::CacheEdit { .. }))
    {
        bail!(
            "cache_edits is a Claude API control; apply it through Claude Code, not by rewriting the transcript file"
        );
    }
    if d.handle.is_active() {
        println!("session appears live; only a separate fork will be prepared; the source remains unchanged");
    }
    if !yes {
        print!("apply? [y/N] ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y") {
            println!("aborted");
            return Ok(());
        }
    }
    let file_edits: Vec<Edit> = plan
        .edits
        .iter()
        .filter(|e| !matches!(e, Edit::ProviderCompact { .. } | Edit::CacheEdit { .. }))
        .cloned()
        .collect();
    let started = std::time::Instant::now();
    // Telemetry records the trigger the decision actually used — the
    // adaptive-adjusted one when the policy opts in.
    let trigger = effective_policy(&transcript, &resolved).0.trigger_tokens;
    // Compacted path: for Codex, an `InjectDigest` edit becomes a real
    // `compacted` record (window chain + replacement_history) appended
    // to the rollout — the provider's own resume mechanism performs the
    // context swap. Elide edits in the same plan still apply normally first.
    let is_compacted = experimental_compacted
        && resolved.strategy == "compacted"
        && d.handle.provider == Provider::Codex;
    let digest_for_compacted = if is_compacted {
        plan.edits.iter().find_map(|e| match e {
            Edit::InjectDigest { digest } => Some(digest.clone()),
            _ => None,
        })
    } else {
        None
    };
    if let Some(digest) = digest_for_compacted {
        let file_result = copy::compact_via_compacted(
            &d.handle,
            &source_sha256,
            &plan,
            &digest,
            copy::COMPACTED_KEEP_TAIL,
            &vault::default_root(),
        )
        .inspect(|receipt| {
            println!("prepared {}", receipt.path.display());
            if let Some(sha) = &receipt.snapshot_manifest_sha256 {
                println!("recovery snapshot: {sha}");
            }
            println!(
                "resume the new session: codex resume {}",
                receipt.session_id
            );
        });
        match file_result {
            Ok(receipt) => {
                emit_event(
                    &d,
                    &plan,
                    "transcript_compact",
                    "planned",
                    trigger,
                    started.elapsed().as_millis() as u64,
                    None,
                );
                println!(
                    "emitted compacted record (window chain advanced; resume performs the swap)"
                );
                println!(
                    "reclaimed ~{} file bytes in the prepared fork",
                    receipt.reclaimed_bytes
                );
            }
            Err(e) => {
                emit_event(
                    &d,
                    &plan,
                    "transcript_compact",
                    "failed",
                    trigger,
                    started.elapsed().as_millis() as u64,
                    Some("apply_failed"),
                );
                return Err(e);
            }
        }
    } else if !file_edits.is_empty() {
        let file_result: anyhow::Result<u64> =
            copy::compact(&d.handle, &source_sha256, &plan, &vault::default_root()).map(
                |receipt| {
                    println!("prepared {}", receipt.path.display());
                    if let Some(sha) = &receipt.snapshot_manifest_sha256 {
                        println!("recovery snapshot: {sha}");
                    }
                    println!(
                        "resume the new session: {} {}",
                        if d.handle.provider == Provider::Codex {
                            "codex resume"
                        } else {
                            "claude --resume"
                        },
                        receipt.session_id
                    );
                    receipt.reclaimed_bytes
                },
            );
        match file_result {
            Ok(reclaimed) => {
                emit_event(
                    &d,
                    &plan,
                    "transcript_compact",
                    "planned",
                    trigger,
                    started.elapsed().as_millis() as u64,
                    None,
                );
                println!("reclaimed ~{} bytes in the prepared fork", reclaimed);
            }
            Err(e) => {
                emit_event(
                    &d,
                    &plan,
                    "transcript_compact",
                    "failed",
                    trigger,
                    started.elapsed().as_millis() as u64,
                    Some("apply_failed"),
                );
                return Err(e);
            }
        }
    }
    if has_provider_control {
        if d.handle.provider == Provider::ClaudeCode {
            bail!("live Claude compaction must be dispatched by its session owner; use /compact in that session");
        }
        let snapshot = snapshot_before_edit(&d, &plan.strategy)?;
        if snapshot.sha256 != source_sha256 {
            bail!("source changed before native fork preparation");
        }
        let forked = fork::restore_copy(
            d.handle.provider,
            &d.handle.path,
            &snapshot.sha256,
            &vault::default_root(),
        )?;
        println!(
            "native compaction targets a separate fork: {}",
            forked.resume_hint
        );
        let d = Discovered {
            handle: gobstopper_core::SessionHandle {
                path: forked.path,
                session_id: forked.session_id,
                ..d.handle.clone()
            },
            usage: d.usage,
        };
        emit_event(&d, &plan, "provider_compact", "planned", trigger, 0, None);
        match provider_compact(
            &d,
            &resolve_codex_bin(cli.codex_bin.as_deref()),
            &roots(cli).codex_home,
        ) {
            Ok(()) => {
                emit_event(
                    &d,
                    &plan,
                    "provider_compact",
                    "applied",
                    trigger,
                    started.elapsed().as_millis() as u64,
                    None,
                );
                println!("provider compaction requested");
            }
            Err(e) => {
                emit_event(
                    &d,
                    &plan,
                    "provider_compact",
                    "failed",
                    trigger,
                    started.elapsed().as_millis() as u64,
                    Some("provider_rejected"),
                );
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Content hash of a file, for the staged-swap unchanged check.
fn sha256_file(path: &std::path::Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).ok()?;
    Some(format!("{:x}", Sha256::digest(&bytes)))
}

/// A fully-applied transcript copy waiting to be swapped in at trigger.
struct Staged {
    path: PathBuf,
    /// Content hash of the source file when staged — if the provider
    /// wrote since, the staged copy is missing records and must be
    /// discarded. A hash, not a length: a same-length rewrite would pass
    /// a size check.
    source_sha256: String,
    plan: CompactionPlan,
}

/// Build a compacted copy of the transcript ahead of the trigger.
/// Returns None when there is nothing to stage (no plan, or the plan
/// only delegates to the provider — that path is instant anyway).
fn stage_compaction(d: &Discovered, resolved: &config::Resolved) -> Option<Staged> {
    let transcript = detect::load(d).ok()?;
    let plan = evaluate(&transcript, resolved).ok()??;
    let file_edits: Vec<Edit> = plan
        .edits
        .iter()
        .filter(|e| !matches!(e, Edit::ProviderCompact { .. } | Edit::CacheEdit { .. }))
        .cloned()
        .collect();
    if file_edits.is_empty() {
        return None;
    }
    let staged_path = d.handle.path.with_extension("gobstopper-staged");
    std::fs::copy(&d.handle.path, &staged_path).ok()?;
    let staged_d = Discovered {
        handle: gobstopper_core::SessionHandle {
            path: staged_path.clone(),
            ..d.handle.clone()
        },
        usage: d.usage,
    };
    let staged_plan = CompactionPlan {
        edits: file_edits,
        ..plan.clone()
    };
    let clean = apply_edits(&staged_d, &staged_plan).is_ok()
        && std::fs::read(&staged_path)
            .ok()
            .map(|b| {
                !verify::verify(d.handle.provider, &b)
                    .iter()
                    .any(|f| f.severity == verify::Severity::Error)
            })
            .unwrap_or(false);
    if !clean {
        let _ = std::fs::remove_file(&staged_path);
        return None;
    }
    let source_sha256 = sha256_file(&d.handle.path)?;
    Some(Staged {
        path: staged_path,
        source_sha256,
        plan,
    })
}

fn cmd_watch(
    cli: &Cli,
    _cfg: &config::Config,
    interval: u64,
    dry_run: bool,
    double_buffer: bool,
    active_only: bool,
    once: bool,
) -> Result<()> {
    if interval == 0 {
        bail!("watch interval must be positive");
    }
    if double_buffer {
        bail!("in-place double-buffer swapping is retired; use copy-only watch without --double-buffer");
    }
    let mut last_fire: std::collections::HashMap<String, std::time::Instant> =
        std::collections::HashMap::new();
    let mut staged: std::collections::HashMap<String, Staged> = std::collections::HashMap::new();
    let mut discovery_cache = detect::DiscoveryCache::default();
    loop {
        let cfg = config::load()?;
        for d in detect::discover_cached(
            &roots(cli),
            if active_only {
                gobstopper_core::SessionHandle::HOT_SECS
            } else {
                detect::default_max_age_secs()
            },
            &mut discovery_cache,
        ) {
            if active_only && !d.handle.is_active() {
                continue;
            }
            let session_key = format!("{}:{}", d.handle.provider.as_str(), d.handle.path.display());
            let Ok(resolved) = cfg.resolve(d.handle.provider, &d.handle.session_id, None, None)
            else {
                continue;
            };
            let trigger = if resolved.policy.adaptive {
                gobstopper_core::adapt(
                    &resolved.policy,
                    &adaptive_sample_usage(d.handle.provider, &d.handle.session_id, &d.usage),
                )
                .policy
                .effective_trigger()
            } else {
                resolved.policy.effective_trigger()
            };
            let ctx = if d.usage.context_tokens > 0 {
                d.usage.context_tokens
            } else {
                detect::load(&d)
                    .map(|t| t.estimated_context_tokens())
                    .unwrap_or(0)
            };
            if ctx < trigger {
                // Below trigger: optionally precompute the compacted file
                // so the trigger crossing is a rename, not a rewrite.
                if double_buffer
                    && !dry_run
                    && ctx >= trigger * 6 / 10
                    && !staged.contains_key(&d.handle.session_id)
                {
                    if let Some(s) = stage_compaction(&d, &resolved) {
                        staged.insert(d.handle.session_id.clone(), s);
                    }
                }
                continue;
            }
            if let Some(t) = last_fire.get(&session_key) {
                if t.elapsed().as_secs() < resolved.policy.min_interval_secs {
                    continue;
                }
            }
            // Staged fast path: source unchanged since staging -> swap.
            if let Some(s) = staged.remove(&d.handle.session_id) {
                let unchanged = sha256_file(&d.handle.path)
                    .map(|h| h == s.source_sha256)
                    .unwrap_or(false);
                if !dry_run && unchanged {
                    let started = std::time::Instant::now();
                    match copy::compact(
                        &d.handle,
                        &s.source_sha256,
                        &s.plan,
                        &vault::default_root(),
                    )
                    .map(|_| ())
                    {
                        Ok(()) => {
                            last_fire.insert(session_key.clone(), std::time::Instant::now());
                            emit_event(
                                &d,
                                &s.plan,
                                "transcript_compact",
                                "planned",
                                trigger,
                                started.elapsed().as_millis() as u64,
                                None,
                            );
                            eprintln!("prepared compacted fork for {}", d.handle.session_id);
                            continue;
                        }
                        Err(e) => eprintln!("staged swap {} failed: {e}", d.handle.session_id),
                    }
                }
                let _ = std::fs::remove_file(&s.path); // stale or dry-run
            }
            let (transcript, source_sha256) = match copy::load_bound(d.handle.clone()) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("load {} failed: {e}", d.handle.session_id);
                    continue;
                }
            };
            match evaluate(&transcript, &resolved) {
                Ok(Some(plan)) => {
                    if dry_run {
                        eprintln!(
                            "[dry-run] {} {}: {}",
                            d.handle.provider.as_str(),
                            &d.handle.session_id[..d.handle.session_id.len().min(12)],
                            plan.rationale
                        );
                        continue;
                    }
                    let started = std::time::Instant::now();
                    let trigger = effective_policy(&transcript, &resolved)
                        .0
                        .effective_trigger();
                    let is_provider = plan.edits.iter().any(|e| {
                        matches!(e, Edit::ProviderCompact { .. } | Edit::CacheEdit { .. })
                    });
                    let action = if is_provider {
                        "provider_compact"
                    } else {
                        "transcript_compact"
                    };
                    if last_fire.len() >= 4096 {
                        last_fire.clear();
                    }
                    last_fire.insert(session_key.clone(), std::time::Instant::now());
                    if is_provider {
                        // Delegation is an expected boundary, not an apply failure.
                        // Nothing ran: do not credit the strategy's projected floor
                        // as reclaimed context in telemetry.
                        let unchanged = CompactionPlan {
                            context_tokens_after: plan.context_tokens_before,
                            ..plan.clone()
                        };
                        emit_event(
                            &d,
                            &unchanged,
                            action,
                            "skipped",
                            trigger,
                            started.elapsed().as_millis() as u64,
                            None,
                        );
                        eprintln!(
                            "deferred native compaction: session owner required; source unchanged"
                        );
                        continue;
                    }
                    let r = copy::compact(&d.handle, &source_sha256, &plan, &vault::default_root())
                        .map(|receipt| {
                            eprintln!("prepared copy {}", receipt.path.display());
                            if let Some(sha) = &receipt.snapshot_manifest_sha256 {
                                eprintln!("recovery snapshot: {sha}");
                            }
                        });
                    match r {
                        Ok(()) => {
                            last_fire.insert(session_key.clone(), std::time::Instant::now());
                            emit_event(
                                &d,
                                &plan,
                                action,
                                "planned",
                                trigger,
                                started.elapsed().as_millis() as u64,
                                None,
                            );
                            eprintln!("prepared compacted fork for {}", d.handle.session_id);
                        }
                        Err(e) => {
                            emit_event(
                                &d,
                                &plan,
                                action,
                                "failed",
                                trigger,
                                started.elapsed().as_millis() as u64,
                                Some("apply_failed"),
                            );
                            eprintln!("compact {} failed: {e}", d.handle.session_id);
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => eprintln!("plan {} failed: {e}", d.handle.session_id),
            }
        }
        if once {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(interval));
    }
}

fn cmd_auth(provider: &str, status: bool, delete: bool) -> Result<()> {
    match provider {
        "jev" | "typesafe" => auth_jev(status, delete),
        other => bail!("unknown provider '{other}' (supported: jev)"),
    }
}

/// `gobstopper auth jev` onboarding: piped stdin wins; else the system
/// clipboard when running interactively. The key is verified against the
/// API before it reaches the OS keychain — a definitively rejected key is
/// never stored.
fn auth_jev(status: bool, delete: bool) -> Result<()> {
    let endpoint = std::env::var("GOBSTOPPER_JEV_ENDPOINT")
        .unwrap_or_else(|_| "https://api.typesafe.ai/v1/systemone".into());
    if delete {
        match secrets::delete_jev_key()? {
            true => println!("removed stored typesafe key"),
            false => println!("no stored typesafe key"),
        }
        return Ok(());
    }
    if status {
        let Some((key, source)) = jev::resolve_key() else {
            println!("jev: no key configured (set TYPESAFE_API_KEY or run `gobstopper auth jev`)");
            return Ok(());
        };
        println!(
            "jev: {} key {} — {}",
            source.describe(),
            secrets::masked(&key),
            match jev::health_check(&key, &endpoint) {
                jev::Health::Ok => "verified",
                jev::Health::Rejected => "rejected by API (401/403)",
                jev::Health::Unverified => "could not verify (network/API error)",
            }
        );
        return Ok(());
    }
    let key = if !std::io::stdin().is_terminal() {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf.trim().to_string()
    } else if let Some(k) = secrets::clipboard_secret() {
        println!("found a key on the clipboard: {}", secrets::masked(&k));
        print!("store it in the OS keychain? [y/N] ");
        std::io::stdout().flush()?;
        let mut ans = String::new();
        std::io::stdin().lock().read_line(&mut ans)?;
        if !matches!(ans.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("aborted");
            return Ok(());
        }
        k
    } else {
        bail!("no key on stdin or clipboard — pipe it in: `pbpaste | gobstopper auth jev`");
    };
    if !(12..=512).contains(&key.chars().count()) || key.contains(char::is_whitespace) {
        bail!(
            "that doesn't look like an API key ({} chars)",
            key.chars().count()
        );
    }
    match jev::health_check(&key, &endpoint) {
        jev::Health::Rejected => bail!("the API rejected that key (401/403) — not stored"),
        health => {
            secrets::store_jev_key(&key)?;
            match health {
                jev::Health::Ok => println!(
                    "typesafe key {} verified and stored in the OS keychain",
                    secrets::masked(&key)
                ),
                jev::Health::Unverified => println!(
                    "typesafe key {} stored in the OS keychain (could not verify: network/API error)",
                    secrets::masked(&key)
                ),
                jev::Health::Rejected => unreachable!(),
            }
            println!("scorer ready: GOBSTOPPER_SCORER=jev gobstopper plan <session>");
        }
    }
    Ok(())
}

fn policy_decision(
    cfg: &config::Config,
    provider: &str,
    context_tokens: u64,
    session_active: bool,
    quota_pressure: Option<QuotaPressure>,
    preset: Option<&str>,
) -> Result<serde_json::Value> {
    let provider_id = match provider {
        "codex" => "codex",
        "claude" | "claude_code" => "claude_code",
        "devin" => "devin",
        other => bail!("unknown provider '{other}'"),
    };
    let mut resolved = cfg.resolve_provider(provider_id, "", preset, None)?;
    if let Some(pressure) = quota_pressure {
        resolved.policy.quota_pressure = pressure;
    }
    let effective_trigger = resolved.policy.effective_trigger();
    let over = context_tokens >= effective_trigger;
    let action = if !over {
        "none"
    } else if session_active || provider_id == "devin" {
        "provider_compact"
    } else {
        "transcript_compact"
    };
    let control = match (over, session_active, provider_id) {
        (true, true, "codex") => Some("thread/compact/start"),
        (true, true, "claude_code") => Some("/compact or relaunch --autocompact"),
        (true, _, "devin") => Some("/compact"),
        _ => None,
    };
    Ok(serde_json::json!({
        "provider": provider_id,
        "action": action,
        "strategy": resolved.strategy,
        "trigger_tokens": resolved.policy.trigger_tokens,
        "effective_trigger_tokens": effective_trigger,
        "min_savings_tokens": resolved.policy.min_savings_tokens,
        "quota_pressure": resolved.policy.quota_pressure,
        "control": control,
    }))
}

fn cmd_policy_check(
    cfg: &config::Config,
    provider: &str,
    context_tokens: u64,
    session_active: bool,
    quota_pressure: Option<QuotaArg>,
    preset: Option<&str>,
    json: bool,
) -> Result<()> {
    let decision = policy_decision(
        cfg,
        provider,
        context_tokens,
        session_active,
        quota_pressure.map(Into::into),
        preset,
    )?;
    if json {
        println!("{}", serde_json::to_string_pretty(&decision)?);
    } else {
        println!(
            "action={} strategy={}",
            decision["action"].as_str().unwrap_or("none"),
            decision["strategy"].as_str().unwrap_or("auto")
        );
        if let Some(control) = decision["control"].as_str() {
            println!("control={control}");
        }
    }
    Ok(())
}

fn cmd_explain() {
    // Sawtooth model: context grows to trigger T, compacts to floor F,
    // grows again. Average occupancy ~ (T+F)/2; per-turn input ~ that.
    // Provider defaults fire near the window ceiling instead.
    let (t, f) = (250_000u64, 40_000u64);
    let window = 1_000_000u64;
    let native_trigger = (window as f64 * 0.9) as u64;
    let native_avg = (native_trigger + 60_000) / 2;
    let ours_avg = (t + f) / 2;
    println!(
        "context economics (steady state, sawtooth model)\n\
         \n\
         provider default: trigger ~{} tokens (90% of {} window), floor ~60k\n\
         gobstopper:       trigger {} tokens, floor {}\n\
         \n\
         average context per turn:\n\
         provider default ~{} tokens\n\
         gobstopper       ~{} tokens\n\
         \n\
         => ~{:.1}x fewer input tokens per turn, and every turn in the\n\
         tail of a long session pays the compacted rate, not the ceiling.\n\
         Aggressive presets (trigger 150k, floor 20k) reach ~{:.1}x.",
        native_trigger,
        window,
        t,
        f,
        native_avg,
        ours_avg,
        native_avg as f64 / ours_avg as f64,
        native_avg as f64 / ((150_000f64 + 20_000.0) / 2.0),
    );
}

fn cmd_plugin(command: &PluginCmd) -> Result<()> {
    match command {
        PluginCmd::Check { manifest } => {
            let checked = plugins::check(manifest)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "protocol_version": 1, "id": checked.manifest.id, "version": checked.manifest.version,
                    "manifest_sha256": checked.manifest_sha256, "capabilities": checked.manifest.capabilities,
                    "code_executed": false, "trusted": false,
                }))?
            );
        }
        PluginCmd::Inspect {
            manifest,
            trusted_sha256,
            provider,
            source,
        } => {
            let bytes = gobstopper_adapters::transaction::read(source)?;
            let request = plugins::Request {
                protocol_version: 1,
                operation: plugins::Capability::ProviderRead,
                provider_id: provider.clone(),
                source_sha256: copy::sha256(&bytes),
                items: Vec::new(),
                usage: Default::default(),
                policy: None,
                content: Some(
                    std::str::from_utf8(&bytes)?
                        .lines()
                        .map(str::to_string)
                        .collect(),
                ),
            };
            let response = plugins::invoke(manifest, trusted_sha256, &request)?;
            println!("{}", serde_json::to_string_pretty(&response.inspection)?);
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = config::load()?;
    match &cli.command {
        Cmd::Plugin { command } => cmd_plugin(command),
        Cmd::Detect { all, json } => cmd_detect(&cli, *all, *json),
        Cmd::Plan {
            session,
            strategy,
            preset,
            trigger,
            floor,
            json,
        } => {
            let d = find_session(&cli, &cfg, session)?;
            let mut resolved = cfg.resolve(
                d.handle.provider,
                &d.handle.session_id,
                preset.as_deref(),
                strategy.as_deref(),
            )?;
            if let Some(t) = trigger {
                resolved.policy.trigger_tokens = *t;
            }
            if let Some(f) = floor {
                resolved.policy.floor_tokens = *f;
            }
            let transcript = detect::load(&d)?;
            match evaluate_detailed(&transcript, &resolved)? {
                Evaluation::Plan(plan) => {
                    let prefix = eval::prefix_tokens(&transcript, &plan);
                    print_plan(&d, &plan, prefix, *json)
                }
                Evaluation::NoPlan(report) => {
                    if *json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        print_no_plan_text(
                            report.context_tokens_before,
                            report.effective_trigger_tokens,
                            &resolved.strategy,
                            &report.adaptive_reasons,
                        );
                    }
                    Ok(())
                }
            }
        }
        Cmd::Apply {
            session,
            strategy,
            preset,
            trigger,
            floor,
            yes,
            in_place,
            no_backup,
            experimental_compacted,
        } => cmd_apply(
            &cli,
            &cfg,
            session,
            strategy.as_deref(),
            preset.as_deref(),
            *trigger,
            *floor,
            *yes,
            *in_place,
            *no_backup,
            *experimental_compacted,
        ),
        Cmd::Verify { session, json } => cmd_verify(&cli, &cfg, session, *json),
        Cmd::Undo {
            session,
            sha,
            yes,
            in_place,
        } => cmd_undo(&cli, &cfg, session, sha.as_deref(), *yes, *in_place),
        Cmd::Fork { session } => cmd_fork(&cli, &cfg, session),
        Cmd::Eval {
            session,
            strategy,
            trigger,
            floor,
            json,
        } => cmd_eval(
            &cli,
            &cfg,
            session,
            strategy.as_deref(),
            *trigger,
            *floor,
            *json,
        ),
        Cmd::CacheEdits {
            session,
            trigger,
            floor,
            plan,
        } => cmd_cache_edits(&cli, &cfg, session, *trigger, *floor, *plan),
        Cmd::InstallHooks => cmd_install_hooks(false, &roots(&cli)),
        Cmd::UninstallHooks => cmd_install_hooks(true, &roots(&cli)),
        Cmd::Hook { event } => cmd_hook(event),
        Cmd::Report {
            strict,
            active_only,
        } => cmd_report(&cli, *strict, *active_only),
        Cmd::Events {
            session,
            tail,
            json,
        } => cmd_events(session.as_deref(), *tail, *json),
        Cmd::Vault { session, json } => cmd_vault(&cli, &cfg, session.as_deref(), *json),
        Cmd::History { session, json } => cmd_history(&cli, &cfg, session, *json),
        Cmd::Show { target, json } => cmd_show(&cli, &cfg, target, *json),
        Cmd::SearchSnapshot {
            sha,
            query,
            limit,
            json: _,
        } => {
            let value = recovery::search_snapshot(sha, query, *limit, &vault::default_root())?;
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(())
        }
        Cmd::ReadSnapshot {
            sha,
            record,
            offset,
            max_bytes,
            json: _,
        } => {
            let value = recovery::read_snapshot_record(
                sha,
                *record,
                *offset,
                *max_bytes,
                &vault::default_root(),
            )?;
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(())
        }
        Cmd::Recall {
            session,
            query,
            sha,
            limit,
            json,
        } => cmd_recall(
            &cli,
            &cfg,
            session.as_deref(),
            query.as_deref(),
            sha.as_deref(),
            *limit,
            *json,
        ),
        Cmd::Diff { a, b, json } => cmd_diff(a, b, *json),
        Cmd::Tune { session, json } => cmd_tune(&cli, &cfg, session, *json),
        Cmd::Bench {
            all,
            trigger,
            floor,
            output,
        } => cmd_bench(&cli, *all, *trigger, *floor, output.as_deref()),
        Cmd::Snapshot { session, label } => cmd_snapshot(&cli, &cfg, session, label.as_deref()),
        Cmd::Mcp { .. } => mcp::run(&cli, &cfg),
        Cmd::Auth {
            provider,
            status,
            delete,
        } => cmd_auth(provider, *status, *delete),
        Cmd::Watch {
            interval,
            dry_run,
            double_buffer,
            active_only,
            once,
        } => cmd_watch(
            &cli,
            &cfg,
            *interval,
            *dry_run,
            *double_buffer,
            *active_only,
            *once,
        ),
        Cmd::PolicyCheck {
            provider,
            context_tokens,
            session_active,
            quota_pressure,
            preset,
            json,
        } => cmd_policy_check(
            &cfg,
            provider,
            *context_tokens,
            *session_active,
            *quota_pressure,
            preset.as_deref(),
            *json,
        ),
        Cmd::Presets => {
            for name in cfg.presets.keys() {
                println!("{name}");
            }
            Ok(())
        }
        Cmd::Explain => {
            cmd_explain();
            Ok(())
        }
    }
}

// Re-export for AdapterError completeness in this crate's error surface.
#[allow(dead_code)]
fn _assert_error_surface(e: AdapterError) -> anyhow::Error {
    anyhow::anyhow!(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Use an immutable executable fixture: tests never write/chmod the file
    /// they execute. Each invocation logs under its explicit CODEX_HOME.
    fn stub_codex() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex-app-server.sh")
    }

    fn tempdir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("gobstopper-test-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn codex_compact_initializes_resumes_then_compacts() {
        let dir = tempdir("happy");
        let stub = stub_codex();
        codex_compact(&stub, "ok-thread", Some(&dir)).unwrap();
        let log = fs::read_to_string(dir.join("requests.log")).unwrap();
        let methods: Vec<String> = log
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| v.get("method").and_then(|m| m.as_str()).map(String::from))
            .collect();
        assert_eq!(
            methods,
            [
                "initialize",
                "initialized",
                "thread/resume",
                "thread/compact/start"
            ]
            .map(String::from)
        );
        let resume: serde_json::Value = serde_json::from_str(log.lines().nth(2).unwrap()).unwrap();
        assert_eq!(
            resume.pointer("/params/excludeTurns"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            resume.pointer("/params/threadId"),
            Some(&serde_json::json!("ok-thread"))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn codex_compact_reports_failed_turn_as_error() {
        let dir = tempdir("failed");
        let stub = stub_codex();
        let err = codex_compact(&stub, "fail-thread", Some(&dir)).unwrap_err();
        assert!(err.to_string().contains("failed"), "got: {err:#}");
        assert!(err.to_string().contains("usage limit"), "got: {err:#}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn codex_compact_propagates_request_errors() {
        let dir = tempdir("err");
        let stub = stub_codex();
        let err = codex_compact(&stub, "bad-resume", Some(&dir)).unwrap_err();
        assert!(err.to_string().contains("cannot resume"), "got: {err:#}");
        let err = codex_compact(&stub, "error-thread", Some(&dir)).unwrap_err();
        assert!(err.to_string().contains("thread not found"), "got: {err:#}");
        let _ = fs::remove_dir_all(&dir);
    }

    fn preset_transcript() -> gobstopper_core::Transcript {
        gobstopper_core::Transcript {
            session: gobstopper_core::SessionHandle {
                provider: gobstopper_core::Provider::Codex,
                session_id: "sess-preset".to_string(),
                path: PathBuf::from("/tmp/sess-preset.jsonl"),
                cwd: None,
                age_secs: 0,
            },
            items: vec![gobstopper_core::model::TranscriptItem {
                line_index: 0,
                kind: gobstopper_core::model::ItemKind::ToolResult,
                est_tokens: 500,
                elidable_bytes: Some(2000),
                elidable_parts: 1,
                label: "tool output".to_string(),
                summary: Some("fake output".to_string()),
                uuid: None,
                parent_uuid: None,
                tool_use_ids: Vec::new(),
                payload_sha256: None,
            }],
            usage: gobstopper_core::model::UsageSample {
                context_tokens: 50_000,
                lifetime_input_tokens: 50_000,
                lifetime_cached_tokens: 0,
                model_context_window: Some(1_000_000),
            },
        }
    }

    fn preset_resolved(command: &str) -> config::Resolved {
        config::Resolved {
            policy: gobstopper_core::strategy::PolicyConfig {
                trigger_tokens: 1_000,
                floor_tokens: 100,
                keep_recent_tool_outputs: 0,
                min_savings_tokens: 0,
                ..Default::default()
            },
            strategy: "agentic".to_string(),
            command: Some(command.to_string()),
            trusted_legacy_command: true,
            plugin: None,
        }
    }

    #[test]
    fn preset_command_empty_edits_defers_without_a_plan() {
        let plan = evaluate(
            &preset_transcript(),
            &preset_resolved("cat > /dev/null; echo '{\"edits\": []}'"),
        )
        .unwrap();
        assert!(plan.is_none(), "empty edits must defer, not apply nothing");
    }

    #[test]
    fn preset_command_edits_flow_into_a_plan() {
        let plan = evaluate(
            &preset_transcript(),
            &preset_resolved(
                "cat > /dev/null; echo '{\"edits\": [{\"op\": \"elide\", \"line_indexes\": [0], \"stub_template\": \"[elided]\"}], \"context_tokens_after\": 1200}'",
            ),
        )
        .unwrap()
        .expect("edits must produce a plan");
        assert_eq!(plan.strategy, "preset:agentic");
        assert_eq!(
            plan.context_tokens_after,
            50_000 - preset_transcript().items[0].estimated_elision_savings()
        );
        assert_ne!(plan.context_tokens_after, 1200);
        assert!(matches!(plan.edits.as_slice(), [Edit::Elide { .. }]));
    }

    #[test]
    fn preset_command_invalid_json_is_an_error() {
        assert!(evaluate(&preset_transcript(), &preset_resolved("echo 'nope'")).is_err());
    }

    fn no_plan_external(edits: Vec<Edit>, minimum: u64) -> Result<Evaluation> {
        let transcript = preset_transcript();
        let mut policy = preset_resolved("").policy;
        policy.min_savings_tokens = minimum;
        let diagnostic = NoPlanReport::new(transcript.context_tokens(), &policy, Vec::new());
        external_plan(&transcript, &policy, "synthetic", edits, diagnostic)
    }

    fn no_plan_report(outcome: Evaluation) -> serde_json::Value {
        let Evaluation::NoPlan(report) = outcome else {
            panic!("expected a rejected proposal");
        };
        serde_json::to_value(report).unwrap()
    }

    #[test]
    fn no_plan_external_empty_edits_have_no_projection() {
        let report = no_plan_report(no_plan_external(Vec::new(), 0).unwrap());
        assert_eq!(report["reason_code"], "empty_external_edits");
        assert!(report["projected_context_tokens_after"].is_null());
        assert!(report["projected_savings_tokens"].is_null());
    }

    #[test]
    fn no_plan_external_nonreducing_precedes_minimum_savings() {
        for minimum in [0, 4096] {
            for edits in [
                vec![Edit::Elide {
                    line_indexes: Vec::new(),
                    stub_template: "[elided]".into(),
                    per_item_stubs: Default::default(),
                }],
                vec![Edit::InjectDigest {
                    digest: gobstopper_core::DigestBlock {
                        summary: Some("synthetic additional state".into()),
                        covers_items: 1,
                        ..Default::default()
                    },
                }],
            ] {
                let report = no_plan_report(no_plan_external(edits, minimum).unwrap());
                assert_eq!(report["reason_code"], "external_nonreducing_plan");
                assert!(report["projected_context_tokens_after"].as_u64().unwrap() >= 50_000);
                assert_eq!(report["projected_savings_tokens"], 0);
            }
        }
    }

    #[test]
    fn no_plan_external_minimum_gate_preserves_its_estimates() {
        let edits = vec![Edit::Elide {
            line_indexes: vec![0],
            stub_template: "[elided]".into(),
            per_item_stubs: Default::default(),
        }];
        let report = no_plan_report(no_plan_external(edits, 4096).unwrap());
        let saved = preset_transcript().items[0].estimated_elision_savings();
        assert_eq!(report["reason_code"], "minimum_savings_not_met");
        assert_eq!(report["projected_context_tokens_after"], 50_000 - saved);
        assert_eq!(report["projected_savings_tokens"], saved);
    }

    #[test]
    fn no_plan_diagnostics_preserve_real_errors() {
        let transcript = preset_transcript();
        for command in ["printf 'not-json'", "printf '{\"edits\":null}'", "exit 7"] {
            assert!(evaluate_detailed(&transcript, &preset_resolved(command)).is_err());
        }
        let mut invalid_policy = preset_resolved("printf '{\"edits\":[]}'");
        invalid_policy.policy.floor_tokens = invalid_policy.policy.trigger_tokens;
        assert!(evaluate_detailed(&transcript, &invalid_policy).is_err());
        let mut untrusted = preset_resolved("printf '{\"edits\":[]}'");
        untrusted.trusted_legacy_command = false;
        assert!(evaluate_detailed(&transcript, &untrusted).is_err());
        let invalid_edits = vec![Edit::Elide {
            line_indexes: vec![999],
            stub_template: "[elided]".into(),
            per_item_stubs: Default::default(),
        }];
        assert!(no_plan_external(invalid_edits, 0).is_err());
        assert!(no_plan_external(
            vec![Edit::ProviderCompact {
                control: "synthetic".into()
            }],
            0
        )
        .is_err());
    }
}
