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
};
use serde_json::{json, Value};
use std::borrow::Cow;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
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
    /// Compact when the estimated outgoing request exceeds this many tokens.
    /// Keep it below the client's own auto-compaction point.
    #[arg(long, default_value_t = DEFAULT_THRESHOLD_TOKENS)]
    threshold: u64,
    /// Newest assistant steps kept verbatim.
    #[arg(long, default_value_t = 3)]
    keep_recent: usize,
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
    /// Upstream for OpenAI API requests (`/v1/...`).
    #[arg(long, default_value = "https://api.openai.com")]
    openai_upstream: String,
    /// Upstream for ChatGPT-authenticated Codex requests (`/backend-api/...`).
    #[arg(long, default_value = "https://chatgpt.com")]
    chatgpt_upstream: String,
}

/// Resolves a session argument to its provider and transcript path.
pub type Resolve<'a> = dyn Fn(&str) -> Result<(gobstopper_core::Provider, std::path::PathBuf)> + 'a;

pub fn run(command: &ProxyCmd, resolve: &Resolve) -> Result<()> {
    match command {
        ProxyCmd::Replay {
            session,
            threshold,
            keep_recent,
            result_max_chars,
            fixed_tokens,
            json,
        } => {
            let (provider, path) = resolve(session)?;
            let (dialect, history) = replay::history_from_file(provider, &path)?;
            let cfg = CliffConfig {
                threshold_tokens: *threshold,
                keep_recent: *keep_recent,
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
                "replayed {} requests from {} ({} messages); threshold {} tokens, keep_recent {}, {} fixed tokens assumed",
                report.requests,
                path.display(),
                history.len(),
                threshold,
                keep_recent,
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
                "threshold {} tokens, keep_recent {}, result_max_chars {}{}{}",
                opts.threshold,
                opts.keep_recent,
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
            log(&format!("{base}, threshold {} tokens", opts.threshold));
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
                println!(
                    "gobstopper proxy on 127.0.0.1:{port}: threshold {} tokens, keep_recent {}{}",
                    status["threshold_tokens"],
                    status["keep_recent"],
                    if status["shadow"] == true {
                        ", shadow"
                    } else {
                        ""
                    }
                );
                println!(
                    "requests {}, compacted {}, reused a compacted prefix {}, retried after a length error {}, upstream errors {}, uptime {}s",
                    status["requests"],
                    status["compacted"],
                    status["matched"],
                    status["reactive_retries"],
                    status["upstream_errors"],
                    status["uptime_secs"]
                );
            }
            Ok(())
        }
    }
}

fn log(message: &str) {
    eprintln!("{} gobstopper proxy: {message}", rfc3339_now());
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
}

struct Proxy {
    engine: Engine,
    shadow: bool,
    anthropic: String,
    openai: String,
    chatgpt: String,
    port: u16,
    started: Instant,
    active: AtomicUsize,
    stats: Stats,
}

impl Proxy {
    fn new(opts: &ProxyOpts, port: u16) -> Result<Self> {
        let cfg = CliffConfig {
            threshold_tokens: opts.threshold,
            keep_recent: opts.keep_recent,
            result_max_chars: opts.result_max_chars,
            keep_thinking: !opts.drop_thinking,
            strict: opts.strict,
            ..CliffConfig::default()
        };
        Ok(Self {
            engine: Engine::new(cfg),
            shadow: opts.shadow,
            anthropic: validate_upstream(&opts.anthropic_upstream)?,
            openai: validate_upstream(&opts.openai_upstream)?,
            chatgpt: validate_upstream(&opts.chatgpt_upstream)?,
            port,
            started: Instant::now(),
            active: AtomicUsize::new(0),
            stats: Stats::default(),
        })
    }

    fn count(&self, counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

    fn status(&self) -> Value {
        let cfg = self.engine.config();
        let (entries, chars) = self.engine.store_stats();
        json!({
            "name": "gobstopper-proxy",
            "version": env!("CARGO_PKG_VERSION"),
            "port": self.port,
            "threshold_tokens": cfg.threshold_tokens,
            "keep_recent": cfg.keep_recent,
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
            "active_connections": self.active.load(Ordering::Relaxed),
            "uptime_secs": self.started.elapsed().as_secs(),
        })
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
    /// Codex uses `/backend-api/`; other `/v1/` paths are OpenAI's; anything
    /// else defaults to Anthropic, the client most likely to send it.
    fn upstream_for(&self, request: &Request) -> &str {
        let path = request.path();
        if request.header("anthropic-version").is_some() {
            &self.anthropic
        } else if path.starts_with("/backend-api/") {
            &self.chatgpt
        } else if path.starts_with("/v1/") {
            &self.openai
        } else {
            &self.anthropic
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
        // Engine failures must never fail the request.
        let prepared = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.engine.prepare(parsed, dialect)
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
        let kind = ctx.dialect.name();
        let path = request.path();
        let shadow = if self.shadow {
            " (shadow: sent unchanged)"
        } else {
            ""
        };
        if ctx.compacted {
            self.stats.compacted.fetch_add(1, Ordering::Relaxed);
            let raised = if ctx.threshold_tokens > self.engine.config().threshold_tokens {
                format!(
                    ", threshold raised to ~{}k by a large verbatim head",
                    ctx.threshold_tokens / 1000
                )
            } else {
                String::new()
            };
            log(&format!(
                "{kind} {path}: compacted ~{}k -> ~{}k est tokens, {} -> {} messages, {} crossing(s), step {}{raised}{}{shadow}",
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
                "{kind} {path}: reused compacted prefix, ~{}k -> ~{}k est tokens{shadow}",
                ctx.est_tokens_in / 1000,
                ctx.est_tokens_out / 1000,
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
                        ctx.est_tokens_out,
                        self.engine.config().threshold_tokens
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
        while is_context_error(&data) && self.engine.reactive(ctx) {
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
}
