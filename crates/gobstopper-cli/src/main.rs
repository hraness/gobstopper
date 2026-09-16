//! gobstopper: automatic context compaction for Codex and Claude Code.

mod config;
mod hooks;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use gobstopper_adapters::detect::{self, Discovered, Roots};
use gobstopper_adapters::{eval, fork, vault, verify, AdapterError};
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
    // Userspace command preset: feed the normalized transcript, read edits.
    if let Some(command) = &resolved.command {
        let plan_json = run_preset_command(command, transcript)?;
        let edits: Vec<Edit> = serde_json::from_value(plan_json["edits"].clone())
            .context("preset command returned invalid edits")?;
        let before = transcript.context_tokens();
        if before < resolved.policy.trigger_tokens {
            return Ok(None);
        }
        let after = plan_json["context_tokens_after"]
            .as_u64()
            .unwrap_or(resolved.policy.floor_tokens);
        return Ok(Some(CompactionPlan {
            strategy: format!("preset:{}", resolved.strategy),
            rationale: "userspace preset command".to_string(),
            edits,
            context_tokens_before: before,
            context_tokens_after: after,
        }));
    }
    let strat = strategy::strategy_by_id(&resolved.strategy)
        .ok_or_else(|| anyhow::anyhow!("unknown strategy '{}'", resolved.strategy))?;
    Ok(strat.evaluate(transcript, &resolved.policy))
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
    let mut child = Command::new("sh")
        .args(["-c", command])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawning preset command '{command}'"))?;
    let payload = serde_json::json!({
        "session_id": transcript.session.session_id,
        "provider": transcript.session.provider,
        "items": transcript.items,
        "usage": transcript.usage,
    });
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.to_string().as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("preset command exited {}", out.status);
    }
    Ok(serde_json::from_slice(&out.stdout)?)
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
fn provider_compact(d: &Discovered) -> Result<()> {
    match d.handle.provider {
        Provider::Codex => codex_compact(&d.handle.session_id),
        Provider::ClaudeCode => bail!(
            "claude sessions compact via /compact in-session or --autocompact at launch; \
             gobstopper cannot inject into a running TUI"
        ),
    }
}

/// Ask the shared Codex app-server daemon to compact a thread.
/// Speaks JSON-RPC over `codex app-server proxy` stdio.
fn codex_compact(thread_id: &str) -> Result<()> {
    let mut child = Command::new("codex")
        .args(["app-server", "proxy"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("spawning `codex app-server proxy` (is the codex daemon running?)")?;
    let mut stdin = child.stdin.take().unwrap();
    let mut send = |v: serde_json::Value| -> Result<()> {
        stdin.write_all(v.to_string().as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    };
    send(serde_json::json!({
        "method": "initialize", "id": 0,
        "params": {"clientInfo": {"name": "gobstopper", "version": env!("CARGO_PKG_VERSION")}}
    }))?;
    send(serde_json::json!({
        "method": "thread/compact/start", "id": 1,
        "params": {"threadId": thread_id}
    }))?;
    drop(stdin);
    let out = child.wait_with_output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    if text.contains("\"error\"") {
        bail!("codex app-server rejected compaction: {}", &text[..text.len().min(400)]);
    }
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
        None => vault::latest_for(&d.handle.path, &root)?.ok_or_else(|| {
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
        print!("overwrite {}? [y/N] ", d.handle.path.display());
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
        let _ = vault::snapshot(
            &d.handle.path,
            d.handle.provider,
            &d.handle.session_id,
            Some("pre-undo"),
            &root,
        );
    }
    vault::restore(&entry.sha256, &d.handle.path, &root)?;
    println!("restored {}", d.handle.path.display());
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

fn cmd_install_hooks(uninstall: bool) -> Result<()> {
    let claude_settings = hooks::default_claude_settings();
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
        let codex_hooks = hooks::default_codex_hooks();
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
                let errors = row
                    .findings
                    .iter()
                    .filter(|f| f.severity == verify::Severity::Error)
                    .count();
                println!(
                    "  {:<11} {} -> ~{} (saves ~{}){}",
                    row.strategy,
                    plan.context_tokens_before,
                    plan.context_tokens_after,
                    row.est_reclaimed,
                    if errors > 0 {
                        format!("  ⚠ {errors} verify errors")
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

fn cmd_apply(
    cli: &Cli,
    cfg: &config::Config,
    session: &str,
    strategy: Option<&str>,
    preset: Option<&str>,
    trigger: Option<u64>,
    yes: bool,
    no_backup: bool,
) -> Result<()> {
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
    let transcript = detect::load(&d)?;
    let Some(plan) = evaluate(&transcript, &resolved)? else {
        report_no_plan(&transcript, &resolved);
        return Ok(());
    };
    print_plan(&d, &plan, false)?;
    if d.handle.is_active() {
        println!("session appears live; transcript edits apply on next resume");
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
    if !file_edits.is_empty() {
        if !no_backup {
            let entry = snapshot_before_edit(&d, &plan.strategy)?;
            eprintln!("snapshot: {} ({})", &entry.sha256[..12], entry.path.display());
        }
        match apply_edits(&d, &plan) {
            Ok(reclaimed) => {
                emit_event(&d, &plan, "transcript_compact", "applied", trigger,
                    started.elapsed().as_millis() as u64, None);
                println!("reclaimed ~{} bytes of tool output", reclaimed);
            }
            Err(e) => {
                emit_event(&d, &plan, "transcript_compact", "failed", trigger,
                    started.elapsed().as_millis() as u64, Some("apply_failed"));
                return Err(e);
            }
        }
    }
    if has_provider_compact {
        match provider_compact(&d) {
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

fn cmd_watch(cli: &Cli, cfg: &config::Config, interval: u64, dry_run: bool) -> Result<()> {
    let mut last_fire: std::collections::HashMap<String, std::time::Instant> =
        std::collections::HashMap::new();
    loop {
        for d in detect::discover(&roots(cli), 0) {
            let Ok(resolved) = cfg.resolve(
                d.handle.provider,
                &d.handle.session_id,
                None,
                None,
            ) else {
                continue;
            };
            if d.usage.context_tokens < resolved.policy.effective_trigger() {
                continue;
            }
            if let Some(t) = last_fire.get(&d.handle.session_id) {
                if t.elapsed().as_secs() < resolved.policy.min_interval_secs {
                    continue;
                }
            }
            let transcript = match detect::load(&d) {
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
                    let r = if is_provider {
                        provider_compact(&d)
                    } else {
                        snapshot_before_edit(&d, &plan.strategy)
                            .and_then(|_| apply_edits(&d, &plan).map_err(anyhow::Error::from))
                            .map(|_| ())
                    };
                    match r {
                        Ok(()) => {
                            last_fire.insert(d.handle.session_id.clone(), std::time::Instant::now());
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = config::load();
    match &cli.command {
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
        } => cmd_apply(
            &cli,
            &cfg,
            session,
            strategy.as_deref(),
            preset.as_deref(),
            *trigger,
            *yes,
            *no_backup,
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
        Cmd::InstallHooks => cmd_install_hooks(false),
        Cmd::UninstallHooks => cmd_install_hooks(true),
        Cmd::Hook { event } => cmd_hook(event),
        Cmd::Vault { session, json } => cmd_vault(&cli, &cfg, session.as_deref(), *json),
        Cmd::Watch { interval, dry_run } => cmd_watch(&cli, &cfg, *interval, *dry_run),
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
