//! gobstopper: automatic context compaction for Codex and Claude Code.

mod apple;
mod apple_digest;
mod apple_scorer;
mod config;
mod hooks;
mod jev;
mod llm_scorer;
mod mcp;
mod native_operations;
mod proxy;
mod report;
mod secrets;
mod telemetry;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use gobstopper_adapters::detect::{self, Discovered, Roots};
use gobstopper_adapters::{
    codex, copy, eval, fork, plugins, recovery, study, vault, verify, AdapterError,
};
use gobstopper_core::events::{append_event, default_log_path, CompactionEvent};
use gobstopper_core::plan::{CompactionPlan, Edit};
use gobstopper_core::strategy::{self, HeuristicScorer, QuotaPressure, ScoredStrategy};
use gobstopper_core::{Provider, SessionHandle};
use std::io::{BufRead as _, IsTerminal as _, Read as _, Write as _};
use std::path::{Path, PathBuf};
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
    #[command(
        about = "Compare source-bound typed retention on private replay copies; never calls providers"
    )]
    EvalStudy {
        session: String,
        #[arg(
            long,
            required_unless_present = "prepare_manifest",
            conflicts_with = "prepare_manifest",
            help = "Source-hashed gobstopper-retention-v1 annotation manifest"
        )]
        manifest: Option<PathBuf>,
        #[arg(
            long,
            help = "Write a new metadata-only heuristic annotation manifest; does not run the study"
        )]
        prepare_manifest: Option<PathBuf>,
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u8).range(1..=10))]
        rounds: u8,
        #[arg(long)]
        trigger: Option<u64>,
        #[arg(long)]
        floor: Option<u64>,
        #[arg(
            long,
            conflicts_with = "prepare_manifest",
            help = "Score-only realized audit: bind the manifest to SESSION, then score retention against AFTER (path or vault:<sha256>); rounds/trigger/floor are unused"
        )]
        against: Option<String>,
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
    /// Export inert provider hook settings candidates; direct mutation requires
    /// provider-owned custody and is disabled. The output must be a new file.
    InstallHooks {
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Export candidates removing exactly owned hook entries.
    UninstallHooks {
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Handle a provider hook callback (reads hook JSON on stdin).
    /// Invoked by provider hook configs, not by users.
    #[command(hide = true)]
    Hook {
        /// "precompact" | "session-start" | "prompt-policy[:claude]"
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
        /// Sample current context without scanning full provider histories.
        /// Lifetime usage may be unavailable.
        #[arg(long)]
        context_only: bool,
    },
    /// Show compaction telemetry: recent events and cumulative savings.
    Events {
        /// Filter to one session id prefix.
        #[arg(long)]
        session: Option<String>,
        /// Only the last N events (default 20).
        #[arg(long, default_value = "20")]
        tail: usize,
        /// Aggregate per-provider rollout-cohort readout (treatment vs
        /// control) instead of the raw event tail.
        #[arg(long)]
        cohort: bool,
        /// Realized-retention view: only events carrying measured
        /// retention, plus a per-provider rollup.
        #[arg(long)]
        retention: bool,
        /// Only events from the last N seconds/minutes/hours/days
        /// (e.g. `3600`, `30m`, `6h`, `2d`).
        #[arg(long)]
        since: Option<String>,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// List snapshots in the undo vault.
    Vault {
        /// Optional session id prefix or path to filter by.
        session: Option<String>,
        /// Inspect bounded storage sizes and index counts without reading object contents.
        #[arg(long, conflicts_with = "session")]
        stats: bool,
        #[arg(long)]
        json: bool,
    },
    /// Prune the undo vault: keep only the newest snapshots per session.
    /// Dry-run by default; pass --yes to delete.
    Prune {
        /// Snapshots to keep per (provider, session) stream.
        #[arg(long, default_value_t = 10)]
        keep: usize,
        /// Actually delete; without it prints the plan only.
        #[arg(long)]
        yes: bool,
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
    /// Run a Model Context Protocol inspection server on stdio, exposing
    /// sessions and the snapshot vault as tools an agent can call
    /// (list_sessions, recall, history, show, diff, plan, verify).
    Mcp {
        /// Expose snapshot search/read tools. Archived content returned by read
        /// becomes visible to the connected agent/model service. Off by default.
        #[arg(long)]
        allow_transcript_content: bool,
    },
    /// Compact live Claude Code and Codex requests before the provider's own
    /// compaction fires: a loopback proxy that applies the cliff rule to each
    /// outgoing request and leaves transcript files alone.
    Proxy {
        #[command(subcommand)]
        command: proxy::ProxyCmd,
    },
    /// Inspect durable native operation metadata and unresolved dispatches.
    /// Does not clear uncertainty, retry a provider call, or create state.
    NativeOperations,
    /// Reconcile only an operation with already recorded matching Codex terminal
    /// evidence. Does not infer completion or retry a provider call.
    NativeReconcile { operation_sha256: String },
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
        /// Restrict watch to one provider; default watches all.
        #[arg(long)]
        provider: Option<String>,
        /// Run one discovery pass and exit, useful for supervised monitoring.
        #[arg(long)]
        once: bool,
        /// Bound transcript loading and plan evaluation to this many seconds
        /// per pass; remaining sessions defer to the next pass. 0 = unbounded.
        #[arg(long, default_value = "0")]
        eval_budget: u64,
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
        /// Context occupancy.
        #[arg(long)]
        context_tokens: Option<u64>,
        /// Read the named Claude Code session's recorded usage instead of
        /// caller-supplied context numbers.
        #[arg(long)]
        session: Option<String>,
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
    /// Write a session's canonical transcript form to stdout — the same
    /// bytes `plan`/`verify`/`eval` consume and snapshots preserve.
    Export {
        /// Session id prefix, or path to a transcript file.
        session: String,
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
    if query.trim().is_empty() || query.len() > 4096 || query.chars().any(char::is_control) {
        bail!("session selector must be nonempty, bounded text without control characters");
    }
    let path = PathBuf::from(query);
    if path.is_file() {
        // A SQLite store is never a transcript: resolve sessions inside it
        // by id instead of treating the file as provider data.
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut file = options.open(&path)?;
        anyhow::ensure!(
            file.metadata()?.is_file(),
            "session input must be a regular file"
        );
        let mut magic = [0u8; 16];
        let read_magic = match file.read_exact(&mut magic) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => false,
            Err(error) => return Err(error.into()),
        };
        if read_magic && magic == *b"SQLite format 3\0" {
            bail!(
                "{} is a SQLite store, not a transcript file",
                path.display()
            );
        }
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

/// Resolve an eval-study byte source: a session id/transcript path, or
/// `vault:<sha256>` for a snapshot object.
fn eval_spec_bytes(
    cli: &Cli,
    cfg: &config::Config,
    spec: &str,
) -> Result<(SessionHandle, Vec<u8>)> {
    if let Some(sha) = spec.strip_prefix("vault:") {
        let root = vault::default_root();
        let entry = vault::list(&root)?
            .into_iter()
            .find(|e| e.sha256 == sha)
            .with_context(|| format!("no vault entry for {sha}"))?;
        let bytes = vault::read_object(&entry.sha256, &root)?;
        return Ok((
            SessionHandle {
                provider: match entry.provider.as_str() {
                    "codex" => Provider::Codex,
                    "claude_code" => Provider::ClaudeCode,
                    other => {
                        anyhow::bail!("snapshot was recorded by unsupported provider {other:?}")
                    }
                },
                session_id: entry.session_id.clone(),
                path: entry.path.clone(),
                cwd: None,
                age_secs: u64::MAX,
            },
            bytes,
        ));
    }
    let d = find_session(cli, cfg, spec)?;
    let bytes = gobstopper_adapters::transaction::read(&d.handle.path)?;
    Ok((d.handle, bytes))
}

/// Recent positive provider-record reductions for one exact source, newest
/// first. Legacy estimates and unavailable accounting never tune thresholds.
fn recent_applied(handle: &SessionHandle) -> Vec<(u64, u64)> {
    let path = gobstopper_core::events::default_log_path();
    let Ok(events) = gobstopper_core::events::read_events(&path) else {
        return Vec::new();
    };
    recent_applied_from(&events, handle)
}

fn recent_applied_from(events: &[CompactionEvent], handle: &SessionHandle) -> Vec<(u64, u64)> {
    let Ok(identity) = detect::source_identity(handle) else {
        return Vec::new();
    };
    gobstopper_core::events::qualified_reductions(events)
        .reductions
        .into_iter()
        .rev()
        .filter(|row| {
            row.event.provider == handle.provider
                && row.event.session_id == handle.session_id
                && row.event.source_identity_sha256.as_ref() == Some(&identity)
                && row.reduction_tokens > 0
        })
        .take(3)
        .map(|row| (row.before_tokens, row.reduction_tokens))
        .collect()
}

/// Full adaptive sample from a parsed transcript.
fn adaptive_sample(transcript: &gobstopper_core::Transcript) -> gobstopper_core::AdaptiveSample {
    gobstopper_core::AdaptiveSample {
        model_context_window: transcript.usage.model_context_window,
        context_tokens: transcript.context_tokens(),
        elidable_tokens: Some(transcript.elidable_tokens()),
        recent: recent_applied(&transcript.session),
    }
}

/// Transcript-free adaptive sample for cheap pre-checks (watch loop):
/// the elidable-share rule is skipped.
fn adaptive_sample_usage(
    handle: &SessionHandle,
    usage: &gobstopper_core::UsageSample,
) -> gobstopper_core::AdaptiveSample {
    gobstopper_core::AdaptiveSample {
        model_context_window: usage.model_context_window,
        context_tokens: usage.context_tokens,
        elidable_tokens: None,
        recent: recent_applied(handle),
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
            eprintln!("warning: scorer_unavailable; using heuristic");
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
    evaluate_with_effects(transcript, resolved, false)
}

/// Deterministic MCP inspection does not inherit the caller's inference or
/// extension environment. It shares admission with executable planning.
fn evaluate_inspection(
    transcript: &gobstopper_core::Transcript,
    resolved: &config::Resolved,
) -> Result<Option<CompactionPlan>> {
    Ok(match evaluate_with_effects(transcript, resolved, true)? {
        Evaluation::Plan(plan) => Some(plan),
        Evaluation::NoPlan(_) => None,
    })
}

fn evaluate_with_effects(
    transcript: &gobstopper_core::Transcript,
    resolved: &config::Resolved,
    inspection: bool,
) -> Result<Evaluation> {
    if inspection {
        resolved.ensure_inspection()?;
    }
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
        let driver = if inspection { None } else { maybe_scorer() };
        let (scores, scorer_summary) = if let Some(driver) = driver {
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
        if !inspection {
            apple_digest::maybe_upgrade(plan, transcript);
        }
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

/// Build a numeric compaction telemetry record (v1 schema). Callers that
/// attach optional evidence fields (snapshot refs, realized retention)
/// build first, mutate, then append themselves.
#[allow(clippy::too_many_arguments)]
fn build_event(
    event_context: &telemetry::EventContext<'_>,
    d: &Discovered,
    plan: &CompactionPlan,
    action: &str,
    outcome: &str,
    trigger_tokens: u64,
    duration_ms: u64,
    error_code: Option<&str>,
) -> CompactionEvent {
    let mut event = CompactionEvent::new(
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
            .fold(0u64, u64::saturating_add),
        duration_ms,
        error_code.map(|s| s.to_string()),
    );
    event_context.annotate(&mut event, &d.handle);
    event
}

/// Emit a numeric compaction telemetry record (v1 schema). Telemetry is
/// best-effort: a logging failure must never fail a compaction.
#[allow(clippy::too_many_arguments)]
fn emit_event(
    event_context: &telemetry::EventContext<'_>,
    d: &Discovered,
    plan: &CompactionPlan,
    action: &str,
    outcome: &str,
    trigger_tokens: u64,
    duration_ms: u64,
    error_code: Option<&str>,
) {
    let ev = build_event(
        event_context,
        d,
        plan,
        action,
        outcome,
        trigger_tokens,
        duration_ms,
        error_code,
    );
    if append_event(&default_log_path(), &ev).is_err() {
        eprintln!("telemetry write failed (non-fatal): event_append_failed");
    }
}

/// Score-only realized audit between two vault objects: heuristic checks
/// bind to the before-bytes and are scored against the after-bytes.
/// Returns (total, literal, lexical). Best-effort — `None` means
/// unmeasured, never a compaction failure. `pub(crate)` so hooks can
/// attach the same measurement to hook-fired compaction events.
pub(crate) fn realized_retention(
    handle: &SessionHandle,
    before_sha: &str,
    after_sha: &str,
) -> Option<(u64, u64, u64)> {
    let root = vault::default_root();
    let before = vault::read_object(before_sha, &root).ok()?;
    let after = vault::read_object(after_sha, &root).ok()?;
    let manifest = study::build_manifest(handle.clone(), &before).ok()?;
    let report = study::audit(handle.clone(), &before, manifest, String::new(), &after).ok()?;
    let r = &report.rows.first()?.retention;
    Some((r.total as u64, r.retained as u64, r.lexical_retained as u64))
}

/// Classify provider completion using a frozen after-state. A cleared usage
/// counter is missing measurement, not proof that all context was reclaimed.
/// Return whether an applied change was actually observed.
fn record_native_completion(
    event_context: &telemetry::EventContext<'_>,
    d: &Discovered,
    plan: &CompactionPlan,
    before: &vault::VaultEntry,
    trigger: u64,
    started: std::time::Instant,
) -> Option<bool> {
    let root = vault::default_root();
    let post = vault::snapshot(
        &d.handle.path,
        d.handle.provider,
        &d.handle.session_id,
        Some("post-compact"),
        &root,
    )
    .ok();
    // Parse accounting from the exact retained objects on both sides. Discovery
    // samples and the plan are estimates; neither can certify these bytes.
    let reader = vault::Reader::open(&root).ok();
    let canonical_handle = d.handle.path.canonicalize().ok().map(|path| SessionHandle {
        path,
        ..d.handle.clone()
    });
    let observation = |entry: &vault::VaultEntry| {
        let bytes = reader.as_ref()?.read_object(&entry.sha256).ok()?;
        if copy::sha256(&bytes) != entry.source_sha256 {
            return None;
        }
        eval::token_observation(canonical_handle.as_ref()?, &bytes, Some(&entry.sha256)).ok()
    };
    let before_observation = observation(before);
    let after_observation = post.as_ref().and_then(observation);
    let before_tokens = before_observation
        .as_ref()
        .and_then(|observed| observed.context_tokens)
        .filter(|tokens| *tokens > 0);
    let after_tokens = after_observation
        .as_ref()
        .and_then(|observed| observed.context_tokens)
        .filter(|tokens| *tokens > 0);
    let same_identity = before_observation
        .as_ref()
        .zip(after_observation.as_ref())
        .is_some_and(|(before, after)| {
            before.source_identity_sha256 == after.source_identity_sha256
        });
    let unchanged = post
        .as_ref()
        .is_some_and(|entry| entry.source_sha256 == before.source_sha256);
    let context_before = before_tokens.unwrap_or(plan.context_tokens_before);
    let (outcome, error, after) = match (before_tokens, after_tokens) {
        _ if unchanged && same_identity => ("skipped", Some("provider_noop"), context_before),
        (Some(before), Some(after)) if same_identity && after >= before => {
            ("skipped", Some("provider_noop"), after)
        }
        (Some(_), Some(after)) if same_identity => ("applied", None, after),
        _ => ("failed", Some("unresolved_context"), context_before),
    };
    let done = CompactionPlan {
        edits: vec![],
        context_tokens_before: context_before,
        context_tokens_after: after,
        ..plan.clone()
    };
    let mut event = build_event(
        event_context,
        d,
        &done,
        "provider_compact",
        outcome,
        trigger,
        started.elapsed().as_millis() as u64,
        error,
    );
    event.snapshot_before_sha256 = Some(before.sha256.clone());
    event.snapshot_after_sha256 = post.map(|entry| entry.sha256);
    event.source_identity_sha256 = before_observation
        .as_ref()
        .map(|observed| observed.source_identity_sha256.clone());
    event.before_observation = before_observation;
    event.after_observation = after_observation;
    drop(reader);
    if outcome == "applied" {
        if let Some(post) = &event.snapshot_after_sha256 {
            if let Some((total, retained, lexical)) =
                realized_retention(&d.handle, &before.sha256, post)
            {
                event.retention_total = Some(total);
                event.retention_retained = Some(retained);
                event.retention_lexical = Some(lexical);
            }
        }
    }
    if append_event(&default_log_path(), &event).is_err() {
        eprintln!("telemetry write failed (non-fatal): event_append_failed");
    }
    eprintln!(
        "native {} compaction: {}{}",
        d.handle.provider.as_str(),
        outcome,
        error.map(|code| format!(" ({code})")).unwrap_or_default(),
    );
    match outcome {
        "applied" => Some(true),
        "skipped" => Some(false),
        _ => None,
    }
}

fn prepare_native_operation(
    cli: &Cli,
    d: &Discovered,
    binary: &Path,
    snapshot: &vault::VaultEntry,
    policy_sha256: &str,
) -> Result<native_operations::Operation> {
    let roots = roots(cli);
    let (home, contract) = match d.handle.provider {
        Provider::Codex => (
            &roots.codex_home,
            "private-app-server-matching-compaction-item-and-turn-v1",
        ),
        Provider::ClaudeCode => (
            &roots.claude_home,
            "private-resume-exit-status-assumption-v1",
        ),
    };
    native_operations::Operation::prepare(
        &d.handle,
        home,
        binary,
        snapshot,
        policy_sha256,
        contract,
    )
}

fn record_native_admission_failure(
    event_context: &telemetry::EventContext<'_>,
    d: &Discovered,
    strategy: &str,
    context: u64,
    trigger: u64,
    error: &anyhow::Error,
) {
    let code = if native_operations::activation_unqualified(error) {
        "native_unqualified"
    } else if native_operations::executable_unavailable(error) {
        "spawn_failed"
    } else {
        "custody_unavailable"
    };
    let refused = CompactionPlan {
        strategy: strategy.to_owned(),
        rationale: "native dispatch admission refused".into(),
        edits: vec![],
        context_tokens_before: context,
        context_tokens_after: context,
    };
    emit_event(
        event_context,
        d,
        &refused,
        "provider_compact",
        if native_operations::activation_unqualified(error) {
            "blocked"
        } else {
            "failed"
        },
        trigger,
        0,
        Some(code),
    );
    eprintln!("native dispatch refused before provider execution ({code})");
}

/// Route a `ProviderCompact` edit to the provider's own machinery.
fn provider_compact(
    d: &Discovered,
    codex_bin: &std::path::Path,
    codex_home: &std::path::Path,
) -> Result<native_operations::TerminalEvidence> {
    match d.handle.provider {
        Provider::Codex => {
            codex_compact(codex_bin, &d.handle.session_id, Some(codex_home), 600_000)
        }
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

/// Cooldown (seconds) for a failed `thread/compact` outcome, or 0 for
/// transient infra errors that should retry on the next pass. A failed
/// provider turn still appends `task_started`/`task_complete` to the
/// rollout, so suppression must key on the session, not the
/// fingerprint — that is what `holddown` stores.
fn codex_failure_hold_secs(msg: &str) -> u64 {
    if msg.contains("usage limit") {
        // Provider quota window — unknown length, retry sparingly.
        4 * 3600
    } else if msg.contains("cannot resume")
        || msg.contains("not found")
        || msg.contains("not supported")
    {
        // Structural for this thread state or account plan — the remote
        // compact task's server-side model may not be provisioned. A
        // day bounds the noise; checked before the generic turn-failed
        // arm because these errors also carry that status text.
        24 * 3600
    } else if msg.contains("outcome unknown")
        || msg.contains("turn failed")
        || msg.contains("turn interrupted")
        || msg.contains("turn aborted")
        || msg.contains("turn unknown")
    {
        // Possibly in-flight or a provider-side turn failure — do not
        // race a write that may still land; if it does, the post-state
        // reads under trigger and skips anyway.
        3600
    } else {
        // Spawn, stream close, response timeout — transient infra.
        0
    }
}

/// Only classify provider errors; provider text can contain transcript content.
fn codex_error_class(message: &str) -> &'static str {
    let message = message.to_ascii_lowercase();
    if message.contains("usage limit") {
        "usage limit exceeded"
    } else if message.contains("cannot resume") {
        "cannot resume provider thread"
    } else if message.contains("not found") {
        "provider thread not found"
    } else if message.contains("not supported") {
        "provider operation not supported"
    } else {
        "provider rejected"
    }
}

/// A completion belongs to this operation only after its compaction item
/// identifies the turn. Request responses and notifications may interleave.
#[derive(Default)]
struct CodexCompactionProgress {
    compact_item: Option<(String, String)>,
    item_completed: bool,
    terminal: Option<Result<(), String>>,
    early_terminals: std::collections::BTreeMap<String, Result<(), String>>,
}

impl CodexCompactionProgress {
    fn observe(&mut self, value: &serde_json::Value, thread_id: &str) {
        if self.terminal.as_ref().is_some_and(Result::is_err) {
            return;
        }
        if value.pointer("/params/threadId").and_then(|v| v.as_str()) != Some(thread_id) {
            return;
        }
        match value.get("method").and_then(|v| v.as_str()) {
            Some(method @ ("item/started" | "item/completed"))
                if value.pointer("/params/item/type").and_then(|v| v.as_str())
                    == Some("contextCompaction") =>
            {
                let (Some(turn), Some(item)) = (
                    value.pointer("/params/turnId").and_then(|v| v.as_str()),
                    value.pointer("/params/item/id").and_then(|v| v.as_str()),
                ) else {
                    return;
                };
                if turn.is_empty()
                    || turn.len() > 256
                    || item.is_empty()
                    || item.len() > 256
                    || turn.chars().any(char::is_control)
                    || item.chars().any(char::is_control)
                {
                    self.terminal = Some(Err(
                        "provider outcome unknown: invalid terminal identity".into(),
                    ));
                    return;
                }
                if self.compact_item.is_none() {
                    self.compact_item = Some((turn.to_owned(), item.to_owned()));
                }
                if self
                    .compact_item
                    .as_ref()
                    .is_some_and(|(t, i)| t == turn && i == item)
                    && method == "item/completed"
                {
                    self.item_completed = true;
                }
            }
            Some("turn/completed") => {
                let Some(turn) = value.pointer("/params/turn/id").and_then(|v| v.as_str()) else {
                    return;
                };
                if turn.is_empty() || turn.len() > 256 || turn.chars().any(char::is_control) {
                    self.terminal = Some(Err(
                        "provider outcome unknown: invalid terminal identity".into(),
                    ));
                    return;
                }
                let status = value
                    .pointer("/params/turn/status")
                    .and_then(|v| v.as_str());
                let outcome = if status == Some("completed") {
                    Ok(())
                } else {
                    let status = match status {
                        Some("failed") => "failed",
                        Some("interrupted") => "interrupted",
                        Some("aborted") => "aborted",
                        _ => "unknown",
                    };
                    let detail = value
                        .pointer("/params/turn/error/message")
                        .and_then(|v| v.as_str())
                        .map(codex_error_class)
                        .unwrap_or("provider rejected");
                    Err(format!("codex compaction turn {status}: {detail}"))
                };
                if let Some(previous) = self.early_terminals.get(turn) {
                    if previous != &outcome {
                        self.terminal = Some(Err(
                            "provider outcome unknown: conflicting terminal evidence".into(),
                        ));
                        return;
                    }
                } else {
                    if self.early_terminals.len() >= 64 {
                        self.terminal = Some(Err(
                            "provider outcome unknown: terminal buffer exhausted".into(),
                        ));
                        return;
                    }
                    self.early_terminals.insert(turn.to_owned(), outcome);
                }
            }
            _ => {}
        }
        if let Some((turn, _)) = &self.compact_item {
            if let Some(outcome) = self.early_terminals.get(turn) {
                if outcome.is_err() || self.item_completed {
                    self.terminal = Some(outcome.clone());
                }
            }
        }
    }
}

struct CodexChild(std::process::Child);

impl CodexChild {
    /// Observe exit without reaping on Unix. Reaping before group cleanup
    /// releases the PID and could direct a later signal at an unrelated group.
    fn has_exited(&mut self) -> std::io::Result<bool> {
        #[cfg(unix)]
        {
            // SAFETY: waitid writes a valid siginfo_t, the PID is our retained
            // child, and WNOWAIT keeps that identity reserved until Drop reaps it.
            let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.0.id() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == -1 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: successful waitid initialized info; si_pid is zero when
            // WNOHANG found no waitable event.
            Ok(unsafe { info.si_pid() } != 0)
        }
        #[cfg(not(unix))]
        {
            self.0.try_wait().map(|status| status.is_some())
        }
    }
}

impl Drop for CodexChild {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // SAFETY: Command::process_group(0) made this child the leader of
            // its own group. It has not been reaped, so its positive PID cannot
            // have been reused by another process or group. Signal before wait.
            unsafe {
                libc::kill(-(self.0.id() as libc::pid_t), libc::SIGKILL);
            }
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn codex_read_frames(
    mut stream: std::process::ChildStdout,
    tx: std::sync::mpsc::SyncSender<Result<serde_json::Value, &'static str>>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;
    const MAX_FRAME: usize = 1024 * 1024;
    let mut pending = Vec::new();
    let mut buffer = [0u8; 8192];
    while !cancelled.load(Ordering::Acquire) {
        match stream.read(&mut buffer) {
            Ok(0) => {
                if !pending.is_empty() {
                    let _ = tx.send(
                        mcp::strict_json(&pending)
                            .map_err(|_| "codex app-server emitted invalid JSON"),
                    );
                }
                return;
            }
            Ok(n) => {
                for part in buffer[..n].split_inclusive(|byte| *byte == b'\n') {
                    if pending.len() + part.len() > MAX_FRAME {
                        let _ = tx.send(Err("codex app-server frame exceeds byte limit"));
                        return;
                    }
                    pending.extend_from_slice(part);
                    if part.last() == Some(&b'\n') {
                        let frame = mcp::strict_json(&pending)
                            .map_err(|_| "codex app-server emitted invalid JSON");
                        let failed = frame.is_err();
                        if tx.send(frame).is_err() || failed {
                            return;
                        }
                        pending.clear();
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                let _ = tx.send(Err("codex app-server stream read failed"));
                return;
            }
        }
    }
}

/// Ask a private Codex app-server to compact a thread. The response only
/// acknowledges dispatch; success needs the compaction item and its matching
/// terminal turn. Once dispatch starts, missing evidence is an unknown outcome.
fn codex_compact(
    codex_bin: &std::path::Path,
    thread_id: &str,
    codex_home: Option<&std::path::Path>,
    outcome_ms: u64,
) -> Result<native_operations::TerminalEvidence> {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    // Keep every request well below a pipe's capacity, even if a broken child
    // answers without consuming stdin. Never interpolate unbounded identity.
    if !cfg!(unix)
        || thread_id.is_empty()
        || thread_id.len() > 256
        || !thread_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        || outcome_ms == 0
        || outcome_ms > 3_600_000
    {
        bail!("invalid codex compaction identity or deadline");
    }
    let mut command = Command::new(codex_bin);
    command
        .args(["app-server", "--listen", "stdio://"])
        .envs(codex_home.map(|home| ("CODEX_HOME", home)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Provider stderr is untrusted transcript-bearing text. Do not retain
        // or echo it, and avoid a second pipe that could block the provider.
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = CodexChild(command.spawn().with_context(|| {
        format!(
            "spawning `{} app-server --listen stdio://`",
            codex_bin.display()
        )
    })?);
    let mut stdin = child.0.stdin.take().unwrap();
    let stdout = child.0.stdout.take().unwrap();
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let fd = stdout.as_raw_fd();
        // SAFETY: stdout owns this live descriptor, which has one reader; only
        // its status flags change. Nonblocking reads allow bounded cancellation
        // even when a descendant creates another session and keeps stdout open.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
        {
            return Err(std::io::Error::last_os_error()).context("configure bounded provider pipe");
        }
    }
    let (tx, rx) = mpsc::sync_channel::<Result<serde_json::Value, &'static str>>(64);
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader_cancelled = cancelled.clone();
    let reader = std::thread::spawn(move || codex_read_frames(stdout, tx, reader_cancelled));
    let mut send = |value: serde_json::Value| -> Result<()> {
        serde_json::to_writer(&mut stdin, &value)?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    };
    let deadline = |ms: u64| Instant::now() + Duration::from_millis(ms);
    let await_response = |id: i64,
                          until: Instant,
                          mut progress: Option<&mut CodexCompactionProgress>|
     -> Result<()> {
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                if id == 2 {
                    bail!("provider outcome unknown awaiting dispatch response; do not replay automatically");
                }
                bail!("codex app-server timed out waiting for response id {id}");
            }
            match rx.recv_timeout(left) {
                Ok(Ok(value)) if value.get("id").and_then(|v| v.as_i64()) == Some(id) => {
                    if let Some(error) = value.get("error") {
                        let class = codex_error_class(
                            error.get("message").and_then(|v| v.as_str()).unwrap_or(""),
                        );
                        bail!("codex app-server: {class}");
                    }
                    if value.get("result").is_none() {
                        if id == 2 {
                            bail!("provider outcome unknown: dispatch response is missing result; do not replay automatically");
                        }
                        bail!("codex app-server response is missing result");
                    }
                    return Ok(());
                }
                Ok(Ok(value)) => {
                    if let Some(progress) = progress.as_deref_mut() {
                        progress.observe(&value, thread_id);
                    }
                }
                Ok(Err(error)) => {
                    if id == 2 {
                        bail!("provider outcome unknown: {error}; do not replay automatically");
                    }
                    bail!("{error}");
                }
                Err(_) if id == 2 => {
                    bail!("provider outcome unknown awaiting dispatch response; do not replay automatically");
                }
                Err(_) => bail!("codex app-server closed or timed out before response id {id}"),
            }
        }
    };
    let outcome = (|| -> Result<native_operations::TerminalEvidence> {
        send(serde_json::json!({
            "method": "initialize", "id": 0,
            "params": {"clientInfo": {"name": "gobstopper", "version": env!("CARGO_PKG_VERSION")}}
        }))?;
        await_response(0, deadline(outcome_ms.min(60_000)), None)?;
        send(serde_json::json!({"method": "initialized"}))?;
        send(serde_json::json!({
            "method": "thread/resume", "id": 1,
            "params": {"threadId": thread_id, "excludeTurns": true}
        }))?;
        await_response(1, deadline(outcome_ms.min(60_000)), None)?;
        let mut progress = CodexCompactionProgress::default();
        send(serde_json::json!({
            "method": "thread/compact/start", "id": 2,
            "params": {"threadId": thread_id}
        }))
        .context("provider outcome unknown during dispatch; do not replay automatically")?;
        await_response(2, deadline(outcome_ms.min(30_000)), Some(&mut progress))?;
        let end = deadline(outcome_ms);
        loop {
            if let Some(outcome) = progress.terminal.take() {
                outcome.map_err(anyhow::Error::msg)?;
                let (turn, item) = progress
                    .compact_item
                    .context("missing compaction identity")?;
                return Ok(native_operations::TerminalEvidence {
                    session_id: thread_id.to_owned(),
                    turn_id: Some(turn),
                    item_id: Some(item),
                });
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("provider outcome unknown at deadline; do not replay automatically");
            }
            match rx.recv_timeout(left) {
                Ok(Ok(value)) => progress.observe(&value, thread_id),
                Ok(Err(error)) => bail!("provider outcome unknown: {error}; do not replay automatically"),
                Err(_) => bail!("provider outcome unknown after disconnect or deadline; do not replay automatically"),
            }
        }
    })();
    drop(stdin);
    drop(rx);
    // Graceful EOF gives the provider a chance to release its writer lock.
    let exit_deadline = deadline(5_000);
    loop {
        match child.has_exited() {
            Ok(true) => break,
            _ if Instant::now() >= exit_deadline => break,
            _ => std::thread::sleep(Duration::from_millis(25)),
        }
    }
    drop(child);
    cancelled.store(true, std::sync::atomic::Ordering::Release);
    // Receiver drop unblocks queued sends; nonblocking reads observe cancellation
    // without waiting for EOF from any escaped descendant. Join, never detach.
    #[cfg(unix)]
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("provider pipe reader panicked"))?;
    // Non-Unix process-tree and pipe cancellation need separate qualification.
    #[cfg(not(unix))]
    drop(reader);
    let terminal = outcome?;
    println!("codex provider compaction: compaction turn completed");
    Ok(terminal)
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
        .map(session_row)
        .collect()
}

fn session_row(d: &detect::Discovered) -> serde_json::Value {
    serde_json::json!({
        "provider": d.handle.provider,
        "session_id": d.handle.session_id,
        "path": d.handle.path.to_str(),
        "path_encoding": if d.handle.path.to_str().is_some() { "utf8" } else { "non_utf8" },
        "cwd": d.handle.cwd.as_deref().and_then(Path::to_str),
        "cwd_encoding": d.handle.cwd.as_deref().map(|path| if path.to_str().is_some() { "utf8" } else { "non_utf8" }),
        "active": d.handle.is_active(),
        "context_tokens": d.usage.context_tokens,
        "context_state": d.usage.context_state,
        "reported_context_tokens": d.usage.reported_context(),
        "context_components": d.usage.context_components,
        "context_reason": d.usage.context_reason,
        "measured_component_subtotal": d.usage.measured_component_subtotal(),
        "component_subtotal_basis": "known_numeric_components_not_complete_occupancy",
        "lifetime_scope": d.usage.lifetime_scope,
        "lifetime_input_tokens": d.usage.lifetime_input_tokens,
        "lifetime_cached_tokens": d.usage.lifetime_cached_tokens,
        "source_identity_sha256": detect::source_identity(&d.handle).ok(),
        "usage_basis": "recorded_provider_accounting_not_billing",
        "model_context_window": d.usage.model_context_window,
    })
}

/// Shorten display-only text without splitting UTF-8 or exceeding the existing
/// byte budget. Stored identities and machine-readable output stay untouched.
fn display_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
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
            display_prefix(&d.handle.session_id, 38),
            if d.handle.is_active() { "live" } else { "idle" },
            d.usage
                .reported_context()
                .map(|tokens| tokens.to_string())
                .unwrap_or_else(|| "unknown".into()),
            if d.usage.lifetime_scope == gobstopper_core::model::LifetimeScope::Full {
                d.usage.lifetime_input_tokens.to_string()
            } else {
                "unknown".into()
            },
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
    use sha2::{Digest, Sha256};
    if in_place {
        bail!("standalone in-place restore is retired because a provider may hold an open writer; omit --in-place to restore a verified fork");
    }
    let d = find_session(cli, cfg, session)?;
    let root = vault::default_root();
    let reader = vault::Reader::open(&root)?;
    let entries: Vec<_> = reader
        .entries()?
        .into_iter()
        .filter(|entry| matches_snapshot_session(entry, &d.handle))
        .collect();
    let entry = match sha {
        Some(prefix) => {
            let full = resolve_snapshot_sha(&entries, prefix)?;
            entries
                .into_iter()
                .find(|entry| entry.sha256 == full)
                .unwrap()
        }
        None => entries
            .into_iter()
            .find(|e| !matches!(e.strategy.as_deref(), Some("post-compact" | "pre-undo")))
            .ok_or_else(|| anyhow::anyhow!("no vault snapshot for {}", d.handle.path.display()))?,
    };
    let retained = reader.read_object(&entry.sha256)?;
    anyhow::ensure!(
        fork::source_session_id(d.handle.provider, &retained)? == d.handle.session_id
            && entry.bytes == retained.len() as u64
            && (entry.source_sha256.is_empty()
                || entry.source_sha256 == format!("{:x}", Sha256::digest(&retained))),
        "snapshot bytes do not match the selected session identity"
    );
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

fn cmd_vault(
    cli: &Cli,
    cfg: &config::Config,
    session: Option<&str>,
    stats: bool,
    json: bool,
) -> Result<()> {
    let root = vault::default_root();
    if stats {
        let accounting = vault::accounting::inspect(&root);
        if json {
            println!("{}", serde_json::to_string_pretty(&accounting)?);
        } else {
            println!(
                "vault accounting: {:?}; non-atomic metadata observation",
                accounting.status
            );
            for (kind, row) in &accounting.directories {
                let count = row
                    .files
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "unavailable".into());
                let bytes = row
                    .logical_bytes
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "unavailable".into());
                println!(
                    "{kind:?}: {count} files, {bytes} logical bytes ({:?})",
                    row.status
                );
            }
            println!(
                "index: {:?}; physical and reclaimable bytes unmeasured",
                accounting.index.status
            );
        }
        return Ok(());
    }
    let mut entries = vault::list(&root)?;
    if let Some(q) = session {
        let d = find_session(cli, cfg, q)?;
        let canonical_path =
            std::fs::canonicalize(&d.handle.path).unwrap_or_else(|_| d.handle.path.clone());
        entries.retain(|e| {
            e.provider == d.handle.provider.as_str()
                && e.session_id == d.handle.session_id
                && (e.path == d.handle.path || e.path == canonical_path)
        });
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
            display_prefix(&e.session_id, 12),
            e.path.display(),
        );
    }
    Ok(())
}

fn cmd_prune(keep: usize, yes: bool, json: bool) -> Result<()> {
    let report = vault::prune(&vault::default_root(), keep, !yes)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "{}: {} streams, {} index entries dropped ({} kept), {} manifests + {} chunks removed, {} bytes reclaimed",
        if yes { "pruned" } else { "plan" },
        report.streams,
        report.dropped_entries,
        report.kept_entries,
        report.manifests_removed,
        report.chunks_removed,
        report.bytes_reclaimed,
    );
    if !yes && report.dropped_entries > 0 {
        println!("dry-run only — rerun with --yes to delete");
    }
    Ok(())
}

fn cmd_history(cli: &Cli, cfg: &config::Config, session: &str, json: bool) -> Result<()> {
    let d = find_session(cli, cfg, session)?;
    let root = vault::default_root();
    let mut entries = vault::list(&root)?;
    let canonical_path =
        std::fs::canonicalize(&d.handle.path).unwrap_or_else(|_| d.handle.path.clone());
    entries.retain(|e| {
        e.provider == d.handle.provider.as_str()
            && e.session_id == d.handle.session_id
            && (e.path == d.handle.path || e.path == canonical_path)
    });
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
            display_prefix(&e.session_id, 12),
            e.path.display(),
        );
    }
    Ok(())
}

fn matches_snapshot_session(entry: &vault::VaultEntry, handle: &SessionHandle) -> bool {
    let canonical = handle
        .path
        .canonicalize()
        .unwrap_or_else(|_| handle.path.clone());
    entry.provider == handle.provider.as_str()
        && entry.session_id == handle.session_id
        && (entry.path == handle.path || entry.path == canonical)
}

fn resolve_snapshot_sha(entries: &[vault::VaultEntry], prefix: &str) -> Result<String> {
    if prefix.is_empty() || prefix.len() > 64 || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("snapshot selector must be a nonempty hexadecimal SHA-256 prefix");
    }
    let prefix = prefix.to_ascii_lowercase();
    let matches: std::collections::BTreeSet<_> = entries
        .iter()
        .filter(|entry| entry.sha256.starts_with(&prefix))
        .map(|entry| entry.sha256.clone())
        .collect();
    match matches.len() {
        0 => bail!("no snapshot matches the requested SHA prefix"),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => bail!("snapshot SHA prefix is ambiguous; use the full SHA-256"),
    }
}

fn show_summary(cli: &Cli, cfg: &config::Config, target: &str) -> Result<serde_json::Value> {
    let root = vault::default_root();
    let reader = vault::Reader::open(&root)?;
    let entries = reader.entries()?;
    let entry = if target.len() >= 16 && target.chars().all(|c| c.is_ascii_hexdigit()) {
        let sha = resolve_snapshot_sha(&entries, target)?;
        let matches: Vec<_> = entries
            .into_iter()
            .filter(|entry| entry.sha256 == sha)
            .collect();
        let selected = matches.first().unwrap();
        if matches.iter().any(|entry| {
            entry.provider != selected.provider.as_str()
                || entry.session_id != selected.session_id
                || entry.path != selected.path
        }) {
            bail!("snapshot has multiple recorded source identities; select a session instead");
        }
        matches.into_iter().next().unwrap()
    } else {
        let d = find_session(cli, cfg, target)?;
        entries
            .into_iter()
            .find(|entry| matches_snapshot_session(entry, &d.handle))
            .with_context(|| format!("no vault snapshot for {}", d.handle.path.display()))?
    };

    let data = reader.read_object(&entry.sha256)?;
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
    let reader = vault::Reader::open(&root)?;
    let selected = session
        .map(|session| find_session(cli, cfg, session))
        .transpose()?;
    let mut entries: Vec<_> = reader
        .entries()?
        .into_iter()
        .filter(|entry| {
            selected
                .as_ref()
                .is_none_or(|d| matches_snapshot_session(entry, &d.handle))
        })
        .collect();
    let full_sha = sha
        .map(|prefix| resolve_snapshot_sha(&entries, prefix))
        .transpose()?;
    if let Some(sha) = &full_sha {
        entries.retain(|entry| &entry.sha256 == sha);
    }
    let mut digests = reader.recall_entries(&entries, query)?;
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
    let reader = vault::Reader::open(&root)?;
    let entries = reader.entries()?;
    let a_full = resolve_snapshot_sha(&entries, a)?;
    let b_full = resolve_snapshot_sha(&entries, b)?;
    let summary = reader.diff(&a_full, &b_full)?;
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
    let reader = vault::Reader::open(&root)?;
    let entries = reader.entries()?;
    let a_full = resolve_snapshot_sha(&entries, a)?;
    let b_full = resolve_snapshot_sha(&entries, b)?;
    let summary = reader.diff(&a_full, &b_full)?;
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

fn cmd_install_hooks(uninstall: bool, roots: &Roots, output: Option<&Path>) -> Result<()> {
    let Some(output) = output else {
        bail!("provider settings custody is unavailable; use --output <new-file> to export an inert candidate bundle");
    };
    let targets = [
        (
            roots.claude_home.join("settings.json"),
            vec![
                hooks::HookTarget::ClaudePreCompact,
                hooks::HookTarget::ClaudeSessionStart,
                hooks::HookTarget::ClaudeUserPromptSubmit,
            ],
        ),
        (
            roots.codex_home.join("hooks.json"),
            vec![
                hooks::HookTarget::CodexPreCompact,
                hooks::HookTarget::CodexSessionStart,
            ],
        ),
    ];
    // Prepare every provider before publishing anything. The bundle contains
    // settings values and is intentionally exported as a private new file.
    let candidates = targets
        .iter()
        .map(|(path, entries)| hooks::prepare_settings(path, entries, Some("hooks"), uninstall))
        .collect::<Result<Vec<_>>>()?;
    let output_parent = fs_canonical_parent(output)?;
    let output_path = output_parent.join(output.file_name().context("output needs a file name")?);
    for (path, _) in &targets {
        if let Ok(parent) = fs_canonical_parent(path) {
            if Some(output_path.as_path())
                == path.file_name().map(|name| parent.join(name)).as_deref()
            {
                bail!("candidate bundle cannot be a provider settings path");
            }
        }
    }
    let bytes = serde_json::to_vec_pretty(&serde_json::json!({
        "schema": "gobstopper/hook-settings-bundle-v1",
        "activation": "unqualified; provider-owned application and trust review required",
        "candidates": candidates,
    }))?;
    gobstopper_adapters::transaction::publish_new(&output_path, &bytes)?;
    println!(
        "Exported {} inert settings candidates to {}; provider settings were not changed.",
        candidates.len(),
        output.display()
    );
    Ok(())
}

fn fs_canonical_parent(path: &Path) -> Result<PathBuf> {
    Ok(std::fs::canonicalize(
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )?)
}

fn cmd_hook(cli: &Cli, cfg: &config::Config, event: &str) -> Result<()> {
    let Some(buf) = hooks::read_stdin_bounded()? else {
        return Ok(());
    };
    if let Some(out) = hooks::handle(event, &buf, &roots(cli), cfg)? {
        println!("{out}");
    }
    Ok(())
}

fn cmd_report(cli: &Cli, strict: bool, active_only: bool, context_only: bool) -> Result<()> {
    let max_age_secs = if active_only {
        gobstopper_core::SessionHandle::HOT_SECS
    } else {
        0
    };
    // The report is read-only; it may still borrow the watcher lanes'
    // advisory discovery snapshot so a warm run costs fingerprints, not
    // file and store reparses.
    let mut cache = detect::DiscoveryCache::default();
    if let Some(dir) = watch_state_path(None).parent() {
        load_persisted_discovery(&mut cache, dir, None);
    }
    let discovery = detect::discover_cached_with_status(
        &roots(cli),
        max_age_secs,
        &mut cache,
        None,
        context_only,
    );
    let events = match gobstopper_core::events::read_events_with_status(&default_log_path()) {
        Ok(events) => events,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(error) => return Err(error).context("compaction telemetry unavailable"),
    };
    let evidence_available = events.invalid_records == 0 && events.oversized_records == 0;
    let mut report = report::build_report(&discovery.sessions, &events.events, evidence_available);
    report["gobstopper"]["discovery"] = serde_json::to_value(discovery.providers)?;
    report["gobstopper"]["eventRead"] = serde_json::json!({
        "validRecords": events.events.len(), "invalidRecords": events.invalid_records,
        "oversizedRecords": events.oversized_records, "generations": events.generations,
    });
    if strict {
        report
            .as_object_mut()
            .map(|object| object.remove("gobstopper"));
        if let Some(list) = report["sessions"].as_array_mut() {
            for s in list {
                s.as_object_mut().map(|o| o.remove("gobstopper"));
            }
        }
    }
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// `--since` duration: bare seconds or a `s`/`m`/`h`/`d` suffix.
fn parse_since(s: &str) -> Result<u64> {
    // ASCII-suffix check via bytes avoids a non-char-boundary split_at.
    let (num, mult) = match s.as_bytes().last() {
        Some(b's') => (&s[..s.len() - 1], 1),
        Some(b'm') => (&s[..s.len() - 1], 60),
        Some(b'h') => (&s[..s.len() - 1], 3_600),
        Some(b'd') => (&s[..s.len() - 1], 86_400),
        _ => (s, 1),
    };
    let n: u64 = num
        .parse()
        .with_context(|| format!("invalid --since duration '{s}'"))?;
    Ok(n.saturating_mul(mult))
}

fn cmd_events(
    cfg: &config::Config,
    session: Option<&str>,
    tail: usize,
    cohort: bool,
    retention: bool,
    since: Option<&str>,
    json: bool,
) -> Result<()> {
    let path = default_log_path();
    let mut events = match gobstopper_core::events::read_events(&path) {
        Ok(events) => events,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(_) => bail!("compaction telemetry unavailable"),
    };
    if let Some(prefix) = session {
        events.retain(|e| e.session_id.starts_with(prefix));
    }
    if let Some(dur) = since {
        let cutoff = now_secs().saturating_sub(parse_since(dur)?);
        events.retain(|e| e.ts >= cutoff);
    }
    if cohort {
        let summary = report::cohort_summary(cfg, &events);
        if json {
            println!("{}", serde_json::to_string_pretty(&summary)?);
            return Ok(());
        }
        println!("cohort readout — {} events", events.len());
        let mut providers: Vec<(&String, &serde_json::Value)> = summary["providers"]
            .as_object()
            .into_iter()
            .flatten()
            .collect();
        providers.sort_by(|a, b| a.0.cmp(b.0));
        for (provider, p) in providers {
            let rollout = p["rollout_pct"]
                .as_u64()
                .map(|v| format!("{v}%"))
                .unwrap_or_else(|| "-".to_string());
            println!("\n{provider} (current rollout {rollout})");
            println!(
                "  {:<10} {:>8} {:>6} {:>6} {:>10} {:>7} {:>10} {:>10} {:>10}",
                "cohort",
                "sessions",
                "shown",
                "suppr.",
                "watch-skip",
                "applies",
                "reclaimed",
                "unresolved",
                "events"
            );
            for cohort_name in ["treatment", "control", "ungated", "unknown"] {
                let c = &p["cohorts"][cohort_name];
                if c["events"].as_u64().unwrap_or(0) == 0 {
                    continue;
                }
                println!(
                    "  {:<10} {:>8} {:>6} {:>6} {:>10} {:>7} {:>10} {:>10} {:>10}",
                    cohort_name,
                    c["sessions"],
                    c["advisories_shown"],
                    c["advisories_suppressed"],
                    c["watch_suppressed"],
                    c["applies"],
                    c["reclaimed_tokens"],
                    c["unresolved_context"],
                    c["events"],
                );
            }
        }
        return Ok(());
    }
    if retention {
        return print_retention(&events, tail, json);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&events)?);
        return Ok(());
    }
    let applied: Vec<&CompactionEvent> = events.iter().filter(|e| e.outcome == "applied").collect();
    let reclaimed = applied.iter().fold(0u64, |total, event| {
        total.saturating_add(event.est_reclaimed_tokens)
    });
    println!(
        "{} retained events ({} labelled applied) — ~{} estimated token reductions (not billing)",
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

/// Exact retained-byte pairs qualify lexical retention, including when provider
/// context usage is absent. Duplicate observations count once; conflicting
/// values for one pair disqualify that pair instead of choosing a winner.
fn qualified_retention(events: &[CompactionEvent]) -> Vec<&CompactionEvent> {
    let mut pairs = std::collections::BTreeMap::new();
    for (index, event) in events.iter().enumerate() {
        let (Some(identity), Some(before), Some(after), Some(total), Some(literal), Some(lexical)) = (
            event.source_identity_sha256.as_ref(),
            event.before_observation.as_ref(),
            event.after_observation.as_ref(),
            event.retention_total,
            event.retention_retained,
            event.retention_lexical,
        ) else {
            continue;
        };
        if event.outcome != "applied"
            || !matches!(
                event.action.as_str(),
                "provider_compact" | "transcript_compact"
            )
            || event.error_code.is_some()
            || !before.is_valid()
            || !after.is_valid()
            || literal > total
            || lexical > total
            || identity != &before.source_identity_sha256
            || identity != &after.source_identity_sha256
            || before.snapshot_manifest_sha256.is_none()
            || after.snapshot_manifest_sha256.is_none()
            || before.snapshot_manifest_sha256 != event.snapshot_before_sha256
            || after.snapshot_manifest_sha256 != event.snapshot_after_sha256
        {
            continue;
        }
        let key = (
            event.provider.as_str(),
            &event.session_id,
            identity,
            &event.snapshot_before_sha256,
            &event.snapshot_after_sha256,
        );
        let counts = (
            total,
            literal,
            lexical,
            &before.source_sha256,
            &after.source_sha256,
        );
        let entry = pairs.entry(key).or_insert((Some(index), counts));
        if entry.1 != counts {
            entry.0 = None;
        }
    }
    let mut indices: Vec<_> = pairs.values().filter_map(|(index, _)| *index).collect();
    indices.sort_unstable();
    indices.into_iter().map(|index| &events[index]).collect()
}

/// Read-only lexical retention over qualified, deduplicated retained-byte pairs.
fn print_retention(events: &[CompactionEvent], tail: usize, json: bool) -> Result<()> {
    let measured = qualified_retention(events);
    let mut by_provider: std::collections::BTreeMap<&str, [u64; 4]> =
        std::collections::BTreeMap::new();
    for e in &measured {
        let agg = by_provider.entry(e.provider.as_str()).or_default();
        agg[0] = agg[0].saturating_add(1);
        agg[1] = agg[1].saturating_add(e.retention_retained.unwrap_or(0));
        agg[2] = agg[2].saturating_add(e.retention_lexical.unwrap_or(0));
        agg[3] = agg[3].saturating_add(e.retention_total.unwrap_or(0));
    }
    if json {
        let providers: serde_json::Map<String, serde_json::Value> = by_provider
            .iter()
            .map(|(name, a)| {
                (
                    name.to_string(),
                    serde_json::json!({
                        "measured_events": a[0],
                        "retained": a[1],
                        "lexical_retained": a[2],
                        "checks": a[3],
                    }),
                )
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "measured_events": measured.len(),
                "measurement_basis": "deduplicated_source_bound_literal_lexical_retention_not_task_success",
                "events": measured,
                "by_provider": providers,
            }))?
        );
        return Ok(());
    }
    println!("{} events carry realized retention", measured.len());
    let mut providers: Vec<_> = by_provider.iter().collect();
    providers.sort_by(|a, b| a.0.cmp(b.0));
    for (name, a) in providers {
        println!(
            "  {name:<12} {} measured — literal {}/{} lexical {}/{}",
            a[0], a[1], a[3], a[2], a[3]
        );
    }
    for e in measured.iter().rev().take(tail).rev() {
        let total = e.retention_total.unwrap_or(0);
        let retained = e.retention_retained.unwrap_or(0);
        let lexical = e.retention_lexical.unwrap_or(0);
        // Flag events whose summary dropped below half the bound checks —
        // a lossy compaction worth reviewing, not a failure.
        let flag = if total > 0 && lexical < total / 2 + total % 2 {
            " LOSSY"
        } else {
            ""
        };
        println!(
            "  {} {:<12} {:<18} literal {}/{} lexical {}/{} {}",
            e.ts,
            e.provider.as_str(),
            e.session_id,
            retained,
            total,
            lexical,
            total,
            flag
        );
    }
    Ok(())
}

fn cmd_fork(cli: &Cli, cfg: &config::Config, session: &str) -> Result<()> {
    let d = find_session(cli, cfg, session)?;
    let r = fork::fork_with_vault(
        d.handle.provider,
        &d.handle.path,
        None,
        &vault::default_root(),
    )?;
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
    config::validate_policy(&policy)?;
    let scorer = maybe_scorer();
    let judge = jev::eval_judge();
    let hooks = eval::EvalHooks {
        scorer: scorer.as_deref(),
        probe_judge: judge.as_deref(),
    };
    let rows = eval::eval_session_with_hooks(&d.handle, &policy, strategy_flag, &hooks)?;
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
            (_, Some(e)) => println!("  {:<11} error: {e}", row.strategy),
            (Some(plan), None) => {
                let probe = match &row.probe_score {
                    Some(s) if s.recall_available => format!(
                        "  literal retention {:.0}% ({}/{}, complete={}){}",
                        s.recall * 100.0,
                        s.probes_recalled,
                        s.probes_requested,
                        s.complete,
                        if s.tail_probes_total > 0 && s.complete && !s.tail_intact {
                            ", sampled tail probe absent"
                        } else {
                            ""
                        },
                    ),
                    _ => "  literal retention unmeasured".to_string(),
                };
                let semantic = match &row.semantic_score {
                    Some(s) if s.recall_available => format!(
                        "  literal+judge estimate {:.0}% ({}/{}, complete={})",
                        s.recall * 100.0,
                        s.probes_recalled,
                        s.probes_requested,
                        s.complete,
                    ),
                    Some(_) => "  judge estimate incomplete".to_string(),
                    _ => String::new(),
                };
                println!(
                    "  {:<11} {} -> ~{} (projected reduction ~{}; {}){}{}{}",
                    row.strategy,
                    plan.context_tokens_before,
                    plan.context_tokens_after,
                    row.est_reclaimed,
                    row.execution_state,
                    probe,
                    semantic,
                    if row.verify_errors > 0 {
                        format!("  ⚠ {} verify errors", row.verify_errors)
                    } else {
                        String::new()
                    },
                );
            }
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

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn bench_failure_row(provider: Provider, session: &str, failure: &str) -> String {
    // Missing measurements stay empty. A closed category avoids emitting source
    // contents or private parser diagnostics into the benchmark artifact.
    let mut fields = vec![String::new(); 24];
    fields[0] = provider.as_str().to_owned();
    fields[1] = csv_field(session);
    fields[16] = "failed".to_owned();
    fields[17] = "unavailable".to_owned();
    fields[20] = "false".to_owned();
    fields[21] = "false".to_owned();
    fields[22] = "unavailable".to_owned();
    fields[23] = failure.to_owned();
    format!("{}\n", fields.join(","))
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
    let scorer = std::cell::OnceCell::new();
    let mut csv = String::new();
    csv.push_str(
        "provider,session,strategy,context_before,context_after,est_reclaimed,prefix_tokens,prefix_ratio,score,verify_errors,verify_warnings,probes_total,probes_recalled,recall,tail_intact,duration_ms,execution_state,token_basis,source_sha256,result_sha256,retention_complete,recall_available,retention_basis,failure\n",
    );
    let mut row_count = 0;
    for d in sessions {
        let mut resolved = match cfg.resolve(d.handle.provider, &d.handle.session_id, None, None) {
            Ok(r) => r,
            Err(_) => {
                csv.push_str(&bench_failure_row(
                    d.handle.provider,
                    &d.handle.session_id,
                    "policy_resolution_failed",
                ));
                row_count += 1;
                continue;
            }
        };
        if let Some(t) = trigger {
            resolved.policy.trigger_tokens = t;
        }
        if let Some(f) = floor {
            resolved.policy.floor_tokens = f;
        }
        if config::validate_policy(&resolved.policy).is_err() {
            csv.push_str(&bench_failure_row(
                d.handle.provider,
                &d.handle.session_id,
                "policy_resolution_failed",
            ));
            row_count += 1;
            continue;
        }
        let hooks = eval::EvalHooks {
            scorer: scorer.get_or_init(maybe_scorer).as_deref(),
            probe_judge: None,
        };
        let rows = match eval::eval_session_with_hooks(&d.handle, &resolved.policy, None, &hooks) {
            Ok(rows) => rows,
            Err(_) => {
                csv.push_str(&bench_failure_row(
                    d.handle.provider,
                    &d.handle.session_id,
                    "session_evaluation_failed",
                ));
                row_count += 1;
                continue;
            }
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
                "{},{},{},{},{},{},{},{:.4},{:.0},{},{},{},{},{:.2},{},{},{},{},{},{},{},{},literal_presence,{}\n",
                d.handle.provider.as_str(),
                csv_field(&d.handle.session_id),
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
                row.duration_ms,
                row.execution_state,
                row.token_basis,
                row.source_sha256,
                row.result_sha256.as_deref().unwrap_or(""),
                row.probe_score.as_ref().is_some_and(|score| score.complete),
                row.probe_score.as_ref().is_some_and(|score| score.recall_available),
                if row.error.is_some() { "strategy_evaluation_failed" } else { "" }
            ));
            row_count += 1;
        }
    }
    if let Some(path) = output {
        gobstopper_adapters::transaction::publish_new(path, csv.as_bytes())?;
        println!("wrote {} rows to {}", row_count, path.display());
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
    let event_context = &telemetry::EventContext::new(cfg, false);
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
    if plan
        .edits
        .iter()
        .any(|edit| matches!(edit, Edit::ProviderCompact { .. }))
    {
        let homes = roots(cli);
        let (home, binary) = match d.handle.provider {
            Provider::Codex => (
                &homes.codex_home,
                resolve_codex_bin(cli.codex_bin.as_deref()),
            ),
            Provider::ClaudeCode => (
                &homes.claude_home,
                claude_bin().context("Claude executable unavailable")?,
            ),
        };
        native_operations::check_activation(&d.handle, home, &binary)?;
    }
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
    let file_edits: Vec<Edit> = plan
        .edits
        .iter()
        .filter(|e| !matches!(e, Edit::ProviderCompact { .. } | Edit::CacheEdit { .. }))
        .cloned()
        .collect();
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
                    event_context,
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
                    event_context,
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
                    event_context,
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
                    event_context,
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
        if snapshot.source_sha256 != source_sha256 {
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
        // Fork preparation changes identity metadata. Compare the provider's
        // result with this exact fork, while retaining the original snapshot
        // above as the recovery point for fork creation itself.
        let native_snapshot = snapshot_before_edit(&d, "pre-compact")?;
        emit_event(
            event_context,
            &d,
            &plan,
            "provider_compact",
            "planned",
            trigger,
            0,
            None,
        );
        let binary = resolve_codex_bin(cli.codex_bin.as_deref());
        let policy_sha256 =
            copy::sha256(format!("{cfg:?}:{:?}:{plan:?}", resolved.policy).as_bytes());
        let mut operation =
            prepare_native_operation(cli, &d, &binary, &native_snapshot, &policy_sha256)?;
        operation.dispatch()?;
        match provider_compact(&d, operation.binary(), &roots(cli).codex_home) {
            Ok(terminal) => {
                let observed = record_native_completion(
                    event_context,
                    &d,
                    &plan,
                    &native_snapshot,
                    trigger,
                    started,
                );
                operation.finish(observed, Some(terminal))?;
            }
            Err(error) => {
                operation.finish(None, None)?;
                let failed = CompactionPlan {
                    edits: vec![],
                    context_tokens_after: plan.context_tokens_before,
                    ..plan.clone()
                };
                let mut event = build_event(
                    event_context,
                    &d,
                    &failed,
                    "provider_compact",
                    "failed",
                    trigger,
                    started.elapsed().as_millis() as u64,
                    Some("provider_rejected"),
                );
                event.snapshot_before_sha256 = Some(native_snapshot.sha256.clone());
                if let Err(error) = append_event(&default_log_path(), &event) {
                    eprintln!("telemetry write failed (non-fatal): {error}");
                }
                return Err(error);
            }
        }
    }
    Ok(())
}

/// Cheap per-session decision version for watch suppression caching:
/// file identity+change clocks, and observed usage plus the live/idle bit.
/// This is a cheap decision version, not a byte-integrity proof or dispatch
/// authority.
fn session_fingerprint(d: &Discovered) -> Option<String> {
    let content = {
        let m = std::fs::metadata(&d.handle.path).ok()?;
        let mtime = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            format!(
                "{}:{mtime}:{}:{}:{}:{}",
                m.len(),
                m.dev(),
                m.ino(),
                m.ctime(),
                m.ctime_nsec(),
            )
        }
        #[cfg(not(unix))]
        format!("{}:{mtime}", m.len())
    };
    Some(format!("{content}:{:?}:{}", d.usage, d.handle.is_active()))
}

/// Cheap per-pass cost estimate for ordering watch work: sessions cost
/// their transcript bytes. Unknown sizes sort last so a giant session
/// cannot starve every small session behind it in a serial pass.
fn session_cost_hint(d: &Discovered) -> u64 {
    std::fs::metadata(&d.handle.path)
        .map(|m| m.len())
        .unwrap_or(u64::MAX)
}

/// Per-daemon persisted watch state: terminal-decision fingerprints,
/// the Claude settle arm, rate-limit clocks, and the last-emitted
/// delegation context all survive a daemon restart, so relaunching does
/// not re-plan every session once (the cold-pass burst). Clocks persist
/// as epoch seconds and reload relative to `now`; entries older than a
/// day are dropped rather than trusted across unknown downtime.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct WatchState {
    /// Additive, numeric-only pass receipt. Old generation-9 readers ignore
    /// these fields. An incomplete pass always has a null completion time.
    #[serde(default)]
    checkpoint_schema: u32,
    #[serde(default)]
    artifact_sha256: String,
    #[serde(default)]
    pass_started_at_ms: Option<u64>,
    #[serde(default)]
    pass_completed_at_ms: Option<u64>,
    #[serde(default)]
    interval_secs: u64,
    #[serde(default)]
    active_only: bool,
    #[serde(default)]
    native_activation: String,
    #[serde(default)]
    decisions: WatchDecisions,
    /// Decision-vocabulary version. `settled` records *why* a session was
    /// suppressed only implicitly — entries written under an older lever
    /// set before a provider-native compact existed would suppress the new
    /// path forever. Bump on any change that adds or alters a terminal
    /// decision; on load, stale-generation suppressions are dropped.
    #[serde(default)]
    generation: u32,
    #[serde(default)]
    config_sha256: String,
    #[serde(default)]
    settled: std::collections::HashMap<String, String>,
    #[serde(default)]
    settle_pass: std::collections::HashMap<String, String>,
    /// Terminal provider outcomes suppress a session for a cooldown
    /// window, keyed on session — not fingerprint. A failed provider
    /// turn still appends to the rollout (codex `task_started`/
    /// `task_complete`), and shared store identity is part of the key,
    /// so a fingerprint-keyed settle can never hold for these.
    #[serde(default)]
    holddown: std::collections::HashMap<String, u64>,
    /// Pre-journal versions mixed uncertain dispatches and known cooldowns.
    /// Preserve these ambiguities permanently instead of guessing after expiry.
    #[serde(default)]
    legacy_unresolved: std::collections::HashSet<String>,
    #[serde(default)]
    last_fire: std::collections::HashMap<String, u64>,
    #[serde(default)]
    last_apply: std::collections::HashMap<String, u64>,
    #[serde(default)]
    delegated_ctx: std::collections::HashMap<String, u64>,
}

/// Counts describe the latest pass only; they are not lifetime effectiveness or
/// provider coverage. No session IDs, paths or policy commands are added here.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct WatchDecisions {
    discovered: u64,
    legacy_unresolved: u64,
    native_unresolved: u64,
    settled: u64,
    /// Sessions that reached the pass loop but skipped transcript work
    /// because `--eval-budget` was exhausted. Not a terminal decision —
    /// deferred sessions are reconsidered next pass in the same order.
    #[serde(default)]
    eval_deferred: u64,
    cooldown: u64,
    below_trigger: u64,
    native_unqualified: u64,
}

/// Current decision vocabulary. v1: initial watch state. v2: provider-native
/// provider-native compact added — pre-v2 suppressions may encode "no
/// lever existed" verdicts that are no longer true. v6: codex closed-session
/// `thread/compact` added — pre-v6 codex suppressions may encode "no
/// closed-session lever" verdicts, and v6 codex failures distinguish
/// permanent rejections (settle) from transient infra errors (retry).
/// v7: terminal provider failures move to a session-keyed cooldown —
/// a failed turn still mutates the rollout, so fingerprint settles
/// never held and produced a retry storm.
/// v8: native no-ops expire on a cooldown; Claude native dispatch precedes
/// planner acceptance; all providers preserve unknown usage as unmeasured.
const WATCH_STATE_GENERATION: u32 = 9;
const MAX_WATCH_STATE_BYTES: u64 = 4 * 1024 * 1024;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn instant_to_epoch(t: std::time::Instant, now_secs: u64) -> u64 {
    now_secs.saturating_sub(t.elapsed().as_secs())
}

/// Only meaningful within a day — a clock older than that is treated as
/// absent rather than trusted across unknown downtime.
fn epoch_to_instant(epoch: u64, now_secs: u64) -> Option<std::time::Instant> {
    if epoch == 0 || epoch > now_secs {
        return None;
    }
    let age = now_secs - epoch;
    if age > 86_400 {
        return None;
    }
    Some(std::time::Instant::now() - std::time::Duration::from_secs(age))
}

/// `watch-state-<provider|all>.json` beside the telemetry log — scoped
/// per `--provider` so concurrently running provider daemons never share
/// a file.
fn watch_state_path(provider: Option<Provider>) -> PathBuf {
    let dir = default_log_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let tag = provider.map(|p| p.as_str()).unwrap_or("all");
    dir.join(format!("watch-state-{tag}.json"))
}

/// Locate the `claude` executable for headless `--resume -p /compact`.
/// LaunchAgents run with a minimal PATH, so fall back to the standard
/// install locations after searching PATH itself.
fn claude_bin() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("GOBSTOPPER_CLAUDE_BIN").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("claude"))
                .collect()
        })
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        candidates.push(home.join(".local/bin/claude"));
        candidates.push(home.join(".claude/local/claude"));
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin/claude"));
    candidates.push(PathBuf::from("/usr/local/bin/claude"));
    candidates.into_iter().find(|c| c.is_file())
}

fn load_watch_state(path: &Path) -> Result<WatchState> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(WatchState::default()),
        Err(e) => {
            return Err(e).context("watch state unavailable; repair required before dispatch")
        }
    };
    if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_WATCH_STATE_BYTES {
        bail!("watch state must be a bounded regular file; repair required");
    }
    let mut bytes = Vec::new();
    file.take(MAX_WATCH_STATE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_WATCH_STATE_BYTES {
        bail!("watch state exceeds byte bound");
    }
    let mut state: WatchState = serde_json::from_slice(&bytes).map_err(|_| {
        anyhow::anyhow!("watch state is malformed; preserve it for repair before dispatch")
    })?;
    if state.generation > WATCH_STATE_GENERATION {
        bail!("watch state was written by a newer protocol; downgrade refused");
    }
    if state.generation < 9 {
        state
            .legacy_unresolved
            .extend(state.holddown.keys().cloned());
    }
    Ok(state)
}

fn legacy_native_uncertainty() -> Result<std::collections::HashSet<String>> {
    let mut unresolved = std::collections::HashSet::new();
    for provider in [None, Some(Provider::Codex), Some(Provider::ClaudeCode)] {
        unresolved.extend(load_watch_state(&watch_state_path(provider))?.legacy_unresolved);
    }
    Ok(unresolved)
}

/// Atomic write (tmp + rename) so a SIGKILL mid-save cannot leave a torn
/// state file that wipes the suppression map on next load.
fn save_watch_state(path: &Path, state: &WatchState) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_WRITE: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .context("watch state has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".watch-state-{}-{}.tmp",
        std::process::id(),
        NEXT_WRITE.fetch_add(1, Ordering::Relaxed),
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&tmp)?;
        serde_json::to_writer(&mut file, state)?;
        if file.metadata()?.len() > MAX_WATCH_STATE_BYTES {
            bail!("watch state exceeds byte bound; prior checkpoint preserved for repair");
        }
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.context("persist watch state")
}

/// Persisted discovery cache: same directory and same write discipline as
/// watch state, but advisory — a corrupt or missing snapshot only costs a
/// rescan, so every reader treats parse or schema drift as a cold start.
const MAX_DISCOVERY_CACHE_BYTES: u64 = 8 * 1024 * 1024;

fn discovery_cache_path(dir: &Path, provider: Provider) -> PathBuf {
    dir.join(format!("discovery-cache-{}.json", provider.as_str()))
}

fn persisted_providers(provider: Option<Provider>) -> impl Iterator<Item = Provider> {
    [Provider::Codex, Provider::ClaudeCode]
        .into_iter()
        .filter(move |p| provider.is_none_or(|q| q == *p))
}

fn load_persisted_discovery(
    cache: &mut detect::DiscoveryCache,
    dir: &Path,
    provider: Option<Provider>,
) {
    for p in persisted_providers(provider) {
        let path = discovery_cache_path(dir, p);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(file) = serde_json::from_str::<detect::DiscoveryCacheFile>(&text) else {
            continue;
        };
        if file.schema != detect::DiscoveryCacheFile::SCHEMA || file.provider != p.as_str() {
            continue;
        }
        cache.merge_persisted(p, file.entries);
    }
}

fn save_persisted_discovery(
    cache: &detect::DiscoveryCache,
    dir: &Path,
    provider: Option<Provider>,
) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_WRITE: AtomicU64 = AtomicU64::new(0);
    for p in persisted_providers(provider) {
        let path = discovery_cache_path(dir, p);
        let entries = cache.persist_rows(p);
        if entries.is_empty() {
            continue;
        }
        let file = detect::DiscoveryCacheFile {
            schema: detect::DiscoveryCacheFile::SCHEMA.to_owned(),
            provider: p.as_str().to_owned(),
            written_unix: now_secs(),
            entries,
        };
        let tmp = dir.join(format!(
            ".discovery-cache-{}-{}.tmp",
            std::process::id(),
            NEXT_WRITE.fetch_add(1, Ordering::Relaxed),
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| -> Result<()> {
            let mut writer = options.open(&tmp)?;
            serde_json::to_writer(&mut writer, &file)?;
            if writer.metadata()?.len() > MAX_DISCOVERY_CACHE_BYTES {
                bail!("discovery cache exceeds byte bound; snapshot not persisted");
            }
            writer.sync_all()?;
            std::fs::rename(&tmp, &path)?;
            #[cfg(unix)]
            std::fs::File::open(dir)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
            eprintln!("discovery cache persist failed (non-fatal): {path:?}");
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_watch(
    cli: &Cli,
    _cfg: &config::Config,
    interval: u64,
    dry_run: bool,
    double_buffer: bool,
    active_only: bool,
    provider: Option<String>,
    once: bool,
    eval_budget: u64,
) -> Result<()> {
    if interval == 0 {
        bail!("watch interval must be positive");
    }
    let provider = provider
        .as_deref()
        .map(|p| match p {
            "codex" => Ok(Provider::Codex),
            "claude" | "claude_code" => Ok(Provider::ClaudeCode),
            other => bail!("unknown provider '{other}'"),
        })
        .transpose()?;
    if double_buffer {
        bail!("in-place double-buffer swapping is retired; use copy-only watch without --double-buffer");
    }
    // Stable artifact identity participates in decision invalidation. It is
    // retained for this process and revalidated at both ends of every pass.
    let artifact_sha256 = if dry_run {
        native_operations::artifact_sha256().unwrap_or("")
    } else {
        native_operations::artifact_sha256()?
    };
    // Persisted watch state (watch-state-<provider>.json beside the
    // telemetry log): fingerprints, settle arms, and rate-limit clocks
    // survive restarts so a relaunch does not re-plan every session.
    let state_path = watch_state_path(provider);
    let persisted = load_watch_state(&state_path)?;
    // A provider-specific watcher and the all-provider watcher share native
    // targets. Migrate uncertainty from every legacy lane before dispatch.
    let mut legacy_unresolved = legacy_native_uncertainty()?;
    legacy_unresolved.extend(persisted.legacy_unresolved);
    let mut decision_config = if persisted.artifact_sha256 == artifact_sha256 {
        persisted.config_sha256
    } else {
        String::new()
    };
    let boot_secs = now_secs();
    let mut last_fire: std::collections::HashMap<String, std::time::Instant> = persisted
        .last_fire
        .iter()
        .filter_map(|(k, &t)| epoch_to_instant(t, boot_secs).map(|i| (k.clone(), i)))
        .collect();
    // Terminal-decision suppression: session_key -> fingerprint recorded
    // when a session was applied, failed, or judged unplannable. Skips
    // the expensive load until the provider actually appends — context
    // metrics alone can otherwise re-trigger every pass.
    // Stale-generation suppressions encode verdicts from an older lever
    // set. Drop them once; sessions re-enter and re-decide under the
    // current vocabulary. Clocks and delegation dedup stay — they are
    // rate-limit state, not terminal decisions.
    let mut settled = if persisted.generation >= WATCH_STATE_GENERATION {
        persisted.settled
    } else {
        std::collections::HashMap::new()
    };
    let mut settle_pass = if persisted.generation >= WATCH_STATE_GENERATION {
        persisted.settle_pass
    } else {
        std::collections::HashMap::new()
    };
    // Terminal provider failures hold a session down for a cooldown —
    // unlike `settled` this is keyed on the session, not a fingerprint:
    // a failed provider turn still mutates its own transcript, so a
    // fingerprint-based settle can never suppress these. Loaded under
    // the same generation gate.
    // Cooldowns represent possibly completed effects, not old planner
    // decisions. Preserve them even when the decision vocabulary changes.
    let mut holddown = persisted.holddown;
    // (settle_pass is loaded above with the same generation gate.)
    // Apply hold-down: session_key -> Instant of the last successful
    // in-place mutation. A session that re-appends and re-triggers
    // within `apply_hold_secs` is churning; hold it out so one bouncy
    // session cannot snapshot+rewrite every cooldown.
    let mut last_apply: std::collections::HashMap<String, std::time::Instant> = persisted
        .last_apply
        .iter()
        .filter_map(|(k, &t)| epoch_to_instant(t, boot_secs).map(|i| (k.clone(), i)))
        .collect();
    // Last emitted delegation context per session — identical
    // provider_compact/skipped events are not re-logged every pass.
    let mut delegated_ctx = persisted.delegated_ctx;
    let mut discovery_cache = detect::DiscoveryCache::default();
    // A cold start (one-shot monitor probe, watcher restart) borrows the
    // snapshot the previous pass left on disk so unchanged session files
    // are not reparsed. Advisory only: any drift means a cold rescan.
    if let Some(dir) = state_path.parent() {
        load_persisted_discovery(&mut discovery_cache, dir, provider);
    }
    let mut pass_started_at_ms;
    let mut pass_completed_at_ms;
    let mut decisions;
    // Every native dispatch first checkpoints a conservative hold. A restart
    // during a provider call must not forget that its outcome is unknown.
    macro_rules! persist_watch_state {
        () => {{
            let save_secs = now_secs();
            save_watch_state(
                &state_path,
                &WatchState {
                    checkpoint_schema: 1,
                    artifact_sha256: artifact_sha256.to_owned(),
                    pass_started_at_ms,
                    pass_completed_at_ms,
                    interval_secs: interval,
                    active_only,
                    native_activation: native_operations::artifact_activation_status().to_owned(),
                    decisions: decisions.clone(),
                    generation: WATCH_STATE_GENERATION,
                    config_sha256: decision_config.clone(),
                    settled: settled.clone(),
                    settle_pass: settle_pass.clone(),
                    holddown: holddown.clone(),
                    legacy_unresolved: legacy_unresolved.clone(),
                    last_fire: last_fire
                        .iter()
                        .map(|(k, t)| (k.clone(), instant_to_epoch(*t, save_secs)))
                        .collect(),
                    last_apply: last_apply
                        .iter()
                        .map(|(k, t)| (k.clone(), instant_to_epoch(*t, save_secs)))
                        .collect(),
                    delegated_ctx: delegated_ctx.clone(),
                },
            )
            .map_err(|_| {
                anyhow::anyhow!("persist watch state failed; repair required before dispatch")
            })
        }};
    }
    // Resolved once: the claude binary path doesn't change mid-watch.
    let claude_bin = claude_bin();
    loop {
        if !dry_run {
            native_operations::artifact_sha256()?;
        }
        pass_started_at_ms = Some(now_millis());
        pass_completed_at_ms = None;
        decisions = WatchDecisions::default();
        let cfg = config::load()?;
        let event_context = &telemetry::EventContext::new(&cfg, false);
        let native_event_context = &telemetry::EventContext::new(&cfg, true);
        legacy_unresolved.extend(legacy_native_uncertainty()?);
        holddown.retain(|_, until| *until > now_secs());
        // An unchanged transcript can have a different decision after a
        // rollout/policy change. Config uses ordered maps; store only a hash
        // of its effective inputs, never command strings or private paths.
        let config_sha256 = {
            use sha2::{Digest, Sha256};
            let mut environment: Vec<_> = std::env::vars_os()
                .filter(|(name, _)| name.to_string_lossy().starts_with("GOBSTOPPER_"))
                .collect();
            environment.sort();
            format!(
                "{:x}",
                Sha256::digest(
                    format!(
                        "{}:{artifact_sha256}:{cfg:?}:{:?}:{:?}:{environment:?}",
                        env!("CARGO_PKG_VERSION"),
                        roots(cli),
                        cli.codex_bin,
                    )
                    .as_bytes()
                )
            )
        };
        if decision_config != config_sha256 {
            settled.clear();
            settle_pass.clear();
            delegated_ctx.clear();
            decision_config = config_sha256;
        }
        // Discovery may be slow. Publish a fresh in-progress receipt rather
        // than presenting the prior pass's completion as this pass's health.
        if !dry_run {
            persist_watch_state!()?;
        }
        let mut found: Vec<Discovered> = detect::discover_cached(
            &roots(cli),
            if active_only {
                gobstopper_core::SessionHandle::HOT_SECS
            } else {
                detect::default_max_age_secs()
            },
            &mut discovery_cache,
            provider,
            true,
        );
        // Cheapest sessions first: a multi-minute apply on one giant
        // session would otherwise delay every session behind it.
        found.sort_by_key(session_cost_hint);
        // --eval-budget bounds only the transcript work (context fallback
        // load, transcript load, plan evaluation). Discovery and the cheap
        // fingerprint/suppression gates above always run, so a deferred
        // session is counted, never mistaken for a clean decision.
        let eval_deadline = (eval_budget > 0)
            .then(|| std::time::Instant::now() + std::time::Duration::from_secs(eval_budget));
        decisions.discovered = found.len() as u64;
        for d in found {
            if provider.is_some_and(|p| p != d.handle.provider) {
                continue;
            }
            if active_only && !d.handle.is_active() {
                continue;
            }
            let session_key = format!(
                "{}:{}:{}",
                d.handle.provider.as_str(),
                d.handle.session_id,
                d.handle.path.display()
            );
            let Ok(resolved) = cfg.resolve(d.handle.provider, &d.handle.session_id, None, None)
            else {
                eprintln!("plan transcript failed: policy_resolution_failed");
                continue;
            };
            if legacy_unresolved.contains(&session_key) {
                decisions.legacy_unresolved += 1;
                eprintln!("legacy native outcome unresolved; automatic dispatch remains blocked");
                continue;
            }
            if !dry_run && resolved.auto_compact_closed {
                let homes = roots(cli);
                let home = match d.handle.provider {
                    Provider::Codex => &homes.codex_home,
                    Provider::ClaudeCode => &homes.claude_home,
                };
                match native_operations::Operation::pending(&d.handle, home) {
                    Ok(false) => {}
                    _ => {
                        decisions.native_unresolved += 1;
                        eprintln!("native dispatch blocked: existing operation needs reconciliation or repair");
                        continue;
                    }
                }
            }
            // Suppression: a session whose decision version matches its
            // last terminal decision can skip the load. This is not authority
            // to dispatch; unknown operations above always take precedence.
            // This check precedes the context fallback load so suppressed
            // sessions cost one metadata/SQL read, not a transcript parse.
            if settled.len() >= 4096 {
                settled.clear();
                settle_pass.clear();
            }
            let fp = session_fingerprint(&d);
            if fp.is_some() && settled.get(&session_key) == fp.as_ref() {
                decisions.settled += 1;
                continue;
            }
            // Cooldown from a terminal provider outcome — suppresses the
            // session even though its failed turn rewrote the file.
            if holddown
                .get(&session_key)
                .is_some_and(|&until| now_secs() < until)
            {
                decisions.cooldown += 1;
                continue;
            }
            holddown.remove(&session_key);
            // Apply hold-down: a session mutated in place within
            // `apply_hold_secs` stays out of the loop even if a provider
            // append moved its fingerprint or stale metrics still read
            // over trigger. Skips the transcript load too — during the
            // hold the pass costs two map lookups and one cheap
            // fingerprint read.
            if let Some(t) = last_apply.get(&session_key) {
                if t.elapsed().as_secs() < resolved.policy.apply_hold_secs {
                    decisions.cooldown += 1;
                    continue;
                }
            }
            let trigger = if resolved.policy.adaptive {
                gobstopper_core::adapt(
                    &resolved.policy,
                    &adaptive_sample_usage(&d.handle, &d.usage),
                )
                .policy
                .effective_trigger()
            } else {
                resolved.policy.effective_trigger()
            };
            if d.usage
                .reported_context()
                .is_some_and(|context| context < trigger)
            {
                decisions.below_trigger += 1;
                continue;
            }
            if let Some(t) = last_fire.get(&session_key) {
                if t.elapsed().as_secs() < resolved.policy.min_interval_secs {
                    decisions.cooldown += 1;
                    continue;
                }
            }
            // Policy already selects a native-only route for this idle
            // session. Artifact refusal needs neither transcript estimation nor
            // a recovery snapshot. Unresolved work was checked above and is
            // never replaced by this terminal, source-bound decision cache.
            if !dry_run && !d.handle.is_active() && resolved.auto_compact_closed {
                let event_context = native_event_context;
                if hooks::rollout_cohort(&cfg, d.handle.provider.as_str(), &d.handle.session_id)
                    == Some(false)
                {
                    let context = d.usage.reported_context();
                    let control = CompactionPlan {
                        strategy: "watch-native:control".into(),
                        rationale: "control cohort: native compaction suppressed".into(),
                        edits: vec![],
                        context_tokens_before: context.unwrap_or(0),
                        context_tokens_after: context.unwrap_or(0),
                    };
                    emit_event(
                        event_context,
                        &d,
                        &control,
                        "provider_compact",
                        "skipped",
                        trigger,
                        0,
                        context.is_none().then_some("unresolved_context"),
                    );
                    if let Some(fp) = &fp {
                        settled.insert(session_key.clone(), fp.clone());
                    }
                    continue;
                }
                let homes = roots(cli);
                let codex_binary = resolve_codex_bin(cli.codex_bin.as_deref());
                let (home, binary) = match d.handle.provider {
                    Provider::Codex => (&homes.codex_home, Some(codex_binary.as_path())),
                    Provider::ClaudeCode => (&homes.claude_home, claude_bin.as_deref()),
                };
                if let Err(error) =
                    native_operations::check_artifact_activation(&d.handle, home, binary)
                {
                    record_native_admission_failure(
                        event_context,
                        &d,
                        &resolved.strategy,
                        d.usage.context_tokens,
                        trigger,
                        &error,
                    );
                    if native_operations::activation_unqualified(&error) {
                        decisions.native_unqualified += 1;
                        if let Some(fp) = &fp {
                            settled.insert(session_key.clone(), fp.clone());
                        }
                    } else {
                        last_fire.insert(session_key.clone(), std::time::Instant::now());
                    }
                    continue;
                }
            }
            // Live fast path: `auto` unconditionally delegates active
            // sessions to the provider, and the delegation arm skips
            // without touching anything. Emit that outcome directly —
            // a churning live session otherwise re-parses its whole
            // transcript every pass because each provider append changes
            // the fingerprint. Custom commands and plugins still load,
            // and dry-run keeps the full path so its preview shows the
            // real evaluated plan.
            if !dry_run
                && d.handle.is_active()
                && resolved.strategy == "auto"
                && resolved.command.is_none()
                && resolved.plugin.is_none()
            {
                let context = d.usage.reported_context();
                let ctx = context.unwrap_or(0);
                let delegated = CompactionPlan {
                    strategy: resolved.strategy.clone(),
                    rationale: "auto (live session): delegated to provider".to_string(),
                    edits: vec![Edit::ProviderCompact {
                        control: match d.handle.provider {
                            Provider::Codex => "codex app-server: thread/compact/start",
                            Provider::ClaudeCode => "claude: /compact (or --autocompact at launch)",
                        }
                        .to_string(),
                    }],
                    context_tokens_before: ctx,
                    context_tokens_after: ctx,
                };
                last_fire.insert(session_key.clone(), std::time::Instant::now());
                // Known context counts deduplicate churning provider records.
                // Unknown context is source-version bound by `settled`, not
                // the legacy numeric zero: changed partial evidence can emit
                // another honest unknown decision without parsing a transcript.
                if context.is_none() || delegated_ctx.get(&session_key) != Some(&ctx) {
                    delegated_ctx.insert(session_key.clone(), ctx);
                    emit_event(
                        event_context,
                        &d,
                        &delegated,
                        "provider_compact",
                        "skipped",
                        trigger,
                        0,
                        context.is_none().then_some("unresolved_context"),
                    );
                    eprintln!(
                        "deferred native compaction: session owner required; source unchanged"
                    );
                }
                if let Some(fp) = &fp {
                    settled.insert(session_key.clone(), fp.clone());
                }
                continue;
            }
            // A session with no usable provider usage sample still needs a
            // context estimate for the trigger gate — but only then. A full
            // transcript parse just to answer "below
            // trigger?" is the expensive part of a pass; any measured hint
            // (reported context, preceding-token total, or a partial
            // component subtotal) answers it without loading. Bounded by
            // --eval-budget like every other transcript load below.
            let ctx = match d.usage.context_hint() {
                Some(context) => context,
                None => {
                    if eval_deadline.is_some_and(|d| std::time::Instant::now() >= d) {
                        decisions.eval_deferred += 1;
                        continue;
                    }
                    detect::load(&d)
                        .map(|t| t.estimated_context_tokens())
                        .unwrap_or(0)
                }
            };
            if ctx < trigger {
                decisions.below_trigger += 1;
                continue;
            }
            // Provider-native Codex compaction needs nothing from our
            // planner. Running
            // it before load+evaluate matters doubly here: a `None`
            // plan would otherwise settle an over-trigger session the
            // native lever can compact, and codex rollouts are large
            // enough that re-parsing one every pass is real cost.
            if !dry_run && d.handle.provider == Provider::Codex && resolved.auto_compact_closed {
                let event_context = native_event_context;
                if d.handle.is_active() || session_fingerprint(&d) != fp {
                    continue;
                }
                // Sub-agent threads cannot be resumed by the app-server
                // ("resume the parent first") — skip rather than log
                // provider_rejected every pass. Plain forks resume fine
                // and stay eligible.
                if gobstopper_adapters::codex::is_subagent_thread(&d.handle.path) {
                    let tagged = CompactionPlan {
                        strategy: "watch-apply:sub-agent".to_string(),
                        rationale: "auto (closed codex): sub-agent thread".to_string(),
                        edits: vec![],
                        context_tokens_before: ctx,
                        context_tokens_after: ctx,
                    };
                    emit_event(
                        event_context,
                        &d,
                        &tagged,
                        "provider_compact",
                        "skipped",
                        trigger,
                        0,
                        Some("parent_thread"),
                    );
                    if let Some(fp) = &fp {
                        settled.insert(session_key.clone(), fp.clone());
                    }
                    continue;
                }
                if hooks::rollout_cohort(&cfg, d.handle.provider.as_str(), &d.handle.session_id)
                    == Some(false)
                {
                    let tagged = CompactionPlan {
                        strategy: "watch-apply:control".to_string(),
                        rationale: "auto (closed codex): rollout control".to_string(),
                        edits: vec![],
                        context_tokens_before: ctx,
                        context_tokens_after: ctx,
                    };
                    emit_event(
                        event_context,
                        &d,
                        &tagged,
                        "provider_compact",
                        "skipped",
                        trigger,
                        0,
                        None,
                    );
                    if let Some(fp) = &fp {
                        settled.insert(session_key.clone(), fp.clone());
                    }
                    continue;
                }
                let pre_snapshot = match snapshot_before_edit(&d, "pre-compact") {
                    Ok(entry) => entry,
                    Err(_) => {
                        eprintln!("codex thread/compact skipped: pre-compact snapshot failed");
                        continue;
                    }
                };
                let started = std::time::Instant::now();
                last_fire.insert(session_key.clone(), started);
                holddown.insert(session_key.clone(), now_secs() + 4200);
                persist_watch_state!()?;
                let binary = resolve_codex_bin(cli.codex_bin.as_deref());
                let mut operation = match prepare_native_operation(
                    cli,
                    &d,
                    &binary,
                    &pre_snapshot,
                    &decision_config,
                ) {
                    Ok(operation) => operation,
                    Err(error) => {
                        holddown.remove(&session_key);
                        record_native_admission_failure(
                            event_context,
                            &d,
                            &resolved.strategy,
                            ctx,
                            trigger,
                            &error,
                        );
                        continue;
                    }
                };
                operation
                    .dispatch()
                    .map_err(|_| anyhow::anyhow!("native dispatch checkpoint failed"))?;
                let outcome = codex_compact(
                    operation.binary(),
                    &d.handle.session_id,
                    Some(&roots(cli).codex_home),
                    600_000,
                );
                holddown.remove(&session_key);
                match outcome {
                    Ok(terminal) => {
                        let done = CompactionPlan {
                            strategy: resolved.strategy.clone(),
                            rationale: "auto (closed codex): thread/compact".to_string(),
                            edits: vec![],
                            context_tokens_before: ctx,
                            context_tokens_after: ctx,
                        };
                        last_fire.insert(session_key.clone(), std::time::Instant::now());
                        let observed = record_native_completion(
                            event_context,
                            &d,
                            &done,
                            &pre_snapshot,
                            trigger,
                            started,
                        );
                        operation.finish(observed, Some(terminal)).map_err(|_| {
                            anyhow::anyhow!(
                                "native completion checkpoint failed; outcome unresolved"
                            )
                        })?;
                        if observed == Some(true) {
                            if let Some(nfp) = session_fingerprint(&d) {
                                settled.insert(session_key.clone(), nfp);
                            }
                            last_apply.insert(session_key.clone(), std::time::Instant::now());
                        } else {
                            // A no-op or unavailable post-state may become actionable
                            // later. A fingerprint settle would outlive this cooldown.
                            holddown.insert(session_key.clone(), now_secs() + 3600);
                        }
                    }
                    Err(e) => {
                        operation.finish(None, None).map_err(|_| {
                            anyhow::anyhow!(
                                "native completion checkpoint failed; outcome unresolved"
                            )
                        })?;
                        let failed = CompactionPlan {
                            strategy: resolved.strategy.clone(),
                            rationale: "auto (closed codex): thread/compact".to_string(),
                            edits: vec![],
                            context_tokens_before: ctx,
                            context_tokens_after: ctx,
                        };
                        // Distinguish infra spawn failures from provider
                        // rejections — a missing binary is not the
                        // provider refusing the compact.
                        let msg = e.to_string();
                        let mut ev = build_event(
                            event_context,
                            &d,
                            &failed,
                            "provider_compact",
                            "failed",
                            trigger,
                            started.elapsed().as_millis() as u64,
                            Some(if msg.contains("spawning") {
                                "spawn_failed"
                            } else if msg.contains("usage limit") {
                                "quota_limited"
                            } else {
                                "provider_rejected"
                            }),
                        );
                        ev.snapshot_before_sha256 = Some(pre_snapshot.sha256.clone());
                        if append_event(&default_log_path(), &ev).is_err() {
                            eprintln!("telemetry write failed (non-fatal): event_append_failed");
                        }
                        eprintln!("codex thread/compact failed: provider outcome unresolved");
                        // Terminal provider outcomes hold the session
                        // down on a cooldown keyed on session_id — the
                        // failed turn rewrote the rollout, so a
                        // fingerprint settle can never suppress the
                        // retry. Transient infra failures only
                        // rate-limit via last_fire; the next pass
                        // retries.
                        let hold_secs = codex_failure_hold_secs(&msg);
                        if hold_secs > 0 {
                            holddown.insert(session_key.clone(), now_secs() + hold_secs);
                        } else {
                            last_fire.insert(session_key.clone(), std::time::Instant::now());
                        }
                    }
                }
                continue;
            }
            // Native summarization does not require an acceptable file-elision
            // plan. Keep it ahead of load/evaluate, as for Codex.
            if !dry_run && d.handle.provider == Provider::ClaudeCode && resolved.auto_compact_closed
            {
                let event_context = native_event_context;
                if d.handle.is_active() {
                    continue;
                }
                let done = CompactionPlan {
                    strategy: resolved.strategy.clone(),
                    rationale: "auto (closed claude): /compact".to_string(),
                    edits: vec![],
                    context_tokens_before: ctx,
                    context_tokens_after: ctx,
                };
                if hooks::rollout_cohort(&cfg, d.handle.provider.as_str(), &d.handle.session_id)
                    == Some(false)
                {
                    let tagged = CompactionPlan {
                        strategy: "watch-apply:control".to_string(),
                        ..done
                    };
                    emit_event(
                        event_context,
                        &d,
                        &tagged,
                        "provider_compact",
                        "skipped",
                        trigger,
                        0,
                        None,
                    );
                    if let Some(fp) = &fp {
                        settled.insert(session_key.clone(), fp.clone());
                    }
                    continue;
                }
                if let Some(bin) = &claude_bin {
                    // Two separate observations must agree before a headless
                    // resume; recheck owner claims immediately before dispatch.
                    match &fp {
                        Some(fp) if settle_pass.get(&session_key) == Some(fp) => {
                            settle_pass.remove(&session_key);
                        }
                        Some(fp) => {
                            settle_pass.insert(session_key.clone(), fp.clone());
                            continue;
                        }
                        None => continue,
                    }
                    if gobstopper_adapters::claude::live_sessions(&roots(cli).claude_home)
                        .contains_key(&d.handle.session_id)
                        || session_fingerprint(&d) != fp
                    {
                        continue;
                    }
                    let pre_snapshot = match snapshot_before_edit(&d, "pre-compact") {
                        Ok(entry) => entry,
                        Err(_) => {
                            eprintln!(
                                "headless claude /compact skipped: pre-compact snapshot failed"
                            );
                            continue;
                        }
                    };
                    let started = std::time::Instant::now();
                    last_fire.insert(session_key.clone(), started);
                    holddown.insert(session_key.clone(), now_secs() + 3840);
                    persist_watch_state!()?;
                    let mut operation = match prepare_native_operation(
                        cli,
                        &d,
                        bin,
                        &pre_snapshot,
                        &decision_config,
                    ) {
                        Ok(operation) => operation,
                        Err(error) => {
                            holddown.remove(&session_key);
                            record_native_admission_failure(
                                event_context,
                                &d,
                                &resolved.strategy,
                                ctx,
                                trigger,
                                &error,
                            );
                            continue;
                        }
                    };
                    operation
                        .dispatch()
                        .map_err(|_| anyhow::anyhow!("native dispatch checkpoint failed"))?;
                    let outcome = gobstopper_adapters::claude::headless_compact_in_home(
                        operation.binary(),
                        &d.handle.session_id,
                        240,
                        Some(&roots(cli).claude_home),
                    );
                    holddown.remove(&session_key);
                    match outcome {
                        Ok(()) => {
                            let observed = record_native_completion(
                                event_context,
                                &d,
                                &done,
                                &pre_snapshot,
                                trigger,
                                started,
                            );
                            operation
                                .finish(
                                    observed,
                                    Some(native_operations::TerminalEvidence {
                                        session_id: d.handle.session_id.clone(),
                                        turn_id: None,
                                        item_id: None,
                                    }),
                                )
                                .map_err(|_| {
                                    anyhow::anyhow!(
                                        "native completion checkpoint failed; outcome unresolved"
                                    )
                                })?;
                            if observed == Some(true) {
                                if let Some(nfp) = session_fingerprint(&d) {
                                    settled.insert(session_key.clone(), nfp);
                                }
                                last_apply.insert(session_key.clone(), std::time::Instant::now());
                            } else {
                                holddown.insert(session_key.clone(), now_secs() + 3600);
                            }
                        }
                        Err(_error) => {
                            operation.finish(None, None).map_err(|_| {
                                anyhow::anyhow!(
                                    "native completion checkpoint failed; outcome unresolved"
                                )
                            })?;
                            let mut event = build_event(
                                event_context,
                                &d,
                                &done,
                                "provider_compact",
                                "failed",
                                trigger,
                                started.elapsed().as_millis() as u64,
                                Some("provider_rejected"),
                            );
                            event.snapshot_before_sha256 = Some(pre_snapshot.sha256);
                            if append_event(&default_log_path(), &event).is_err() {
                                eprintln!(
                                    "telemetry write failed (non-fatal): event_append_failed"
                                );
                            }
                            eprintln!("headless claude /compact outcome unresolved; automatic replay blocked");
                            // A failed/timeout process can already have changed the
                            // session. Reconcile on a later pass, never fall through
                            // into file surgery on this uncertain provider outcome.
                            holddown.insert(session_key.clone(), now_secs() + 3600);
                        }
                    }
                    continue;
                }
                eprintln!("native claude compaction unavailable: provider executable not found");
                last_fire.insert(session_key.clone(), std::time::Instant::now());
                continue;
            }
            if eval_deadline.is_some_and(|d| std::time::Instant::now() >= d) {
                decisions.eval_deferred += 1;
                continue;
            }
            let (transcript, source_sha256) = match copy::load_bound(d.handle.clone()) {
                Ok(t) => t,
                Err(_) => {
                    if let Some(fp) = fp {
                        settled.insert(session_key.clone(), fp);
                    }
                    eprintln!("load transcript failed: transcript_read_failed");
                    continue;
                }
            };
            match evaluate(&transcript, &resolved) {
                Ok(Some(plan)) => {
                    if dry_run {
                        eprintln!(
                            "[dry-run] {} {}: {}",
                            d.handle.provider.as_str(),
                            display_prefix(&d.handle.session_id, 12),
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
                        if delegated_ctx.get(&session_key) != Some(&plan.context_tokens_before) {
                            delegated_ctx.insert(session_key.clone(), plan.context_tokens_before);
                            emit_event(
                                event_context,
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
                        }
                        // Unchanged content will plan to delegation again;
                        // re-evaluate only after the provider writes.
                        if let Some(fp) = &fp {
                            settled.insert(session_key.clone(), fp.clone());
                        }
                        continue;
                    }
                    if d.handle.provider == Provider::ClaudeCode && resolved.auto_apply_inplace {
                        let blocked = CompactionPlan {
                            context_tokens_after: plan.context_tokens_before,
                            ..plan.clone()
                        };
                        emit_event(
                            event_context,
                            &d,
                            &blocked,
                            action,
                            "blocked",
                            trigger,
                            started.elapsed().as_millis() as u64,
                            Some("custody_unavailable"),
                        );
                        eprintln!(
                            "direct provider mutation disabled: lifetime custody unavailable"
                        );
                        if let Some(fp) = &fp {
                            settled.insert(session_key.clone(), fp.clone());
                        }
                        continue;
                    }
                    let r = copy::compact(&d.handle, &source_sha256, &plan, &vault::default_root());
                    match r {
                        Ok(_) => {
                            last_fire.insert(session_key.clone(), std::time::Instant::now());
                            emit_event(
                                event_context,
                                &d,
                                &plan,
                                action,
                                "planned",
                                trigger,
                                started.elapsed().as_millis() as u64,
                                None,
                            );
                            eprintln!("prepared compacted fork");
                            // The fork does not touch the source; until the
                            // source changes there is nothing new to prepare.
                            if let Some(fp) = &fp {
                                settled.insert(session_key.clone(), fp.clone());
                            }
                        }
                        Err(_) => {
                            emit_event(
                                event_context,
                                &d,
                                &plan,
                                action,
                                "failed",
                                trigger,
                                started.elapsed().as_millis() as u64,
                                Some("apply_failed"),
                            );
                            eprintln!("compact transcript failed: copy_prepare_failed");
                            if let Some(fp) = &fp {
                                settled.insert(session_key.clone(), fp.clone());
                            }
                        }
                    }
                }
                Ok(None) => {
                    if let Some(fp) = &fp {
                        settled.insert(session_key.clone(), fp.clone());
                    }
                }
                Err(_) => {
                    if let Some(fp) = &fp {
                        settled.insert(session_key.clone(), fp.clone());
                    }
                    eprintln!("plan transcript failed: plan_failed");
                }
            }
        }
        // Persist suppression/clocks each pass: a restart then resumes
        // from the same terminal decisions instead of re-planning all
        // sessions once. Small file, written atomically. Dry-run passes
        // (e.g. monitor's --once probes) observe only — never mutate
        // persisted state.
        if !dry_run {
            native_operations::artifact_sha256()?;
            pass_completed_at_ms = Some(now_millis());
            persist_watch_state!()?;
            // Next cold process skips reparsing fingerprint-unchanged
            // session files. Advisory snapshot; failure never fails a pass.
            if let Some(dir) = state_path.parent() {
                save_persisted_discovery(&discovery_cache, dir, provider);
            }
        } else if once && decisions.eval_deferred > 0 {
            // Shape differs from "[dry-run] provider session: plan" on
            // purpose: a deferred count is pass coverage, not a plan line.
            eprintln!(
                "[dry-run] eval budget: {} session(s) deferred unevaluated",
                decisions.eval_deferred
            );
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
            "jev: {} key configured — {}",
            source.describe(),
            match jev::health_check(&key, &endpoint) {
                jev::Health::Ok => "verified",
                jev::Health::Rejected => "rejected by API (401/403)",
                jev::Health::Unverified => "could not verify (network/API error)",
            }
        );
        return Ok(());
    }
    let key = if !std::io::stdin().is_terminal() {
        let buf = hooks::read_stdin_bounded()?.context(
            "authentication input must be valid UTF-8 with EOF within 64 KiB and five seconds",
        )?;
        buf.trim().to_string()
    } else if let Some(k) = secrets::clipboard_secret() {
        println!("found a plausible key on the clipboard");
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
                jev::Health::Ok => println!("typesafe key verified and stored in the OS keychain"),
                jev::Health::Unverified => println!(
                    "typesafe key stored in the OS keychain (could not verify: network/API error)"
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
    } else if session_active {
        "provider_compact"
    } else {
        "transcript_compact"
    };
    let control = match (over, session_active, provider_id) {
        (true, true, "codex") => Some("thread/compact/start"),
        (true, true, "claude_code") => Some("/compact or relaunch --autocompact"),
        _ => None,
    };
    Ok(serde_json::json!({
        "provider": provider_id,
        "action": action,
        "context_tokens": context_tokens,
        "session_active": session_active,
        "strategy": resolved.strategy,
        "trigger_tokens": resolved.policy.trigger_tokens,
        "effective_trigger_tokens": effective_trigger,
        "block_tokens": resolved.policy.block_tokens,
        "min_savings_tokens": resolved.policy.min_savings_tokens,
        "quota_pressure": resolved.policy.quota_pressure,
        "control": control,
    }))
}

#[allow(clippy::too_many_arguments)]
fn cmd_policy_check(
    cli: &Cli,
    cfg: &config::Config,
    provider: &str,
    context_tokens: Option<u64>,
    session: Option<&str>,
    session_active: bool,
    quota_pressure: Option<QuotaArg>,
    preset: Option<&str>,
    json: bool,
) -> Result<()> {
    // `--session` reads a discovered session's recorded usage so hook
    // scripts and wrappers don't have to measure context themselves.
    let (usage, session_active) = if let Some(session_id) = session {
        match provider {
            "claude" | "claude_code" => {
                let Some(d) = detect::find(&roots(cli), session_id)
                    .into_iter()
                    .find(|d| d.handle.provider == Provider::ClaudeCode)
                else {
                    bail!("no claude session '{session_id}'");
                };
                (d.usage, d.handle.is_active())
            }
            other => bail!("--session lookup is not supported for provider '{other}'"),
        }
    } else {
        let mut usage = gobstopper_core::UsageSample::default();
        usage.observe_cumulative_report(context_tokens, None, None, None);
        (usage, session_active)
    };
    let context = usage.reported_context();
    let mut decision = policy_decision(
        cfg,
        provider,
        context.unwrap_or(0),
        session_active,
        quota_pressure.map(Into::into),
        preset,
    )?;
    decision["context_state"] = serde_json::json!(usage.context_state);
    decision["reported_context_tokens"] = serde_json::json!(context);
    decision["context_components"] = serde_json::json!(usage.context_components);
    decision["context_reason"] = serde_json::json!(usage.context_reason);
    decision["measured_component_subtotal"] =
        serde_json::json!(usage.measured_component_subtotal());
    decision["component_subtotal_basis"] =
        serde_json::json!("known_numeric_components_not_complete_occupancy");
    decision["decision_available"] = serde_json::json!(context.is_some());
    decision["decision_reason"] =
        serde_json::json!(context.is_none().then_some("unresolved_context"));
    decision["usage_basis"] = serde_json::json!(if session.is_some() {
        "recorded_provider_accounting_not_billing"
    } else {
        "caller_supplied_context"
    });
    if context.is_none() {
        decision["action"] = serde_json::json!("none");
        decision["control"] = serde_json::Value::Null;
    }
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
        if let Some(reason) = decision["decision_reason"].as_str() {
            println!("decision_available=false reason={reason}");
        }
    }
    Ok(())
}

fn cmd_export(cli: &Cli, cfg: &config::Config, session: &str) -> Result<()> {
    let d = find_session(cli, cfg, session)?;
    use std::io::Write;
    let bytes = transaction_read(&d.handle.path)?;
    std::io::stdout().write_all(&bytes)?;
    Ok(())
}

fn transaction_read(path: &std::path::Path) -> Result<Vec<u8>> {
    gobstopper_adapters::transaction::read(path).map_err(|e| anyhow::anyhow!(e))
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
        Cmd::EvalStudy {
            session,
            manifest,
            prepare_manifest,
            rounds,
            trigger,
            floor,
            against,
            json,
        } => {
            let (handle, bytes) = eval_spec_bytes(&cli, &cfg, session)?;
            if let Some(path) = prepare_manifest {
                let count = gobstopper_adapters::study::prepare_manifest(handle, &bytes, path)?;
                println!(
                    "{}",
                    serde_json::json!({"checks":count,"label_source":"heuristic","study_run":false})
                );
                return Ok(());
            }
            let (manifest, digest) = gobstopper_adapters::study::read_manifest(
                manifest.as_deref().context("manifest is required")?,
            )?;
            let report = if let Some(after_spec) = against {
                let (after_handle, after_bytes) = eval_spec_bytes(&cli, &cfg, after_spec)?;
                anyhow::ensure!(
                    after_handle.provider == handle.provider,
                    "--against provider mismatch"
                );
                gobstopper_adapters::study::audit(handle, &bytes, manifest, digest, &after_bytes)?
            } else {
                let mut policy = cfg
                    .resolve(handle.provider, &handle.session_id, None, None)?
                    .policy;
                if let Some(value) = trigger {
                    policy.trigger_tokens = *value;
                }
                if let Some(value) = floor {
                    policy.floor_tokens = *value;
                }
                gobstopper_adapters::study::evaluate(
                    handle,
                    &bytes,
                    manifest,
                    digest,
                    &policy,
                    usize::from(*rounds),
                )?
            };
            if *json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "{}: {} (no provider calls; cost and continuation unmeasured)",
                    report.schema, report.replay_mode
                );
                for row in &report.rows {
                    println!("{} round {}: {} ({} applied), ~{} -> ~{} tokens; retention {}/{}; floor reached: {}", row.arm, row.round, row.status, row.applied_rounds, row.estimated_context_before, row.estimated_context_after, row.retention.same_origin_retained, row.retention.total, row.floor_reached);
                }
            }
            Ok(())
        }
        Cmd::CacheEdits {
            session,
            trigger,
            floor,
            plan,
        } => cmd_cache_edits(&cli, &cfg, session, *trigger, *floor, *plan),
        Cmd::InstallHooks { output } => cmd_install_hooks(false, &roots(&cli), output.as_deref()),
        Cmd::UninstallHooks { output } => cmd_install_hooks(true, &roots(&cli), output.as_deref()),
        Cmd::Hook { event } => cmd_hook(&cli, &cfg, event),
        Cmd::Report {
            strict,
            active_only,
            context_only,
        } => cmd_report(&cli, *strict, *active_only, *context_only),
        Cmd::Events {
            session,
            tail,
            cohort,
            retention,
            since,
            json,
        } => cmd_events(
            &cfg,
            session.as_deref(),
            *tail,
            *cohort,
            *retention,
            since.as_deref(),
            *json,
        ),
        Cmd::Vault {
            session,
            stats,
            json,
        } => cmd_vault(&cli, &cfg, session.as_deref(), *stats, *json),
        Cmd::Prune { keep, yes, json } => cmd_prune(*keep, *yes, *json),
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
        Cmd::Proxy { command } => proxy::run(command, &|session: &str| {
            let d = find_session(&cli, &cfg, session)?;
            Ok((d.handle.provider, d.handle.path.clone()))
        }),
        Cmd::NativeOperations => {
            let mut rows = native_operations::inspect()?;
            let mut legacy: Vec<_> = legacy_native_uncertainty()?.into_iter().collect();
            legacy.sort();
            rows.extend(legacy.into_iter().map(|target| {
                serde_json::json!({
                    "state": "legacy_unknown", "target_key": target,
                    "automatic_replay_blocked": true,
                    "reason": "legacy state did not preserve operation terminal identity",
                })
            }));
            println!("{}", serde_json::to_string_pretty(&rows)?);
            Ok(())
        }
        Cmd::NativeReconcile { operation_sha256 } => {
            native_operations::reconcile(operation_sha256)?;
            println!("native operation reconciled from its recorded matching terminal evidence");
            Ok(())
        }
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
            provider,
            once,
            eval_budget,
        } => cmd_watch(
            &cli,
            &cfg,
            *interval,
            *dry_run,
            *double_buffer,
            *active_only,
            provider.clone(),
            *once,
            *eval_budget,
        ),
        Cmd::PolicyCheck {
            provider,
            context_tokens,
            session,
            session_active,
            quota_pressure,
            preset,
            json,
        } => cmd_policy_check(
            &cli,
            &cfg,
            provider,
            *context_tokens,
            session.as_deref(),
            *session_active,
            *quota_pressure,
            preset.as_deref(),
            *json,
        ),
        Cmd::Export { session } => cmd_export(&cli, &cfg, session),
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
    #[test]
    fn benchmark_csv_quotes_record_delimiters() {
        assert_eq!(super::csv_field("ordinary"), "ordinary");
        assert_eq!(
            super::csv_field("a,b\r\n\"quoted\""),
            "\"a,b\r\n\"\"quoted\"\"\""
        );
        let row = super::bench_failure_row(
            gobstopper_core::Provider::Codex,
            "a,b",
            "session_evaluation_failed",
        );
        assert!(row.starts_with("codex,\"a,b\","));
        assert!(row.ends_with(",session_evaluation_failed\n"));
    }

    use super::*;

    #[cfg(unix)]
    #[test]
    fn session_json_preserves_partial_usage_and_unrepresentable_path_state() {
        use gobstopper_core::model::{ContextComponents, ContextReason, UsageSample};
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 0xff]));
        let mut discovered = detect::Discovered {
            handle: SessionHandle {
                provider: Provider::ClaudeCode,
                session_id: "synthetic".into(),
                path: path.clone(),
                cwd: Some(path),
                age_secs: 0,
            },
            usage: UsageSample::default(),
        };
        discovered.usage.observe_context_components(
            ContextComponents {
                input_tokens: Some(12),
                cache_read_tokens: Some(3),
                cache_creation_tokens: None,
                output_tokens: Some(2),
            },
            Some(ContextReason::NullComponent),
        );
        let row = session_row(&discovered);
        assert!(row["path"].is_null());
        assert!(row["cwd"].is_null());
        assert_eq!(row["path_encoding"], "non_utf8");
        assert_eq!(row["cwd_encoding"], "non_utf8");
        assert!(row["source_identity_sha256"].is_null());
        assert!(row["reported_context_tokens"].is_null());
        assert_eq!(row["context_state"], "unknown");
        assert_eq!(row["context_reason"], "null_component");
        assert_eq!(row["measured_component_subtotal"], 17);
        discovered.handle.path = PathBuf::from("/synthetic-missing-session");
        discovered.handle.cwd = None;
        let ordinary = session_row(&discovered);
        assert_eq!(ordinary["path"], "/synthetic-missing-session");
        assert_eq!(ordinary["path_encoding"], "utf8");
        assert!(ordinary["cwd"].is_null());
        assert!(ordinary["cwd_encoding"].is_null());
    }

    #[test]
    fn adaptive_history_is_store_bound_measured_and_deduplicated() {
        use gobstopper_core::model::ContextState;
        let dir = tempdir("adaptive-history");
        let handle = |name: &str| {
            let path = dir.join(name);
            fs::write(&path, b"synthetic source").unwrap();
            SessionHandle {
                provider: gobstopper_core::Provider::Codex,
                session_id: "same-session".into(),
                path: fs::canonicalize(path).unwrap(),
                cwd: None,
                age_secs: 0,
            }
        };
        let selected = handle("selected.jsonl");
        let foreign = handle("foreign.jsonl");
        let event = |handle: &SessionHandle, tokens: u64, after: u64, manifest: &str| {
            let observation = |tokens, snapshot: &str| {
                let bytes = format!(
                    "{}\n{}\n",
                    serde_json::json!({"type":"session_meta","payload":{"id":handle.session_id}}),
                    serde_json::json!({"type":"token_usage_record","payload":{
                        "usage":{"input_tokens":tokens,"output_tokens":0},
                        "thread_token_usage":{"input_tokens":tokens,"cached_input_tokens":0}}}),
                );
                // The production observation producer, rather than the helper
                // under test, establishes the canonical identity and accounting.
                eval::token_observation(handle, bytes.as_bytes(), Some(snapshot)).unwrap()
            };
            let before = observation(
                tokens,
                &copy::sha256(format!("before-{manifest}").as_bytes()),
            );
            let after = observation(after, &copy::sha256(format!("after-{manifest}").as_bytes()));
            let mut event = CompactionEvent::new(
                handle.provider,
                &handle.session_id,
                "auto",
                "provider_compact",
                "applied",
                250_000,
                500_000,
                0,
                0,
                1,
                None,
            );
            event.source_identity_sha256 = Some(before.source_identity_sha256.clone());
            event.snapshot_before_sha256 = before.snapshot_manifest_sha256.clone();
            event.snapshot_after_sha256 = after.snapshot_manifest_sha256.clone();
            event.before_observation = Some(before);
            event.after_observation = Some(after);
            event
        };
        let valid = event(&selected, 1000, 980, "a");
        assert_eq!(
            recent_applied_from(std::slice::from_ref(&valid), &selected),
            [(1000, 20)],
            "legacy context and reclaimed estimates must not enter tuning"
        );
        assert!(recent_applied_from(&[event(&foreign, 1000, 980, "a")], &selected).is_empty());
        for mode in 0..11 {
            let mut invalid = valid.clone();
            match mode {
                0 => invalid.source_identity_sha256 = None,
                1 => invalid.before_observation = None,
                2 => invalid.error_code = Some("unresolved_context".into()),
                3 => invalid.outcome = "planned".into(),
                4 => invalid.action = "none".into(),
                5 => invalid.provider = Provider::ClaudeCode,
                6 => invalid.session_id = "another-session".into(),
                7 => invalid.snapshot_after_sha256 = Some("e".repeat(64)),
                8 => {
                    let after = invalid.after_observation.as_mut().unwrap();
                    after.context_state = ContextState::Reset;
                    after.context_tokens = None;
                }
                9 => invalid.after_observation.as_mut().unwrap().context_tokens = Some(0),
                _ => invalid.after_observation.as_mut().unwrap().context_tokens = Some(1000),
            }
            assert!(
                recent_applied_from(&[invalid], &selected).is_empty(),
                "unqualified history mode {mode}"
            );
        }
        assert_eq!(
            recent_applied_from(&[valid.clone(), valid.clone()], &selected),
            [(1000, 20)],
            "one repeated low-yield observation must not become repeated history"
        );
        for conflict in [
            event(&selected, 1000, 975, "a"),
            event(&selected, 1000, 1000, "a"),
            {
                let mut changed = valid.clone();
                changed.after_observation.as_mut().unwrap().source_sha256 = "e".repeat(64);
                changed
            },
        ] {
            assert!(
                recent_applied_from(&[valid.clone(), conflict, valid.clone()], &selected,)
                    .is_empty()
            );
        }
        let history = [
            valid.clone(),
            event(&selected, 2000, 1960, "b"),
            event(&selected, 3000, 2940, "c"),
            event(&selected, 4000, 3920, "d"),
            valid.clone(),
        ];
        assert_eq!(
            recent_applied_from(&history, &selected),
            [(4000, 80), (3000, 60), (2000, 40)],
            "newest three distinct measurements; replay cannot refresh an old pair"
        );
        fs::remove_file(&selected.path).unwrap();
        assert!(recent_applied_from(&[valid], &selected).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn retention_rollup_requires_exact_evidence_and_rejects_conflicting_duplicates() {
        use gobstopper_core::events::TokenObservation;
        use gobstopper_core::model::{ContextState, LifetimeScope};
        let observation = |source: &str, manifest: &str| TokenObservation {
            source_sha256: source.repeat(64),
            source_identity_sha256: "a".repeat(64),
            snapshot_manifest_sha256: Some(manifest.repeat(64)),
            context_state: ContextState::Absent,
            context_tokens: None,
            estimated_context_tokens: 1,
            lifetime_scope: LifetimeScope::Absent,
            lifetime_input_tokens: None,
            lifetime_cached_tokens: None,
        };
        let mut event = CompactionEvent::new(
            Provider::Codex,
            "session",
            "elide",
            "transcript_compact",
            "applied",
            10,
            20,
            5,
            1,
            1,
            None,
        );
        event.source_identity_sha256 = Some("a".repeat(64));
        event.snapshot_before_sha256 = Some("b".repeat(64));
        event.snapshot_after_sha256 = Some("c".repeat(64));
        event.before_observation = Some(observation("d", "b"));
        event.after_observation = Some(observation("e", "c"));
        event.retention_total = Some(2);
        event.retention_retained = Some(1);
        event.retention_lexical = Some(2);
        // Byte-bound retention is independent of missing provider usage.
        assert_eq!(
            qualified_retention(&[event.clone(), event.clone()]).len(),
            1
        );
        let mut conflicting = event.clone();
        conflicting.retention_lexical = Some(0);
        assert!(qualified_retention(&[event.clone(), conflicting, event.clone()]).is_empty());
        for mode in 0..7 {
            let mut invalid = event.clone();
            match mode {
                0 => invalid.before_observation = None,
                1 => {
                    invalid
                        .after_observation
                        .as_mut()
                        .unwrap()
                        .source_identity_sha256 = "f".repeat(64)
                }
                2 => invalid.snapshot_before_sha256 = Some("f".repeat(64)),
                3 => invalid.error_code = Some("unresolved_context".into()),
                4 => invalid.outcome = "failed".into(),
                5 => invalid.retention_retained = Some(3),
                _ => invalid.action = "none".into(),
            }
            assert!(qualified_retention(&[invalid]).is_empty(), "mode {mode}");
        }
    }

    #[test]
    fn snapshot_prefix_selection_rejects_collisions_and_empty_selectors() {
        let entry = |sha: String| vault::VaultEntry {
            ts: 1,
            sha256: sha,
            path: PathBuf::from("/synthetic/source"),
            session_id: "synthetic".into(),
            provider: "codex".into(),
            bytes: 0,
            strategy: None,
            record_count: 0,
            source_sha256: "0".repeat(64),
        };
        let first = entry(format!("{}{}", "a".repeat(16), "0".repeat(48)));
        let second = entry(format!("{}{}", "a".repeat(16), "1".repeat(48)));
        let entries = [first.clone(), second, first.clone()];
        assert!(resolve_snapshot_sha(&entries, &"a".repeat(16)).is_err());
        assert!(resolve_snapshot_sha(&entries, "").is_err());
        assert!(resolve_snapshot_sha(&entries, "../outside").is_err());
        assert_eq!(
            resolve_snapshot_sha(&entries, &first.sha256.to_uppercase()).unwrap(),
            first.sha256
        );
    }
    use std::fs;

    #[test]
    fn display_prefix_preserves_utf8_and_existing_byte_budgets() {
        for limit in [12, 38] {
            for ch in ['é', '界', '😀'] {
                let crossing = format!("{}{ch}tail", "a".repeat(limit - 1));
                assert_eq!(display_prefix(&crossing, limit), "a".repeat(limit - 1));
                let boundary = format!("{}{ch}tail", "a".repeat(limit - ch.len_utf8()));
                let prefix = display_prefix(&boundary, limit);
                assert_eq!(prefix.len(), limit);
                assert!(prefix.ends_with(ch));
            }
        }
        assert_eq!(display_prefix("😀", 0), "");
        assert_eq!(display_prefix("😀", 3), "");
        assert_eq!(display_prefix("😀", 4), "😀");
    }

    #[test]
    fn display_prefix_preserves_ascii_and_short_values() {
        let ascii = "a".repeat(50);
        for limit in [0, 12, 38, 50, usize::MAX] {
            assert_eq!(
                display_prefix(&ascii, limit),
                &ascii[..limit.min(ascii.len())]
            );
        }
        assert_eq!(display_prefix("", 12), "");
        assert_eq!(display_prefix("short", 12), "short");
        assert_eq!(display_prefix("é界😀", 12), "é界😀");
    }

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
        codex_compact(&stub, "ok-thread", Some(&dir), 5_000).unwrap();
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
        let err = codex_compact(&stub, "fail-thread", Some(&dir), 5_000).unwrap_err();
        assert!(err.to_string().contains("failed"), "got: {err:#}");
        assert!(err.to_string().contains("usage limit"), "got: {err:#}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn codex_compact_propagates_request_errors() {
        let dir = tempdir("err");
        let stub = stub_codex();
        let err = codex_compact(&stub, "bad-resume", Some(&dir), 5_000).unwrap_err();
        assert!(err.to_string().contains("cannot resume"), "got: {err:#}");
        let err = codex_compact(&stub, "error-thread", Some(&dir), 5_000).unwrap_err();
        assert!(err.to_string().contains("thread not found"), "got: {err:#}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn codex_compact_observes_completion_before_dispatch_ack() {
        let dir = tempdir("early-notifications");
        codex_compact(&stub_codex(), "early-thread", Some(&dir), 1_000).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn codex_compact_requires_the_compaction_items_terminal_turn() {
        let dir = tempdir("unrelated-turn");
        codex_compact(
            &stub_codex(),
            "early-foreign-failure-thread",
            Some(&dir),
            1_000,
        )
        .unwrap();
        let error =
            codex_compact(&stub_codex(), "wrong-turn-thread", Some(&dir), 1_000).unwrap_err();
        assert!(error.to_string().contains("turn failed"), "{error:#}");
        for thread in [
            "no-item-thread",
            "uncorrelated-failure-thread",
            "no-terminal-open-thread",
        ] {
            fs::remove_file(dir.join("requests.log")).unwrap();
            let started = std::time::Instant::now();
            // Correlation failures close after their deliberately unmatched
            // notifications. The separate open fixture exercises the actual
            // terminal deadline, with the normal fixture startup allowance.
            let error = codex_compact(&stub_codex(), thread, Some(&dir), 1_000).unwrap_err();
            assert!(
                error.to_string().starts_with("provider outcome unknown"),
                "{thread}: {error:#}"
            );
            assert!(
                !error.to_string().contains("dispatch response"),
                "{thread}: {error:#}"
            );
            let log = fs::read_to_string(dir.join("requests.log")).unwrap();
            let requests: Vec<serde_json::Value> = log
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(requests.len(), 4, "{thread}");
            assert_eq!(requests[3]["method"], "thread/compact/start", "{thread}");
            assert_eq!(requests[3]["params"]["threadId"], thread);
            if thread == "no-terminal-open-thread" {
                assert!(started.elapsed() >= std::time::Duration::from_millis(1_000));
                assert!(started.elapsed() < std::time::Duration::from_secs(10));
            }
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn codex_compact_bounds_frames_and_treats_lost_ack_as_unknown() {
        let dir = tempdir("bounded-protocol");
        for thread in [
            "oversized-thread",
            "lost-response-thread",
            "malformed-response-thread",
        ] {
            let error = codex_compact(&stub_codex(), thread, Some(&dir), 1_000).unwrap_err();
            assert!(
                error.to_string().contains("outcome unknown"),
                "{thread}: {error:#}"
            );
            assert!(codex_failure_hold_secs(&error.to_string()) > 0);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn codex_compact_classifies_structural_failures_without_echoing_provider_text() {
        let dir = tempdir("private-provider-error");
        let error =
            codex_compact(&stub_codex(), "structural-thread", Some(&dir), 1_000).unwrap_err();
        assert_eq!(codex_failure_hold_secs(&error.to_string()), 24 * 3600);
        assert!(!error.to_string().contains("private-transcript-marker"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn codex_compact_collects_descendant_pipe_holders() {
        let dir = tempdir("child-custody");
        let started = std::time::Instant::now();
        codex_compact(&stub_codex(), "descendant-thread", Some(&dir), 1_000).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        let pid = fs::read_to_string(dir.join("descendant.pid")).unwrap();
        assert!(pid.trim().parse::<u32>().is_ok());
        // Success requires the reader to finish. The descendant holds stdout
        // open for 60s unless our owned process group was collected.
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn codex_compact_cancels_reader_when_descendant_escapes_process_group() {
        let dir = tempdir("escaped-pipe-holder");
        let started = std::time::Instant::now();
        let result = codex_compact(&stub_codex(), "escaped-pipe-thread", Some(&dir), 5_000);
        // Cooperatively stop this exact fixture child, even if the assertion
        // fails. No PID probing/signaling can race unrelated host processes.
        fs::write(dir.join("escaped-stop"), b"stop").unwrap();
        let finish = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !dir.join("escaped-finished").exists() && std::time::Instant::now() < finish {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            dir.join("escaped-finished").exists(),
            "fixture child did not finish"
        );
        result.unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        fs::remove_dir_all(dir).unwrap();
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
                context_state: gobstopper_core::model::ContextState::Reported,
                context_components: None,
                context_reason: None,
                lifetime_scope: gobstopper_core::model::LifetimeScope::Full,
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
            auto_apply_inplace: false,
            auto_compact_closed: false,
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

    #[test]
    fn session_fingerprint_tracks_len_and_mtime() {
        let dir = tempdir("fp");
        let path = dir.join("t.jsonl");
        fs::write(&path, "line1\n").unwrap();
        let d = Discovered {
            handle: gobstopper_core::SessionHandle {
                provider: Provider::ClaudeCode,
                session_id: "s".into(),
                path: path.clone(),
                cwd: None,
                age_secs: 10,
            },
            usage: gobstopper_core::UsageSample::default(),
        };
        let fp1 = session_fingerprint(&d).unwrap();
        // Unchanged file: identical fingerprint.
        assert_eq!(fp1, session_fingerprint(&d).unwrap());
        // Append changes length.
        fs::write(&path, "line1\nline2\n").unwrap();
        let fp2 = session_fingerprint(&d).unwrap();
        assert_ne!(fp1, fp2);
        // Same length but a fresh write moves mtime.
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        filetime_set(&path, later);
        let fp3 = session_fingerprint(&d).unwrap();
        assert_ne!(fp2, fp3);
        // Missing file → None (never suppresses).
        fs::remove_file(&path).unwrap();
        assert!(session_fingerprint(&d).is_none());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn watch_state_roundtrips_fingerprints_and_drops_stale_clocks() {
        let dir = tempdir("watch-state");
        let path = dir.join("state.json");
        let now = now_secs();
        let mut state = WatchState::default();
        state.settled.insert("k".to_string(), "fp".to_string());
        state.settle_pass.insert("k".to_string(), "fp".to_string());
        state.last_fire.insert("fresh".to_string(), now - 60);
        state.last_apply.insert("stale".to_string(), now - 200_000); // > 1 day old
        state.holddown.insert("k".to_string(), now + 3600);
        state.delegated_ctx.insert("k".to_string(), 300_000);
        save_watch_state(&path, &state).unwrap();
        let loaded = load_watch_state(&path).unwrap();
        assert_eq!(loaded.settled.get("k").map(String::as_str), Some("fp"));
        assert_eq!(loaded.settle_pass.get("k").map(String::as_str), Some("fp"));
        assert_eq!(loaded.holddown.get("k"), Some(&(now + 3600)));
        assert_eq!(loaded.delegated_ctx.get("k"), Some(&300_000));
        assert_eq!(loaded.last_fire.get("fresh"), Some(&(now - 60)));
        // The Instant conversion is exercised in cmd_watch; here verify
        // the stale-entry policy boundary.
        assert!(epoch_to_instant(now - 60, now).is_some());
        assert!(epoch_to_instant(now - 200_000, now).is_none());
        assert!(epoch_to_instant(0, now).is_none());
        assert!(epoch_to_instant(now + 60, now).is_none());
        // Damaged state requires repair; an absent state has no prior operations.
        fs::write(&path, "{not json").unwrap();
        assert!(load_watch_state(&path).is_err());
        fs::remove_file(&path).unwrap();
        assert!(load_watch_state(&path).unwrap().settled.is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn watch_state_oversized_save_preserves_prior_uncertainty() {
        let dir = tempdir("watch-state-size");
        let path = dir.join("state.json");
        let mut state = WatchState {
            generation: WATCH_STATE_GENERATION,
            ..WatchState::default()
        };
        let overhead = serde_json::to_vec(&state).unwrap().len();
        state
            .legacy_unresolved
            .insert("u".repeat(MAX_WATCH_STATE_BYTES as usize - overhead - 32));
        save_watch_state(&path, &state).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(before.len() as u64 <= MAX_WATCH_STATE_BYTES);
        assert_eq!(
            load_watch_state(&path).unwrap().legacy_unresolved,
            state.legacy_unresolved
        );
        state.artifact_sha256 = "a".repeat(64);
        assert!(save_watch_state(&path, &state).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(
            load_watch_state(&path).unwrap().legacy_unresolved,
            state.legacy_unresolved
        );
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_since_accepts_suffixed_and_bare_durations() {
        assert_eq!(parse_since("3600").unwrap(), 3_600);
        assert_eq!(parse_since("30m").unwrap(), 1_800);
        assert_eq!(parse_since("6h").unwrap(), 21_600);
        assert_eq!(parse_since("2d").unwrap(), 172_800);
        assert!(parse_since("").is_err());
        assert!(parse_since("h").is_err());
        assert!(parse_since("1x").is_err());
    }

    #[test]
    fn codex_failure_classes_map_to_cooldowns() {
        // Terminal provider outcomes → session-keyed cooldown.
        assert_eq!(
            codex_failure_hold_secs("codex compaction turn failed: usage limit exceeded"),
            4 * 3600
        );
        assert_eq!(
            codex_failure_hold_secs("cannot resume an unloaded multi-agent v2 sub-agent"),
            24 * 3600
        );
        assert_eq!(codex_failure_hold_secs("thread not found"), 24 * 3600);
        // Structural > generic: the remote compact task's model being
        // unprovisioned must outrank the "turn failed" wrapper text.
        assert_eq!(
            codex_failure_hold_secs(
                "codex compaction turn failed: Error running remote compact task: \
                 {\"error\":{\"message\":\"The 'gpt-5-mini' model is not supported\"}}"
            ),
            24 * 3600
        );
        assert_eq!(
            codex_failure_hold_secs(
                "provider outcome unknown at deadline; do not replay automatically"
            ),
            3600
        );
        assert_eq!(
            codex_failure_hold_secs("codex compaction turn failed"),
            3600
        );
        assert_eq!(
            codex_failure_hold_secs("codex compaction turn interrupted"),
            3600
        );
        assert_eq!(
            codex_failure_hold_secs("codex compaction turn aborted"),
            3600
        );
        assert_eq!(
            codex_failure_hold_secs("codex compaction turn unknown"),
            3600
        );
        // Transient infra before dispatch → rate-limit, retry next pass.
        assert_eq!(
            codex_failure_hold_secs("spawning `codex app-server --listen stdio://`"),
            0
        );
        assert_eq!(
            codex_failure_hold_secs("codex app-server closed its stream"),
            0
        );
        assert_eq!(
            codex_failure_hold_secs("provider outcome unknown awaiting dispatch response"),
            3600
        );
    }

    #[test]
    fn codex_terminal_order_duplicates_and_ambiguity_are_explicit() {
        for thread in ["terminal-first-thread", "duplicate-terminal-thread"] {
            let dir = tempdir("codex-terminal-order");
            let terminal = codex_compact(&stub_codex(), thread, Some(&dir), 1_000).unwrap();
            assert_eq!(terminal.session_id, thread);
            assert_eq!(terminal.turn_id.as_deref(), Some("compact-turn"));
            assert_eq!(terminal.item_id.as_deref(), Some("compact-item"));
            fs::remove_dir_all(&dir).unwrap();
        }
        for thread in [
            "conflicting-terminal-thread",
            "duplicate-key-thread",
            "control-id-thread",
        ] {
            let dir = tempdir("codex-terminal-ambiguity");
            assert!(codex_compact(&stub_codex(), thread, Some(&dir), 1_000).is_err());
            fs::remove_dir_all(&dir).unwrap();
        }
    }

    /// Touch a file's mtime without changing its length.
    fn filetime_set(path: &std::path::Path, t: std::time::SystemTime) {
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(t).unwrap();
    }
}
