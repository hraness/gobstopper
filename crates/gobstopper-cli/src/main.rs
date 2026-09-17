//! gobstopper: automatic context compaction for Codex and Claude Code.

mod config;
mod hooks;
mod report;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use gobstopper_adapters::detect::{self, Discovered, Roots};
use gobstopper_adapters::{copy, eval, fork, plugins, vault, verify, AdapterError};
use gobstopper_core::events::{append_event, default_log_path, CompactionEvent};
use gobstopper_core::plan::{CompactionPlan, Edit};
use gobstopper_core::strategy::{self, QuotaPressure};
use gobstopper_core::Provider;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Parser)]
#[command(name = "gobstopper", version, about = "Automatic context compaction for coding-agent sessions")]
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
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
        /// Don't snapshot the transcript into the undo vault first
        /// (not recommended).
        #[arg(long)]
        no_backup: bool,
        /// Codex only: emit a real `compacted` record (provider window
        /// chain + replacement_history) instead of a plain digest line.
        /// Experimental — shape is verified against real rollouts but
        /// not yet validated against live resume.
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
        /// Emit JSON rows.
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
    /// Poll for sessions over threshold and compact them automatically.
    Watch {
        /// Poll interval in seconds.
        #[arg(long, default_value = "30")]
        interval: u64,
        /// Report plans without applying them.
        #[arg(long)]
        dry_run: bool,
        /// Precompute compacted transcripts at 60% of trigger and swap
        /// atomically at the trigger — zero-stall compaction.
        #[arg(long)]
        double_buffer: bool,
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
    Check { manifest: PathBuf },
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
                session_id: meta.0.unwrap_or_else(|| query.to_string()),
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

fn evaluate(
    transcript: &gobstopper_core::Transcript,
    resolved: &config::Resolved,
) -> Result<Option<CompactionPlan>> {
    let before = transcript.context_tokens();
    if before < resolved.policy.effective_trigger() { return Ok(None); }
    if let Some(selection) = &resolved.plugin {
        let checked = plugins::check(&selection.manifest)?;
        let bytes = gobstopper_adapters::transaction::read(&transcript.session.path)?;
        let content = if checked.manifest.capabilities.contains(&plugins::Capability::ReadContent) {
            Some(std::str::from_utf8(&bytes)?.lines().map(str::to_string).collect())
        } else { None };
        let request = plugins::Request {
            protocol_version: 1, operation: plugins::Capability::Strategy,
            provider_id: transcript.session.provider.as_str().into(), source_sha256: copy::sha256(&bytes),
            items: transcript.items.clone(), usage: transcript.usage,
            policy: Some(resolved.policy.clone()), content,
        };
        let response = plugins::invoke(&selection.manifest, &selection.trusted_sha256, &request)?;
        return external_plan(transcript, resolved, response.edits);
    }
    // Userspace command preset: feed the normalized transcript, read edits.
    if let Some(command) = &resolved.command {
        if !resolved.trusted_legacy_command { bail!("legacy command execution is not explicitly trusted"); }
        let plan_json = run_preset_command(command, transcript)?;
        let edits: Vec<Edit> = serde_json::from_value(plan_json["edits"].clone())
            .context("preset command returned invalid edits")?;
        // An empty edit list is the command's defer answer — report no plan
        // rather than applying nothing and recording a phantom compaction.
        if edits.is_empty() {
            return Ok(None);
        }
        return external_plan(transcript, resolved, edits);
    }
    let strat = strategy::strategy_by_id(&resolved.strategy)
        .ok_or_else(|| anyhow::anyhow!("unknown strategy '{}'", resolved.strategy))?;
    let plan = strat.evaluate(transcript, &resolved.policy);
    if let Some(plan) = &plan {
        gobstopper_core::validation::validate_edits(transcript, &resolved.policy, &plan.edits).map_err(anyhow::Error::msg)?;
    }
    Ok(plan)
}

fn external_plan(transcript: &gobstopper_core::Transcript, resolved: &config::Resolved, edits: Vec<Edit>) -> Result<Option<CompactionPlan>> {
    if edits.is_empty() { return Ok(None); }
    gobstopper_core::validation::validate_edits(transcript, &resolved.policy, &edits).map_err(anyhow::Error::msg)?;
    let before = transcript.context_tokens();
    let mut after = before;
    for edit in &edits {
        match edit {
            Edit::Elide { line_indexes, .. } => {
                for item in transcript.items.iter().filter(|item| line_indexes.contains(&item.line_index)) {
                    after = after.saturating_sub(item.estimated_elision_savings());
                }
            }
            Edit::InjectDigest { digest } => after = after.saturating_add(gobstopper_core::estimate::estimate_tokens(serde_json::to_vec(digest)?.len())),
            Edit::ProviderCompact { .. } => bail!("external strategy plugins cannot dispatch provider controls"),
        }
    }
    if after >= before { return Ok(None); }
    Ok(Some(CompactionPlan { strategy: format!("preset:{}", resolved.strategy), rationale: "validated userspace proposal; savings are projected".into(), edits, context_tokens_before: before, context_tokens_after: after }))
}

/// Explain a `None` plan: under trigger vs. over trigger but nothing to cut.
fn report_no_plan(transcript: &gobstopper_core::Transcript, resolved: &config::Resolved) {
    let ctx = transcript.context_tokens();
    let trigger = resolved.policy.effective_trigger();
    if ctx < trigger {
        println!("nothing to do: context ~{ctx} under trigger {trigger}");
    } else {
        println!(
            "context ~{ctx} exceeds trigger {trigger} but the '{}' strategy found no applicable edits",
            resolved.strategy
        );
    }
}

fn run_preset_command(
    command: &str,
    transcript: &gobstopper_core::Transcript,
) -> Result<serde_json::Value> {
    if command.len() > 4096 { bail!("legacy command exceeds byte limit"); }
    let mut process = Command::new("sh");
    process.args(["-c", command]);
    let payload = serde_json::json!({
        "session_id": transcript.session.session_id,
        "provider": transcript.session.provider,
        "items": transcript.items,
        "usage": transcript.usage,
    });
    let output = plugins::run_bounded(process, serde_json::to_vec(&payload)?, 30_000, 1024 * 1024)?;
    serde_json::from_slice(&output).map_err(|_| anyhow::anyhow!("legacy command returned invalid JSON"))
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
fn provider_compact(d: &Discovered, codex_bin: &std::path::Path, codex_home: &std::path::Path) -> Result<()> {
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
fn codex_compact(codex_bin: &std::path::Path, thread_id: &str, codex_home: Option<&std::path::Path>) -> Result<()> {
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
    std::thread::spawn(move || { let _ = std::io::copy(&mut stderr.take(64 * 1024), &mut std::io::sink()); });
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
                        let code = if msg.contains("cannot resume") { "cannot resume provider thread" }
                            else if msg.contains("usage limit") { "usage limit exceeded" } else { "provider_rejected" };
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
                    let is_our_turn = v.get("method").and_then(|m| m.as_str()) == Some("turn/completed")
                        && v.pointer("/params/threadId").and_then(|s| s.as_str()) == Some(thread_id);
                    if is_our_turn {
                        let status = v
                            .pointer("/params/turn/status")
                            .and_then(|s| s.as_str())
                            .unwrap_or("unknown");
                        let detail = v
                            .pointer("/params/turn/error/message")
                            .and_then(|s| s.as_str())
                            .map(|m| if m.contains("usage limit") { ": usage limit exceeded".to_string() } else { ": provider_error".to_string() })
                            .unwrap_or_default();
                        if status == "completed" {
                            return Ok("compaction turn completed".into());
                        }
                        let status = match status { "failed" => "failed", "interrupted" => "interrupted", _ => "unknown" };
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

fn print_plan(d: &Discovered, plan: &CompactionPlan, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(plan)?);
        return Ok(());
    }
    println!(
        "{} {} ({})\n  context: {} -> ~{} tokens (saves ~{})\n  strategy: {}\n  {}",
        d.handle.provider.as_str(),
        d.handle.session_id,
        if d.handle.is_active() { "active" } else { "idle" },
        plan.context_tokens_before,
        plan.context_tokens_after,
        plan.est_savings(),
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
            Edit::ProviderCompact { control } => println!("  provider control: {control}"),
        }
    }
    Ok(())
}

fn cmd_detect(cli: &Cli, all: bool, json: bool) -> Result<()> {
    let max_age = if all { 0 } else { detect::default_max_age_secs() };
    let sessions = detect::discover(&roots(cli), max_age);
    if json {
        let out: Vec<serde_json::Value> = sessions
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
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    println!("{:<12} {:<38} {:<6} {:>12} {:>14}  PATH", "PROVIDER", "SESSION", "STATE", "CTX TOKENS", "LIFETIME IN");
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
) -> Result<()> {
    let d = find_session(cli, cfg, session)?;
    let root = vault::default_root();
    let entry = match sha {
        Some(prefix) => vault::list(&root)?
            .into_iter()
            .filter(|e| e.path == d.handle.path)
            .find(|e| e.sha256.starts_with(prefix))
            .ok_or_else(|| anyhow::anyhow!("no vault snapshot matching '{prefix}' for this session"))?,
        None => vault::latest_pre_compaction(&d.handle.path, &root)?.ok_or_else(|| {
            anyhow::anyhow!("no vault snapshot for {}", d.handle.path.display())
        })?,
    };
    println!(
        "restore snapshot {} — {} bytes, session {}",
        &entry.sha256[..16],
        entry.bytes,
        entry.session_id
    );
    if !yes {
        print!("restore snapshot into a separate fork of {}? [y/N] ", d.handle.path.display());
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
    println!("restored copy {}\n{}", restored.path.display(), restored.resume_hint);
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
    println!("{}: +{} -{}", report.path.display(), report.added.len(), report.skipped.len());
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
        println!("{}: +{} -{}", report.path.display(), report.added.len(), report.skipped.len());
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

fn cmd_report(cli: &Cli, strict: bool) -> Result<()> {
    let sessions = detect::discover(&roots(cli), 0);
    let events = gobstopper_core::events::read_events(&default_log_path()).unwrap_or_default();
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
    let applied: Vec<&CompactionEvent> = events
        .iter()
        .filter(|e| e.outcome == "applied")
        .collect();
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
    json: bool,
) -> Result<()> {
    let d = find_session(cli, cfg, session)?;
    let resolved = cfg.resolve(d.handle.provider, &d.handle.session_id, None, None)?;
    let mut policy = resolved.policy;
    if let Some(t) = trigger {
        policy.trigger_tokens = t;
    }
    let rows = eval::eval_transcript(
        d.handle.provider,
        &d.handle.path,
        &policy,
        strategy_flag,
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
                println!(
                    "  {:<11} {} -> ~{} (saves ~{}){}{}",
                    row.strategy,
                    plan.context_tokens_before,
                    plan.context_tokens_after,
                    row.est_reclaimed,
                    probe,
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
    yes: bool,
    no_backup: bool,
    experimental_compacted: bool,
) -> Result<()> {
    if no_backup { bail!("snapshots are mandatory; --no-backup is no longer supported"); }
    if experimental_compacted { bail!("custom compacted records are quarantined pending live qualification"); }
    let d = find_session(cli, cfg, session)?;
    let mut resolved = cfg.resolve(
        d.handle.provider,
        &d.handle.session_id,
        preset,
        strategy,
    )?;
    if let Some(t) = trigger {
        resolved.policy.trigger_tokens = t;
    }
    let (transcript, source_sha256) = copy::load_bound(d.handle.clone())?;
    let Some(plan) = evaluate(&transcript, &resolved)? else {
        report_no_plan(&transcript, &resolved);
        return Ok(());
    };
    print_plan(&d, &plan, false)?;
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
    let has_provider_compact = plan
        .edits
        .iter()
        .any(|e| matches!(e, Edit::ProviderCompact { .. }));
    let file_edits: Vec<Edit> = plan
        .edits
        .iter()
        .filter(|e| !matches!(e, Edit::ProviderCompact { .. }))
        .cloned()
        .collect();
    let started = std::time::Instant::now();
    let trigger = resolved.policy.trigger_tokens;
    // Experimental Codex path: an InjectDigest edit becomes a real
    // `compacted` record (window chain + replacement_history) appended
    // to the rollout — the provider's own resume mechanism performs the
    // swap. Elide edits in the same plan still apply normally first.
    let digest_for_compacted = if experimental_compacted && d.handle.provider == Provider::Codex {
        plan.edits.iter().find_map(|e| match e {
            Edit::InjectDigest { digest } => Some(digest.clone()),
            _ => None,
        })
    } else {
        None
    };
    if !file_edits.is_empty() {
        if digest_for_compacted.is_some() { bail!("custom replacement history is not qualified"); }
        let file_result = copy::compact(&d.handle, &source_sha256, &plan, &vault::default_root()).map(|receipt| {
            println!("prepared {}", receipt.path.display());
            println!("resume the new session: {} {}", if d.handle.provider == Provider::Codex { "codex resume" } else { "claude --resume" }, receipt.session_id);
            receipt.reclaimed_bytes
        });
        match file_result {
            Ok(reclaimed) => {
                emit_event(&d, &plan, "transcript_compact", "applied", trigger,
                    started.elapsed().as_millis() as u64, None);
                println!("reclaimed ~{} bytes of tool output", reclaimed);
                if digest_for_compacted.is_some() {
                    println!("emitted compacted record (window chain advanced; resume performs the swap)");
                }
            }
            Err(e) => {
                emit_event(&d, &plan, "transcript_compact", "failed", trigger,
                    started.elapsed().as_millis() as u64, Some("apply_failed"));
                return Err(e);
            }
        }
    }
    if has_provider_compact {
        if d.handle.provider == Provider::ClaudeCode { bail!("live Claude compaction must be dispatched by its session owner; use /compact in that session"); }
        let snapshot = snapshot_before_edit(&d, &plan.strategy)?;
        if snapshot.sha256 != source_sha256 { bail!("source changed before native fork preparation"); }
        let forked = fork::restore_copy(d.handle.provider, &d.handle.path, &snapshot.sha256, &vault::default_root())?;
        println!("native compaction targets a separate fork: {}", forked.resume_hint);
        let d = Discovered { handle: gobstopper_core::SessionHandle { path: forked.path, session_id: forked.session_id, ..d.handle.clone() }, usage: d.usage };
        emit_event(&d, &plan, "provider_compact", "planned", trigger, 0, None);
        match provider_compact(&d, &resolve_codex_bin(cli.codex_bin.as_deref()), &roots(cli).codex_home) {
            Ok(()) => {
                emit_event(&d, &plan, "provider_compact", "applied", trigger,
                    started.elapsed().as_millis() as u64, None);
                println!("provider compaction requested");
            }
            Err(e) => {
                emit_event(&d, &plan, "provider_compact", "failed", trigger,
                    started.elapsed().as_millis() as u64, Some("provider_rejected"));
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
        .filter(|e| !matches!(e, Edit::ProviderCompact { .. }))
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
) -> Result<()> {
    if interval == 0 { bail!("watch interval must be positive"); }
    if double_buffer { bail!("in-place double-buffer swapping is retired; use copy-only watch without --double-buffer"); }
    let mut last_fire: std::collections::HashMap<String, std::time::Instant> =
        std::collections::HashMap::new();
    let mut staged: std::collections::HashMap<String, Staged> =
        std::collections::HashMap::new();
    let mut discovery_cache = detect::DiscoveryCache::default();
    loop {
        let cfg = config::load()?;
        for d in detect::discover_cached(&roots(cli), 0, &mut discovery_cache) {
            let session_key = format!("{}:{}", d.handle.provider.as_str(), d.handle.path.display());
            let Ok(resolved) = cfg.resolve(
                d.handle.provider,
                &d.handle.session_id,
                None,
                None,
            ) else {
                continue;
            };
            let trigger = resolved.policy.effective_trigger();
            let ctx = if d.usage.context_tokens > 0 { d.usage.context_tokens } else {
                detect::load(&d).map(|t| t.estimated_context_tokens()).unwrap_or(0)
            };
            if ctx < trigger {
                // Below trigger: optionally precompute the compacted file
                // so the trigger crossing is a rename, not a rewrite.
                if double_buffer && !dry_run && ctx >= trigger * 6 / 10
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
                    match copy::compact(&d.handle, &s.source_sha256, &s.plan, &vault::default_root())
                        .map(|_| ())
                    {
                        Ok(()) => {
                            last_fire.insert(session_key.clone(), std::time::Instant::now());
                            emit_event(&d, &s.plan, "transcript_compact", "applied",
                                trigger, started.elapsed().as_millis() as u64, None);
                            eprintln!("compacted {} (staged swap)", d.handle.session_id);
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
                    let trigger = resolved.policy.trigger_tokens;
                    let is_provider = plan
                        .edits
                        .iter()
                        .any(|e| matches!(e, Edit::ProviderCompact { .. }));
                    let action = if is_provider { "provider_compact" } else { "transcript_compact" };
                    if last_fire.len() >= 4096 { last_fire.clear(); }
                    last_fire.insert(session_key.clone(), std::time::Instant::now());
                    let r = if is_provider {
                        Err(anyhow::anyhow!("native compaction requires the session owner; watch will not create a second writer"))
                    } else {
                        copy::compact(&d.handle, &source_sha256, &plan, &vault::default_root())
                            .map(|receipt| { eprintln!("prepared copy {}", receipt.path.display()); })
                    };
                    match r {
                        Ok(()) => {
                            last_fire.insert(session_key.clone(), std::time::Instant::now());
                            emit_event(&d, &plan, action, "applied", trigger,
                                started.elapsed().as_millis() as u64, None);
                            eprintln!("compacted {}", d.handle.session_id);
                        }
                        Err(e) => {
                            emit_event(&d, &plan, action, "failed", trigger,
                                started.elapsed().as_millis() as u64, Some("apply_failed"));
                            eprintln!("compact {} failed: {e}", d.handle.session_id);
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => eprintln!("plan {} failed: {e}", d.handle.session_id),
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(interval));
    }
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
    let provider = match provider {
        "codex" => Provider::Codex,
        "claude" | "claude_code" => Provider::ClaudeCode,
        other => bail!("unknown provider '{other}'"),
    };
    let mut resolved = cfg.resolve(provider, "", preset, None)?;
    if let Some(q) = quota_pressure {
        resolved.policy.quota_pressure = q.into();
    }
    let effective_trigger = resolved.policy.effective_trigger();
    let over = context_tokens >= effective_trigger;
    // Mirror AutoStrategy::select without a transcript: live sessions get
    // provider compaction; idle sessions get the transcript-path default.
    let action = if !over {
        "none"
    } else if session_active {
        "provider_compact"
    } else {
        "transcript_compact"
    };
    let control = match (over, session_active, provider) {
        (true, true, Provider::Codex) => Some("thread/compact/start"),
        (true, true, Provider::ClaudeCode) => Some("/compact or relaunch --autocompact"),
        _ => None,
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "action": action,
                "strategy": resolved.strategy,
                "trigger_tokens": resolved.policy.trigger_tokens,
                "effective_trigger_tokens": effective_trigger,
                "quota_pressure": resolved.policy.quota_pressure,
                "control": control,
            }))?
        );
    } else {
        println!("action={action} strategy={}", resolved.strategy);
        if let Some(c) = control {
            println!("control={c}");
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
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                "protocol_version": 1, "id": checked.manifest.id, "version": checked.manifest.version,
                "manifest_sha256": checked.manifest_sha256, "capabilities": checked.manifest.capabilities,
                "code_executed": false, "trusted": false,
            }))?);
        }
        PluginCmd::Inspect { manifest, trusted_sha256, provider, source } => {
            let bytes = gobstopper_adapters::transaction::read(source)?;
            let request = plugins::Request {
                protocol_version:1, operation:plugins::Capability::ProviderRead, provider_id:provider.clone(),
                source_sha256:copy::sha256(&bytes), items:Vec::new(), usage:Default::default(),
                policy:None, content:Some(std::str::from_utf8(&bytes)?.lines().map(str::to_string).collect()),
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
            let transcript = detect::load(&d)?;
            match evaluate(&transcript, &resolved)? {
                Some(plan) => print_plan(&d, &plan, *json),
                None => {
                    report_no_plan(&transcript, &resolved);
                    Ok(())
                }
            }
        }
        Cmd::Apply {
            session,
            strategy,
            preset,
            trigger,
            yes,
            no_backup,
            experimental_compacted,
        } => cmd_apply(
            &cli,
            &cfg,
            session,
            strategy.as_deref(),
            preset.as_deref(),
            *trigger,
            *yes,
            *no_backup,
            *experimental_compacted,
        ),
        Cmd::Verify { session, json } => cmd_verify(&cli, &cfg, session, *json),
        Cmd::Undo { session, sha, yes } => {
            cmd_undo(&cli, &cfg, session, sha.as_deref(), *yes)
        }
        Cmd::Fork { session } => cmd_fork(&cli, &cfg, session),
        Cmd::Eval {
            session,
            strategy,
            trigger,
            json,
        } => cmd_eval(&cli, &cfg, session, strategy.as_deref(), *trigger, *json),
        Cmd::InstallHooks => cmd_install_hooks(false, &roots(&cli)),
        Cmd::UninstallHooks => cmd_install_hooks(true, &roots(&cli)),
        Cmd::Hook { event } => cmd_hook(event),
        Cmd::Report { strict } => cmd_report(&cli, *strict),
        Cmd::Events {
            session,
            tail,
            json,
        } => cmd_events(session.as_deref(), *tail, *json),
        Cmd::Vault { session, json } => cmd_vault(&cli, &cfg, session.as_deref(), *json),
        Cmd::Watch {
            interval,
            dry_run,
            double_buffer,
        } => cmd_watch(&cli, &cfg, *interval, *dry_run, *double_buffer),
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
    use std::os::unix::fs::PermissionsExt;

    /// Write a fake `codex` app-server: logs every inbound JSON-RPC line to
    /// `requests.log` beside itself and answers initialize/resume/compact.
    /// The emitted `turn/completed` status is keyed on the thread id so one
    /// stub covers success, provider failure, and request-error paths.
    fn stub_codex(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("codex-stub");
        fs::write(
            &path,
            r#"#!/bin/sh
log="$(dirname "$0")/requests.log"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  case "$line" in
    *'"initialize"'*) printf '{"id":0,"result":{}}\n' ;;
    *'"thread/resume"'*)
      case "$line" in
        *bad-resume*) printf '{"id":1,"error":{"message":"cannot resume an unloaded multi-agent v2 sub-agent through its parent"}}\n' ;;
        *) printf '{"id":1,"result":{}}\n' ;;
      esac ;;
    *'"thread/compact/start"'*)
      case "$line" in
        *fail-thread*)
          printf '{"id":2,"result":{}}\n'
          printf '{"method":"turn/completed","params":{"threadId":"fail-thread","turn":{"status":"failed","error":{"message":"usage limit exceeded"}}}}\n' ;;
        *error-thread*) printf '{"id":2,"error":{"message":"thread not found"}}\n' ;;
        *)
          printf '{"id":2,"result":{}}\n'
          printf '{"method":"turn/completed","params":{"threadId":"ok-thread","turn":{"status":"completed"}}}\n' ;;
      esac ;;
  esac
done
"#,
        )
        .unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&path, perms).unwrap();
        path
    }

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gobstopper-test-{}-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn codex_compact_initializes_resumes_then_compacts() {
        let dir = tempdir("happy");
        let stub = stub_codex(&dir);
        codex_compact(&stub, "ok-thread", None).unwrap();
        let log = fs::read_to_string(dir.join("requests.log")).unwrap();
        let methods: Vec<String> = log
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| v.get("method").and_then(|m| m.as_str()).map(String::from))
            .collect();
        assert_eq!(
            methods,
            ["initialize", "initialized", "thread/resume", "thread/compact/start"].map(String::from)
        );
        let resume: serde_json::Value =
            serde_json::from_str(log.lines().nth(2).unwrap()).unwrap();
        assert_eq!(resume.pointer("/params/excludeTurns"), Some(&serde_json::json!(true)));
        assert_eq!(resume.pointer("/params/threadId"), Some(&serde_json::json!("ok-thread")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn codex_compact_reports_failed_turn_as_error() {
        let dir = tempdir("failed");
        let stub = stub_codex(&dir);
        let err = codex_compact(&stub, "fail-thread", None).unwrap_err();
        assert!(err.to_string().contains("failed"), "got: {err}");
        assert!(err.to_string().contains("usage limit"), "got: {err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn codex_compact_propagates_request_errors() {
        let dir = tempdir("err");
        let stub = stub_codex(&dir);
        let err = codex_compact(&stub, "bad-resume", None).unwrap_err();
        assert!(err.to_string().contains("cannot resume"), "got: {err}");
        let err = codex_compact(&stub, "error-thread", None).unwrap_err();
        assert!(err.to_string().contains("thread not found"), "got: {err}");
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
                label: "tool output".to_string(),
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
                keep_recent_tool_outputs: 0,
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
        assert_eq!(plan.context_tokens_after, 50_000 - preset_transcript().items[0].estimated_elision_savings());
        assert_ne!(plan.context_tokens_after, 1200);
        assert!(matches!(plan.edits.as_slice(), [Edit::Elide { .. }]));
    }

    #[test]
    fn preset_command_invalid_json_is_an_error() {
        assert!(evaluate(&preset_transcript(), &preset_resolved("echo 'nope'")).is_err());
    }
}
