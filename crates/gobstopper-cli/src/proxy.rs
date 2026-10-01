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
use gobstopper_adapters::request::calibrate::{
    calibrated_threshold, reported_input_tokens, Calibration,
};
use gobstopper_adapters::request::{
    replay, CliffConfig, Dialect, Engine, RequestCtx, DEFAULT_THRESHOLD_TOKENS,
    MAX_KEEP_TAIL_PERCENT,
};
use serde_json::{json, Value};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const DEFAULT_PORT: u16 = 8260;
#[path = "proxy_launch.rs"]
mod launch;
#[path = "proxy_context.rs"]
mod scoped_context;
const STATUS_PATH: &str = "/gobstopper/status";
const READY_PATH: &str = "/gobstopper/ready";
const MAX_HEAD_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 256 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 256;
const CONTROL_RESERVE: usize = 16;
const REQUEST_HEAD_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_BODY_TIMEOUT: Duration = Duration::from_secs(120);
const CONTROL_WRITE_TIMEOUT: Duration = Duration::from_millis(300);
const REJECT_DRAIN_TIMEOUT: Duration = Duration::from_millis(300);
const REJECT_DRAIN_BYTES: usize = 64 * 1024;
#[path = "proxy_work.rs"]
mod work;
/// Most (upstream, model) pairs the proxy keeps a calibration for; later
/// pairs are not calibrated.
const MAX_CALIBRATIONS: usize = 64;
/// Most bytes of a JSON response kept to read its usage after the relay;
/// a larger body is not sampled.
const MAX_TAP_JSON_BYTES: usize = 4 * 1024 * 1024;
/// Most bytes of an event stream kept to find its usage event: the first
/// bytes for an Anthropic stream, whose `message_start` opens it, and the
/// last bytes for an OpenAI stream, which ends with the count.
const MAX_TAP_STREAM_BYTES: usize = 64 * 1024;

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
    "x-gobstopper-scope",
    "x-gobstopper-service-id",
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
    /// Start Claude with bounded direct fallback, or Codex on its existing healthy proxy.
    /// Checks before launch; never changes saved settings or replays a request.
    Launch {
        #[command(flatten)]
        opts: launch::LaunchOptions,
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
        /// Characters of the human's words and the assistant's visible
        /// replies each summary carries from the turns earlier compactions
        /// summarized, newest kept, never tool output or thinking. At most a
        /// quarter of the room above the verbatim head. 0 turns carrying off.
        #[arg(long, value_name = "CHARS",
              default_value_t = CliffConfig::default().carry_max_chars)]
        carry_max_chars: usize,
        /// Original evidence bytes kept across compactions; 0 disables evidence.
        #[arg(long, default_value_t = CliffConfig::default().evidence_max_bytes)]
        evidence_max_bytes: usize,
        #[arg(long, default_value_t = CliffConfig::default().evidence_max_chars)]
        evidence_max_chars: usize,
        /// Tokens assumed for the system prompt and tool definitions, which
        /// transcripts do not record.
        #[arg(long, default_value_t = 20_000)]
        fixed_tokens: u64,
        /// Size requests at four characters per token only, as before
        /// calibration. By default a Claude Code replay lowers the threshold
        /// by the ratio of reported to estimated input learned from the
        /// transcript's earlier replies, as `proxy serve` does.
        #[arg(long)]
        no_calibrate: bool,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// Install a user startup service and start the proxy now.
    Install {
        #[command(flatten)]
        opts: ProxyOpts,
        /// Loopback port.
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Replace an existing owned user service.
        #[arg(long)]
        replace: bool,
        /// Print the platform service definition and change nothing.
        #[arg(long)]
        print: bool,
    },
    /// Stop the owned user service and remove it, so the proxy no longer
    /// starts at login.
    Uninstall,
    /// Check the installed user service, its identity, and its configuration.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Migrate an exactly identified legacy startup service, preserving rollback.
    MigrateService {
        #[arg(long)]
        print: bool,
    },
    /// Repair a managed service without replacing modified or unrelated files.
    Repair {
        #[arg(long)]
        print: bool,
    },
    /// Show a running proxy's settings and counters.
    Status {
        /// Loopback port; defaults to the installed service's port, or 8260.
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
        port: Option<u16>,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args, Clone)]
pub struct ProxyOpts {
    /// Deadline for optional compaction work; timeout sends the original request
    /// unless an explicit capacity requires verified sizing.
    #[arg(long, default_value_t = 5000, value_parser = clap::value_parser!(u64).range(1..=60000))]
    transform_timeout_ms: u64,
    /// Allow idle sleep during inference. Display sleep is always allowed.
    #[arg(long)]
    no_keep_awake: bool,
    /// Disable the local content-free session observation journal.
    #[arg(long)]
    no_session_data: bool,
    /// Operator-configured context capacity for this upstream route.
    #[arg(long)]
    context_window: Option<u64>,
    /// Client capacity, if lower than the provider capacity (proxy run scopes).
    #[arg(long)]
    client_context_window: Option<u64>,
    /// Output headroom when creating a proxy run scope.
    #[arg(long, default_value_t = 32_000)]
    output_reserve: u64,
    /// Opt into bounded context rescue for proxy run scopes.
    #[arg(long)]
    adaptive_context: bool,
    #[arg(long, hide = true)]
    service_id: Option<String>,
    #[arg(long, hide = true, requires = "service_id")]
    service_state_dir: Option<std::path::PathBuf>,
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
    /// Characters of the human's words and the assistant's visible replies
    /// each summary carries from the turns earlier compactions summarized,
    /// newest kept, never tool output or thinking. At most a quarter of the
    /// room above the verbatim head. 0 turns carrying off.
    #[arg(long, value_name = "CHARS",
          default_value_t = CliffConfig::default().carry_max_chars)]
    carry_max_chars: usize,
    /// UTF-8 bytes of original tool evidence retained across summaries. 0 disables it.
    #[arg(long, default_value_t = CliffConfig::default().evidence_max_bytes)]
    evidence_max_bytes: usize,
    /// Characters of original text evidence retained across summaries.
    #[arg(long, default_value_t = CliffConfig::default().evidence_max_chars)]
    evidence_max_chars: usize,
    /// Leave thinking and reasoning text out of summaries.
    #[arg(long)]
    drop_thinking: bool,
    /// Size requests at four characters per token only. By default, once an
    /// upstream and model have answered 5 requests with usage, the
    /// threshold is divided by the running ratio of reported to estimated
    /// input (from 1.0 to 2.0), so compaction starts earlier and never later.
    #[arg(long)]
    no_calibrate: bool,
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
            carry_max_chars,
            evidence_max_bytes,
            evidence_max_chars,
            fixed_tokens,
            no_calibrate,
            json,
        } => {
            let (provider, path) = resolve(session)?;
            let (dialect, history, usage) = replay::history_from_file(provider, &path)?;
            let cfg = CliffConfig {
                threshold_tokens: *threshold,
                keep_recent: *keep_recent,
                keep_tail_percent: *keep_tail_percent,
                result_max_chars: *result_max_chars,
                carry_max_chars: *carry_max_chars,
                evidence_max_bytes: *evidence_max_bytes,
                evidence_max_chars: *evidence_max_chars,
                ..CliffConfig::default()
            };
            let report = replay::replay_calibrated(
                &history,
                &usage,
                dialect,
                cfg,
                *fixed_tokens,
                !*no_calibrate,
            );
            if *json {
                println!("{}", serde_json::to_string_pretty(&report)?);
                return Ok(());
            }
            let k = |tokens: u64| format!("~{}k", tokens / 1000);
            println!(
                "replayed {} requests from {} ({} messages); threshold {} tokens, keep_recent {}, keep_tail_percent {}, carry_max_chars {}, {} fixed tokens assumed",
                report.requests,
                path.display(),
                history.len(),
                threshold,
                keep_recent,
                keep_tail_percent,
                carry_max_chars,
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
            if report.usage_requests > 0 {
                let ratio = |permille: Option<u64>| {
                    permille.map_or_else(|| "-".into(), |p| format!("{:.2}", p as f64 / 1000.0))
                };
                println!(
                    "  provider-reported input on {} requests: {} to {} times the estimate (median {}); calibration {}, last ratio applied {:.2}",
                    report.usage_requests,
                    ratio(report.reported_ratio_min_permille),
                    ratio(report.reported_ratio_max_permille),
                    ratio(report.reported_ratio_median_permille),
                    if report.calibrate { "on" } else { "off" },
                    f64::from(report.last_ratio_permille) / 1000.0,
                );
                println!(
                    "  largest request sent in reported tokens: {}; requests over the {} token threshold in reported tokens: {}",
                    k(report.peak_reported_tokens_out),
                    threshold,
                    report.reported_over_threshold
                );
            }
            Ok(())
        }
        ProxyCmd::Serve { opts, port } => {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, *port))
                .with_context(|| format!("bind 127.0.0.1:{port}"))?;
            let port = listener.local_addr()?.port();
            let proxy = Arc::new(Proxy::new(opts, port)?);
            diagnostics::startup(port);
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
            std::thread::Builder::new()
                .name("gobstopper-listener".into())
                .spawn(move || serve(listener, server))
                .context("start scoped proxy listener")?;
            let scope = proxy
                .control
                .get()
                .context("context control unavailable")?
                .create(
                    opts.context_window,
                    opts.client_context_window,
                    opts.output_reserve,
                    opts.adaptive_context,
                    None,
                )?;
            let base = format!("http://127.0.0.1:{port}/__gobstopper/s/{scope}");
            log(&format!(
                "scoped proxy on port {port}, {}",
                proxy.settings()
            ));
            let status = Command::new(&command[0])
                .args(&command[1..])
                .env("GOBSTOPPER_SCOPE", &scope)
                .env("ANTHROPIC_BASE_URL", &base)
                .env("OPENAI_BASE_URL", format!("{base}/v1"))
                .status()
                .with_context(|| format!("run {}", command[0]));
            if let Some(control) = proxy.control.current() {
                let _ = control.close(&scope);
            }
            let status = status?;
            log(&proxy.summary());
            std::process::exit(status.code().unwrap_or(1));
        }
        ProxyCmd::Launch { opts } => launch::run(opts),
        ProxyCmd::Install {
            opts,
            port,
            replace,
            print,
        } => {
            threshold_1m(opts.threshold, opts.threshold_1m)?;
            crate::proxy_agent::install(&install_serve_args(), *port, *replace, *print)
        }
        ProxyCmd::Uninstall => crate::proxy_agent::uninstall(),
        ProxyCmd::MigrateService { print } => crate::proxy_agent::migrate(*print),
        ProxyCmd::Repair { print } => crate::proxy_agent::repair(*print),
        ProxyCmd::Doctor { .. } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&crate::proxy_agent::inspect()?)?
            );
            Ok(())
        }
        ProxyCmd::Status { port, json } => {
            let port = crate::proxy_agent::status_port(*port)?;
            let status = match fetch_status(port) {
                Ok(status) => status,
                Err(error) if is_unavailable(&error) => return Err(proxy_down(port)),
                Err(error) => return Err(error),
            };
            if *json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                print!("{}", status_text(port, &status));
            }
            Ok(())
        }
    }
}

#[path = "proxy_log.rs"]
mod diagnostics;

fn log(message: &str) {
    diagnostics::write(&format!("{} gobstopper proxy: {message}", rfc3339_now()));
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
        "gobstopper proxy on 127.0.0.1:{port}: threshold {} tokens{}, keep_recent {}{}{}{}\n",
        status["threshold_tokens"],
        present("threshold_1m_tokens", &|value| format!(
            ", threshold_1m {value} tokens"
        )),
        status["keep_recent"],
        present("keep_tail_percent", &|value| format!(
            ", keep_tail_percent {value}"
        )),
        present("carry_max_chars", &|value| format!(
            ", carry_max_chars {value}"
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
        present("requests_1m", &|value| format!(
            " ({value} with a 1M window)"
        )),
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
    match status.get("calibrate").and_then(Value::as_bool) {
        None => {}
        Some(false) => text += "estimate calibration: off\n",
        Some(true) => {
            let rows = status["calibrations"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if rows.is_empty() {
                text += "estimate calibration: on, no provider usage seen yet\n";
            }
            for row in rows {
                text += &format!(
                    "estimate calibration: {} via {}: ratio {} applied, {} measured over {} samples\n",
                    row["model"].as_str().unwrap_or("-"),
                    row["upstream"].as_str().unwrap_or("-"),
                    row["ratio"],
                    row["measured_ratio"],
                    row["samples"],
                );
            }
        }
    }
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

#[path = "proxy_stats.rs"]
mod stats_log;
use stats_log::StatsLog;

impl StatsLog {
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
            "carry_chars": ctx.carry_chars,
            "window": window_name(request, ctx.dialect),
            "threshold_tokens": ctx.threshold_tokens,
            "ratio_permille": ctx.ratio_permille,
            "compacted": ctx.compacted,
            "reused_prefix": ctx.matched,
            "over_budget": ctx.over_budget,
            "rung": ctx.rung,
            "shadow": shadow,
        });
        self.enqueue(&record);
    }
}

/// A failed initial open must not disable scoped inference for the lifetime of
/// a healthy proxy. Only scoped traffic retries, and concurrent callers share
/// one bounded open attempt. Status remains nonblocking while that attempt runs.
struct ContextAccess {
    path: Option<std::path::PathBuf>,
    state: Mutex<ContextAccessState>,
    available: AtomicBool,
    jobs: work::Workers,
    observer: scoped_context::Observer,
}

struct ContextAccessState {
    control: Option<Arc<crate::context::Control>>,
    retry_after: Instant,
}

impl ContextAccess {
    fn new(path: Option<std::path::PathBuf>) -> Self {
        Self {
            available: AtomicBool::new(false),
            jobs: work::Workers::default(),
            observer: scoped_context::Observer::default(),
            path,
            state: Mutex::new(ContextAccessState {
                control: None,
                retry_after: Instant::now(),
            }),
        }
    }

    fn current(&self) -> Option<Arc<crate::context::Control>> {
        self.state.try_lock().ok()?.control.clone()
    }

    fn get(&self) -> Option<Arc<crate::context::Control>> {
        let mut state = self.state.try_lock().ok()?;
        if state.control.is_none() && Instant::now() >= state.retry_after {
            if let Some(path) = &self.path {
                state.control = crate::context::Control::open(path).ok().map(Arc::new);
                self.available
                    .store(state.control.is_some(), Ordering::Release);
            }
            state.retry_after = Instant::now() + Duration::from_secs(30);
        }
        state.control.clone()
    }
}

struct Prepared {
    ctx: RequestCtx,
    key: (String, String),
    capacity: Option<u64>,
    outgoing: Option<Vec<u8>>,
    evidence: Option<Vec<gobstopper_adapters::request::EvidenceObservation>>,
}
struct PreparationUnavailable;

struct Proxy {
    engine: Arc<Engine>,
    transforms: work::Workers,
    transform_timeout: Duration,
    memory: work::MemoryBudget,
    power: crate::power::Power,
    observations: crate::proxy_observations::Recorder,
    control: Arc<ContextAccess>,
    context_window: Option<u64>,
    service_id: Option<String>,
    /// Threshold for Anthropic requests that declare a 1M-token window.
    threshold_1m: u64,
    shadow: bool,
    /// Learn the estimate ratio from provider-reported usage.
    calibrate: bool,
    /// Running ratio per (upstream, model), at most `MAX_CALIBRATIONS`.
    calibrations: Mutex<HashMap<(String, String), Calibration>>,
    anthropic: String,
    openai: String,
    chatgpt: String,
    port: u16,
    started: Instant,
    active: AtomicUsize,
    forwarding: AtomicUsize,
    inference: AtomicUsize,
    executable: Option<String>,
    admission: Mutex<crate::proxy_drain::Admission>,
    instance_id: String,
    startup_guard: bool,
    stats: Stats,
    stats_log: StatsLog,
}

impl Proxy {
    fn new(opts: &ProxyOpts, port: u16) -> Result<Self> {
        crate::proxy_agent::check_startup_drain(
            opts.service_id.as_deref(),
            opts.service_state_dir.as_deref(),
            port,
        )?;
        let cfg = CliffConfig {
            threshold_tokens: opts.threshold,
            keep_recent: opts.keep_recent,
            keep_tail_percent: opts.keep_tail_percent,
            result_max_chars: opts.result_max_chars,
            carry_max_chars: opts.carry_max_chars,
            evidence_max_bytes: opts.evidence_max_bytes,
            evidence_max_chars: opts.evidence_max_chars,
            keep_thinking: !opts.drop_thinking,
            strict: opts.strict,
            ..CliffConfig::default()
        };
        let control = Arc::new(ContextAccess::new(crate::context::default_path().ok()));
        Ok(Self {
            power: crate::power::Power::new(!opts.no_keep_awake),
            observations: crate::proxy_observations::Recorder::open(!opts.no_session_data),
            control,
            context_window: opts.context_window,
            service_id: opts.service_id.clone(),
            threshold_1m: threshold_1m(opts.threshold, opts.threshold_1m)?,
            engine: Arc::new(Engine::new(cfg)),
            transforms: work::Workers::default(),
            transform_timeout: Duration::from_millis(opts.transform_timeout_ms),
            memory: work::MemoryBudget::new(work::BODY_BUDGET),
            shadow: opts.shadow,
            calibrate: !opts.no_calibrate,
            calibrations: Mutex::new(HashMap::new()),
            anthropic: validate_upstream(&opts.anthropic_upstream)?,
            openai: validate_upstream(&opts.openai_upstream)?,
            chatgpt: validate_upstream(&opts.chatgpt_upstream)?,
            port,
            started: Instant::now(),
            active: AtomicUsize::new(0),
            forwarding: AtomicUsize::new(0),
            inference: AtomicUsize::new(0),
            executable: std::env::current_exe()
                .ok()
                .and_then(|path| path.canonicalize().ok())
                .map(|path| path.display().to_string()),
            admission: Mutex::new(crate::proxy_drain::Admission::default()),
            instance_id: crate::proxy_agent::unique_id(),
            startup_guard: opts.service_state_dir.is_some(),
            stats: Stats::default(),
            stats_log: StatsLog::open(),
        })
    }

    fn count(&self, counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

    /// Readiness must not wait for optional storage, compaction or power APIs.
    /// A busy admission lock is conservatively unavailable, never permission to
    /// bypass a drain. Process identity remains available in either state.
    fn readiness(&self) -> Value {
        let mut drain = match self.admission.try_lock() {
            Ok(mut admission) => Some(admission.snapshot(Instant::now())),
            Err(std::sync::TryLockError::Poisoned(error)) => {
                Some(error.into_inner().snapshot(Instant::now()))
            }
            Err(std::sync::TryLockError::WouldBlock) => None,
        };
        if let Some(drain) = &mut drain {
            drain["startup_guard"] = json!(self.startup_guard);
        }
        let ready = drain.as_ref().is_some_and(|value| value["phase"] == "open");
        json!({
            "name":"gobstopper-proxy", "version":env!("CARGO_PKG_VERSION"),
            "port":self.port, "pid":std::process::id(), "service_id":self.service_id,
            "instance_id":self.instance_id, "executable":self.executable,
            "official_upstreams": self.anthropic == "https://api.anthropic.com"
                && self.openai == "https://api.openai.com"
                && self.chatgpt == "https://chatgpt.com",
            "context_constrained": self.context_window.is_some() || self.engine.config().strict,
            "ready":ready, "drain_control":drain,
            "active_inference":self.inference.load(Ordering::Acquire),
            "active_connections":self.active.load(Ordering::Relaxed),
        })
    }

    fn status(&self) -> Value {
        let cfg = self.engine.config();
        let store = self.engine.try_store_stats();
        let totals = self.stats_log.totals(
            self.count(&self.stats.est_tokens_in),
            self.count(&self.stats.est_tokens_out),
        );
        let context_available = self.control.available.load(Ordering::Acquire);
        let mut drain = self
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot(Instant::now());
        drain["startup_guard"] = json!(self.startup_guard);
        let mut status = json!({
            "name": "gobstopper-proxy",
            "pid": std::process::id(),
            "executable": self.executable,
            "service_id": self.service_id,
            "keep_awake": self.power.status(),
            "observations": self.observations.status(),
            "context_control": {"available": context_available, "error": (!context_available).then_some("context_control_unavailable"), "configured_window": self.context_window},
            "version": env!("CARGO_PKG_VERSION"),
            "port": self.port,
            "threshold_tokens": cfg.threshold_tokens,
            "threshold_1m_tokens": self.threshold_1m,
            "keep_recent": cfg.keep_recent,
            "keep_tail_percent": cfg.keep_tail_percent,
            "carry_max_chars": cfg.carry_max_chars,
            "evidence_max_bytes": cfg.evidence_max_bytes,
            "evidence_max_chars": cfg.evidence_max_chars,
            "result_max_chars": cfg.result_max_chars,
            "keep_thinking": cfg.keep_thinking,
            "shadow": self.shadow,
            "strict": cfg.strict,
            "calibrate": self.calibrate,
            "calibrations": self.calibration_status(),
            "store_entries": store.map(|sizes| sizes.0),
            "store_chars": store.map(|sizes| sizes.1),
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
            "draining": drain["phase"] != "open",
            "drain_control": drain,
            "instance_id": self.instance_id,
            "uptime_secs": self.started.elapsed().as_secs(),
        });
        status["store_details_available"] = json!(store.is_some());
        status["stats_persistence"] = self.stats_log.status();
        status["transforms"] = self.transforms.status();
        status["transforms"]["timeout_ms"] = json!(self.transform_timeout.as_millis());
        status["request_memory"] =
            json!({"used_bytes":self.memory.used(),"limit_bytes":work::BODY_BUDGET});
        status["diagnostic_log"] = diagnostics::status();
        status["context_control"]["operations"] = self.control.jobs.status();
        status["context_control"]["operations"]["timeout_ms"] =
            json!(scoped_context::TIMEOUT.as_millis());
        status["context_control"]["observations"] = self.control.observer.status();
        status
    }

    fn calibrations(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), Calibration>> {
        self.calibrations.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// One row per (upstream, model) with its samples, measured ratio and
    /// applied ratio (1.0 until enough samples), sorted by key.
    fn calibration_status(&self) -> Value {
        let calibrations = self.calibrations();
        let mut rows: Vec<_> = calibrations.iter().collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));
        Value::Array(
            rows.into_iter()
                .map(|((upstream, model), calibration)| {
                    json!({
                        "upstream": upstream,
                        "model": model,
                        "samples": calibration.samples(),
                        "measured_ratio": calibration
                            .measured_permille()
                            .map(|p| p as f64 / 1000.0),
                        "ratio": f64::from(calibration.ratio_permille()) / 1000.0,
                    })
                })
                .collect(),
        )
    }

    /// Record the provider's input count for a forwarded request whose
    /// estimate was `est_tokens`.
    fn observe(&self, key: (String, String), est_tokens: u64, reported_tokens: u64) {
        let mut calibrations = self.calibrations();
        if calibrations.len() >= MAX_CALIBRATIONS && !calibrations.contains_key(&key) {
            return;
        }
        let calibration = calibrations.entry(key).or_default();
        let before = calibration.ratio_permille();
        calibration.observe(est_tokens, reported_tokens);
        let after = calibration.ratio_permille();
        if after != before && (before == 1000 || after.abs_diff(before) >= 50) {
            log(&format!(
                "estimate calibration: ratio {:.2} after {} samples",
                f64::from(after) / 1000.0,
                calibration.samples()
            ));
        }
    }

    /// The settings both startup lines show (serve and run).
    fn settings(&self) -> String {
        let cfg = self.engine.config();
        format!(
            "threshold {} tokens, threshold_1m {} tokens, keep_recent {}, keep_tail_percent {}, carry_max_chars {}, calibrate {}",
            cfg.threshold_tokens,
            self.threshold_1m,
            cfg.keep_recent,
            cfg.keep_tail_percent,
            cfg.carry_max_chars,
            if self.calibrate { "on" } else { "off" }
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

    /// The engine's view of a request, and the (upstream, model) key its
    /// calibration is kept under.
    #[cfg(test)]
    fn prepare(&self, request: &Request) -> Option<(RequestCtx, (String, String))> {
        self.prepare_scoped(request, None)
            .ok()
            .flatten()
            .map(|mut prepared| {
                self.engine.publish_prepared(&mut prepared.ctx);
                (prepared.ctx, prepared.key)
            })
    }

    fn prepare_scoped(
        &self,
        request: &Request,
        budget: Option<&crate::context::Decision>,
    ) -> std::result::Result<Option<Prepared>, PreparationUnavailable> {
        if request.method != "POST" || request.body.is_empty() {
            return Ok(None);
        }
        let Some(dialect) = Dialect::detect(request.path()) else {
            return Ok(None);
        };
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
            return Ok(None);
        }
        if request.body.len() > work::TRANSFORM_LIMIT {
            self.stats.fail_open.fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        }
        let Some(job) = self.transforms.acquire(&self.memory, request.body.len()) else {
            self.stats.fail_open.fetch_add(1, Ordering::Relaxed);
            return Err(PreparationUnavailable);
        };
        // A stuck optional calibration lock cannot stall inference either. Do
        // not substitute a weaker sizing ratio when capacity is explicit.
        let calibrations = if self.calibrate {
            match self.calibrations.try_lock() {
                Ok(values) => values.clone(),
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner().clone(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    self.stats.fail_open.fetch_add(1, Ordering::Relaxed);
                    return Err(PreparationUnavailable);
                }
            }
        } else {
            HashMap::new()
        };
        let ordinary = self.threshold_for(request, dialect);
        let budget = budget.cloned();
        let context_window = self.context_window;
        let body = request.body.clone();
        let upstream = self.upstream_for(request).to_string();
        let engine = Arc::clone(&self.engine);
        let shadow = self.shadow;
        let result = job.run(self.transform_timeout, move || {
            let Value::Object(parsed) = serde_json::from_slice::<Value>(&body).ok()? else {
                return None;
            };
            let threshold = budget
                .as_ref()
                .map_or(ordinary, |b| b.effective_input_tokens);
            let capacity = budget
                .as_ref()
                .and_then(|b| b.input_capacity_tokens)
                .or_else(|| context_window.map(|n| n.saturating_sub(requested_output(&parsed))));
            let policy = budget
                .as_ref()
                .map_or_else(String::new, crate::context::Decision::policy_identity);
            let evidence = budget.as_ref().filter(|b| b.adaptive).map(|_| {
                let messages = parsed
                    .get(dialect.messages_key())
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                gobstopper_adapters::request::evidence_observations(messages, dialect)
            });
            let model = parsed
                .get("model")
                .and_then(Value::as_str)
                .filter(|m| m.len() <= 1024)
                .unwrap_or_default()
                .to_string();
            let key = (upstream, model);
            let ratio = calibrations
                .get(&key)
                .map_or(1000, Calibration::ratio_permille);
            let ctx =
                engine.prepare_deferred(parsed, dialect, threshold, ratio, &policy, capacity)?;
            let outgoing = if ctx.modified && !shadow {
                Some(serde_json::to_vec(&ctx.outgoing_body()).ok()?)
            } else {
                None
            };
            Some(Prepared {
                ctx,
                key,
                capacity,
                outgoing,
                evidence,
            })
        });
        let prepared = result.map_err(|_| PreparationUnavailable);
        if !matches!(&prepared, Ok(Some(_))) {
            self.stats.fail_open.fetch_add(1, Ordering::Relaxed);
        }
        prepared
    }

    fn publish_prepared(
        &self,
        prepared: &mut Prepared,
        request: &Request,
        budget: Option<&crate::context::Decision>,
    ) {
        self.engine.publish_prepared(&mut prepared.ctx);
        if !self.shadow {
            if let (Some(scope), Some(evidence), Some(budget)) = (
                request.header("x-gobstopper-scope"),
                prepared.evidence.take(),
                budget,
            ) {
                self.control.observe(
                    scope,
                    evidence,
                    &prepared.ctx.evicted_evidence_digests,
                    budget,
                );
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
        // Sizes and the window only, never content or header values: the
        // carried section is conversation text, so only its length appears.
        let sizes = format!(
            "(head ~{}k, summary ~{}k, tail ~{}k, carry {} chars)",
            ctx.est_head_tokens / 1000,
            ctx.est_summary_tokens / 1000,
            ctx.est_tail_tokens / 1000,
            ctx.carry_chars,
        );
        let window = window_name(request, ctx.dialect);
        let window = if ctx.ratio_permille == 1000 {
            window.to_string()
        } else {
            format!(
                "{window}, ratio {:.2}",
                f64::from(ctx.ratio_permille) / 1000.0
            )
        };
        let shadow = if self.shadow {
            " (shadow: sent unchanged)"
        } else {
            ""
        };
        if ctx.compacted {
            self.stats.compacted.fetch_add(1, Ordering::Relaxed);
            // Compared against the threshold selected for this request, so a
            // 1M-window request at `threshold_1m` is not reported as raised.
            let raised = if ctx.threshold_tokens > ctx.calibrated_threshold_tokens {
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
                if ctx.over_budget {
                    ", still over threshold"
                } else {
                    ""
                },
            ));
        } else if ctx.matched {
            self.stats.matched.fetch_add(1, Ordering::Relaxed);
            log(&format!(
                "{kind} {path}: reused compacted prefix, ~{}k -> ~{}k est tokens {sizes}, window={window}{shadow}",
                ctx.est_tokens_in / 1000,
                ctx.est_tokens_out / 1000,
            ));
        } else if ctx.est_tokens_in > ctx.calibrated_threshold_tokens {
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

    fn handle(&self, mut client: TcpStream, limit: usize) -> Result<()> {
        let mut wire = Wire {
            stream: &mut client,
            buffer: Vec::new(),
            deadline: Instant::now() + REQUEST_HEAD_TIMEOUT,
        };
        let mut request = match read_request_head(&mut wire) {
            Ok(request) => request,
            Err(error) => {
                return reject_unread(wire.stream, |client| {
                    write_error(
                        client,
                        400,
                        "invalid_request_error",
                        &format!("gobstopper proxy: {error}"),
                    )
                });
            }
        };
        if !host_is_loopback(request.header("host")) {
            return reject_unread(wire.stream, |client| {
                write_error(
                    client,
                    403,
                    "permission_error",
                    "gobstopper proxy: only loopback hosts are served",
                )
            });
        }
        if let Err(error) = request.bind_scope() {
            return reject_unread(wire.stream, |client| {
                write_error(client, 400, "gobstopper_invalid_scope", &error.to_string())
            });
        }
        let control = matches!(request.path(), STATUS_PATH | READY_PATH)
            || request.path().starts_with("/gobstopper/service/");
        // Reserve capacity before reading a possibly slow or large body. The
        // reserve serves local control only; it never adds inference capacity.
        let _forwarding = if control {
            None
        } else if let Some(slot) = CountGuard::try_acquire(&self.forwarding, limit) {
            Some(slot)
        } else {
            return reject_unread(wire.stream, |client| {
                write_error(
                    client,
                    503,
                    "overloaded_error",
                    "gobstopper proxy: too many requests; retry shortly",
                )
            });
        };
        wire.deadline = Instant::now()
            + if control {
                REQUEST_HEAD_TIMEOUT
            } else {
                REQUEST_BODY_TIMEOUT
            };
        let mut body_memory = self.memory.lease();
        if let Err(error) = read_request_body(
            &mut wire,
            &mut request,
            if control { 4096 } else { MAX_BODY_BYTES },
            (!control).then_some(&mut body_memory),
        ) {
            if error.is::<work::MemoryExhausted>() {
                return reject_unread(wire.stream, |client| {
                    write_buffered(client, 503, "Service Unavailable", &[
                    ("content-type".into(), "application/json".into()), ("retry-after".into(), "1".into()),
                ], br#"{"error":{"type":"proxy_overloaded","message":"request memory is temporarily full; retry shortly"}}"#)
                });
            }
            return reject_unread(wire.stream, |client| {
                write_error(
                    client,
                    400,
                    "invalid_request_error",
                    &format!("gobstopper proxy: {error}"),
                )
            });
        }
        drop(wire);
        if request.path() == READY_PATH {
            if request.method != "GET" {
                return write_error(
                    &mut client,
                    405,
                    "invalid_request_error",
                    "readiness requires GET",
                );
            }
            let body = self.readiness();
            let (status, reason) = if body["ready"] == true {
                (200, "OK")
            } else {
                (503, "Service Unavailable")
            };
            return write_buffered(
                &mut client,
                status,
                reason,
                &[("content-type".into(), "application/json".into())],
                &serde_json::to_vec(&body)?,
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
        if request.path().starts_with("/gobstopper/service/") {
            if request.method != "POST"
                || request.header("origin").is_some()
                || self
                    .service_id
                    .as_deref()
                    .is_none_or(|id| request.header("x-gobstopper-service-id") != Some(id))
            {
                return write_error(
                    &mut client,
                    403,
                    "permission_error",
                    "owned service identity is required",
                );
            }
            if request.body.len() > 4096 {
                return write_error(
                    &mut client,
                    400,
                    "invalid_request_error",
                    "drain control body exceeds 4096 bytes",
                );
            }
            let mut admission = self.admission.lock().unwrap_or_else(|e| e.into_inner());
            let active = self.inference.load(Ordering::Acquire) as u64;
            let result = if let Some(action) = request
                .path()
                .strip_prefix("/gobstopper/service/drain-lease/")
            {
                let body: Value = match serde_json::from_slice(&request.body) {
                    Ok(body) => body,
                    Err(_) => {
                        drop(admission);
                        return write_error(
                            &mut client,
                            400,
                            "invalid_request_error",
                            "invalid drain control body",
                        );
                    }
                };
                if body["instance_id"] != self.instance_id || body["pid"] != std::process::id() {
                    drop(admission);
                    return write_error(
                        &mut client,
                        409,
                        "service_identity_changed",
                        "proxy process incarnation differs",
                    );
                }
                admission
                    .control(action, &body, active, Instant::now())
                    .map(|mut result| {
                        result["instance_id"] = json!(self.instance_id);
                        result["pid"] = json!(std::process::id());
                        result
                    })
            } else if matches!(
                request.path(),
                "/gobstopper/service/drain" | "/gobstopper/service/resume"
            ) {
                let drain = request.path().ends_with("/drain");
                admission
                    .legacy(drain, active, Instant::now())
                    .map(|()| json!({"drained":drain}))
            } else {
                drop(admission);
                return write_error(
                    &mut client,
                    404,
                    "not_found",
                    "unknown local service control action",
                );
            };
            drop(admission);
            let body = match result {
                Ok(body) => body,
                Err(error) => {
                    return write_error(&mut client, 409, "service_control_conflict", error)
                }
            };
            return write_buffered(
                &mut client,
                200,
                "OK",
                &[("content-type".into(), "application/json".into())],
                &serde_json::to_vec(&body)?,
            );
        }
        client.set_write_timeout(Some(Duration::from_secs(600)))?;
        self.stats.requests.fetch_add(1, Ordering::Relaxed);
        self.forward(&request, &mut client)
    }

    fn forward(&self, request: &Request, client: &mut TcpStream) -> Result<()> {
        let upstream = self.upstream_for(request);
        let dialect = Dialect::detect(request.path()).filter(|_| request.method == "POST");
        // Native provider compaction performs inference even though its request
        // must not be rewritten by the ordinary Responses adapter.
        let inference = dialect.is_some()
            || (request.method == "POST" && request.path().ends_with("/responses/compact"));
        let _awake = if inference {
            // Lease acquisition and inference admission share one lock. A drain
            // can therefore never observe zero while a request slips upstream.
            let mut admission = self.admission.lock().unwrap_or_else(|e| e.into_inner());
            if admission.blocked(Instant::now()) {
                drop(admission);
                return write_buffered(client, 503, "Service Unavailable", &[
                    ("content-type".into(), "application/json".into()),
                    ("retry-after".into(), "1".into()),
                ], br#"{"error":{"type":"service_draining","message":"the owned proxy is restarting; retry shortly"}}"#);
            }
            let inference = CountGuard::acquire(&self.inference);
            drop(admission);
            Some((inference, self.power.acquire()))
        } else {
            None
        };
        let scoped = dialect.is_some() && request.header("x-gobstopper-scope").is_some();
        if scoped && request.body.len() > work::TRANSFORM_LIMIT {
            return write_error(
                client,
                400,
                "gobstopper_capacity_unknown",
                "payload exceeds the bounded sizing limit for an explicit context scope",
            );
        }
        let budget = if let (Some(scope), Some(dialect)) =
            (request.header("x-gobstopper-scope"), dialect)
        {
            let base = if declares_1m_window(request, dialect) {
                self.threshold_1m
            } else {
                self.engine.config().threshold_tokens
            };
            match self.control.consume(
                &self.memory,
                &request.body,
                scope,
                scoped_context::Policy {
                    base,
                    window: self.context_window,
                },
                scoped_context::TIMEOUT,
            ) {
                Ok(decision) => Some(decision),
                Err(scoped_context::Failure::Unavailable) => {
                    return write_buffered(client, 503, "Service Unavailable", &[
                        ("content-type".into(), "application/json".into()), ("retry-after".into(), "1".into()),
                    ], br#"{"error":{"type":"gobstopper_scope_unavailable","message":"context storage is temporarily unavailable; no inference was sent; retry shortly"}}"#);
                }
                Err(scoped_context::Failure::Invalid) => {
                    return write_error(
                        client,
                        400,
                        "gobstopper_scope_unavailable",
                        "context scope is unavailable; inspect gobstopper context status",
                    );
                }
            }
        } else {
            None
        };
        let (mut prepared, preparation_unavailable) =
            match self.prepare_scoped(request, budget.as_ref()) {
                Ok(prepared) => (prepared, false),
                Err(_) => (None, true),
            };
        let input_capacity = prepared.as_ref().and_then(|p| p.capacity);
        let explicit_capacity = self.context_window.is_some()
            || budget
                .as_ref()
                .is_some_and(|b| b.input_capacity_tokens.is_some());
        let sizing_required = explicit_capacity || (self.engine.config().strict && !self.shadow);
        if dialect.is_some() && sizing_required && prepared.is_none() {
            if preparation_unavailable {
                return write_buffered(client, 503, "Service Unavailable", &[
                    ("content-type".into(), "application/json".into()), ("retry-after".into(), "1".into()),
                ], br#"{"error":{"type":"proxy_transform_unavailable","message":"context sizing is temporarily unavailable; retry shortly"}}"#);
            }
            return write_error(
                client,
                400,
                "gobstopper_capacity_unknown",
                "cannot verify the configured context policy for this payload",
            );
        }
        let mut body: Cow<[u8]> = Cow::Borrowed(&request.body);
        if let Some(prepared) = &mut prepared {
            let ctx = &prepared.ctx;
            let sent_tokens = if self.shadow {
                ctx.est_tokens_in
            } else {
                ctx.est_tokens_out
            };
            if ctx.capacity_exceeded_by_floor
                || !within_input_capacity(sent_tokens, ctx.ratio_permille, input_capacity)
            {
                return write_error(client,400,"gobstopper_capacity_exceeded","the preserved input exceeds this scope's configured input capacity; increase its supported capacity or reduce the immutable input");
            }
            if ctx.over_budget && self.engine.config().strict && !self.shadow {
                return write_error(
                    client,
                    400,
                    "gobstopper_over_budget",
                    &format!(
                        "gobstopper proxy strict mode: ~{} est tokens after compaction, over the {} token threshold",
                        ctx.est_tokens_out, ctx.threshold_tokens
                    ),
                );
            }
            self.publish_prepared(prepared, request, budget.as_ref());
            self.report(&prepared.ctx, request);
            if let Some(outgoing) = prepared.outgoing.take() {
                body = Cow::Owned(outgoing);
            }
        }
        let request_id = dialect.and_then(|_| self.observations.request_id());
        let session = self.observations.session_id(request.header("session_id"));
        let kind = dialect.unwrap_or(Dialect::Responses);
        let model = prepared.as_ref().map(|p| p.key.1.as_str());
        let mut attempt = self
            .observations
            .start(request_id.clone(), session.clone(), kind, model);
        if let Some(prepared) = &prepared {
            record_context(&attempt, &prepared.ctx, self.shadow, budget.as_ref());
        }
        let response = match send_upstream(request, upstream, &body) {
            Ok(response) => response,
            Err(error) => {
                attempt.finish(crate::session_data::Outcome::Error, None, None);
                return self.upstream_failed(client, &error);
            }
        };
        match prepared.as_mut() {
            Some(prepared) if response.status == 400 && !self.shadow => self.reactive(
                request,
                upstream,
                &mut prepared.ctx,
                &prepared.key,
                response,
                client,
                attempt,
                request_id,
                session,
                budget.as_ref(),
                input_capacity,
            ),
            Some(prepared) => self.relay_observed(
                client,
                response,
                request,
                attempt,
                kind,
                Some((
                    prepared.key.clone(),
                    if self.shadow {
                        prepared.ctx.est_tokens_in
                    } else {
                        prepared.ctx.est_tokens_out
                    },
                )),
            ),
            None => self.relay_observed(client, response, request, attempt, kind, None),
        }
    }

    fn relay_observed(
        &self,
        client: &mut TcpStream,
        response: Upstream,
        request: &Request,
        mut attempt: crate::proxy_observations::Attempt<'_>,
        dialect: Dialect,
        calibration: Option<((String, String), u64)>,
    ) -> Result<()> {
        let status = response.status;
        let mut tap = UsageTap::new(&response.headers, dialect);
        if let Some(tap) = &mut tap {
            tap.metrics = crate::proxy_observations::StreamMetrics::new(
                dialect,
                tap.event_stream,
                attempt.started,
            );
        }
        let result = relay_tapped(client, response, &request.method, tap.as_mut());
        if let Some(tap) = &mut tap {
            tap.metrics.finish_json();
        }
        let outcome = if result.is_err() {
            crate::session_data::Outcome::Interrupted
        } else if status >= 500 {
            crate::session_data::Outcome::Error
        } else if status >= 400 {
            crate::session_data::Outcome::Refused
        } else if tap.as_ref().is_some_and(|t| t.metrics.failed) {
            crate::session_data::Outcome::Error
        } else if result.as_ref().is_ok_and(|complete| !complete)
            || tap
                .as_ref()
                .is_some_and(|t| t.event_stream && !t.metrics.terminal)
        {
            crate::session_data::Outcome::Interrupted
        } else if !(200..300).contains(&status) || tap.as_ref().is_none_or(|t| !t.metrics.terminal)
        {
            crate::session_data::Outcome::Unknown
        } else {
            crate::session_data::Outcome::Success
        };
        attempt.finish(outcome, Some(status), tap.as_ref().map(|t| &t.metrics));
        if self.calibrate && status == 200 && result.is_ok() {
            if let (Some((key, sent)), Some(reported)) = (
                calibration,
                tap.as_ref().and_then(UsageTap::reported_tokens),
            ) {
                self.observe(key, sent, reported);
            }
        }
        result.map(|_| ())
    }

    /// Length retries remain separate attempts of the same logical request.
    #[allow(clippy::too_many_arguments)]
    fn reactive(
        &self,
        request: &Request,
        upstream: &str,
        ctx: &mut RequestCtx,
        key: &(String, String),
        response: Upstream,
        client: &mut TcpStream,
        mut attempt: crate::proxy_observations::Attempt<'_>,
        request_id: Option<crate::session_data::OpaqueId>,
        session: Option<crate::session_data::OpaqueId>,
        budget: Option<&crate::context::Decision>,
        input_capacity: Option<u64>,
    ) -> Result<()> {
        let (mut status, mut headers) = (response.status, response.headers.clone());
        let mut data = response.read_all(MAX_ERROR_BODY_BYTES)?;
        attempt.finish(crate::session_data::Outcome::Refused, Some(status), None);
        if ctx.modified
            && !is_context_error(&data)
            && within_input_capacity(ctx.est_tokens_in, ctx.ratio_permille, input_capacity)
        {
            self.stats.fail_open.fetch_add(1, Ordering::Relaxed);
            log("provider rejected the compacted request; resending the original");
            let mut original_attempt =
                self.observations
                    .start(request_id, session, ctx.dialect, Some(&key.1));
            return match send_upstream(request, upstream, &request.body) {
                Ok(original) => self.relay_observed(
                    client,
                    original,
                    request,
                    original_attempt,
                    ctx.dialect,
                    Some((key.clone(), ctx.est_tokens_in)),
                ),
                Err(error) => {
                    original_attempt.finish(crate::session_data::Outcome::Error, None, None);
                    self.upstream_failed(client, &error)
                }
            };
        }
        while is_context_error(&data) {
            let Some((mut next, body)) = self.reactive_step(ctx, request.body.len()) else {
                break;
            };
            if !within_input_capacity(next.est_tokens_out, next.ratio_permille, input_capacity) {
                break;
            }
            self.engine.publish_prepared(&mut next);
            *ctx = next;
            self.stats.reactive_retries.fetch_add(1, Ordering::Relaxed);
            log(&format!(
                "{} {}: retrying length rejection at ~{}k est tokens (step {})",
                ctx.dialect.name(),
                request.path(),
                ctx.est_tokens_out / 1000,
                ctx.rung
            ));
            let mut retry = self.observations.start(
                request_id.clone(),
                session.clone(),
                ctx.dialect,
                Some(&key.1),
            );
            record_context(&retry, ctx, false, budget);
            let again = match send_upstream(request, upstream, &body) {
                Ok(again) => again,
                Err(error) => {
                    retry.finish(crate::session_data::Outcome::Error, None, None);
                    return self.upstream_failed(client, &error);
                }
            };
            if again.status != 400 {
                return self.relay_observed(
                    client,
                    again,
                    request,
                    retry,
                    ctx.dialect,
                    Some((key.clone(), ctx.est_tokens_out)),
                );
            }
            status = again.status;
            headers = again.headers.clone();
            data = again.read_all(MAX_ERROR_BODY_BYTES)?;
            retry.finish(crate::session_data::Outcome::Refused, Some(status), None);
        }
        let headers: Vec<_> = headers
            .into_iter()
            .filter(|(name, _)| !STRIP_RESPONSE.contains(&name.to_ascii_lowercase().as_str()))
            .collect();
        write_buffered(client, status, "Bad Request", &headers, &data)
    }

    fn reactive_step(&self, ctx: &RequestCtx, input_bytes: usize) -> Option<(RequestCtx, Vec<u8>)> {
        let Some(job) = self.transforms.acquire(&self.memory, input_bytes) else {
            self.stats.fail_open.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let mut next = ctx.clone();
        let engine = Arc::clone(&self.engine);
        match job.run(self.transform_timeout, move || {
            if !engine.reactive(&mut next) {
                return None;
            }
            let body = serde_json::to_vec(&next.outgoing_body()).ok()?;
            Some((next, body))
        }) {
            Ok(next) => next,
            Err(_) => {
                self.stats.fail_open.fetch_add(1, Ordering::Relaxed);
                None
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

/// Raw character-based estimates and provider capacities use different units
/// after calibration. Apply exactly the engine's bounded conversion, including
/// integer rounding, to every body that could be sent upstream.
fn within_input_capacity(
    estimated_tokens: u64,
    ratio_permille: u32,
    capacity: Option<u64>,
) -> bool {
    capacity.is_none_or(|limit| estimated_tokens <= calibrated_threshold(limit, ratio_permille))
}

fn requested_output(parsed: &serde_json::Map<String, Value>) -> u64 {
    ["max_tokens", "max_output_tokens", "max_completion_tokens"]
        .iter()
        .filter_map(|name| parsed.get(*name).and_then(Value::as_u64))
        .max()
        .unwrap_or(32_000)
}
fn record_context(
    attempt: &crate::proxy_observations::Attempt<'_>,
    ctx: &RequestCtx,
    shadow: bool,
    budget: Option<&crate::context::Decision>,
) {
    use crate::session_data::{Event, OpaqueId, PolicyObservation};
    attempt.context(Event::ContextDecision {
        estimated_before_tokens: ctx.est_tokens_in,
        estimated_after_tokens: if shadow {
            ctx.est_tokens_in
        } else {
            ctx.est_tokens_out
        },
        threshold_tokens: ctx.threshold_tokens,
        compacted: ctx.compacted,
        shadow,
        policy: Some(PolicyObservation {
            requested_input_tokens: budget.and_then(|b| b.requested_input_tokens),
            input_capacity_tokens: budget.and_then(|b| b.input_capacity_tokens),
            policy_generation: budget.map(|b| b.policy_generation),
            limiting_reason: budget.map(|b| b.limiting_reason.clone()),
            scope_id: budget.map(|b| OpaqueId(b.scope_id.clone())),
            evidence_bytes: Some(ctx.evidence_bytes as u64),
            evicted_evidence_count: Some(ctx.evicted_evidence_digests.len() as u64),
            rescued: budget.is_some_and(|b| b.rescue_count > 0),
        }),
    });
}

struct CountGuard<'a>(&'a AtomicUsize);

impl<'a> CountGuard<'a> {
    fn acquire(count: &'a AtomicUsize) -> Self {
        count.fetch_add(1, Ordering::AcqRel);
        Self(count)
    }

    fn try_acquire(count: &'a AtomicUsize, limit: usize) -> Option<Self> {
        count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                (value < limit).then_some(value + 1)
            })
            .ok()
            .map(|_| Self(count))
    }
}

impl Drop for CountGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Own the slot before spawning, so a failed spawn drops both the socket and
/// its accounting. Nothing in the accept loop writes to a client socket.
struct ConnectionSlot(Arc<Proxy>);

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn dispatch_connection(
    stream: TcpStream,
    proxy: Arc<Proxy>,
    limit: usize,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
) {
    if proxy
        .active
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            (value < limit + CONTROL_RESERVE).then_some(value + 1)
        })
        .is_err()
    {
        // Even a rejection can block on a stopped reader. At the hard socket
        // bound close without a response; a later client connection can retry.
        return;
    }
    let slot = ConnectionSlot(proxy);
    if stream.set_read_timeout(Some(REQUEST_HEAD_TIMEOUT)).is_err()
        || stream
            .set_write_timeout(Some(CONTROL_WRITE_TIMEOUT))
            .is_err()
    {
        return;
    }
    let _ = stream.set_nodelay(true);
    let job = Box::new(move || {
        if let Err(error) = slot.0.handle(stream, limit) {
            log(&format!("connection error: {error:#}"));
        }
        drop(slot);
    });
    if let Err(error) = spawn(job) {
        log(&format!("connection worker unavailable: {error}"));
    }
}

fn serve(listener: TcpListener, proxy: Arc<Proxy>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            // A persistent resource failure must not turn accept into a CPU
            // spin. Existing workers keep custody of their requests.
            std::thread::sleep(Duration::from_millis(10));
            continue;
        };
        dispatch_connection(stream, Arc::clone(&proxy), MAX_CONNECTIONS, |job| {
            std::thread::Builder::new()
                .name("gobstopper-request".into())
                .spawn(job)
                .map(|_| ())
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
    fn bind_scope(&mut self) -> Result<()> {
        if let Some(rest) = self.target.strip_prefix("/__gobstopper/s/") {
            let (scope, path) = rest
                .split_once('/')
                .context("invalid scoped request path")?;
            if scope.len() != 64 || !scope.bytes().all(|b| b.is_ascii_hexdigit()) {
                bail!("invalid context scope");
            }
            if self
                .header("x-gobstopper-scope")
                .is_some_and(|header| header != scope)
            {
                bail!("conflicting context scopes");
            }
            let scope = scope.to_string();
            let target = format!("/{path}");
            self.headers
                .retain(|(name, _)| !name.eq_ignore_ascii_case("x-gobstopper-scope"));
            self.headers.push(("x-gobstopper-scope".into(), scope));
            self.target = target;
        }
        Ok(())
    }
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
    deadline: Instant,
}

impl Wire<'_> {
    fn fill(&mut self) -> Result<()> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .context("request deadline exceeded")?;
        self.stream.set_read_timeout(Some(remaining))?;
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
                if at > limit {
                    bail!("request head too large");
                }
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
        let mut result = Vec::new();
        self.extend_exact(&mut result, len)?;
        Ok(result)
    }

    fn extend_exact(&mut self, result: &mut Vec<u8>, len: usize) -> Result<()> {
        result
            .try_reserve_exact(len)
            .map_err(|_| work::MemoryExhausted)?;
        let mut left = len;
        while left > 0 {
            if self.buffer.is_empty() {
                self.fill()?;
            }
            let take = left.min(self.buffer.len());
            result.extend(self.buffer.drain(..take));
            left -= take;
        }
        Ok(())
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn read_request_head(wire: &mut Wire<'_>) -> Result<Request> {
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
    Ok(Request {
        method: method.to_string(),
        target: target.to_string(),
        headers,
        body: Vec::new(),
    })
}

fn read_request_body(
    wire: &mut Wire<'_>,
    request: &mut Request,
    limit: usize,
    mut memory: Option<&mut work::MemoryLease>,
) -> Result<()> {
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
            if size > limit.saturating_sub(body.len()) {
                bail!("request body too large");
            }
            if memory.as_mut().is_some_and(|m| !m.grow(size)) {
                return Err(work::MemoryExhausted.into());
            }
            wire.extend_exact(&mut body, size)?;
            wire.take_exact(2)?;
        }
        request.body = body;
    } else if let Some(length) = request.header("content-length") {
        let length: usize = length.parse().context("invalid content-length")?;
        if length > limit {
            bail!("request body too large");
        }
        if memory.as_mut().is_some_and(|m| !m.grow(length)) {
            return Err(work::MemoryExhausted.into());
        }
        request.body = wire.take_exact(length)?;
    }
    Ok(())
}

/// A response streaming from a curl child.
struct Upstream {
    child: Child,
    stdout: BufReader<ChildStdout>,
    status: u16,
    reason: String,
    headers: Vec<(String, String)>,
}

impl Drop for Upstream {
    fn drop(&mut self) {
        // All error and unwind paths retain custody of the exact spawned child.
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
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
        if let Err(error) = std::thread::Builder::new()
            .name("gobstopper-upload".into())
            .spawn(move || {
                let _ = stdin.write_all(&body);
            })
        {
            // Curl may already have connected. Preserve the no-replay rule,
            // and retain custody even though no response wrapper exists yet.
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("start upstream upload worker");
        }
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
                });
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

/// Send an early final response before discarding any unread request bytes.
/// Closing immediately can reset TCP and hide the response from the client.
/// Half-close first, then give a cooperative peer a bounded chance to finish;
/// a stalled or flooding sender cannot retain a worker indefinitely.
fn reject_unread(
    client: &mut TcpStream,
    respond: impl FnOnce(&mut TcpStream) -> Result<()>,
) -> Result<()> {
    respond(client)?;
    client.shutdown(Shutdown::Write)?;
    let deadline = Instant::now() + REJECT_DRAIN_TIMEOUT;
    let mut remaining = REJECT_DRAIN_BYTES;
    let mut buffer = [0u8; 4096];
    while remaining > 0 {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        if left.is_zero() || client.set_read_timeout(Some(left)).is_err() {
            break;
        }
        let count = remaining.min(buffer.len());
        match client.read(&mut buffer[..count]) {
            Ok(0) => break,
            Ok(read) => remaining -= read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
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

/// Keeps a bounded part of a response body so the proxy can read the
/// provider's usage after the relay: the whole body of a JSON reply, the
/// start of an Anthropic stream (whose `message_start` event opens it),
/// and the tail of an OpenAI stream (whose usage event ends it). It sees
/// each chunk only after the chunk was written to the client, never
/// changes it, and gives up past its bound.
struct UsageTap {
    metrics: crate::proxy_observations::StreamMetrics,
    dialect: Dialect,
    event_stream: bool,
    data: Vec<u8>,
    overflow: bool,
}

impl UsageTap {
    /// A tap for a JSON or event-stream response; `None` for anything else.
    /// A missing content type means an event stream: the ChatGPT backend
    /// sends none, and a JSON body parses no `data:` lines anyway.
    fn new(headers: &[(String, String)], dialect: Dialect) -> Option<Self> {
        let kind = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.to_ascii_lowercase());
        let event_stream = match kind.as_deref() {
            Some(value) if value.contains("text/event-stream") => true,
            Some(value) if value.contains("json") => false,
            Some(_) => return None,
            None => true,
        };
        Some(Self {
            metrics: crate::proxy_observations::StreamMetrics::new(
                dialect,
                event_stream,
                Instant::now(),
            ),
            dialect,
            event_stream,
            data: Vec::new(),
            overflow: false,
        })
    }

    fn observe(&mut self, bytes: &[u8]) {
        self.metrics.observe(bytes);
        let limit = if self.event_stream {
            MAX_TAP_STREAM_BYTES
        } else {
            MAX_TAP_JSON_BYTES
        };
        if self.event_stream && self.dialect != Dialect::Anthropic {
            // The usage event ends an OpenAI stream: keep the tail.
            self.data.extend_from_slice(bytes);
            let over = self.data.len().saturating_sub(limit);
            self.data.drain(..over);
            return;
        }
        let room = limit - self.data.len();
        if bytes.len() > room {
            self.overflow = true;
        }
        self.data.extend_from_slice(&bytes[..bytes.len().min(room)]);
    }

    /// The input tokens the response reports: the top-level `usage` of a
    /// JSON body, the `message.usage` of an Anthropic stream's
    /// `message_start` event, the `response.usage` of a Responses stream's
    /// last event carrying it, or the `usage` of a Chat Completions
    /// stream's last chunk carrying one. `None` when absent, malformed, or
    /// cut off by the bound.
    fn reported_tokens(&self) -> Option<u64> {
        if !self.event_stream {
            if self.overflow {
                return None;
            }
            let body: Value = serde_json::from_slice(&self.data).ok()?;
            return reported_input_tokens(self.dialect, body.get("usage")?);
        }
        let mut events = self
            .data
            .split(|b| *b == b'\n')
            .filter_map(|line| line.strip_prefix(b"data:"))
            .filter_map(|data| serde_json::from_slice::<Value>(data.trim_ascii()).ok());
        match self.dialect {
            Dialect::Anthropic => events
                .find(|event| event.get("type").and_then(Value::as_str) == Some("message_start"))
                .and_then(|event| {
                    reported_input_tokens(self.dialect, event.get("message")?.get("usage")?)
                }),
            Dialect::Responses => events
                .filter_map(|event| event.get("response").and_then(|r| r.get("usage")).cloned())
                .filter_map(|usage| reported_input_tokens(self.dialect, &usage))
                .next_back(),
            Dialect::ChatCompletions => events
                .filter_map(|event| event.get("usage").cloned())
                .filter_map(|usage| reported_input_tokens(self.dialect, &usage))
                .next_back(),
        }
    }
}

/// A writer that hands each chunk to a tap after writing it.
struct Tee<'a, W: Write> {
    inner: &'a mut W,
    tap: Option<&'a mut UsageTap>,
}

impl<W: Write> Write for Tee<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buf)?;
        if let Some(tap) = self.tap.as_deref_mut() {
            tap.observe(&buf[..written]);
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Stream and re-frame an upstream response, tapping bytes written to the client.
fn relay_tapped(
    client: &mut TcpStream,
    mut upstream: Upstream,
    method: &str,
    mut tap: Option<&mut UsageTap>,
) -> Result<bool> {
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
                let mut body = (&mut upstream.stdout).take(length);
                let copied = match tap.as_deref_mut() {
                    None => std::io::copy(&mut body, client)?,
                    Some(tap) => std::io::copy(
                        &mut body,
                        &mut Tee {
                            inner: &mut *client,
                            tap: Some(tap),
                        },
                    )?,
                };
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
                    if let Some(tap) = tap.as_deref_mut() {
                        tap.observe(&buffer[..read]);
                    }
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
    Ok(complete && (exited_cleanly || length.is_some()))
}

/// The `serve` settings given to `proxy install`, as typed: everything after
/// `install` except the flags that only `install` reads.
fn install_serve_args() -> Vec<String> {
    serve_args_after_install(&std::env::args().collect::<Vec<_>>())
}

/// Everything after `proxy install` except `--replace`, `--print`, and the
/// global session-folder options, which `serve` never reads and which could
/// hold paths relative to this shell rather than launchd's `/`.
fn serve_args_after_install(args: &[String]) -> Vec<String> {
    const GLOBAL: [&str; 3] = ["--codex-home", "--claude-home", "--codex-bin"];
    let Some(at) = args
        .windows(2)
        .position(|pair| pair[0] == "proxy" && pair[1] == "install")
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut rest = args[at + 2..].iter();
    while let Some(arg) = rest.next() {
        if matches!(arg.as_str(), "--replace" | "--print") {
            continue;
        }
        if GLOBAL.contains(&arg.as_str()) {
            rest.next();
            continue;
        }
        if GLOBAL
            .iter()
            .any(|flag| arg.starts_with(&format!("{flag}=")))
        {
            continue;
        }
        out.push(arg.clone());
    }
    out
}

fn is_unavailable(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|error| {
            matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::BrokenPipe
            )
        })
}

/// What to do when nothing answers on the status port.
fn proxy_down(port: u16) -> anyhow::Error {
    if crate::proxy_agent::installed() {
        let log = crate::proxy_agent::log_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "~/Library/Logs/gobstopper-proxy.log".into());
        crate::ux::guided_code(
            "proxy-not-running",
            format!(
                "The proxy is installed but isn't answering on 127.0.0.1:{port}. Its log is {log}"
            ),
            crate::proxy_agent::restart_command(),
        )
    } else {
        crate::ux::guided_code(
            "proxy-not-running",
            format!("No proxy is running on 127.0.0.1:{port}"),
            "gobstopper proxy install",
        )
    }
}

fn fetch_status(port: u16) -> Result<Value> {
    fetch_status_with_timeout(port, Duration::from_secs(2))
}

fn fetch_status_with_timeout(port: u16, timeout: Duration) -> Result<Value> {
    fetch_control_with_timeout(port, STATUS_PATH, timeout)
}

fn fetch_control_with_timeout(port: u16, path: &str, timeout: Duration) -> Result<Value> {
    const MAX_STATUS_BYTES: usize = 256 * 1024;
    let deadline = Instant::now() + timeout;
    let remaining = || -> std::io::Result<Duration> {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "proxy status timed out")
            })
    };
    let address = (Ipv4Addr::LOCALHOST, port).into();
    let mut stream = TcpStream::connect_timeout(&address, remaining()?)
        .with_context(|| format!("no gobstopper proxy on 127.0.0.1:{port}"))?;
    stream.set_write_timeout(Some(remaining()?))?;
    stream.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )?;
    let mut response = Vec::new();
    loop {
        // Recompute one deadline after each read: a peer sending occasional
        // bytes must not keep a diagnostic command alive indefinitely.
        stream.set_read_timeout(Some(remaining()?))?;
        let mut chunk = [0; 8192];
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        if count > MAX_STATUS_BYTES.saturating_sub(response.len()) {
            bail!("proxy status response exceeds its size limit");
        }
        response.extend_from_slice(&chunk[..count]);
    }
    let at = find(&response, b"\r\n\r\n").context("malformed status response")?;
    let header = std::str::from_utf8(&response[..at]).context("malformed status headers")?;
    let first: Vec<_> = header
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    if first.len() < 2
        || !matches!(first[0], "HTTP/1.0" | "HTTP/1.1")
        || (first[1] != "200" && !(path == READY_PATH && first[1] == "503"))
    {
        bail!("proxy status did not return HTTP 200");
    }
    let value: Value =
        serde_json::from_slice(&response[at + 4..]).context("status response is not JSON")?;
    if value["name"] != "gobstopper-proxy" || value["port"] != port {
        bail!("status response belongs to a different service or port");
    }
    Ok(value)
}

#[cfg(test)]
#[path = "proxy_resilience_tests.rs"]
mod resilience_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn status_fixture(
        response: impl FnOnce(u16) -> Vec<u8> + Send + 'static,
    ) -> (u16, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 1024];
            assert!(socket.read(&mut request).unwrap() > 0);
            // Oversized-response tests intentionally close before all bytes send.
            let _ = socket.write_all(&response(port));
        });
        (port, server)
    }

    #[test]
    fn proxy_status_accepts_legacy_identity_and_rejects_unrelated_or_error_responses() {
        for case in 0..5 {
            let (port, server) = status_fixture(move |port| {
                let status = if case == 1 {
                    "503 Unavailable"
                } else {
                    "200 OK"
                };
                let name = if case == 2 {
                    "unrelated"
                } else {
                    "gobstopper-proxy"
                };
                let port = if case == 3 { 0 } else { port };
                let body = if case == 4 {
                    "not json".to_string()
                } else {
                    json!({"name":name,"port":port,"version":"0.4.1"}).to_string()
                };
                format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .into_bytes()
            });
            let result = fetch_status(port);
            assert_eq!(result.is_ok(), case == 0, "case {case}: {result:?}");
            server.join().unwrap();
        }
    }

    #[test]
    fn proxy_status_bounds_response_bytes() {
        let (port, server) = status_fixture(|_| vec![b' '; 256 * 1024 + 1]);
        let error = fetch_status(port).unwrap_err();
        assert!(error.to_string().contains("size limit"), "{error}");
        server.join().unwrap();
    }

    #[test]
    fn proxy_status_deadline_covers_unresponsive_and_trickling_peers() {
        for trickle in [false, true] {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let (done, finished) = std::sync::mpsc::channel();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_millis(100)))
                    .unwrap();
                loop {
                    match finished.recv_timeout(Duration::from_millis(10)) {
                        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    if trickle && socket.write_all(b" ").is_err() {
                        break;
                    }
                }
            });
            let started = Instant::now();
            let result = fetch_status_with_timeout(port, Duration::from_millis(100));
            let elapsed = started.elapsed();
            let _ = done.send(());
            server.join().unwrap();
            let error = result.unwrap_err();
            assert!(is_unavailable(&error), "{error}");
            assert!(
                elapsed < Duration::from_secs(2),
                "status deadline exceeded: {elapsed:?}"
            );
        }
    }

    #[test]
    fn context_access_recovers_existing_reservation_once_after_storage_returns() {
        let root = std::env::temp_dir().join(format!(
            "gobstopper-context-recovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let directory = root.join("private");
        let saved = root.join("saved");
        let path = directory.join("context.sqlite3");
        let original = crate::context::Control::open(&path).unwrap();
        let scope = original
            .create(Some(1_000_000), Some(1_000_000), 32_000, false, None)
            .unwrap();
        original.reserve(&scope, 500_000, 32, 60).unwrap();
        drop(original);
        std::fs::rename(&directory, &saved).unwrap();
        std::fs::write(&directory, b"temporarily unavailable storage").unwrap();

        let access = Arc::new(ContextAccess::new(Some(path)));
        assert!(access.get().is_none());
        assert!(!access.available.load(Ordering::Acquire));
        std::fs::remove_file(&directory).unwrap();
        std::fs::rename(&saved, &directory).unwrap();
        // Recovery is rate limited even when the storage problem is resolved.
        assert!(access.get().is_none());
        access.state.lock().unwrap().retry_after = Instant::now();

        let barrier = Arc::new(std::sync::Barrier::new(16));
        let workers: Vec<_> = (0..16)
            .map(|_| {
                let access = Arc::clone(&access);
                let barrier = Arc::clone(&barrier);
                let scope = scope.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let control = loop {
                        if let Some(control) = access.get() {
                            break control;
                        }
                        assert!(
                            Instant::now() < deadline,
                            "recover existing context database"
                        );
                        std::thread::sleep(Duration::from_millis(1));
                    };
                    let decision = control.consume(&scope, 128_000, None, 32_000).unwrap();
                    assert_eq!(decision.effective_input_tokens, 500_000);
                    control
                })
            })
            .collect();
        let controls: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        assert!(controls.iter().all(|c| Arc::ptr_eq(c, &controls[0])));
        assert_eq!(controls[0].status(&scope).unwrap().remaining_requests, 16);
        assert!(access.available.load(Ordering::Acquire));
        drop(controls);
        drop(access);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn context_access_initialization_never_touches_optional_storage() {
        let path = std::env::temp_dir().join(format!(
            "gobstopper-lazy-context-{}",
            crate::proxy_agent::unique_id()
        ));
        let access = ContextAccess::new(Some(path.clone()));
        assert!(!path.exists());
        assert!(access.current().is_none());
        assert!(!access.available.load(Ordering::Acquire));
        // Simulate a filesystem open stalled while holding the initialization lock.
        let held = access.state.lock().unwrap();
        let started = Instant::now();
        assert!(access.get().is_none());
        assert!(access.current().is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(held);
        assert!(!path.exists());
    }

    #[test]
    fn context_access_recovery_preserves_unrecognized_storage() {
        let root = std::env::temp_dir().join(format!(
            "gobstopper-context-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = root.join("context.sqlite3");
        drop(crate::context::Control::open(&path).unwrap());
        let db = rusqlite::Connection::open(&path).unwrap();
        db.pragma_update(None, "application_id", 123).unwrap();
        drop(db);
        let before = std::fs::read(&path).unwrap();
        let access = ContextAccess::new(Some(path.clone()));
        access.state.lock().unwrap().retry_after = Instant::now();
        assert!(access.get().is_none());
        assert!(!access.available.load(Ordering::Acquire));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(access.state.lock().unwrap().retry_after > Instant::now());
        drop(access);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn input_capacity_uses_calibration_and_rounds_down() {
        assert!(within_input_capacity(1_000, 1_000, Some(1_000)));
        assert!(!within_input_capacity(1_001, 1_000, Some(1_000)));
        assert!(within_input_capacity(976, 1_024, Some(1_000)));
        assert!(!within_input_capacity(977, 1_024, Some(1_000)));
        assert!(within_input_capacity(500, 2_000, Some(1_000)));
        assert!(!within_input_capacity(600, 2_000, Some(1_000)));
        assert!(within_input_capacity(u64::MAX, 2_000, None));
        assert!(within_input_capacity(0, 2_000, Some(0)));
        assert!(!within_input_capacity(1, 2_000, Some(0)));
        // Match the engine's clamping and avoid arithmetic overflow.
        assert!(within_input_capacity(u64::MAX, 0, Some(u64::MAX)));
        assert!(within_input_capacity(
            u64::MAX / 2,
            u32::MAX,
            Some(u64::MAX)
        ));
        assert!(!within_input_capacity(
            u64::MAX / 2 + 1,
            u32::MAX,
            Some(u64::MAX)
        ));
    }

    #[test]
    fn input_capacity_rejects_calibrated_soft_overflow_with_a_small_head() {
        let payload = json!({"model":"test", "max_tokens":10,"messages":[
            {"role":"user","content":"hi"},
            {"role":"assistant","content":"ok"},
            {"role":"user","content":"x".repeat(2_400)}
        ]});
        let engine = Engine::new(CliffConfig::default());
        let ctx = engine
            .prepare_with_policy(
                payload.as_object().unwrap().clone(),
                Dialect::Anthropic,
                128_000,
                2_000,
                "capacity-regression",
                Some(1_000),
            )
            .unwrap();
        assert!(!ctx.capacity_exceeded_by_floor);
        assert!(ctx.over_budget);
        assert!(
            ctx.est_tokens_out <= 1_000,
            "the uncalibrated comparison would permit this request"
        );
        assert!(!within_input_capacity(
            ctx.est_tokens_out,
            ctx.ratio_permille,
            Some(1_000)
        ));
        assert!(
            !within_input_capacity(ctx.est_tokens_in, ctx.ratio_permille, Some(1_000)),
            "the original-body fallback must also be refused"
        );
    }

    #[test]
    fn install_keeps_serve_settings_and_drops_install_and_global_options() {
        let args: Vec<String> = [
            "gobstopper",
            "--codex-home",
            "./codex",
            "proxy",
            "install",
            "--threshold",
            "256000",
            "--replace",
            "--claude-home",
            "rel/claude",
            "--codex-bin=./codex",
            "--port",
            "8261",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            serve_args_after_install(&args),
            ["--threshold", "256000", "--port", "8261"]
        );
    }

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
    pub(super) fn test_proxy(threshold_tokens: u64, threshold_1m: u64) -> Proxy {
        Proxy {
            power: crate::power::Power::new(false),
            observations: crate::proxy_observations::Recorder::disabled(),
            control: Arc::new(ContextAccess::new(None)),
            context_window: None,
            service_id: None,
            admission: Mutex::new(crate::proxy_drain::Admission::default()),
            instance_id: crate::proxy_agent::unique_id(),
            startup_guard: false,
            engine: Arc::new(Engine::new(CliffConfig {
                threshold_tokens,
                ..CliffConfig::default()
            })),
            transforms: work::Workers::default(),
            transform_timeout: Duration::from_secs(5),
            memory: work::MemoryBudget::new(work::BODY_BUDGET),
            threshold_1m,
            shadow: false,
            calibrate: true,
            calibrations: Mutex::new(HashMap::new()),
            anthropic: "https://api.anthropic.com".into(),
            openai: "https://api.openai.com".into(),
            chatgpt: "https://chatgpt.com".into(),
            port: 0,
            started: Instant::now(),
            active: AtomicUsize::new(0),
            forwarding: AtomicUsize::new(0),
            inference: AtomicUsize::new(0),
            executable: None,
            stats: Stats::default(),
            stats_log: StatsLog::at(None),
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
            .unwrap()
            .0;
        assert_eq!(ctx.base_threshold_tokens, 256_000);
        assert_eq!(proxy.count(&proxy.stats.requests_1m), 1);

        let base = [("anthropic-version", "2023-06-01")];
        let ctx = proxy
            .prepare(&request("/v1/messages", &base, &messages))
            .unwrap()
            .0;
        assert_eq!(ctx.base_threshold_tokens, 128_000);

        // The header means nothing to the OpenAI dialects.
        let ctx = proxy
            .prepare(&request("/v1/responses", &one_m, &input))
            .unwrap()
            .0;
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
        // Both startup lines show both thresholds, the tail percent and the
        // carry.
        assert_eq!(
            test_proxy(128_000, 128_000).settings(),
            format!(
                "threshold 128000 tokens, threshold_1m 128000 tokens, keep_recent 3, keep_tail_percent 0, carry_max_chars {}, calibrate on",
                CliffConfig::default().carry_max_chars
            )
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
        current["carry_max_chars"] = json!(24_000);
        let text = status_text(8260, &current);
        assert_eq!(
            text.lines().next(),
            Some(
                "gobstopper proxy on 127.0.0.1:8260: threshold 128000 tokens, threshold_1m 256000 tokens, keep_recent 3, keep_tail_percent 40, carry_max_chars 24000"
            )
        );
        assert!(
            text.contains("\nrequests 10 (4 with a 1M window), compacted 2,"),
            "{text}"
        );

        // An explicit null is treated as absent.
        current["requests_1m"] = Value::Null;
        current["carry_max_chars"] = Value::Null;
        assert!(!status_text(8260, &current).contains("null"));
    }

    fn tap(dialect: Dialect, content_type: &str, chunks: &[&[u8]]) -> UsageTap {
        let mut tap = UsageTap::new(&headers(&[("Content-Type", content_type)]), dialect).unwrap();
        for chunk in chunks {
            tap.observe(chunk);
        }
        tap
    }

    #[test]
    fn the_usage_tap_reads_json_and_the_stream_start_and_tolerates_the_rest() {
        let body = br#"{"type":"message","content":[],"usage":{"input_tokens":10,"cache_creation_input_tokens":200,"cache_read_input_tokens":3000,"output_tokens":9}}"#;
        let (a, b) = body.split_at(40);
        assert_eq!(
            tap(Dialect::Anthropic, "application/json", &[a, b]).reported_tokens(),
            Some(3210)
        );
        assert_eq!(
            tap(Dialect::Anthropic, "application/json", &[b"{\"usage\":{}}"]).reported_tokens(),
            None
        );
        assert_eq!(
            tap(Dialect::Anthropic, "application/json", &[b"{\"usage\":"]).reported_tokens(),
            None
        );
        assert_eq!(
            tap(Dialect::Anthropic, "application/json", &[b"not json"]).reported_tokens(),
            None
        );
        assert_eq!(
            tap(Dialect::Anthropic, "application/json", &[b"[1]"]).reported_tokens(),
            None
        );

        let stream = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":4,\"cache_read_input_tokens\":96}}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"input_tokens\":1}}\n\n";
        let (a, b) = stream.as_bytes().split_at(30);
        assert_eq!(
            tap(
                Dialect::Anthropic,
                "text/event-stream; charset=utf-8",
                &[a, b]
            )
            .reported_tokens(),
            Some(100)
        );
        assert_eq!(
            tap(
                Dialect::Anthropic,
                "text/event-stream",
                &[b"data: {\"type\":\"message_start\",\"message\":{}}\n"]
            )
            .reported_tokens(),
            None
        );
        assert_eq!(
            tap(
                Dialect::Anthropic,
                "text/event-stream",
                &[b"data: {broken\n", b"\n"]
            )
            .reported_tokens(),
            None
        );
        assert_eq!(
            tap(Dialect::Anthropic, "text/event-stream", &[]).reported_tokens(),
            None
        );
        assert!(UsageTap::new(
            &headers(&[("content-type", "text/plain")]),
            Dialect::Anthropic
        )
        .is_none());
        // The ChatGPT backend sends no content type on its event streams.
        assert!(UsageTap::new(&[], Dialect::Responses).is_some());
    }

    #[test]
    fn the_usage_tap_reads_openai_json_and_stream_tails() {
        // A Responses JSON reply and a Chat Completions JSON reply.
        let responses = tap(
            Dialect::Responses,
            "application/json",
            &[br#"{"status":"completed","usage":{"input_tokens":410,"output_tokens":9}}"#],
        );
        assert_eq!(responses.reported_tokens(), Some(410));
        let chat = tap(
            Dialect::ChatCompletions,
            "application/json",
            &[br#"{"choices":[],"usage":{"prompt_tokens":512,"completion_tokens":9}}"#],
        );
        assert_eq!(chat.reported_tokens(), Some(512));
        // A Chat Completions usage field shaped like the other dialect is
        // not read.
        assert_eq!(
            tap(
                Dialect::ChatCompletions,
                "application/json",
                &[br#"{"usage":{"input_tokens":7}}"#]
            )
            .reported_tokens(),
            None
        );

        // A Responses stream's terminal `response.completed` carries the
        // count; an earlier event's incomplete usage does not win.
        let stream = concat!(
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"usage\":null}}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":777,\"output_tokens\":3}}}\n\n"
        );
        assert_eq!(
            tap(
                Dialect::Responses,
                "text/event-stream",
                &[stream.as_bytes()]
            )
            .reported_tokens(),
            Some(777)
        );
        // Past the stream bound the tap still sees the tail.
        let mut padded = format!("data: {}\n\n", "x".repeat(MAX_TAP_STREAM_BYTES));
        padded.push_str(stream);
        let (a, rest) = padded.as_bytes().split_at(10);
        let (b, c) = rest.split_at(rest.len() / 2);
        assert_eq!(
            tap(Dialect::Responses, "text/event-stream", &[a, b, c]).reported_tokens(),
            Some(777)
        );

        // A Chat Completions stream's final `usage` chunk.
        let chat_stream = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":640,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n"
        );
        assert_eq!(
            tap(
                Dialect::ChatCompletions,
                "text/event-stream",
                &[chat_stream.as_bytes()]
            )
            .reported_tokens(),
            Some(640)
        );
        // Without a usage chunk or terminal event there is no sample.
        assert_eq!(
            tap(
                Dialect::ChatCompletions,
                "text/event-stream",
                &[b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n"]
            )
            .reported_tokens(),
            None
        );
        assert_eq!(
            tap(
                Dialect::Responses,
                "text/event-stream",
                &[b"data: {\"type\":\"response.output_text.delta\"}\n\n"]
            )
            .reported_tokens(),
            None
        );
    }

    #[test]
    fn the_usage_tap_is_bounded() {
        let mut big = tap(
            Dialect::Anthropic,
            "application/json",
            &[b"{\"usage\":{\"input_tokens\":5}}"],
        );
        assert_eq!(big.reported_tokens(), Some(5));
        big.observe(&vec![b' '; MAX_TAP_JSON_BYTES]);
        assert_eq!(big.data.len(), MAX_TAP_JSON_BYTES);
        assert_eq!(
            big.reported_tokens(),
            None,
            "a cut-off JSON body is not sampled"
        );

        let start =
            b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5}}}\n";
        let mut stream = tap(Dialect::Anthropic, "text/event-stream", &[start]);
        stream.observe(&vec![b'x'; 3 * MAX_TAP_STREAM_BYTES]);
        assert_eq!(stream.data.len(), MAX_TAP_STREAM_BYTES);
        assert_eq!(
            stream.reported_tokens(),
            Some(5),
            "the stream start survives the bound"
        );

        let mut openai = tap(Dialect::Responses, "text/event-stream", &[]);
        openai.observe(&vec![b'x'; 3 * MAX_TAP_STREAM_BYTES]);
        assert_eq!(
            openai.data.len(),
            MAX_TAP_STREAM_BYTES,
            "an OpenAI stream keeps the tail, not the start"
        );
    }

    #[test]
    fn the_tee_writes_every_byte_unchanged_before_the_tap_sees_it() {
        let mut out = Vec::new();
        let mut usage = tap(Dialect::Anthropic, "application/json", &[]);
        let body = br#"{"usage":{"input_tokens":77}}"#.to_vec();
        let copied = std::io::copy(
            &mut &body[..],
            &mut Tee {
                inner: &mut out,
                tap: Some(&mut usage),
            },
        )
        .unwrap();
        assert_eq!(copied as usize, body.len());
        assert_eq!(out, body);
        assert_eq!(usage.reported_tokens(), Some(77));
    }

    #[test]
    fn calibration_applies_per_upstream_and_model_after_enough_samples() {
        let proxy = test_proxy(128_000, 256_000);
        let base = [("anthropic-version", "2023-06-01")];
        let messages = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
        let (ctx, key) = proxy
            .prepare(&request("/v1/messages", &base, &messages))
            .unwrap();
        assert_eq!(
            key,
            ("https://api.anthropic.com".to_string(), "m".to_string())
        );
        assert_eq!(ctx.ratio_permille, 1000);
        for _ in 0..5 {
            proxy.observe(key.clone(), 100_000, 125_000);
        }
        let (ctx, _) = proxy
            .prepare(&request("/v1/messages", &base, &messages))
            .unwrap();
        assert_eq!(ctx.ratio_permille, 1250);
        assert_eq!(ctx.calibrated_threshold_tokens, 102_400);
        assert_eq!(ctx.base_threshold_tokens, 128_000);

        // Another model, and another upstream, are not affected.
        let other = json!({"model": "n", "messages": [{"role": "user", "content": "hi"}]});
        let (ctx, _) = proxy
            .prepare(&request("/v1/messages", &base, &other))
            .unwrap();
        assert_eq!(ctx.ratio_permille, 1000);
        let input = json!({"model": "m", "input": [{"role": "user", "content": "hi"}]});
        let (ctx, _) = proxy
            .prepare(&request("/v1/responses", &[], &input))
            .unwrap();
        assert_eq!(ctx.ratio_permille, 1000);
        // The learned ratio is per upstream and model, not per dialect: a
        // Responses request routed to the calibrated upstream gets it too.
        let (ctx, _) = proxy
            .prepare(&request("/v1/responses", &base, &input))
            .unwrap();
        assert_eq!(ctx.ratio_permille, 1250);

        let status = proxy.status();
        assert_eq!(status["calibrate"], true);
        assert_eq!(
            status["calibrations"],
            json!([{"upstream": "https://api.anthropic.com", "model": "m",
                    "samples": 5, "measured_ratio": 1.25, "ratio": 1.25}])
        );
        let text = status_text(8260, &status);
        assert!(
            text.ends_with("estimate calibration: m via https://api.anthropic.com: ratio 1.25 applied, 1.25 measured over 5 samples\n"),
            "{text}"
        );

        // Off: the learned ratio is never applied.
        let mut off = test_proxy(128_000, 256_000);
        off.calibrate = false;
        for _ in 0..5 {
            off.observe(key.clone(), 100_000, 125_000);
        }
        let (ctx, _) = off
            .prepare(&request("/v1/messages", &base, &messages))
            .unwrap();
        assert_eq!(ctx.ratio_permille, 1000);
        assert!(status_text(8260, &off.status()).ends_with("estimate calibration: off\n"));
    }

    #[test]
    fn calibration_keys_are_bounded() {
        let proxy = test_proxy(128_000, 256_000);
        for i in 0..MAX_CALIBRATIONS + 10 {
            proxy.observe(("u".into(), format!("m{i}")), 100_000, 125_000);
        }
        assert_eq!(proxy.calibrations().len(), MAX_CALIBRATIONS);
        proxy.observe(("u".into(), "m0".into()), 100_000, 125_000);
        assert_eq!(
            proxy.calibrations()[&("u".into(), "m0".into())].samples(),
            2
        );
    }
}
