//! `gobstopper proxy`: request-time compaction for Claude Code and Codex.
//!
//! A loopback HTTP server between an agent and its provider. Every request
//! is forwarded through the system `curl` (the transport the model scorers
//! already use, so no TLS crate is linked). Anthropic Messages and OpenAI
//! Responses bodies over the threshold are first compacted with the cliff
//! rule in `gobstopper_adapters::request`; any parse or engine failure
//! forwards the original bytes unchanged. Because the provider then reports
//! the compacted size, the client's own auto-compaction does not reach its
//! trigger. Request headers carry API keys and OAuth tokens, so they reach
//! curl through its environment rather than argv, and nothing from a
//! request or response body is logged.

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use gobstopper_adapters::codex_compact::rfc3339_now;
use gobstopper_adapters::request::{
    replay, CliffConfig, Dialect, Engine, RequestCtx, DEFAULT_THRESHOLD_TOKENS,
    MAX_KEEP_TAIL_PERCENT,
};
use serde_json::{json, Value};
use std::borrow::Cow;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const DEFAULT_PORT: u16 = 8260;
const STATUS_PATH: &str = "/gobstopper/status";
const MAX_HEAD_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 256 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 256;

/// Never forwarded upstream: hop-by-hop fields and fields the proxy sets.
const STRIP_REQUEST: &[&str] = &[
    "host",
    "content-length",
    "connection",
    "transfer-encoding",
    "accept-encoding",
    "expect",
    "keep-alive",
    "proxy-connection",
    "proxy-authorization",
    "te",
    "trailer",
    "upgrade",
];
/// Never relayed to the client: framing is re-established here.
const STRIP_RESPONSE: &[&str] = &[
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "upgrade",
];

#[derive(Subcommand)]
pub enum ProxyCmd {
    /// Serve on a loopback port until stopped.
    Serve {
        #[command(flatten)]
        opts: ProxyOpts,
        /// Loopback port (0 picks a free one and prints it).
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },
    /// Start the proxy on a free port, run a command with
    /// ANTHROPIC_BASE_URL and OPENAI_BASE_URL pointed at it, and stop when
    /// the command exits: `gobstopper proxy run -- claude`.
    Run {
        #[command(flatten)]
        opts: ProxyOpts,
        /// The command and its arguments, after `--`.
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Preview what the proxy would have sent over a recorded Claude Code or
    /// Codex session. Reads the transcript and calls no provider; sizes are
    /// estimates, not billed tokens.
    Replay {
        /// Session id prefix, or path to a transcript file.
        session: String,
        #[arg(long, default_value_t = DEFAULT_THRESHOLD_TOKENS)]
        threshold: u64,
        #[arg(long, default_value_t = 3)]
        keep_recent: usize,
        /// Percent (0-60) of the room above the verbatim head that the
        /// summary and the kept steps may fill. 0 keeps exactly --keep-recent.
        #[arg(long, default_value_t = CliffConfig::default().keep_tail_percent,
              value_parser = keep_tail_percent_arg())]
        keep_tail_percent: u8,
        #[arg(long, default_value_t = 500)]
        result_max_chars: usize,
        /// Tokens assumed for the system prompt and tool definitions, which
        /// transcripts do not record.
        #[arg(long, default_value_t = 20_000)]
        fixed_tokens: u64,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show a running proxy's settings and counters.
    Status {
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args, Clone)]
pub struct ProxyOpts {
    /// Compact when the estimated outgoing request exceeds this many tokens;
    /// Anthropic requests that declare a 1M-token window use --threshold-1m.
    /// Keep it below the client's own auto-compaction point.
    #[arg(long, default_value_t = DEFAULT_THRESHOLD_TOKENS)]
    threshold: u64,
    /// Newest assistant steps kept verbatim.
    #[arg(long, default_value_t = 3)]
    keep_recent: usize,
    /// Percent (0-60) of the room above the verbatim head that the summary
    /// and the kept steps may fill: more steps than --keep-recent stay
    /// verbatim while they fit. 0 keeps exactly --keep-recent.
    #[arg(long, default_value_t = CliffConfig::default().keep_tail_percent,
          value_parser = keep_tail_percent_arg())]
    keep_tail_percent: u8,
    /// Threshold for Anthropic requests whose anthropic-beta header declares
    /// a 1M-token context window (a token starting with context-1m). Unset:
    /// the larger of 256,000 and --threshold. It may not be below
    /// --threshold, and equal to it applies one threshold to every request.
    /// Keep it below the client's own auto-compaction point.
    #[arg(long = "threshold-1m", value_name = "TOKENS")]
    threshold_1m: Option<u64>,
    /// Older tool results longer than this many characters are dropped from
    /// the summary; shorter ones are kept verbatim.
    #[arg(long, default_value_t = 500)]
    result_max_chars: usize,
    /// Leave thinking and reasoning text out of summaries.
    #[arg(long)]
    drop_thinking: bool,
    /// Observe only: log what would be compacted and forward every request
    /// unchanged.
    #[arg(long)]
    shadow: bool,
    /// Refuse (HTTP 400) a request still over the threshold after every
    /// escalation step, instead of sending it anyway.
    #[arg(long)]
    strict: bool,
    /// Upstream for Anthropic requests (Claude Code).
    #[arg(long, default_value = "https://api.anthropic.com")]
    anthropic_upstream: String,
    /// Upstream for OpenAI API requests (`/v1/...`, `.../chat/completions`).
    #[arg(long, default_value = "https://api.openai.com")]
    openai_upstream: String,
    /// Upstream for ChatGPT-authenticated Codex requests (`/backend-api/...`).
    #[arg(long, default_value = "https://chatgpt.com")]
    chatgpt_upstream: String,
}

/// `--keep-tail-percent` accepts 0 through `MAX_KEEP_TAIL_PERCENT`.
fn keep_tail_percent_arg() -> clap::builder::RangedI64ValueParser<u8> {
    clap::value_parser!(u8).range(0..=i64::from(MAX_KEEP_TAIL_PERCENT))
}

/// The 1M-window threshold when `--threshold-1m` is unset (or `--threshold`
/// when that is larger).
const DEFAULT_THRESHOLD_1M_TOKENS: u64 = 256_000;

/// The threshold for requests that declare a 1M-token window: the explicit
/// `--threshold-1m`, which may not be below `--threshold`, or the larger of
/// the default and `--threshold`.
fn threshold_1m(threshold: u64, explicit: Option<u64>) -> Result<u64> {
    match explicit {
        Some(value) if value < threshold => {
            bail!("--threshold-1m {value} is below --threshold {threshold}")
        }
        Some(value) => Ok(value),
        None => Ok(DEFAULT_THRESHOLD_1M_TOKENS.max(threshold)),
    }
}

/// Whether any `anthropic-beta` header line lists a token starting with
/// `context-1m`. Header names match case-insensitively and a request may
/// repeat the header (RFC 9110 section 5.3), so every line is checked; the
/// tokens are comma-separated and match case-sensitively. This runs outside
/// the engine's panic guard, so it uses no indexing.
fn declares_1m_context(headers: &[(String, String)]) -> bool {
    headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta"))
        .flat_map(|(_, value)| value.split(','))
        .any(|token| token.trim().starts_with("context-1m"))
}

/// Whether a request gets the 1M-window threshold: an Anthropic request
/// that declares the window. Every OpenAI-dialect request uses the base one.
fn declares_1m_window(request: &Request, dialect: Dialect) -> bool {
    dialect == Dialect::Anthropic && declares_1m_context(&request.headers)
}

/// The window tag on log lines and ledger records: `1m` or `base`.
fn window_name(request: &Request, dialect: Dialect) -> &'static str {
    if declares_1m_window(request, dialect) {
        "1m"
    } else {
        "base"
    }
}

/// Resolves a session argument to its provider and transcript path.
pub type Resolve<'a> = dyn Fn(&str) -> Result<(gobstopper_core::Provider, std::path::PathBuf)> + 'a;

pub fn run(command: &ProxyCmd, resolve: &Resolve) -> Result<()> {
    match command {
        ProxyCmd::Replay {
            session,
            threshold,
            keep_recent,
            keep_tail_percent,
            result_max_chars,
            fixed_tokens,
            json,
        } => {
            let (provider, path) = resolve(session)?;
            let (dialect, history) = replay::history_from_file(provider, &path)?;
            let cfg = CliffConfig {
                threshold_tokens: *threshold,
                keep_recent: *keep_recent,
                keep_tail_percent: *keep_tail_percent,
                result_max_chars: *result_max_chars,
                ..CliffConfig::default()
            };
            let report = replay::replay(&history, dialect, cfg, *fixed_tokens);
            if *json {
                println!("{}", serde_json::to_string_pretty(&report)?);
                return Ok(());
            }
            let k = |tokens: u64| format!("~{}k", tokens / 1000);
            println!(
                "replayed {} requests from {} ({} messages); threshold {} tokens, keep_recent {}, keep_tail_percent {}, {} fixed tokens assumed",
                report.requests,
                path.display(),
                history.len(),
                threshold,
                keep_recent,
                keep_tail_percent,
                fixed_tokens
            );
            println!(
                "  peak request: {} est tokens without the proxy, {} with it",
                k(report.peak_est_tokens_in),
                k(report.peak_est_tokens_out)
            );
            println!(
                "  compactions: {}; requests reusing a compacted prefix: {}; still over the threshold after compaction: {}",
                report.compacted, report.reused_prefix, report.over_threshold_after
            );
            if report.raised_threshold > 0 {
                println!(
                    "  requests where a large verbatim head raised the threshold: {}",
                    report.raised_threshold
                );
            }
            if report.total_est_tokens_in > 0 {
                let saved = 100.0
                    * (1.0
                        - report.total_est_tokens_out as f64 / report.total_est_tokens_in as f64);
                println!(
                    "  context sent across all requests: {} -> {} est tokens ({saved:.0}% less; estimates, not billed tokens)",
                    k(report.total_est_tokens_in),
                    k(report.total_est_tokens_out)
                );
            }
            println!(
                "  histories with broken tool pairing: {}",
                report.pairing_violations
            );
            Ok(())
        }
        ProxyCmd::Serve { opts, port } => {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, *port))
                .with_context(|| format!("bind 127.0.0.1:{port}"))?;
            let port = listener.local_addr()?.port();
            let proxy = Arc::new(Proxy::new(opts, port)?);
            println!("gobstopper proxy listening on http://127.0.0.1:{port}");
            std::io::stdout().flush()?;
            log(&format!(
                "{}, result_max_chars {}{}{}",
                proxy.settings(),
                opts.result_max_chars,
                if opts.shadow { ", shadow" } else { "" },
                if opts.strict { ", strict" } else { "" },
            ));
            serve(listener, proxy);
            Ok(())
        }
        ProxyCmd::Run { opts, command } => {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).context("bind 127.0.0.1")?;
            let port = listener.local_addr()?.port();
            let proxy = Arc::new(Proxy::new(opts, port)?);
            let server = Arc::clone(&proxy);
            std::thread::spawn(move || serve(listener, server));
            let base = format!("http://127.0.0.1:{port}");
            log(&format!("{base}, {}", proxy.settings()));
            let status = Command::new(&command[0])
                .args(&command[1..])
                .env("ANTHROPIC_BASE_URL", &base)
                .env("OPENAI_BASE_URL", format!("{base}/v1"))
                .status()
                .with_context(|| format!("run {}", command[0]))?;
            log(&proxy.summary());
            std::process::exit(status.code().unwrap_or(1));
        }
        ProxyCmd::Status { port, json } => {
            let status = fetch_status(*port)?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                print!("{}", status_text(*port, &status));
            }
            Ok(())
        }
    }
}

fn log(message: &str) {
    eprintln!("{} gobstopper proxy: {message}", rfc3339_now());
}

/// The `proxy status` text for a server's status JSON. Keys added after
/// 0.4.1 print only when the server returns them: after an upgrade the new
/// CLI can query a server that still runs the old binary.
fn status_text(port: u16, status: &Value) -> String {
    let present = |key: &str, render: &dyn Fn(&Value) -> String| {
        status
            .get(key)
            .filter(|value| !value.is_null())
            .map(render)
            .unwrap_or_default()
    };
    let mut text = format!(
        "gobstopper proxy on 127.0.0.1:{port}: threshold {} tokens{}, keep_recent {}{}{}\n",
        status["threshold_tokens"],
        present("threshold_1m_tokens", &|value| format!(
            ", threshold_1m {value} tokens"
        )),
        status["keep_recent"],
        present("keep_tail_percent", &|value| format!(
            ", keep_tail_percent {value}"
        )),
        if status["shadow"] == true {
            ", shadow"
        } else {
            ""
        }
    );
    text += &format!(
        "requests {}{}, compacted {}, reused a compacted prefix {}, retried after a length error {}, upstream errors {}, uptime {}s\n",
        status["requests"],
        present("requests_1m", &|value| format!(" ({value} with a 1M window)")),
        status["compacted"],
        status["matched"],
        status["reactive_retries"],
        status["upstream_errors"],
        status["uptime_secs"]
    );
    text += &format!(
        "estimated tokens this run: {} -> {} ({}% less); all time: {} -> {} ({}% less){}\n",
        status["est_tokens_in"],
        status["est_tokens_out"],
        pct(
            status["est_tokens_in"].as_u64(),
            status["est_tokens_out"].as_u64()
        ),
        status["all_time_est_tokens_in"],
        status["all_time_est_tokens_out"],
        pct(
            status["all_time_est_tokens_in"].as_u64(),
            status["all_time_est_tokens_out"].as_u64()
        ),
        status["stats_file"]
            .as_str()
            .map(|p| format!(", ledger {p}"))
            .unwrap_or_default(),
    );
    text
}

fn pct(input: Option<u64>, output: Option<u64>) -> u64 {
    let (Some(input), Some(output)) = (input, output) else {
        return 0;
    };
    if input == 0 {
        return 0;
    }
    input.saturating_sub(output) * 100 / input
}

fn validate_upstream(url: &str) -> Result<String> {
    let trimmed = url.trim_end_matches('/');
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        bail!("upstream must be an http:// or https:// URL: {url}");
    }
    if trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("upstream URL contains whitespace: {url}");
    }
    Ok(trimmed.to_string())
}

#[derive(Default)]
struct Stats {
    requests: AtomicU64,
    compacted: AtomicU64,
    matched: AtomicU64,
    reactive_retries: AtomicU64,
    upstream_errors: AtomicU64,
    fail_open: AtomicU64,
    /// Anthropic requests that declared a 1M-token window. Counted when the
    /// threshold is selected, so it includes requests the engine then left
    /// unchanged or failed open on.
    requests_1m: AtomicU64,
    /// Estimated tokens the clients sent this process lifetime.
    est_tokens_in: AtomicU64,
    /// Estimated tokens forwarded upstream this process lifetime.
    est_tokens_out: AtomicU64,
}

/// One JSONL record per compactable request, appended under the gobstopper
/// data dir so totals survive restarts. Records carry sizes and flags only.
struct StatsLog {
    writer: Mutex<Option<std::io::BufWriter<std::fs::File>>>,
    /// Totals recovered from the file at startup, before this process adds.
    prior_in: u64,
    prior_out: u64,
    path: Option<std::path::PathBuf>,
}

impl StatsLog {
    fn open() -> Self {
        let path = match std::env::var_os("GOBSTOPPER_STATS_FILE") {
            Some(value) if value == "off" => None,
            Some(value) => Some(std::path::PathBuf::from(value)),
            None => default_stats_path(),
        };
        let mut stats_log = StatsLog {
            writer: Mutex::new(None),
            prior_in: 0,
            prior_out: 0,
            path: None,
        };
        let Some(path) = path else { return stats_log };
        if let Ok(file) = std::fs::File::open(&path) {
            for line in std::io::BufRead::lines(std::io::BufReader::new(file)).map_while(Result::ok)
            {
                if let Ok(record) = serde_json::from_str::<Value>(&line) {
                    stats_log.prior_in += record["est_tokens_in"].as_u64().unwrap_or(0);
                    stats_log.prior_out += record["est_tokens_out"].as_u64().unwrap_or(0);
                }
            }
        }
        let opened = path
            .parent()
            .and_then(|dir| std::fs::create_dir_all(dir).ok())
            .and_then(|_| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .ok()
            });
        match opened {
            Some(file) => {
                stats_log.writer = Mutex::new(Some(std::io::BufWriter::new(file)));
                stats_log.path = Some(path);
            }
            None => {
                log(&format!(
                    "stats file {} is not writable; continuing without it",
                    path.display()
                ));
            }
        }
        stats_log
    }

    fn record(&self, ctx: &RequestCtx, request: &Request, shadow: bool) {
        if self.path.is_none() {
            return;
        }
        let est_tokens_out = if shadow {
            ctx.est_tokens_in
        } else {
            ctx.est_tokens_out
        };
        // The head, summary and tail sizes describe the list the engine
        // built; in shadow mode that list was not sent.
        let record = json!({
            "ts": rfc3339_now(),
            "dialect": ctx.dialect.name(),
            "path": request.path(),
            "est_tokens_in": ctx.est_tokens_in,
            "est_tokens_out": est_tokens_out,
            "est_head_tokens": ctx.est_head_tokens,
            "est_summary_tokens": ctx.est_summary_tokens,
            "est_tail_tokens": ctx.est_tail_tokens,
            "window": window_name(request, ctx.dialect),
            "threshold_tokens": ctx.threshold_tokens,
            "compacted": ctx.compacted,
            "reused_prefix": ctx.matched,
            "over_budget": ctx.over_budget,
            "rung": ctx.rung,
            "shadow": shadow,
        });
        if let Ok(mut guard) = self.writer.lock() {
            if let Some(writer) = guard.as_mut() {
                use std::io::Write;
                let _ = writeln!(writer, "{record}").and_then(|()| writer.flush());
            }
        }
    }

    fn totals(&self, stats: &Stats) -> (u64, u64) {
        (
            self.prior_in + stats.est_tokens_in.load(Ordering::Relaxed),
            self.prior_out + stats.est_tokens_out.load(Ordering::Relaxed),
        )
    }
}

fn default_stats_path() -> Option<std::path::PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/share"))
        })?;
    Some(data.join("gobstopper").join("proxy-stats.jsonl"))
}

struct Proxy {
    engine: Engine,
    /// Threshold for Anthropic requests that declare a 1M-token window.
    threshold_1m: u64,
    shadow: bool,
    anthropic: String,
    openai: String,
    chatgpt: String,
    port: u16,
    started: Instant,
    active: AtomicUsize,
    stats: Stats,
    stats_log: StatsLog,
}

impl Proxy {
    fn new(opts: &ProxyOpts, port: u16) -> Result<Self> {
        let cfg = CliffConfig {
            threshold_tokens: opts.threshold,
            keep_recent: opts.keep_recent,
            keep_tail_percent: opts.keep_tail_percent,
            result_max_chars: opts.result_max_chars,
            keep_thinking: !opts.drop_thinking,
            strict: opts.strict,
            ..CliffConfig::default()
        };
        Ok(Self {
            threshold_1m: threshold_1m(opts.threshold, opts.threshold_1m)?,
            engine: Engine::new(cfg),
            shadow: opts.shadow,
            anthropic: validate_upstream(&opts.anthropic_upstream)?,
            openai: validate_upstream(&opts.openai_upstream)?,
            chatgpt: validate_upstream(&opts.chatgpt_upstream)?,
            port,
            started: Instant::now(),
            active: AtomicUsize::new(0),
            stats: Stats::default(),
            stats_log: StatsLog::open(),
        })
    }

    fn count(&self, counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

    fn status(&self) -> Value {
        let cfg = self.engine.config();
        let (entries, chars) = self.engine.store_stats();
        let totals = self.stats_log.totals(&self.stats);
        json!({
            "name": "gobstopper-proxy",
            "version": env!("CARGO_PKG_VERSION"),
            "port": self.port,
            "threshold_tokens": cfg.threshold_tokens,
            "threshold_1m_tokens": self.threshold_1m,
            "keep_recent": cfg.keep_recent,
            "keep_tail_percent": cfg.keep_tail_percent,
            "result_max_chars": cfg.result_max_chars,
            "keep_thinking": cfg.keep_thinking,
            "shadow": self.shadow,
            "strict": cfg.strict,
            "store_entries": entries,
            "store_chars": chars,
            "requests": self.count(&self.stats.requests),
            "compacted": self.count(&self.stats.compacted),
            "matched": self.count(&self.stats.matched),
            "reactive_retries": self.count(&self.stats.reactive_retries),
            "upstream_errors": self.count(&self.stats.upstream_errors),
            "fail_open": self.count(&self.stats.fail_open),
            "requests_1m": self.count(&self.stats.requests_1m),
            "est_tokens_in": self.count(&self.stats.est_tokens_in),
            "est_tokens_out": self.count(&self.stats.est_tokens_out),
            "all_time_est_tokens_in": totals.0,
            "all_time_est_tokens_out": totals.1,
            "stats_file": self.stats_log.path.as_ref().map(|p| p.display().to_string()),
            "active_connections": self.active.load(Ordering::Relaxed),
            "uptime_secs": self.started.elapsed().as_secs(),
        })
    }

    /// The settings both startup lines show (serve and run).
    fn settings(&self) -> String {
        let cfg = self.engine.config();
        format!(
            "threshold {} tokens, threshold_1m {} tokens, keep_recent {}, keep_tail_percent {}",
            cfg.threshold_tokens, self.threshold_1m, cfg.keep_recent, cfg.keep_tail_percent
        )
    }

    fn summary(&self) -> String {
        format!(
            "{} requests, {} compacted, {} reused a compacted prefix",
            self.count(&self.stats.requests),
            self.count(&self.stats.compacted),
            self.count(&self.stats.matched)
        )
    }

    /// Anthropic clients always send `anthropic-version`; ChatGPT-backed
    /// Codex uses `/backend-api/`; `/v1/` paths and Chat Completions calls
    /// are OpenAI-compatible traffic; anything else defaults to Anthropic,
    /// the client most likely to send it. The `/chat/completions` arm is
    /// needed because LiteLLM-style clients may post to the bare path.
    fn upstream_for(&self, request: &Request) -> &str {
        let path = request.path();
        if request.header("anthropic-version").is_some() {
            &self.anthropic
        } else if path.starts_with("/backend-api/") {
            &self.chatgpt
        } else if path.starts_with("/v1/") || path.ends_with("/chat/completions") {
            &self.openai
        } else {
            &self.anthropic
        }
    }

    /// `threshold_1m` for an Anthropic request that declares a 1M-token
    /// window, counted in `requests_1m`; the configured threshold for every
    /// other request. The header gives the model's window, never the model
    /// name.
    fn threshold_for(&self, request: &Request, dialect: Dialect) -> u64 {
        if declares_1m_window(request, dialect) {
            self.stats.requests_1m.fetch_add(1, Ordering::Relaxed);
            self.threshold_1m
        } else {
            self.engine.config().threshold_tokens
        }
    }

    fn prepare(&self, request: &Request) -> Option<RequestCtx> {
        if request.method != "POST" || request.body.is_empty() {
            return None;
        }
        let dialect = Dialect::detect(request.path())?;
        if request
            .header("content-encoding")
            .is_some_and(|encoding| !encoding.eq_ignore_ascii_case("identity"))
        {
            self.stats.fail_open.fetch_add(1, Ordering::Relaxed);
            log(&format!(
                "{} {}: compressed body, forwarded unchanged",
                dialect.name(),
                request.path()
            ));
            return None;
        }
        let parsed = match serde_json::from_slice::<Value>(&request.body) {
            Ok(Value::Object(map)) => map,
            _ => {
                self.stats.fail_open.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        };
        let threshold = self.threshold_for(request, dialect);
        // Engine failures must never fail the request.
        let prepared = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.engine.prepare_at(parsed, dialect, threshold)
        }));
        match prepared {
            Ok(ctx) => ctx,
            Err(_) => {
                self.stats.fail_open.fetch_add(1, Ordering::Relaxed);
                log(&format!(
                    "{} {}: engine error, forwarded unchanged",
                    dialect.name(),
                    request.path()
                ));
                None
            }
        }
    }

    fn report(&self, ctx: &RequestCtx, request: &Request) {
        self.stats
            .est_tokens_in
            .fetch_add(ctx.est_tokens_in, Ordering::Relaxed);
        // In shadow mode the original bytes are forwarded, so the counters
        // record the size actually sent, not the hypothetical compacted size.
        self.stats.est_tokens_out.fetch_add(
            if self.shadow {
                ctx.est_tokens_in
            } else {
                ctx.est_tokens_out
            },
            Ordering::Relaxed,
        );
        self.stats_log.record(ctx, request, self.shadow);
        let kind = ctx.dialect.name();
        let path = request.path();
        // Sizes and the window only, never content or header values.
        let sizes = format!(
            "(head ~{}k, summary ~{}k, tail ~{}k)",
            ctx.est_head_tokens / 1000,
            ctx.est_summary_tokens / 1000,
            ctx.est_tail_tokens / 1000,
        );
        let window = window_name(request, ctx.dialect);
        let shadow = if self.shadow {
            " (shadow: sent unchanged)"
        } else {
            ""
        };
        if ctx.compacted {
            self.stats.compacted.fetch_add(1, Ordering::Relaxed);
            // Compared against the threshold selected for this request, so a
            // 1M-window request at `threshold_1m` is not reported as raised.
            let raised = if ctx.threshold_tokens > ctx.base_threshold_tokens {
                format!(
                    ", threshold raised to ~{}k by a large verbatim head",
                    ctx.threshold_tokens / 1000
                )
            } else {
                String::new()
            };
            log(&format!(
                "{kind} {path}: compacted ~{}k -> ~{}k est tokens {sizes}, {} -> {} messages, {} crossing(s), step {}, window={window}{raised}{}{shadow}",
                ctx.est_tokens_in / 1000,
                ctx.est_tokens_out / 1000,
                ctx.original_len(),
                ctx.messages().len(),
                ctx.chain_steps,
                ctx.rung,
                if ctx.over_budget { ", still over threshold" } else { "" },
            ));
        } else if ctx.matched {
            self.stats.matched.fetch_add(1, Ordering::Relaxed);
            log(&format!(
                "{kind} {path}: reused compacted prefix, ~{}k -> ~{}k est tokens {sizes}, window={window}{shadow}",
                ctx.est_tokens_in / 1000,
                ctx.est_tokens_out / 1000,
            ));
        } else if ctx.est_tokens_in > ctx.base_threshold_tokens {
            // Over the selected threshold but sent unchanged. Say why, or the
            // request looks missed.
            let why = if ctx.est_tokens_in <= ctx.threshold_tokens {
                format!(
                    "under the threshold raised to ~{}k by a large verbatim head",
                    ctx.threshold_tokens / 1000
                )
            } else {
                format!(
                    "over the ~{}k threshold with nothing to compact",
                    ctx.threshold_tokens / 1000
                )
            };
            log(&format!(
                "{kind} {path}: sent unchanged at ~{}k est tokens, {why}, window={window}",
                ctx.est_tokens_in / 1000,
            ));
        }
    }

    fn handle(&self, mut client: TcpStream) -> Result<()> {
        client.set_read_timeout(Some(Duration::from_secs(120)))?;
        client.set_write_timeout(Some(Duration::from_secs(600)))?;
        let _ = client.set_nodelay(true);
        let request = match read_request(&mut client) {
            Ok(request) => request,
            Err(error) => {
                return write_error(
                    &mut client,
                    400,
                    "invalid_request_error",
                    &format!("gobstopper proxy: {error}"),
                );
            }
        };
        if !host_is_loopback(request.header("host")) {
            return write_error(
                &mut client,
                403,
                "permission_error",
                "gobstopper proxy: only loopback hosts are served",
            );
        }
        if request.path() == STATUS_PATH {
            let body = serde_json::to_vec_pretty(&self.status())?;
            return write_buffered(
                &mut client,
                200,
                "OK",
                &[("content-type".into(), "application/json".into())],
                &body,
            );
        }
        self.stats.requests.fetch_add(1, Ordering::Relaxed);
        self.forward(&request, &mut client)
    }

    fn forward(&self, request: &Request, client: &mut TcpStream) -> Result<()> {
        let upstream = self.upstream_for(request);
        let mut ctx = self.prepare(request);
        let mut body: Cow<[u8]> = Cow::Borrowed(&request.body);
        if let Some(ctx) = &ctx {
            self.report(ctx, request);
            if ctx.modified && !self.shadow {
                body = Cow::Owned(serde_json::to_vec(&ctx.outgoing_body())?);
            }
            if ctx.over_budget && self.engine.config().strict && !self.shadow {
                return write_error(
                    client,
                    400,
                    "gobstopper_over_budget",
                    &format!(
                        "gobstopper proxy strict mode: ~{} est tokens after every compaction step, over the {} token threshold",
                        ctx.est_tokens_out, ctx.base_threshold_tokens
                    ),
                );
            }
        }
        let response = match send_upstream(request, upstream, &body) {
            Ok(response) => response,
            Err(error) => return self.upstream_failed(client, &error),
        };
        match ctx.as_mut() {
            Some(ctx) if response.status == 400 && !self.shadow => {
                self.reactive(request, upstream, ctx, response, client)
            }
            _ => relay(client, response, &request.method),
        }
    }

    /// The provider rejected the request: when it rejected it for length,
    /// walk the escalation ladder and replay until it is accepted.
    fn reactive(
        &self,
        request: &Request,
        upstream: &str,
        ctx: &mut RequestCtx,
        response: Upstream,
        client: &mut TcpStream,
    ) -> Result<()> {
        let (mut status, mut headers) = (response.status, response.headers.clone());
        let mut data = response.read_all(MAX_ERROR_BODY_BYTES)?;
        if ctx.modified && !is_context_error(&data) {
            // The provider refused the rewritten request for another reason:
            // send the client's original bytes once, so the proxy never leaves
            // a session worse off than running without it.
            self.stats.fail_open.fetch_add(1, Ordering::Relaxed);
            log(&format!(
                "{} {}: provider rejected the compacted request; resending the original",
                ctx.dialect.name(),
                request.path()
            ));
            return match send_upstream(request, upstream, &request.body) {
                Ok(original) => relay(client, original, &request.method),
                Err(error) => self.upstream_failed(client, &error),
            };
        }
        while is_context_error(&data)
            && self.guarded_step(ctx.dialect, request.path(), || self.engine.reactive(ctx))
        {
            self.stats.reactive_retries.fetch_add(1, Ordering::Relaxed);
            log(&format!(
                "{} {}: provider rejected the length; replaying at ~{}k est tokens (step {})",
                ctx.dialect.name(),
                request.path(),
                ctx.est_tokens_out / 1000,
                ctx.rung
            ));
            let body = serde_json::to_vec(&ctx.outgoing_body())?;
            let again = match send_upstream(request, upstream, &body) {
                Ok(again) => again,
                Err(error) => return self.upstream_failed(client, &error),
            };
            if again.status != 400 {
                return relay(client, again, &request.method);
            }
            status = again.status;
            headers = again.headers.clone();
            data = again.read_all(MAX_ERROR_BODY_BYTES)?;
        }
        let headers: Vec<(String, String)> = headers
            .into_iter()
            .filter(|(name, _)| !STRIP_RESPONSE.contains(&name.to_ascii_lowercase().as_str()))
            .collect();
        write_buffered(client, status, "Bad Request", &headers, &data)
    }

    /// One reactive ladder step, which runs the compaction chain outside the
    /// `catch_unwind` in `prepare`. A panic there must not fail the request:
    /// it counts `fail_open` and stops the ladder (`false`), so the caller
    /// relays the provider's last 400 unchanged.
    fn guarded_step(&self, dialect: Dialect, path: &str, step: impl FnOnce() -> bool) -> bool {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(step)) {
            Ok(retry) => retry,
            Err(_) => {
                self.stats.fail_open.fetch_add(1, Ordering::Relaxed);
                log(&format!(
                    "{} {path}: engine error during the length retry; relaying the provider's response",
                    dialect.name()
                ));
                false
            }
        }
    }

    fn upstream_failed(&self, client: &mut TcpStream, error: &anyhow::Error) -> Result<()> {
        self.stats.upstream_errors.fetch_add(1, Ordering::Relaxed);
        log(&format!("upstream request failed: {error:#}"));
        write_error(
            client,
            502,
            "api_error",
            &format!("gobstopper proxy: upstream request failed: {error:#}"),
        )
    }
}

fn serve(listener: TcpListener, proxy: Arc<Proxy>) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        if proxy.active.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            let _ = write_error(
                &mut stream,
                503,
                "overloaded_error",
                "gobstopper proxy: too many connections",
            );
            continue;
        }
        let proxy = Arc::clone(&proxy);
        proxy.active.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            struct Active<'a>(&'a AtomicUsize);
            impl Drop for Active<'_> {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::Relaxed);
                }
            }
            let _active = Active(&proxy.active);
            if let Err(error) = proxy.handle(stream) {
                log(&format!("connection error: {error:#}"));
            }
        });
    }
}

fn host_is_loopback(host: Option<&str>) -> bool {
    let Some(host) = host else {
        return true;
    };
    let name = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        host.rsplit_once(':').map_or(host, |(name, _)| name)
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "::1"
    )
}

/// Providers phrase context-overflow errors inconsistently; match broadly
/// (case-insensitive, `.` = any one character, `.?` = optional one).
fn is_context_error(body: &[u8]) -> bool {
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    let text = text.as_bytes();
    const PATTERNS: &[(&[&str], bool)] = &[
        (&["context", "length"], true),
        (&["context", "window"], true),
        (&["prompt is too long"], false),
        (&["input", "tokens"], false),
        (&["maximum", "context"], false),
        (&["exceeds", "limit"], false),
        (&["too", "many", "tokens"], false),
        (&["reduce", "the", "length"], false),
        (&["reduce", "the", "input"], false),
    ];
    PATTERNS.iter().any(|(parts, optional)| {
        (0..text.len()).any(|start| matches_at(text, start, parts, *optional))
    })
}

fn matches_at(text: &[u8], at: usize, parts: &[&str], optional_gap: bool) -> bool {
    let Some((first, rest)) = parts.split_first() else {
        return true;
    };
    if !text[at.min(text.len())..].starts_with(first.as_bytes()) {
        return false;
    }
    let end = at + first.len();
    if rest.is_empty() {
        return true;
    }
    let gaps: &[usize] = if optional_gap { &[0, 1] } else { &[1] };
    gaps.iter()
        .any(|gap| end + gap <= text.len() && matches_at(text, end + gap, rest, optional_gap))
}

struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or(&self.target)
    }
}

/// Buffered reads from the client socket.
struct Wire<'a> {
    stream: &'a mut TcpStream,
    buffer: Vec<u8>,
}

impl Wire<'_> {
    fn fill(&mut self) -> Result<()> {
        let mut chunk = [0u8; 16 * 1024];
        let read = self.stream.read(&mut chunk)?;
        if read == 0 {
            bail!("connection closed mid-request");
        }
        self.buffer.extend_from_slice(&chunk[..read]);
        Ok(())
    }

    fn take_until(&mut self, delimiter: &[u8], limit: usize) -> Result<Vec<u8>> {
        loop {
            if let Some(at) = find(&self.buffer, delimiter) {
                let taken = self.buffer[..at].to_vec();
                self.buffer.drain(..at + delimiter.len());
                return Ok(taken);
            }
            if self.buffer.len() > limit {
                bail!("request head too large");
            }
            self.fill()?;
        }
    }

    fn take_exact(&mut self, len: usize) -> Result<Vec<u8>> {
        while self.buffer.len() < len {
            self.fill()?;
        }
        Ok(self.buffer.drain(..len).collect())
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn read_request(stream: &mut TcpStream) -> Result<Request> {
    let mut wire = Wire {
        stream,
        buffer: Vec::new(),
    };
    let head = wire.take_until(b"\r\n\r\n", MAX_HEAD_BYTES)?;
    let head = String::from_utf8(head).context("request head is not UTF-8")?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        bail!("malformed request line");
    };
    if !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        bail!("unsupported request line");
    }
    let mut headers = Vec::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            bail!("malformed header line");
        };
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }
    let mut request = Request {
        method: method.to_string(),
        target: target.to_string(),
        headers,
        body: Vec::new(),
    };
    if request
        .header("expect")
        .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"))
    {
        wire.stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
    }
    let chunked = request
        .header("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"));
    if chunked {
        let mut body = Vec::new();
        loop {
            let size_line = wire.take_until(b"\r\n", MAX_HEAD_BYTES)?;
            let size_text = String::from_utf8_lossy(&size_line);
            let size_text = size_text.split(';').next().unwrap_or("").trim();
            let size = usize::from_str_radix(size_text, 16).context("invalid chunk size")?;
            if size == 0 {
                // Trailers, then the final empty line.
                while !wire.take_until(b"\r\n", MAX_HEAD_BYTES)?.is_empty() {}
                break;
            }
            if body.len() + size > MAX_BODY_BYTES {
                bail!("request body too large");
            }
            body.extend(wire.take_exact(size)?);
            wire.take_exact(2)?;
        }
        request.body = body;
    } else if let Some(length) = request.header("content-length") {
        let length: usize = length.parse().context("invalid content-length")?;
        if length > MAX_BODY_BYTES {
            bail!("request body too large");
        }
        request.body = wire.take_exact(length)?;
    }
    Ok(request)
}

/// A response streaming from a curl child.
struct Upstream {
    child: Child,
    stdout: BufReader<ChildStdout>,
    status: u16,
    reason: String,
    headers: Vec<(String, String)>,
}

impl Upstream {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn read_all(mut self, limit: usize) -> Result<Vec<u8>> {
        let mut data = Vec::new();
        (&mut self.stdout)
            .take(limit as u64)
            .read_to_end(&mut data)?;
        let _ = self.child.kill();
        let _ = self.child.wait();
        Ok(data)
    }
}

fn send_upstream(request: &Request, upstream: &str, body: &[u8]) -> Result<Upstream> {
    let mut command = Command::new("curl");
    command
        .args(["-q", "-sS", "-N", "-i", "--suppress-connect-headers"])
        .args(["--proto", "=http,https", "--connect-timeout", "30"])
        .args([
            "--speed-time",
            "900",
            "--speed-limit",
            "1",
            "--max-time",
            "7200",
        ])
        .args(["-X", request.method.as_str()])
        .args(["-H", "Expect:", "-H", "Accept-Encoding: identity"]);
    if request.header("content-type").is_none() {
        // Without this, curl labels a posted body as a form.
        command.args(["-H", "Content-Type:"]);
    }
    let mut index = 0;
    for (name, value) in &request.headers {
        let lower = name.to_ascii_lowercase();
        if STRIP_REQUEST.contains(&lower.as_str())
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            continue;
        }
        let variable = format!("GOBSTOPPER_PROXY_H{index}");
        command
            .arg("--variable")
            .arg(format!("%{variable}"))
            .arg("--expand-header")
            .arg(format!("{name}: {{{{{variable}}}}}"))
            .env(&variable, value);
        index += 1;
    }
    let sends_body =
        !body.is_empty() || matches!(request.method.as_str(), "POST" | "PUT" | "PATCH");
    if sends_body {
        command.args(["--data-binary", "@-"]);
    }
    command
        .arg("--url")
        .arg(format!("{upstream}{}", request.target))
        .stdin(if sends_body {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().context("start curl (is it installed?)")?;
    if let Some(mut stdin) = child.stdin.take() {
        let body = body.to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&body);
        });
    }
    let stdout = child.stdout.take().context("curl stdout")?;
    let mut stdout = BufReader::new(stdout);
    loop {
        match read_response_head(&mut stdout) {
            Ok((status, _, _)) if (100..200).contains(&status) => continue,
            Ok((status, reason, headers)) => {
                return Ok(Upstream {
                    child,
                    stdout,
                    status,
                    reason,
                    headers,
                })
            }
            Err(error) => {
                let _ = child.kill();
                let output = child.wait_with_output().ok();
                let detail = output
                    .map(|output| String::from_utf8_lossy(&output.stderr).trim().to_string())
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| format!("{error:#}"));
                bail!("{detail}");
            }
        }
    }
}

type Headers = Vec<(String, String)>;

fn read_response_head(stdout: &mut impl BufRead) -> Result<(u16, String, Headers)> {
    let mut line = Vec::new();
    let mut total = 0;
    let mut read_line = |line: &mut Vec<u8>| -> Result<String> {
        line.clear();
        let read = stdout.read_until(b'\n', line)?;
        total += read;
        if read == 0 {
            bail!("upstream closed before a response");
        }
        if total > MAX_HEAD_BYTES {
            bail!("upstream response head too large");
        }
        Ok(String::from_utf8_lossy(line)
            .trim_end_matches(['\r', '\n'])
            .to_string())
    };
    let status_line = read_line(&mut line)?;
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/") {
        bail!("unexpected upstream status line");
    }
    let status: u16 = parts
        .next()
        .and_then(|code| code.parse().ok())
        .context("unexpected upstream status code")?;
    let reason = parts.next().unwrap_or("").to_string();
    let mut headers = Vec::new();
    loop {
        let header = read_line(&mut line)?;
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    Ok((status, reason, headers))
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        529 => "Overloaded",
        _ => "Status",
    }
}

fn write_head(
    client: &mut impl Write,
    status: u16,
    reason: &str,
    headers: &[(String, String)],
    framing: &str,
) -> std::io::Result<()> {
    let reason = if reason.is_empty() {
        reason_phrase(status)
    } else {
        reason
    };
    let mut head = format!("HTTP/1.1 {status} {reason}\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(framing);
    head.push_str("connection: close\r\n\r\n");
    client.write_all(head.as_bytes())
}

fn write_buffered(
    client: &mut TcpStream,
    status: u16,
    reason: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<()> {
    write_head(
        client,
        status,
        reason,
        headers,
        &format!("content-length: {}\r\n", body.len()),
    )?;
    client.write_all(body)?;
    client.flush()?;
    Ok(())
}

fn write_error(client: &mut TcpStream, status: u16, kind: &str, message: &str) -> Result<()> {
    let body = serde_json::to_vec(&json!({
        "type": "error",
        "error": {"type": kind, "message": message},
    }))?;
    write_buffered(
        client,
        status,
        "",
        &[("content-type".into(), "application/json".into())],
        &body,
    )
}

/// Stream an upstream response to the client, re-framing the body.
fn relay(client: &mut TcpStream, mut upstream: Upstream, method: &str) -> Result<()> {
    let headers: Vec<(String, String)> = upstream
        .headers
        .iter()
        .filter(|(name, _)| !STRIP_RESPONSE.contains(&name.to_ascii_lowercase().as_str()))
        .cloned()
        .collect();
    let bodyless = method == "HEAD" || matches!(upstream.status, 204 | 304);
    let length = upstream
        .header("content-length")
        .filter(|_| upstream.header("transfer-encoding").is_none())
        .and_then(|value| value.parse::<u64>().ok());
    let result = (|| -> Result<bool> {
        if bodyless {
            write_head(client, upstream.status, &upstream.reason, &headers, "")?;
            return Ok(true);
        }
        let mut buffer = vec![0u8; 64 * 1024];
        match length {
            Some(length) => {
                write_head(
                    client,
                    upstream.status,
                    &upstream.reason,
                    &headers,
                    &format!("content-length: {length}\r\n"),
                )?;
                let copied = std::io::copy(&mut (&mut upstream.stdout).take(length), client)?;
                client.flush()?;
                Ok(copied == length)
            }
            None => {
                write_head(
                    client,
                    upstream.status,
                    &upstream.reason,
                    &headers,
                    "transfer-encoding: chunked\r\n",
                )?;
                loop {
                    let read = upstream.stdout.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    client.write_all(format!("{read:x}\r\n").as_bytes())?;
                    client.write_all(&buffer[..read])?;
                    client.write_all(b"\r\n")?;
                    client.flush()?;
                }
                Ok(true)
            }
        }
    })();
    let complete = match result {
        Ok(complete) => complete,
        Err(error) => {
            // The client went away: stop the upstream transfer too.
            let _ = upstream.child.kill();
            let _ = upstream.child.wait();
            return Err(error);
        }
    };
    let exit = upstream.child.wait().ok().and_then(|status| status.code());
    let exited_cleanly = exit == Some(0);
    if complete && exited_cleanly && length.is_none() && !bodyless {
        client.write_all(b"0\r\n\r\n")?;
        client.flush()?;
    } else if !complete || (!exited_cleanly && length.is_none() && !bodyless) {
        // A fixed-length body that arrived whole is complete even when the
        // upstream closed the connection afterwards.
        log(&format!(
            "upstream stream ended early (curl exit {}); the client sees a truncated response",
            exit.map_or_else(|| "signal".to_string(), |code| code.to_string())
        ));
    }
    Ok(())
}

fn fetch_status(port: u16) -> Result<Value> {
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
        .with_context(|| format!("no gobstopper proxy on 127.0.0.1:{port}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.write_all(
        format!(
            "GET {STATUS_PATH} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let at = find(&response, b"\r\n\r\n").context("malformed status response")?;
    serde_json::from_slice(&response[at + 4..]).context("status response is not JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_provider_length_errors_only() {
        for body in [
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 213000 tokens > 200000 maximum"}}"#,
            r#"{"error":{"code":"context_length_exceeded","message":"Your input exceeds the context window of this model."}}"#,
            "This model's maximum context length is 128000 tokens",
            "Input tokens exceed the configured limit",
        ] {
            assert!(is_context_error(body.as_bytes()), "{body}");
        }
        for body in [
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: must be positive"}}"#,
            "rate limited",
            "",
        ] {
            assert!(!is_context_error(body.as_bytes()), "{body}");
        }
    }

    #[test]
    fn only_loopback_hosts_are_served() {
        for host in ["127.0.0.1:8260", "localhost", "LOCALHOST:1", "[::1]:8260"] {
            assert!(host_is_loopback(Some(host)), "{host}");
        }
        for host in ["evil.example:8260", "192.168.1.2", "[fe80::1]:80"] {
            assert!(!host_is_loopback(Some(host)), "{host}");
        }
        assert!(host_is_loopback(None));
    }

    #[test]
    fn parses_curl_response_heads_after_informational_blocks() {
        let raw = b"HTTP/2 200\r\ncontent-type: text/event-stream\r\nx-request-id: abc\r\n\r\ndata: {}\n\n";
        let mut reader = BufReader::new(&raw[..]);
        let (status, reason, headers) = read_response_head(&mut reader).unwrap();
        assert_eq!((status, reason.as_str()), (200, ""));
        assert_eq!(
            headers[0],
            ("content-type".into(), "text/event-stream".into())
        );
        let mut rest = String::new();
        reader.read_to_string(&mut rest).unwrap();
        assert_eq!(rest, "data: {}\n\n");
        assert!(read_response_head(&mut BufReader::new(&b""[..])).is_err());
    }

    #[test]
    fn rejects_non_http_upstreams() {
        assert_eq!(
            validate_upstream("https://api.anthropic.com/").unwrap(),
            "https://api.anthropic.com"
        );
        assert!(validate_upstream("file:///etc/passwd").is_err());
        assert!(validate_upstream("https://a b").is_err());
    }

    fn headers(lines: &[(&str, &str)]) -> Vec<(String, String)> {
        lines
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn a_context_1m_token_on_any_beta_line_declares_the_long_window() {
        let declared: [&[(&str, &str)]; 6] = [
            &[("anthropic-beta", "context-1m-2025-08-07")],
            &[(
                "anthropic-beta",
                "claude-code-20250219,context-1m-2025-08-07,interleaved-thinking-2025-05-14",
            )],
            &[
                ("anthropic-beta", "claude-code-20250219"),
                ("anthropic-beta", "context-1m-2025-08-07"),
            ],
            &[(
                "anthropic-beta",
                "  claude-code-20250219 ,   context-1m-2025-08-07  ",
            )],
            &[("Anthropic-Beta", "context-1m-2025-08-07")],
            &[
                ("ANTHROPIC-BETA", "fine-grained-tool-streaming-2025-05-14"),
                ("content-type", "application/json"),
                ("anthropic-beta", "context-1m"),
            ],
        ];
        for lines in declared {
            assert!(declares_1m_context(&headers(lines)), "{lines:?}");
        }
        let undeclared: [&[(&str, &str)]; 10] = [
            &[],
            &[(
                "anthropic-beta",
                "claude-code-20250219,interleaved-thinking-2025-05-14",
            )],
            &[("anthropic-beta", "xcontext-1m-2025-08-07")],
            &[("anthropic-beta", "context-1")],
            &[("anthropic-beta", "Context-1M-2025-08-07")],
            &[("anthropic-beta", "")],
            &[("anthropic-beta", ", ,")],
            &[("x-anthropic-beta", "context-1m-2025-08-07")],
            &[("anthropic-version", "context-1m-2025-08-07")],
            &[("anthropic-beta-extra", "context-1m-2025-08-07")],
        ];
        for lines in undeclared {
            assert!(!declares_1m_context(&headers(lines)), "{lines:?}");
        }
    }

    #[test]
    fn the_1m_threshold_defaults_above_the_base_and_is_never_below_it() {
        assert_eq!(threshold_1m(128_000, None).unwrap(), 256_000);
        assert_eq!(threshold_1m(300_000, None).unwrap(), 300_000);
        assert_eq!(threshold_1m(128_000, Some(400_000)).unwrap(), 400_000);
        assert_eq!(threshold_1m(128_000, Some(200_000)).unwrap(), 200_000);
        // Equal to the base threshold: one threshold for every request.
        assert_eq!(threshold_1m(128_000, Some(128_000)).unwrap(), 128_000);
        let error = threshold_1m(128_000, Some(127_999))
            .unwrap_err()
            .to_string();
        assert_eq!(error, "--threshold-1m 127999 is below --threshold 128000");
    }

    #[derive(clap::Parser)]
    struct OptsOnly {
        #[command(flatten)]
        opts: ProxyOpts,
    }

    #[test]
    fn threshold_1m_is_an_optional_token_count() {
        use clap::Parser;
        let unset = OptsOnly::try_parse_from(["proxy"]).unwrap().opts;
        assert_eq!(unset.threshold_1m, None);
        let set = OptsOnly::try_parse_from([
            "proxy",
            "--threshold",
            "100000",
            "--threshold-1m",
            "500000",
        ])
        .unwrap()
        .opts;
        assert_eq!((set.threshold, set.threshold_1m), (100_000, Some(500_000)));
        assert!(OptsOnly::try_parse_from(["proxy", "--threshold-1m", "many"]).is_err());
    }

    /// A proxy without a stats file, so unit tests touch no disk.
    fn test_proxy(threshold_tokens: u64, threshold_1m: u64) -> Proxy {
        Proxy {
            engine: Engine::new(CliffConfig {
                threshold_tokens,
                ..CliffConfig::default()
            }),
            threshold_1m,
            shadow: false,
            anthropic: "https://api.anthropic.com".into(),
            openai: "https://api.openai.com".into(),
            chatgpt: "https://chatgpt.com".into(),
            port: 0,
            started: Instant::now(),
            active: AtomicUsize::new(0),
            stats: Stats::default(),
            stats_log: StatsLog {
                writer: Mutex::new(None),
                prior_in: 0,
                prior_out: 0,
                path: None,
            },
        }
    }

    fn request(target: &str, lines: &[(&str, &str)], body: &Value) -> Request {
        Request {
            method: "POST".into(),
            target: target.into(),
            headers: headers(lines),
            body: serde_json::to_vec(body).unwrap(),
        }
    }

    #[test]
    fn only_anthropic_requests_that_declare_1m_are_prepared_at_the_1m_threshold() {
        let proxy = test_proxy(128_000, 256_000);
        let one_m = [
            ("anthropic-version", "2023-06-01"),
            (
                "anthropic-beta",
                "claude-code-20250219,context-1m-2025-08-07",
            ),
        ];
        let messages = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
        let input = json!({"model": "m", "input": [{"role": "user", "content": "hi"}]});

        let ctx = proxy
            .prepare(&request("/v1/messages?beta=true", &one_m, &messages))
            .unwrap();
        assert_eq!(ctx.base_threshold_tokens, 256_000);
        assert_eq!(proxy.count(&proxy.stats.requests_1m), 1);

        let base = [("anthropic-version", "2023-06-01")];
        let ctx = proxy
            .prepare(&request("/v1/messages", &base, &messages))
            .unwrap();
        assert_eq!(ctx.base_threshold_tokens, 128_000);

        // The header means nothing to the OpenAI dialects.
        let ctx = proxy
            .prepare(&request("/v1/responses", &one_m, &input))
            .unwrap();
        assert_eq!(ctx.base_threshold_tokens, 128_000);
        assert_eq!(proxy.count(&proxy.stats.requests_1m), 1);

        let status = proxy.status();
        assert_eq!(
            (&status["threshold_1m_tokens"], &status["requests_1m"]),
            (&json!(256_000), &json!(1))
        );
        assert_eq!(status["threshold_tokens"], 128_000);
    }

    #[test]
    fn a_panicking_reactive_step_fails_open_and_stops_the_ladder() {
        let proxy = test_proxy(128_000, 256_000);
        let path = "/v1/messages";
        assert!(proxy.guarded_step(Dialect::Anthropic, path, || true));
        assert!(!proxy.guarded_step(Dialect::Anthropic, path, || false));
        assert_eq!(proxy.count(&proxy.stats.fail_open), 0);
        let retry = proxy.guarded_step(Dialect::Anthropic, path, || {
            panic!("synthetic engine failure")
        });
        assert!(!retry, "a panic stops the ladder");
        assert_eq!(proxy.count(&proxy.stats.fail_open), 1);
        assert_eq!(proxy.status()["fail_open"], 1);
    }

    #[test]
    fn the_window_tag_follows_the_threshold_selection() {
        let one_m = [
            ("anthropic-version", "2023-06-01"),
            ("anthropic-beta", "context-1m-2025-08-07"),
        ];
        let body = json!({"model": "m", "messages": []});
        let tagged = |target: &str, lines: &[(&str, &str)], dialect: Dialect| {
            window_name(&request(target, lines, &body), dialect)
        };
        assert_eq!(tagged("/v1/messages", &one_m, Dialect::Anthropic), "1m");
        assert_eq!(
            tagged("/v1/messages", &one_m[..1], Dialect::Anthropic),
            "base"
        );
        assert_eq!(tagged("/v1/responses", &one_m, Dialect::Responses), "base");
        assert_eq!(
            tagged("/v1/chat/completions", &one_m, Dialect::ChatCompletions),
            "base"
        );
        // Both startup lines show both thresholds and the tail percent.
        assert_eq!(
            test_proxy(128_000, 128_000).settings(),
            "threshold 128000 tokens, threshold_1m 128000 tokens, keep_recent 3, keep_tail_percent 40"
        );
    }

    /// Status JSON as a 0.4.1 server returns it, before the 1M threshold,
    /// the tail percent and the 1M request counter.
    fn status_0_4_1() -> Value {
        json!({
            "name": "gobstopper-proxy", "version": "0.4.1", "port": 8260,
            "threshold_tokens": 128_000, "keep_recent": 3, "result_max_chars": 500,
            "keep_thinking": true, "shadow": false, "strict": false,
            "store_entries": 2, "store_chars": 9000, "requests": 10, "compacted": 2,
            "matched": 7, "reactive_retries": 0, "upstream_errors": 0, "fail_open": 0,
            "est_tokens_in": 1000, "est_tokens_out": 400,
            "all_time_est_tokens_in": 5000, "all_time_est_tokens_out": 2000,
            "stats_file": null, "active_connections": 1, "uptime_secs": 60
        })
    }

    #[test]
    fn status_text_prints_the_new_keys_only_when_the_server_returns_them() {
        let old = status_text(8260, &status_0_4_1());
        assert!(!old.contains("null"), "{old}");
        assert_eq!(
            old.lines().next(),
            Some("gobstopper proxy on 127.0.0.1:8260: threshold 128000 tokens, keep_recent 3")
        );
        assert!(old.contains("\nrequests 10, compacted 2,"), "{old}");

        let mut current = status_0_4_1();
        current["threshold_1m_tokens"] = json!(256_000);
        current["keep_tail_percent"] = json!(40);
        current["requests_1m"] = json!(4);
        let text = status_text(8260, &current);
        assert_eq!(
            text.lines().next(),
            Some("gobstopper proxy on 127.0.0.1:8260: threshold 128000 tokens, threshold_1m 256000 tokens, keep_recent 3, keep_tail_percent 40")
        );
        assert!(
            text.contains("\nrequests 10 (4 with a 1M window), compacted 2,"),
            "{text}"
        );

        // An explicit null is treated as absent.
        current["requests_1m"] = Value::Null;
        assert!(!status_text(8260, &current).contains("null"));
    }
}
