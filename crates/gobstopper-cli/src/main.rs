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
    codex, copy, devin, eval, fork, plugins, recovery, study, vault, verify, AdapterError,
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
    /// Devin data dir containing sessions.db (default: $DEVIN_DATA_DIR or
    /// $XDG_DATA_HOME/devin/cli or ~/.local/share/devin/cli).
    #[arg(long, global = true)]
    devin_home: Option<PathBuf>,
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
        /// "precompact" | "session-start" | "prompt-policy[:devin|:claude]"
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
        /// Restrict watch to one provider (e.g. `devin`); default watches all.
        #[arg(long)]
        provider: Option<String>,
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
        /// Context occupancy. Optional when `--session` resolves it from
        /// the provider's own store (currently devin only).
        #[arg(long)]
        context_tokens: Option<u64>,
        /// Devin session id: read context_tokens and lock state straight
        /// from sessions.db instead of trusting caller-reported flags.
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
    /// Write a session's canonical transcript form to stdout. For Devin
    /// this is the sessions.db row set serialized as export JSONL — the
    /// same bytes `plan`/`verify`/`eval` consume and snapshots preserve.
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
    if let Some(p) = &cli.devin_home {
        r.devin_home = p.clone();
    }
    r
}

fn find_session(cli: &Cli, cfg: &config::Config, query: &str) -> Result<Discovered> {
    let path = PathBuf::from(query);
    if path.is_file() {
        // A SQLite store is never a transcript: resolve sessions inside it
        // by id instead of treating the file as provider data.
        if std::fs::File::open(&path)
            .and_then(|mut f| {
                use std::io::Read;
                let mut magic = [0u8; 16];
                f.read_exact(&mut magic).map(|_| magic)
            })
            .is_ok_and(|m| m == *b"SQLite format 3\0")
        {
            bail!(
                "{} is a SQLite store; pass a devin session id (e.g. `gobstopper plan <id>`), not the file",
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
            Provider::Devin => gobstopper_adapters::devin::scan_meta_export(&path),
        };
        let usage = match provider {
            Provider::Codex => gobstopper_adapters::codex::scan_usage(&path),
            Provider::ClaudeCode => gobstopper_adapters::claude::scan_usage(&path),
            Provider::Devin => gobstopper_adapters::devin::scan_usage_export(&path),
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
/// `vault:<sha256>` for a snapshot object (Devin store snapshots are
/// materialized and exported to the transcript dialect first).
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
        // Devin vault objects are the session's canonical export already;
        // only a hypothetical raw store image needs materializing first.
        let bytes = if entry.provider == Provider::Devin && bytes.starts_with(b"SQLite format 3") {
            let temp = std::env::temp_dir().join(format!("gobstopper-audit-{sha}.db"));
            std::fs::write(&temp, &bytes).with_context(|| format!("write {}", temp.display()))?;
            let exported = devin::export_bytes(&temp, &entry.session_id);
            let _ = std::fs::remove_file(&temp);
            exported?
        } else {
            bytes
        };
        return Ok((
            SessionHandle {
                provider: entry.provider,
                session_id: entry.session_id.clone(),
                path: entry.path.clone(),
                cwd: None,
                age_secs: u64::MAX,
            },
            bytes,
        ));
    }
    let d = find_session(cli, cfg, spec)?;
    let bytes = if d.handle.provider == Provider::Devin && devin::is_store_path(&d.handle.path) {
        devin::export_bytes(&d.handle.path, &d.handle.session_id)?
    } else {
        gobstopper_adapters::transaction::read(&d.handle.path)?
    };
    Ok((d.handle, bytes))
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

/// Build a numeric compaction telemetry record (v1 schema). Callers that
/// attach optional evidence fields (snapshot refs, realized retention)
/// build first, mutate, then append themselves.
fn build_event(
    d: &Discovered,
    plan: &CompactionPlan,
    action: &str,
    outcome: &str,
    trigger_tokens: u64,
    duration_ms: u64,
    error_code: Option<&str>,
) -> CompactionEvent {
    CompactionEvent::new(
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
    )
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
    let ev = build_event(
        d,
        plan,
        action,
        outcome,
        trigger_tokens,
        duration_ms,
        error_code,
    );
    if let Err(e) = append_event(&default_log_path(), &ev) {
        eprintln!("telemetry write failed (non-fatal): {e}");
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

fn apply_edits(d: &Discovered, plan: &CompactionPlan) -> Result<u64> {
    match d.handle.provider {
        Provider::Codex => gobstopper_adapters::codex::apply(&d.handle.path, &plan.edits)
            .map_err(|e| anyhow::anyhow!(e)),
        Provider::ClaudeCode => gobstopper_adapters::claude::apply(&d.handle.path, &plan.edits)
            .map_err(|e| anyhow::anyhow!(e)),
        Provider::Devin => gobstopper_adapters::devin::apply(&d.handle.path, &plan.edits)
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
        Provider::Codex => {
            codex_compact(codex_bin, &d.handle.session_id, Some(codex_home), 600_000)
        }
        Provider::ClaudeCode => bail!(
            "claude sessions compact via /compact in-session or --autocompact at launch; \
             gobstopper cannot inject into a running TUI"
        ),
        Provider::Devin => bail!(
            "devin sessions compact via /compact in-session; \
             gobstopper cannot inject into a running Devin CLI"
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
    outcome_ms: u64,
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
    let mut stderr = child.stderr.take().unwrap();
    // Draining must outlive the child: once the reader stops, a full
    // stderr/stdout pipe blocks the app-server mid-write — observed
    // live as repeated init timeouts. Keep a bounded tail so protocol
    // failures can report why the server died.
    let stderr_tail = std::sync::Arc::new(std::sync::Mutex::new(
        std::collections::VecDeque::<u8>::with_capacity(16 * 1024),
    ));
    {
        let tail = stderr_tail.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 4096];
            loop {
                match stderr.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut q = tail.lock().unwrap();
                        for &b in &buf[..n] {
                            if q.len() >= 16 * 1024 {
                                q.pop_front();
                            }
                            q.push_back(b);
                        }
                    }
                }
            }
        });
    }
    let (tx, rx) = mpsc::sync_channel::<serde_json::Value>(64);
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
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
                            "cannot resume provider thread".to_string()
                        } else if msg.contains("usage limit") {
                            "usage limit exceeded".to_string()
                        } else {
                            // Keep the provider's message — downstream
                            // failure classification keys on it.
                            format!("provider rejected: {msg}")
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
        // Cold start under load (compile jobs, a full watch scan) pushed
        // a real deployment past a 15s budget twice in a row.
        let boot = deadline(60_000);
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
        // Compaction accepted. The provider-side turn is owned by this
        // app-server process — exiting early orphans it mid-write — so the
        // caller picks the window (large threads need minutes, not 90s).
        let end = deadline(outcome_ms);
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
    // Close stdin first so the app-server exits on EOF and releases
    // any provider-side writer lock on the thread; SIGKILL is only a
    // fallback — a killed server can leave the lock held and every
    // later thread/resume then stalls behind it (observed live).
    drop(stdin);
    let exit_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            _ if Instant::now() >= exit_deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            _ => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    if outcome.is_err() {
        let tail: Vec<u8> = stderr_tail.lock().unwrap().iter().copied().collect();
        if !tail.is_empty() {
            let start = tail.len().saturating_sub(2048);
            eprintln!(
                "codex app-server stderr tail: {}",
                String::from_utf8_lossy(&tail[start..]).trim_end()
            );
        }
    }
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
            d.usage.context_tokens,
            d.usage.lifetime_input_tokens,
            d.handle.path.display(),
        );
    }
    Ok(())
}

fn cmd_verify(cli: &Cli, cfg: &config::Config, session: &str, json: bool) -> Result<()> {
    let d = find_session(cli, cfg, session)?;
    // Devin verification runs on the session's canonical export, not the
    // shared database file (which is not a readable transcript).
    let findings = if d.handle.provider == Provider::Devin
        && gobstopper_adapters::devin::is_store_path(&d.handle.path)
    {
        let bytes = gobstopper_adapters::devin::export_bytes(&d.handle.path, &d.handle.session_id)
            .map_err(|e| anyhow::anyhow!(e))?;
        verify::verify(d.handle.provider, &bytes)
    } else {
        verify::verify_path(d.handle.provider, &d.handle.path)
            .with_context(|| format!("reading {}", d.handle.path.display()))?
    };
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
    // Devin snapshots share the store path; the session id disambiguates.
    let canonical = d
        .handle
        .path
        .canonicalize()
        .unwrap_or_else(|_| d.handle.path.clone());
    let matches_session = |e: &vault::VaultEntry| {
        (e.path == d.handle.path || e.path == canonical)
            && (d.handle.provider != Provider::Devin || e.session_id == d.handle.session_id)
    };
    let entry = match sha {
        Some(prefix) => vault::list(&root)?
            .into_iter()
            .filter(|e| matches_session(e))
            .find(|e| e.sha256.starts_with(prefix))
            .ok_or_else(|| {
                anyhow::anyhow!("no vault snapshot matching '{prefix}' for this session")
            })?,
        None => vault::list(&root)?
            .into_iter()
            .find(|e| {
                matches_session(e)
                    && !matches!(e.strategy.as_deref(), Some("post-compact" | "pre-undo"))
            })
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
    if d.handle.provider == Provider::Devin {
        // In-place restore: the snapshot object is the session's canonical
        // export; the store gets its payloads and chain head back, and any
        // digest nodes we injected are removed.
        let current = devin::export_bytes(&d.handle.path, &d.handle.session_id)
            .map_err(|e| anyhow::anyhow!(e))?;
        vault::snapshot_data(
            &current,
            &d.handle.path,
            d.handle.provider,
            &d.handle.session_id,
            Some("pre-undo"),
            &root,
        )?;
        let snapshot_bytes = vault::read_object(&entry.sha256, &root)?;
        let report = devin::restore_store(
            &roots(cli).devin_home,
            &d.handle.session_id,
            &snapshot_bytes,
        )
        .map_err(|e| anyhow::anyhow!(e))?;
        println!(
            "restored {} payloads in place; resume: {}",
            report.nodes_rewritten, report.resume_hint
        );
        return Ok(());
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
            display_prefix(&e.session_id, 12),
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

/// Devin's hook config lives in the *config* dir, not the data dir:
/// `$DEVIN_CONFIG_DIR/hooks.v1.json`, else `$XDG_CONFIG_HOME/devin/
/// hooks.v1.json`, else `~/.config/devin/hooks.v1.json`.
fn devin_config_home() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    std::env::var_os("DEVIN_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(|p| PathBuf::from(p).join("devin")))
        .unwrap_or_else(|| home.join(".config").join("devin"))
}

fn cmd_install_hooks(uninstall: bool, roots: &Roots) -> Result<()> {
    let claude_settings = roots.claude_home.join("settings.json");
    let claude_targets = [
        hooks::HookTarget::ClaudePreCompact,
        hooks::HookTarget::ClaudeSessionStart,
        hooks::HookTarget::ClaudeUserPromptSubmit,
    ];
    let report = if uninstall {
        hooks::uninstall(&claude_settings, Some("hooks"))?
    } else {
        hooks::install(&claude_settings, &claude_targets, Some("hooks"))?
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
    // Devin's user-level hooks live in config.json under a "hooks" key —
    // the same nested shape as Claude's settings.json. (Flat
    // hooks.v1.json is only a *project*-level file: .devin/hooks.v1.json.)
    let devin_config = devin_config_home().join("config.json");
    let devin_targets = [
        hooks::HookTarget::DevinUserPromptSubmit,
        hooks::HookTarget::DevinPostCompaction,
    ];
    let report = if uninstall {
        hooks::uninstall(&devin_config, Some("hooks"))?
    } else {
        hooks::install(&devin_config, &devin_targets, Some("hooks"))?
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
            hooks::uninstall(&codex_hooks, Some("hooks"))?
        } else {
            hooks::install(&codex_hooks, &codex_targets, Some("hooks"))?
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

fn cmd_hook(cli: &Cli, cfg: &config::Config, event: &str) -> Result<()> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    if let Some(out) = hooks::handle(event, &buf, &roots(cli), cfg)? {
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
    let mut events = gobstopper_core::events::read_events(&path).unwrap_or_default();
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
            println!("\n{provider} (rollout {rollout})");
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
            for cohort_name in ["treatment", "control", "ungated"] {
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

/// `events --retention`: per-event realized retention plus a
/// per-provider rollup. Read-only over the local event log.
fn print_retention(events: &[CompactionEvent], tail: usize, json: bool) -> Result<()> {
    let measured: Vec<&CompactionEvent> = events
        .iter()
        .filter(|e| e.retention_total.is_some())
        .collect();
    let mut by_provider: std::collections::BTreeMap<&str, [u64; 4]> =
        std::collections::BTreeMap::new();
    for e in &measured {
        let agg = by_provider.entry(e.provider.as_str()).or_default();
        agg[0] += 1;
        agg[1] += e.retention_retained.unwrap_or(0);
        agg[2] += e.retention_lexical.unwrap_or(0);
        agg[3] += e.retention_total.unwrap_or(0);
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
        let flag = if total > 0 && lexical * 2 < total {
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
    let file_edits: Vec<Edit> = plan
        .edits
        .iter()
        .filter(|e| !matches!(e, Edit::ProviderCompact { .. } | Edit::CacheEdit { .. }))
        .cloned()
        .collect();
    if d.handle.is_active() {
        // Devin has no fork artifact: a live session must use /compact.
        if d.handle.provider == Provider::Devin && !file_edits.is_empty() {
            bail!(
                "devin session is live (provider holds its lock); compact with /compact in-session"
            );
        }
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
    } else if d.handle.provider == Provider::Devin && !file_edits.is_empty() {
        // In-place store path: snapshot the canonical export, then one
        // guarded SQLite transaction rewrites payloads. A detached fork
        // file is not a resumable Devin artifact, so this provider gets
        // no copy path at all.
        let file_result: anyhow::Result<u64> = copy::compact_devin_store(
            &d.handle,
            &source_sha256,
            &plan,
            &vault::default_root(),
            &roots(cli).devin_home,
        )
        .map(|receipt| {
            println!(
                "rewrote {} payloads in place{}",
                receipt.nodes_rewritten,
                receipt
                    .digest_node_id
                    .map(|id| format!("; digest node {id}"))
                    .unwrap_or_default()
            );
            if let Some(sha) = &receipt.snapshot_manifest_sha256 {
                println!("recovery snapshot: {sha}");
            }
            println!("resume the session: {}", receipt.resume_hint);
            receipt.reclaimed_bytes
        });
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
                println!("reclaimed ~{reclaimed} bytes in the session store");
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
                // `codex_compact` returns after the provider-side turn
                // completes, so the fork's post state is final — vault it
                // and score realized retention on the event.
                let post_sha = vault::snapshot(
                    &d.handle.path,
                    d.handle.provider,
                    &d.handle.session_id,
                    Some("post-compact"),
                    &vault::default_root(),
                )
                .map(|e| e.sha256)
                .ok();
                let mut ev = build_event(
                    &d,
                    &plan,
                    "provider_compact",
                    "applied",
                    trigger,
                    started.elapsed().as_millis() as u64,
                    None,
                );
                ev.snapshot_before_sha256 = Some(snapshot.sha256.clone());
                ev.snapshot_after_sha256 = post_sha.clone();
                if let Some(post) = &post_sha {
                    if let Some((total, retained, lexical)) =
                        realized_retention(&d.handle, &snapshot.sha256, post)
                    {
                        ev.retention_total = Some(total);
                        ev.retention_retained = Some(retained);
                        ev.retention_lexical = Some(lexical);
                    }
                }
                if let Err(e) = append_event(&default_log_path(), &ev) {
                    eprintln!("telemetry write failed (non-fatal): {e}");
                }
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

/// Cheap per-session decision version for watch suppression caching:
/// file length+mtime for JSONL providers, chain head+node count for
/// Devin's shared store, plus the live/idle bit the planner keys on. An
/// unchanged fingerprint means the plan outcome is deterministic-repeat:
/// no provider append, no Gobstopper write, no live→idle transition.
fn session_fingerprint(d: &Discovered) -> Option<String> {
    let content = if d.handle.provider == Provider::Devin {
        devin::chain_fingerprint(&d.handle.path, &d.handle.session_id)?
    } else {
        let m = std::fs::metadata(&d.handle.path).ok()?;
        let mtime = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{}:{mtime}", m.len())
    };
    Some(format!("{content}:{}", d.handle.is_active()))
}

/// Cheap per-pass cost estimate for ordering watch work: Devin sessions
/// cost their chain length (the export walk is the expensive part),
/// file providers cost their transcript bytes. Unknown sizes sort last
/// so a giant session cannot starve every small session behind it in a
/// serial pass.
fn session_cost_hint(d: &Discovered) -> u64 {
    match d.handle.provider {
        Provider::Devin => devin::chain_fingerprint(&d.handle.path, &d.handle.session_id)
            .and_then(|fp| fp.split(':').nth(1)?.parse().ok())
            .unwrap_or(u64::MAX),
        _ => std::fs::metadata(&d.handle.path)
            .map(|m| m.len())
            .unwrap_or(u64::MAX),
    }
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

/// Per-daemon persisted watch state: terminal-decision fingerprints,
/// the Claude settle arm, rate-limit clocks, and the last-emitted
/// delegation context all survive a daemon restart, so relaunching does
/// not re-plan every session once (the cold-pass burst). Clocks persist
/// as epoch seconds and reload relative to `now`; entries older than a
/// day are dropped rather than trusted across unknown downtime.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct WatchState {
    /// Decision-vocabulary version. `settled` records *why* a session was
    /// suppressed only implicitly — entries written under an older lever
    /// set (e.g. before Devin ACP compact existed) would suppress the new
    /// path forever. Bump on any change that adds or alters a terminal
    /// decision; on load, stale-generation suppressions are dropped.
    #[serde(default)]
    generation: u32,
    #[serde(default)]
    settled: std::collections::HashMap<String, String>,
    #[serde(default)]
    settle_pass: std::collections::HashMap<String, String>,
    /// Terminal provider outcomes suppress a session for a cooldown
    /// window, keyed on session — not fingerprint. A failed provider
    /// turn still appends to the rollout (codex `task_started`/
    /// `task_complete`), and the devin store is shared across sessions,
    /// so a fingerprint-keyed settle can never hold for these.
    #[serde(default)]
    holddown: std::collections::HashMap<String, u64>,
    #[serde(default)]
    last_fire: std::collections::HashMap<String, u64>,
    #[serde(default)]
    last_apply: std::collections::HashMap<String, u64>,
    #[serde(default)]
    delegated_ctx: std::collections::HashMap<String, u64>,
}

/// Current decision vocabulary. v1: initial watch state. v2: Devin ACP
/// provider-native compact added — pre-v2 suppressions may encode "no
/// lever existed" verdicts that are no longer true. v3: ACP requests
/// serialize (pipelined prompts raced session/load into "not found"),
/// so earlier provider_rejected verdicts are stale too. v4: acp_compact
/// waits for the async `_cognition.ai/compaction` terminal status — v3
/// recorded "applied" on the prompt ack alone, before compaction ran.
/// v5: acp_timeout_secs (default 1800) — session/load timeouts recorded
/// under the 600s budget may succeed now. v6: codex closed-session
/// `thread/compact` added — pre-v6 codex suppressions may encode "no
/// closed-session lever" verdicts, and v6 codex failures distinguish
/// permanent rejections (settle) from transient infra errors (retry).
/// v7: terminal provider failures move to a session-keyed cooldown —
/// a failed turn still mutates the rollout, so fingerprint settles
/// never held and produced a retry storm.
const WATCH_STATE_GENERATION: u32 = 7;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
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

/// Locate the `devin` executable for ACP `session/load` + `/compact`.
/// Same minimal-PATH problem as `claude_bin` under LaunchAgents.
fn devin_bin() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.join("devin")).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        candidates.push(home.join(".local/bin/devin"));
        candidates.push(home.join(".local/share/devin/cli/_versions/current/bin/devin"));
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin/devin"));
    candidates.push(PathBuf::from("/usr/local/bin/devin"));
    candidates.into_iter().find(|c| c.is_file())
}

fn load_watch_state(path: &Path) -> WatchState {
    let Ok(text) = std::fs::read_to_string(path) else {
        return WatchState::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Atomic write (tmp + rename) so a SIGKILL mid-save cannot leave a torn
/// state file that wipes the suppression map on next load.
fn save_watch_state(path: &Path, state: &WatchState) {
    let Ok(text) = serde_json::to_string(state) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
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
) -> Result<()> {
    if interval == 0 {
        bail!("watch interval must be positive");
    }
    let provider = provider
        .as_deref()
        .map(|p| match p {
            "codex" => Ok(Provider::Codex),
            "claude" | "claude_code" => Ok(Provider::ClaudeCode),
            "devin" => Ok(Provider::Devin),
            other => bail!("unknown provider '{other}'"),
        })
        .transpose()?;
    if double_buffer {
        bail!("in-place double-buffer swapping is retired; use copy-only watch without --double-buffer");
    }
    // Persisted watch state (watch-state-<provider>.json beside the
    // telemetry log): fingerprints, settle arms, and rate-limit clocks
    // survive restarts so a relaunch does not re-plan every session.
    let state_path = watch_state_path(provider);
    let persisted = load_watch_state(&state_path);
    let boot_secs = now_secs();
    let mut last_fire: std::collections::HashMap<String, std::time::Instant> = persisted
        .last_fire
        .iter()
        .filter_map(|(k, &t)| epoch_to_instant(t, boot_secs).map(|i| (k.clone(), i)))
        .collect();
    // Terminal-decision suppression: session_key -> fingerprint recorded
    // when a session was applied, failed, or judged unplannable. Skips
    // the expensive load until the provider actually appends — Devin
    // store metrics go stale post-apply, so context alone re-triggers
    // every pass otherwise.
    // Stale-generation suppressions encode verdicts from an older lever
    // set (e.g. "unplannable" recorded before the Devin ACP compact path
    // existed). Drop them once; sessions re-enter and re-decide under the
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
    let mut holddown: std::collections::HashMap<String, u64> =
        if persisted.generation >= WATCH_STATE_GENERATION {
            persisted.holddown
        } else {
            std::collections::HashMap::new()
        };
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
    let mut staged: std::collections::HashMap<String, Staged> = std::collections::HashMap::new();
    let mut discovery_cache = detect::DiscoveryCache::default();
    // Resolved once: the claude binary path doesn't change mid-watch.
    let claude_bin = claude_bin();
    let devin_bin = devin_bin();
    loop {
        let cfg = config::load()?;
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
        for d in found {
            if provider.is_some_and(|p| p != d.handle.provider) {
                continue;
            }
            if active_only && !d.handle.is_active() {
                continue;
            }
            // Devin sessions share one store path, so the key must carry
            // the session id, not just the file.
            let session_key = format!(
                "{}:{}:{}",
                d.handle.provider.as_str(),
                d.handle.session_id,
                d.handle.path.display()
            );
            let Ok(resolved) = cfg.resolve(d.handle.provider, &d.handle.session_id, None, None)
            else {
                continue;
            };
            // Suppression: a session whose content fingerprint matches its
            // last terminal decision is byte-identical — skip the load.
            // This check precedes the context fallback load so suppressed
            // sessions cost one metadata/SQL read, not a transcript parse.
            if settled.len() >= 4096 {
                settled.clear();
                settle_pass.clear();
            }
            let fp = session_fingerprint(&d);
            if fp.is_some() && settled.get(&session_key) == fp.as_ref() {
                continue;
            }
            // Cooldown from a terminal provider outcome — suppresses the
            // session even though its failed turn rewrote the file.
            if holddown
                .get(&session_key)
                .is_some_and(|&until| now_secs() < until)
            {
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
                    continue;
                }
            }
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
                    // A codex session headed for provider-native
                    // thread/compact gains nothing from a staged fork.
                    && !(d.handle.provider == Provider::Codex
                        && resolved.auto_compact_closed)
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
            // A codex session with provider-native compact enabled skips
            // the swap and reaches the thread/compact arm below instead.
            let codex_native = d.handle.provider == Provider::Codex && resolved.auto_compact_closed;
            if let Some(s) = staged.remove(&d.handle.session_id) {
                let unchanged = sha256_file(&d.handle.path)
                    .map(|h| h == s.source_sha256)
                    .unwrap_or(false);
                if !dry_run && unchanged && !codex_native {
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
                            eprintln!(
                                "prepared compacted fork for {}",
                                d.handle.cwd.as_deref().unwrap_or(&d.handle.path).display()
                            );
                            continue;
                        }
                        Err(e) => eprintln!(
                            "staged swap {} failed: {e}",
                            d.handle.cwd.as_deref().unwrap_or(&d.handle.path).display()
                        ),
                    }
                }
                let _ = std::fs::remove_file(&s.path); // stale or dry-run
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
                let delegated = CompactionPlan {
                    strategy: resolved.strategy.clone(),
                    rationale: "auto (live session): delegated to provider".to_string(),
                    edits: vec![Edit::ProviderCompact {
                        control: match d.handle.provider {
                            Provider::Codex => "codex app-server: thread/compact/start",
                            Provider::ClaudeCode => "claude: /compact (or --autocompact at launch)",
                            Provider::Devin => "devin: /compact",
                        }
                        .to_string(),
                    }],
                    context_tokens_before: ctx,
                    context_tokens_after: ctx,
                };
                last_fire.insert(session_key.clone(), std::time::Instant::now());
                // A churning live session re-plans to delegation every
                // pass; log only when the context actually moved so the
                // event stream is deltas, not a per-pass heartbeat.
                if delegated_ctx.get(&session_key) != Some(&ctx) {
                    delegated_ctx.insert(session_key.clone(), ctx);
                    emit_event(
                        &d,
                        &delegated,
                        "provider_compact",
                        "skipped",
                        trigger,
                        0,
                        None,
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
            // Provider-native Devin compaction needs nothing from our
            // planner — the provider summarizes the session itself. For
            // an idle treatment session with `auto_compact_closed`, try
            // `devin acp` `session/load` + `/compact` before paying the
            // export+evaluate cost; on failure the normal store-apply
            // path below is the fallback. Live sessions defer above.
            if !dry_run
                && d.handle.provider == Provider::Devin
                && resolved.auto_compact_closed
                && !d.handle.is_active()
                && hooks::rollout_cohort(&cfg, d.handle.provider.as_str(), &d.handle.session_id)
                    != Some(false)
            {
                if let Some(bin) = &devin_bin {
                    // Provider-delegated compaction still mutates the
                    // session — preserve the exact before-state first, same
                    // guarantee as the transcript-apply path. A failed
                    // snapshot aborts this pass rather than compacting with
                    // no recovery point.
                    let pre_snapshot = match snapshot_before_edit(&d, "pre-compact") {
                        Ok(entry) => entry,
                        Err(e) => {
                            eprintln!(
                                "acp /compact for {} skipped: pre-compact snapshot failed ({e})",
                                d.handle.session_id
                            );
                            continue;
                        }
                    };
                    let started = std::time::Instant::now();
                    let cwd = d
                        .handle
                        .cwd
                        .clone()
                        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
                        .unwrap_or_else(|| PathBuf::from("/"));
                    match gobstopper_adapters::devin::acp_compact(
                        bin,
                        &d.handle.session_id,
                        &cwd,
                        resolved.acp_timeout_secs,
                    ) {
                        Ok(()) => {
                            // The provider's store write can lag the
                            // `completed` status — caring-suit read an
                            // unchanged context right after completion and
                            // the summary chain landed moments later. Poll
                            // briefly before declaring a provider no-op.
                            let mut after = 0u64;
                            for _ in 0..6 {
                                after = gobstopper_adapters::devin::session_observation(
                                    &roots(cli).devin_home,
                                    &d.handle.session_id,
                                )
                                .map(|(usage, _)| usage.context_tokens)
                                .unwrap_or(0);
                                if after > 0 && after < ctx {
                                    break;
                                }
                                std::thread::sleep(std::time::Duration::from_secs(15));
                            }
                            let done = CompactionPlan {
                                strategy: resolved.strategy.clone(),
                                rationale: "auto (closed devin): acp /compact".to_string(),
                                edits: vec![],
                                context_tokens_before: ctx,
                                context_tokens_after: if after > 0 { after } else { ctx },
                            };
                            last_fire.insert(session_key.clone(), std::time::Instant::now());
                            // The provider reports `completed` even when its
                            // compactor found nothing to do — context then
                            // reads unchanged. Record that as a verified
                            // no-op, not an apply, so reclaimed-token stats
                            // and `provider-compacted` log lines stay honest.
                            let noop = after > 0 && after >= ctx;
                            // Land the after-state in the vault so every
                            // delegated compaction produces a scorable
                            // before/after pair, then attach realized
                            // retention to the event. Best-effort: a missing
                            // post snapshot or failed score never fails the
                            // compaction that already applied.
                            let post_sha = if noop {
                                None
                            } else {
                                vault::snapshot(
                                    &d.handle.path,
                                    Provider::Devin,
                                    &d.handle.session_id,
                                    Some("post-compact"),
                                    &vault::default_root(),
                                )
                                .map(|e| e.sha256)
                                .ok()
                            };
                            let mut ev = build_event(
                                &d,
                                &done,
                                "provider_compact",
                                if noop { "skipped" } else { "applied" },
                                trigger,
                                started.elapsed().as_millis() as u64,
                                if noop {
                                    Some("provider_noop")
                                } else if after == 0 {
                                    Some("unresolved_context")
                                } else {
                                    None
                                },
                            );
                            ev.snapshot_before_sha256 = Some(pre_snapshot.sha256.clone());
                            ev.snapshot_after_sha256 = post_sha.clone();
                            if let Some(post) = &post_sha {
                                if let Some((total, retained, lexical)) =
                                    realized_retention(&d.handle, &pre_snapshot.sha256, post)
                                {
                                    ev.retention_total = Some(total);
                                    ev.retention_retained = Some(retained);
                                    ev.retention_lexical = Some(lexical);
                                }
                            }
                            if let Err(e) = append_event(&default_log_path(), &ev) {
                                eprintln!("telemetry write failed (non-fatal): {e}");
                            }
                            if noop {
                                eprintln!(
                                    "acp /compact for {} completed but context is unchanged (provider no-op)",
                                    d.handle.session_id
                                );
                            } else {
                                eprintln!(
                                    "provider-compacted closed devin session {} via acp /compact",
                                    d.handle.session_id
                                );
                                if last_apply.len() >= 4096 {
                                    last_apply.clear();
                                }
                                last_apply.insert(session_key.clone(), started);
                            }
                            if let Some(nfp) = session_fingerprint(&d) {
                                settled.insert(session_key.clone(), nfp);
                            }
                            continue;
                        }
                        Err(e) => {
                            let failed = CompactionPlan {
                                strategy: resolved.strategy.clone(),
                                rationale: "auto (closed devin): acp /compact".to_string(),
                                edits: vec![],
                                context_tokens_before: ctx,
                                context_tokens_after: ctx,
                            };
                            let mut ev = build_event(
                                &d,
                                &failed,
                                "provider_compact",
                                "failed",
                                trigger,
                                started.elapsed().as_millis() as u64,
                                Some("provider_rejected"),
                            );
                            ev.snapshot_before_sha256 = Some(pre_snapshot.sha256.clone());
                            if let Err(e) = append_event(&default_log_path(), &ev) {
                                eprintln!("telemetry write failed (non-fatal): {e}");
                            }
                            // A compaction the provider started but never
                            // confirmed is still potentially writing —
                            // store elision underneath it could race its
                            // chain update. Only an outright rejection
                            // falls back.
                            let in_flight = e.to_string().contains("acp_compaction_in_flight");
                            eprintln!(
                                "acp devin /compact for {} failed ({e}){}",
                                d.handle.session_id,
                                if in_flight {
                                    "; leaving session untouched (compaction may still be running)"
                                } else if resolved.auto_apply_store {
                                    "; falling back to store elision"
                                } else {
                                    ""
                                },
                            );
                            if in_flight || !resolved.auto_apply_store {
                                // The store is shared across sessions —
                                // its fingerprint always moves, so a
                                // settle cannot suppress this. Hold the
                                // session down for an hour instead.
                                if holddown.len() >= 4096 {
                                    holddown.clear();
                                }
                                holddown.insert(session_key.clone(), now_secs() + 3600);
                                continue;
                            }
                        }
                    }
                }
            }
            // Provider-native Codex compaction also needs nothing from
            // our planner — same shape as the Devin arm above. Running
            // it before load+evaluate matters doubly here: a `None`
            // plan would otherwise settle an over-trigger session the
            // native lever can compact, and codex rollouts are large
            // enough that re-parsing one every pass is real cost.
            if !dry_run && d.handle.provider == Provider::Codex && resolved.auto_compact_closed {
                if d.handle.is_active() {
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
                    emit_event(&d, &tagged, "provider_compact", "skipped", trigger, 0, None);
                    if let Some(fp) = &fp {
                        settled.insert(session_key.clone(), fp.clone());
                    }
                    continue;
                }
                let pre_snapshot = match snapshot_before_edit(&d, "pre-compact") {
                    Ok(entry) => entry,
                    Err(e) => {
                        eprintln!(
                            "codex thread/compact for {} skipped: pre-compact snapshot failed ({e})",
                            d.handle.session_id
                        );
                        continue;
                    }
                };
                let started = std::time::Instant::now();
                match codex_compact(
                    &resolve_codex_bin(cli.codex_bin.as_deref()),
                    &d.handle.session_id,
                    Some(&roots(cli).codex_home),
                    600_000,
                ) {
                    Ok(()) => {
                        // Post-state read: a `compacted` record resets
                        // scan_usage to 0 — that IS the provider's
                        // post-state, not an unreadable context. A turn
                        // that completed without dropping context was a
                        // provider no-op, not an apply.
                        let after =
                            gobstopper_adapters::codex::scan_usage(&d.handle.path).context_tokens;
                        let noop = after >= ctx;
                        let done = CompactionPlan {
                            strategy: resolved.strategy.clone(),
                            rationale: "auto (closed codex): thread/compact".to_string(),
                            edits: vec![],
                            context_tokens_before: ctx,
                            context_tokens_after: after,
                        };
                        let post_sha = if noop {
                            None
                        } else {
                            vault::snapshot(
                                &d.handle.path,
                                d.handle.provider,
                                &d.handle.session_id,
                                Some("post-compact"),
                                &vault::default_root(),
                            )
                            .map(|e| e.sha256)
                            .ok()
                        };
                        let mut ev = build_event(
                            &d,
                            &done,
                            "provider_compact",
                            if noop { "skipped" } else { "applied" },
                            trigger,
                            started.elapsed().as_millis() as u64,
                            if noop { Some("provider_noop") } else { None },
                        );
                        ev.snapshot_before_sha256 = Some(pre_snapshot.sha256.clone());
                        ev.snapshot_after_sha256 = post_sha.clone();
                        if let Some(post) = &post_sha {
                            if let Some((total, retained, lexical)) =
                                realized_retention(&d.handle, &pre_snapshot.sha256, post)
                            {
                                ev.retention_total = Some(total);
                                ev.retention_retained = Some(retained);
                                ev.retention_lexical = Some(lexical);
                            }
                        }
                        if let Err(e) = append_event(&default_log_path(), &ev) {
                            eprintln!("telemetry write failed (non-fatal): {e}");
                        }
                        eprintln!(
                            "provider-compacted closed codex session {} via thread/compact",
                            d.handle.session_id
                        );
                        if let Some(nfp) = session_fingerprint(&d) {
                            settled.insert(session_key.clone(), nfp);
                        }
                        if last_apply.len() >= 4096 {
                            last_apply.clear();
                        }
                        last_apply.insert(session_key.clone(), started);
                    }
                    Err(e) => {
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
                        if let Err(e2) = append_event(&default_log_path(), &ev) {
                            eprintln!("telemetry write failed (non-fatal): {e2}");
                        }
                        eprintln!(
                            "codex thread/compact for {} failed: {e}",
                            d.handle.session_id
                        );
                        // Terminal provider outcomes hold the session
                        // down on a cooldown keyed on session_id — the
                        // failed turn rewrote the rollout, so a
                        // fingerprint settle can never suppress the
                        // retry. Transient infra failures only
                        // rate-limit via last_fire; the next pass
                        // retries.
                        let hold_secs = codex_failure_hold_secs(&msg);
                        if hold_secs > 0 {
                            if holddown.len() >= 4096 {
                                holddown.clear();
                            }
                            holddown.insert(session_key.clone(), now_secs() + hold_secs);
                        } else {
                            last_fire.insert(session_key.clone(), std::time::Instant::now());
                        }
                    }
                }
                continue;
            }
            let (transcript, source_sha256) = match copy::load_bound(d.handle.clone()) {
                Ok(t) => t,
                Err(e) => {
                    if let Some(fp) = fp {
                        settled.insert(session_key.clone(), fp);
                    }
                    eprintln!(
                        "load {} failed: {e}",
                        d.handle.cwd.as_deref().unwrap_or(&d.handle.path).display()
                    );
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
                    if d.handle.provider == Provider::Devin {
                        // Devin idle-session levers: provider-native
                        // `devin acp` /compact (auto_compact_closed) and/or
                        // the guarded in-place store write
                        // (auto_apply_store).
                        if !resolved.auto_apply_store && !resolved.auto_compact_closed {
                            continue;
                        }
                        if d.handle.is_active() {
                            eprintln!(
                                "devin session in {} is live; /compact in-session",
                                d.handle.cwd.as_deref().unwrap_or(Path::new("?")).display()
                            );
                            continue;
                        }
                        // Rollout gate: control-cohort sessions log one
                        // decision per content version and skip, so cohort
                        // comparison attributes savings to automation.
                        if hooks::rollout_cohort(
                            &cfg,
                            d.handle.provider.as_str(),
                            &d.handle.session_id,
                        ) == Some(false)
                        {
                            let mut tagged = plan.clone();
                            tagged.strategy = "watch-apply:control".to_string();
                            emit_event(
                                &d,
                                &tagged,
                                action,
                                "skipped",
                                trigger,
                                started.elapsed().as_millis() as u64,
                                None,
                            );
                            if let Some(fp) = &fp {
                                settled.insert(session_key.clone(), fp.clone());
                            }
                            continue;
                        }
                        if !resolved.auto_apply_store {
                            continue;
                        }
                        let r = copy::compact_devin_store(
                            &d.handle,
                            &source_sha256,
                            &plan,
                            &vault::default_root(),
                            &roots(cli).devin_home,
                        );
                        match r {
                            Ok(receipt) => {
                                let before_sha = receipt.snapshot_manifest_sha256.clone();
                                let post_sha = vault::snapshot(
                                    &d.handle.path,
                                    d.handle.provider,
                                    &d.handle.session_id,
                                    Some("post-compact"),
                                    &vault::default_root(),
                                )
                                .map(|e| e.sha256)
                                .ok();
                                let mut ev = build_event(
                                    &d,
                                    &plan,
                                    action,
                                    "applied",
                                    trigger,
                                    started.elapsed().as_millis() as u64,
                                    None,
                                );
                                ev.snapshot_before_sha256 = before_sha.clone();
                                ev.snapshot_after_sha256 = post_sha.clone();
                                if let (Some(before), Some(post)) = (&before_sha, &post_sha) {
                                    if let Some((total, retained, lexical)) =
                                        realized_retention(&d.handle, before, post)
                                    {
                                        ev.retention_total = Some(total);
                                        ev.retention_retained = Some(retained);
                                        ev.retention_lexical = Some(lexical);
                                    }
                                }
                                if let Err(e) = append_event(&default_log_path(), &ev) {
                                    eprintln!("telemetry write failed (non-fatal): {e}");
                                }
                                eprintln!(
                                    "compacted devin session in {} in place (~{} bytes reclaimed; snapshot {})",
                                    d.handle.cwd.as_deref().unwrap_or(Path::new("?")).display(),
                                    receipt.reclaimed_bytes,
                                    receipt.snapshot_manifest_sha256.as_deref().unwrap_or("?"),
                                );
                                // Post-write fingerprint: our own write
                                // moved the chain, so store the new value
                                // and stay suppressed until the provider
                                // appends again.
                                if let Some(nfp) = session_fingerprint(&d) {
                                    settled.insert(session_key.clone(), nfp);
                                }
                                if last_apply.len() >= 4096 {
                                    last_apply.clear();
                                }
                                last_apply.insert(session_key.clone(), started);
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
                                eprintln!(
                                    "devin store compact in {} failed: {e}",
                                    d.handle.cwd.as_deref().unwrap_or(Path::new("?")).display()
                                );
                                if let Some(fp) = &fp {
                                    settled.insert(session_key.clone(), fp.clone());
                                }
                            }
                        }
                        continue;
                    }
                    if d.handle.provider == Provider::ClaudeCode
                        && (resolved.auto_apply_inplace || resolved.auto_compact_closed)
                    {
                        // Closed-session handling for Claude: provider-native
                        // headless compaction and/or in-place JSONL rewrite.
                        // For in-place rewrite the provider opens the
                        // transcript per write (no persistent handle), so
                        // the rename swap cannot orphan appends, and
                        // transaction::apply rechecks bytes before
                        // replacing. Snapshot first.
                        if d.handle.is_active() {
                            continue;
                        }
                        // Rollout gate: same cohort contract as Devin —
                        // control sessions log and skip, one decision per
                        // content version.
                        if hooks::rollout_cohort(
                            &cfg,
                            d.handle.provider.as_str(),
                            &d.handle.session_id,
                        ) == Some(false)
                        {
                            let mut tagged = plan.clone();
                            tagged.strategy = "watch-apply:control".to_string();
                            emit_event(
                                &d,
                                &tagged,
                                action,
                                "skipped",
                                trigger,
                                started.elapsed().as_millis() as u64,
                                None,
                            );
                            if let Some(fp) = &fp {
                                settled.insert(session_key.clone(), fp.clone());
                            }
                            continue;
                        }
                        // Two-pass settle: mtime alone leaks sessions whose
                        // provider appended between discovery and apply
                        // (ChangedDuringWrite would abort anyway, but the
                        // churn is wasted). Require an unchanged
                        // fingerprint across consecutive passes.
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
                        // Provider-native compaction for closed sessions:
                        // no live pid owns this transcript (is_active
                        // above is authoritative via ~/.claude/sessions),
                        // and the fingerprint held steady across passes.
                        // `claude --resume -p /compact` runs the
                        // provider's own summarization — far deeper than
                        // deterministic elision — and fires the
                        // PreCompact hook that snapshots into the vault.
                        if resolved.auto_compact_closed {
                            if let Some(bin) = &claude_bin {
                                // Same invariant as every other mutating
                                // path: preserve the before-state before the
                                // provider rewrites it; a failed snapshot
                                // aborts this pass.
                                let pre_snapshot = match snapshot_before_edit(&d, "pre-compact") {
                                    Ok(entry) => entry,
                                    Err(e) => {
                                        eprintln!(
                                            "headless claude /compact for {} skipped: pre-compact snapshot failed ({e})",
                                            d.handle.session_id
                                        );
                                        continue;
                                    }
                                };
                                match gobstopper_adapters::claude::headless_compact(
                                    bin,
                                    &d.handle.session_id,
                                    240,
                                ) {
                                    Ok(()) => {
                                        let after =
                                            gobstopper_adapters::claude::scan_usage(&d.handle.path)
                                                .context_tokens;
                                        let mut done = plan.clone();
                                        // The provider compacted — the
                                        // plan's elision edits were not
                                        // applied, so report items = 0.
                                        done.edits.clear();
                                        // Post-compact tail may carry no
                                        // usage yet (boundary + summary);
                                        // undercount reclaimed rather
                                        // than claim before→0.
                                        done.context_tokens_after = if after > 0 {
                                            after
                                        } else {
                                            done.context_tokens_before
                                        };
                                        let post_sha = vault::snapshot(
                                            &d.handle.path,
                                            d.handle.provider,
                                            &d.handle.session_id,
                                            Some("post-compact"),
                                            &vault::default_root(),
                                        )
                                        .map(|e| e.sha256)
                                        .ok();
                                        let mut ev = build_event(
                                            &d,
                                            &done,
                                            "provider_compact",
                                            "applied",
                                            trigger,
                                            started.elapsed().as_millis() as u64,
                                            if after == 0 {
                                                Some("unresolved_context")
                                            } else {
                                                None
                                            },
                                        );
                                        ev.snapshot_before_sha256 =
                                            Some(pre_snapshot.sha256.clone());
                                        ev.snapshot_after_sha256 = post_sha.clone();
                                        if let Some(post) = &post_sha {
                                            if let Some((total, retained, lexical)) =
                                                realized_retention(
                                                    &d.handle,
                                                    &pre_snapshot.sha256,
                                                    post,
                                                )
                                            {
                                                ev.retention_total = Some(total);
                                                ev.retention_retained = Some(retained);
                                                ev.retention_lexical = Some(lexical);
                                            }
                                        }
                                        if let Err(e) = append_event(&default_log_path(), &ev) {
                                            eprintln!("telemetry write failed (non-fatal): {e}");
                                        }
                                        eprintln!(
                                            "provider-compacted closed claude session {} via /compact",
                                            d.handle.path.display()
                                        );
                                        if let Some(nfp) = session_fingerprint(&d) {
                                            settled.insert(session_key.clone(), nfp);
                                        }
                                        if last_apply.len() >= 4096 {
                                            last_apply.clear();
                                        }
                                        last_apply.insert(session_key.clone(), started);
                                        continue;
                                    }
                                    Err(e) => {
                                        eprintln!(
                                            "headless claude /compact for {} failed ({e}){}",
                                            d.handle.path.display(),
                                            if resolved.auto_apply_inplace {
                                                "; falling back to in-place elision"
                                            } else {
                                                ""
                                            },
                                        );
                                        emit_event(
                                            &d,
                                            &plan,
                                            "provider_compact",
                                            "failed",
                                            trigger,
                                            started.elapsed().as_millis() as u64,
                                            Some("provider_rejected"),
                                        );
                                        if let Some(fp) = &fp {
                                            settled.insert(session_key.clone(), fp.clone());
                                        }
                                        if !resolved.auto_apply_inplace {
                                            continue;
                                        }
                                    }
                                }
                            } else if !resolved.auto_apply_inplace {
                                // Headless-only mode without a resolvable
                                // claude binary: nothing else may mutate.
                                continue;
                            }
                        }
                        if !resolved.auto_apply_inplace {
                            continue;
                        }
                        let file_edits: Vec<Edit> = plan
                            .edits
                            .iter()
                            .filter(|e| {
                                !matches!(e, Edit::ProviderCompact { .. } | Edit::CacheEdit { .. })
                            })
                            .cloned()
                            .collect();
                        if file_edits.is_empty() {
                            continue;
                        }
                        let snap = vault::snapshot(
                            &d.handle.path,
                            d.handle.provider,
                            &d.handle.session_id,
                            Some(&resolved.strategy),
                            &vault::default_root(),
                        );
                        let r = snap.and_then(|entry| {
                            let file_plan = CompactionPlan {
                                edits: file_edits,
                                ..plan.clone()
                            };
                            apply_edits(&d, &file_plan).map(|reclaimed| (entry, reclaimed))
                        });
                        match r {
                            Ok((entry, reclaimed)) => {
                                // Post-apply snapshot + realized retention —
                                // every in-place compaction emits a scorable
                                // before/after pair on its event. Best-effort:
                                // evidence gaps never fail a completed apply.
                                let post_sha = vault::snapshot(
                                    &d.handle.path,
                                    d.handle.provider,
                                    &d.handle.session_id,
                                    Some("post-compact"),
                                    &vault::default_root(),
                                )
                                .map(|e| e.sha256)
                                .ok();
                                let mut ev = build_event(
                                    &d,
                                    &plan,
                                    action,
                                    "applied",
                                    trigger,
                                    started.elapsed().as_millis() as u64,
                                    None,
                                );
                                ev.snapshot_before_sha256 = Some(entry.sha256.clone());
                                ev.snapshot_after_sha256 = post_sha.clone();
                                if let Some(post) = &post_sha {
                                    if let Some((total, retained, lexical)) =
                                        realized_retention(&d.handle, &entry.sha256, post)
                                    {
                                        ev.retention_total = Some(total);
                                        ev.retention_retained = Some(retained);
                                        ev.retention_lexical = Some(lexical);
                                    }
                                }
                                if let Err(e) = append_event(&default_log_path(), &ev) {
                                    eprintln!("telemetry write failed (non-fatal): {e}");
                                }
                                eprintln!(
                                    "compacted claude transcript {} in place (~{reclaimed} bytes; snapshot {})",
                                    d.handle.path.display(),
                                    entry.sha256,
                                );
                                if let Some(nfp) = session_fingerprint(&d) {
                                    settled.insert(session_key.clone(), nfp);
                                }
                                if last_apply.len() >= 4096 {
                                    last_apply.clear();
                                }
                                last_apply.insert(session_key.clone(), started);
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
                                eprintln!(
                                    "claude in-place compact {} failed: {e}",
                                    d.handle.path.display()
                                );
                                if let Some(fp) = &fp {
                                    settled.insert(session_key.clone(), fp.clone());
                                }
                            }
                        }
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
                            eprintln!(
                                "prepared compacted fork for {}",
                                d.handle.cwd.as_deref().unwrap_or(&d.handle.path).display()
                            );
                            // The fork does not touch the source; until the
                            // source changes there is nothing new to prepare.
                            if let Some(fp) = &fp {
                                settled.insert(session_key.clone(), fp.clone());
                            }
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
                            eprintln!(
                                "compact {} failed: {e}",
                                d.handle.cwd.as_deref().unwrap_or(&d.handle.path).display()
                            );
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
                Err(e) => {
                    if let Some(fp) = &fp {
                        settled.insert(session_key.clone(), fp.clone());
                    }
                    eprintln!(
                        "plan {} failed: {e}",
                        d.handle.cwd.as_deref().unwrap_or(&d.handle.path).display()
                    );
                }
            }
        }
        // Persist suppression/clocks each pass: a restart then resumes
        // from the same terminal decisions instead of re-planning all
        // sessions once. Small file, written atomically. Dry-run passes
        // (e.g. monitor's --once probes) observe only — never mutate
        // persisted state.
        if !dry_run {
            let save_secs = now_secs();
            save_watch_state(
                &state_path,
                &WatchState {
                    generation: WATCH_STATE_GENERATION,
                    settled: settled.clone(),
                    settle_pass: settle_pass.clone(),
                    holddown: holddown.clone(),
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
    // `--session` reads the provider's own store so hook scripts and
    // wrappers don't have to measure context themselves.
    let (context_tokens, session_active) = if let Some(session_id) = session {
        match provider {
            "devin" => {
                let root = roots(cli).devin_home;
                // "current" resolves the caller's own session: the
                // provider-locked session bound to this working directory.
                let session_id = if session_id == "current" {
                    let cwd = std::env::current_dir()?;
                    devin::current_session(&root, &cwd).ok_or_else(|| {
                        anyhow::anyhow!("no single active devin session bound to {}", cwd.display())
                    })?
                } else {
                    // Accept a session-id prefix or title substring, same
                    // as `find`: prefer an exact id, else the most
                    // recently active match.
                    let matches = devin::find(&root, session_id);
                    matches
                        .iter()
                        .find(|d| d.handle.session_id == session_id)
                        .or_else(|| matches.first())
                        .map(|d| d.handle.session_id.clone())
                        .unwrap_or_else(|| session_id.to_string())
                };
                let Some((usage, locked)) = devin::session_observation(&root, &session_id) else {
                    bail!("no devin session '{session_id}' in {}", root.display());
                };
                (usage.context_tokens, locked)
            }
            "claude" | "claude_code" => {
                let Some(d) = detect::find(&roots(cli), session_id)
                    .into_iter()
                    .find(|d| d.handle.provider == Provider::ClaudeCode)
                else {
                    bail!("no claude session '{session_id}'");
                };
                (d.usage.context_tokens, d.handle.is_active())
            }
            other => bail!("--session lookup is not supported for provider '{other}'"),
        }
    } else {
        (context_tokens.unwrap_or(0), session_active)
    };
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

fn cmd_export(cli: &Cli, cfg: &config::Config, session: &str) -> Result<()> {
    let d = find_session(cli, cfg, session)?;
    use std::io::Write;
    let bytes = match d.handle.provider {
        Provider::Devin if gobstopper_adapters::devin::is_store_path(&d.handle.path) => {
            gobstopper_adapters::devin::export_bytes(&d.handle.path, &d.handle.session_id)
                .map_err(|e| anyhow::anyhow!(e))?
        }
        // Codex/Claude transcripts are already canonical files; a detached
        // Devin export round-trips unchanged.
        _ => transaction_read(&d.handle.path)?,
    };
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
        Cmd::InstallHooks => cmd_install_hooks(false, &roots(&cli)),
        Cmd::UninstallHooks => cmd_install_hooks(true, &roots(&cli)),
        Cmd::Hook { event } => cmd_hook(&cli, &cfg, event),
        Cmd::Report {
            strict,
            active_only,
        } => cmd_report(&cli, *strict, *active_only),
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
        Cmd::Vault { session, json } => cmd_vault(&cli, &cfg, session.as_deref(), *json),
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
        } => cmd_watch(
            &cli,
            &cfg,
            *interval,
            *dry_run,
            *double_buffer,
            *active_only,
            provider.clone(),
            *once,
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
    use super::*;
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
            auto_apply_store: false,
            auto_apply_inplace: false,
            auto_compact_closed: false,
            acp_timeout_secs: 1800,
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
        save_watch_state(&path, &state);
        let loaded = load_watch_state(&path);
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
        // Corrupt or missing state falls back to empty, never fails.
        fs::write(&path, "{not json").unwrap();
        assert!(load_watch_state(&path).settled.is_empty());
        fs::remove_file(&path).unwrap();
        assert!(load_watch_state(&path).settled.is_empty());
        fs::remove_dir_all(&dir).ok();
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
        // Transient infra → no cooldown, retry next pass.
        assert_eq!(
            codex_failure_hold_secs("spawning `codex app-server --listen stdio://`"),
            0
        );
        assert_eq!(
            codex_failure_hold_secs("codex app-server closed its stream"),
            0
        );
        assert_eq!(
            codex_failure_hold_secs("timed out waiting for response id 2"),
            0
        );
    }

    /// Touch a file's mtime without changing its length.
    fn filetime_set(path: &std::path::Path, t: std::time::SystemTime) {
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(t).unwrap();
    }
}
