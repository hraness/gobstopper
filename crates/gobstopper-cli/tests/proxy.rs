//! `gobstopper proxy` end to end: the real binary between a raw HTTP client
//! and a fake upstream, over real sockets and the system curl.

#[path = "proxy/drain.rs"]
mod drain;

use gobstopper_adapters::request::{CliffConfig, Dialect, SUMMARY_HEADER};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug)]
struct Recorded {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Recorded {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

enum Reply {
    Json(u16, Value),
    Sse(Vec<String>),
    /// An event stream with no content type, as the ChatGPT backend sends.
    SseBare(Vec<String>),
}

type Responder = dyn Fn(&Recorded) -> Reply + Send + Sync;

struct Fake {
    port: u16,
    seen: Arc<Mutex<Vec<Recorded>>>,
}

impl Fake {
    fn start(responder: impl Fn(&Recorded) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let responder: Arc<Responder> = Arc::new(responder);
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (log, responder) = (Arc::clone(&log), Arc::clone(&responder));
                std::thread::spawn(move || serve_fake(stream, &log, &*responder));
            }
        });
        Self { port, seen }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn seen(&self) -> Vec<Recorded> {
        self.seen.lock().unwrap().clone()
    }
}

fn read_head(reader: &mut impl BufRead) -> Option<(String, Vec<(String, String)>)> {
    let mut first = String::new();
    if reader.read_line(&mut first).ok()? == 0 {
        return None;
    }
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }
    Some((first.trim_end().to_string(), headers))
}

fn read_body(reader: &mut impl BufRead, headers: &[(String, String)]) -> Vec<u8> {
    let find = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    };
    let mut body = Vec::new();
    if find("transfer-encoding").is_some_and(|v| v.contains("chunked")) {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size).unwrap();
            let size = usize::from_str_radix(size.trim(), 16).unwrap();
            if size == 0 {
                let mut end = String::new();
                reader.read_line(&mut end).unwrap();
                break;
            }
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk).unwrap();
            body.extend_from_slice(&chunk[..size]);
        }
    } else if let Some(length) = find("content-length") {
        body.resize(length.parse().unwrap(), 0);
        reader.read_exact(&mut body).unwrap();
    } else {
        reader.read_to_end(&mut body).unwrap();
    }
    body
}

fn serve_fake(stream: TcpStream, log: &Mutex<Vec<Recorded>>, responder: &Responder) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let Some((line, headers)) = read_head(&mut reader) else {
        return;
    };
    let body = read_body(&mut reader, &headers);
    let mut parts = line.split(' ');
    let recorded = Recorded {
        method: parts.next().unwrap().to_string(),
        target: parts.next().unwrap().to_string(),
        headers,
        body,
    };
    log.lock().unwrap().push(recorded.clone());
    let mut out = stream;
    let reply = responder(&recorded);
    let bare = matches!(&reply, Reply::SseBare(_));
    match reply {
        Reply::Json(status, value) => {
            let body = serde_json::to_vec(&value).unwrap();
            let head = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\nx-upstream: fake\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            out.write_all(head.as_bytes()).unwrap();
            out.write_all(&body).unwrap();
        }
        Reply::Sse(events) | Reply::SseBare(events) => {
            let content_type = if bare {
                ""
            } else {
                "content-type: text/event-stream\r\n"
            };
            out.write_all(
                format!("HTTP/1.1 200 OK\r\n{content_type}transfer-encoding: chunked\r\nconnection: close\r\n\r\n").as_bytes(),
            )
            .unwrap();
            for event in events {
                out.write_all(format!("{:x}\r\n{event}\r\n", event.len()).as_bytes())
                    .unwrap();
                out.flush().unwrap();
            }
            out.write_all(b"0\r\n\r\n").unwrap();
        }
    }
}

struct ProxyProcess {
    data_root: std::path::PathBuf,
    child: Child,
    port: u16,
}

impl Drop for ProxyProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.data_root);
    }
}

#[cfg(unix)]
#[test]
fn stalled_startup_stdout_does_not_block_proxy_readiness() {
    use std::io::Read;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::time::Instant;

    let (unread, mut output) = UnixStream::pair().unwrap();
    output.set_nonblocking(true).unwrap();
    loop {
        match output.write(&[b'x'; 4096]) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("fill task-owned output socket: {error}"),
        }
    }
    output.set_nonblocking(false).unwrap();
    let output: OwnedFd = output.into();
    let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = reservation.local_addr().unwrap().port();
    let data_root = std::env::temp_dir().join(format!(
        "gobstopper-blocked-startup-{}-{port}",
        std::process::id()
    ));
    let mut command = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GOBSTOPPER_") {
            command.env_remove(key);
        }
    }
    command
        .env("HRANESS_AUDIENCE", "quiet")
        .env("HRANESS_NO_UPDATE", "1")
        .env("XDG_CONFIG_HOME", data_root.join("config"))
        .env("GOBSTOPPER_DATA_DIR", data_root.join("data"))
        .env("GOBSTOPPER_STATS_FILE", "off")
        .args([
            "proxy",
            "serve",
            "--port",
            &port.to_string(),
            "--no-keep-awake",
        ])
        .stdout(Stdio::from(output))
        .stderr(Stdio::null());
    drop(reservation);
    let mut proxy = ProxyProcess {
        child: command.spawn().unwrap(),
        port,
        data_root,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stream = loop {
        if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)) {
            break stream;
        }
        assert!(Instant::now() < deadline, "proxy never bound its port");
        assert!(
            proxy.child.try_wait().unwrap().is_none(),
            "proxy exited during startup"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .write_all(
            b"GET /gobstopper/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let (_, body) = response.split_once("\r\n\r\n").unwrap();
    let ready: Value = serde_json::from_str(body).unwrap();
    assert_eq!(ready["pid"], proxy.child.id());
    assert_eq!(ready["ready"], true);
    // Stop only our test child before closing its deliberately unread stdout.
    drop(proxy);
    drop(unread);
}

fn start_proxy(upstream: &str, extra: &[&str]) -> ProxyProcess {
    start_proxy_with(upstream, upstream, extra)
}

fn start_proxy_env(upstream: &str, extra: &[&str], env: &[(&str, &str)]) -> ProxyProcess {
    start_proxy_inner(upstream, upstream, upstream, extra, env, Stdio::null())
}

fn start_proxy_with(anthropic: &str, chatgpt: &str, extra: &[&str]) -> ProxyProcess {
    start_proxy_inner(anthropic, anthropic, chatgpt, extra, &[], Stdio::null())
}

/// A proxy whose log lines (stderr) go to `log`.
fn start_proxy_logged(upstream: &str, extra: &[&str], log: &std::path::Path) -> ProxyProcess {
    let file = std::fs::File::create(log).unwrap();
    start_proxy_inner(upstream, upstream, upstream, extra, &[], Stdio::from(file))
}

fn start_proxy_inner(
    anthropic: &str,
    openai: &str,
    chatgpt: &str,
    extra: &[&str],
    env: &[(&str, &str)],
    stderr: Stdio,
) -> ProxyProcess {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let data_root = std::env::temp_dir().join(format!(
        "gobstopper-proxy-data-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut command = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
    // Read as a pipe would, even from an agent session.
    command.env("HRANESS_AUDIENCE", "quiet");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GOBSTOPPER_") {
            command.env_remove(key);
        }
    }
    // No ledger unless a test names one: the default path is the user's
    // real ledger, whose all-time totals a test run must not change.
    command
        .env("XDG_CONFIG_HOME", std::env::temp_dir())
        .env("GOBSTOPPER_STATS_FILE", "off")
        .env("GOBSTOPPER_DATA_DIR", &data_root)
        .args(["proxy", "serve", "--port", "0"])
        .args(["--anthropic-upstream", anthropic])
        .args(["--openai-upstream", openai])
        .args(["--chatgpt-upstream", chatgpt])
        .args(extra);
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(stderr)
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port = line
        .trim()
        .rsplit(':')
        .next()
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("unexpected proxy banner: {line}"));
    ProxyProcess {
        child,
        port,
        data_root,
    }
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

fn request(
    port: u16,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Response {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut head = format!("{method} {target} HTTP/1.1\r\n");
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("host"))
    {
        head.push_str(&format!("host: 127.0.0.1:{port}\r\n"));
    }
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    ));
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut reader = BufReader::new(stream);
    let (status_line, headers) = read_head(&mut reader).unwrap();
    let status = status_line.split(' ').nth(1).unwrap().parse().unwrap();
    let body = read_body(&mut reader, &headers);
    Response {
        status,
        headers,
        body,
    }
}

const ANTHROPIC: &[(&str, &str)] = &[
    ("content-type", "application/json"),
    ("anthropic-version", "2023-06-01"),
    ("x-api-key", "sk-ant-synthetic-test-key"),
];

fn session(turns: usize) -> Vec<Value> {
    let mut messages = vec![json!({"role": "user", "content": "Fix the failing test."})];
    for i in 0..turns {
        messages.push(json!({"role": "assistant", "content": [
            {"type": "text", "text": format!("Step {i}: running the tests.")},
            {"type": "tool_use", "id": format!("tu_{i}"), "name": "bash", "input": {"command": format!("pytest -x {i}")}}
        ]}));
        let output = if i % 2 == 0 {
            "X".repeat(3000)
        } else {
            format!("test_{i} passed")
        };
        messages.push(json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": format!("tu_{i}"), "content": output, "cache_control": {"type": "ephemeral"}}
        ]}));
    }
    messages
}

fn body(messages: &[Value]) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "model": "claude-test",
        "max_tokens": 1024,
        "stream": true,
        "system": "You are a coding agent.",
        "messages": messages,
    }))
    .unwrap()
}

const OPENAI: &[(&str, &str)] = &[
    ("content-type", "application/json"),
    ("authorization", "Bearer synthetic"),
];

/// An OpenAI Chat Completions session: system and user, then `turns`
/// assistant calls with their tool results.
fn chat_session(turns: usize) -> Vec<Value> {
    let mut messages = vec![
        json!({"role": "system", "content": "you are an agent"}),
        json!({"role": "user", "content": "Fix the bug."}),
    ];
    for n in 0..turns {
        messages.push(json!({"role": "assistant", "content": format!("step {n}"),
            "tool_calls": [{"id": format!("call_{n}"), "type": "function",
                "function": {"name": "bash", "arguments": format!("{{\"cmd\":\"s{n}\"}}")}}]}));
        messages.push(json!({"role": "tool", "tool_call_id": format!("call_{n}"),
            "content": "R".repeat(3000)}));
    }
    messages
}

fn chat_body(messages: &[Value]) -> Vec<u8> {
    serde_json::to_vec(&json!({"model": "gpt-test", "messages": messages, "stream": true})).unwrap()
}

fn responses_body(messages: &[Value]) -> Vec<u8> {
    serde_json::to_vec(&json!({"model": "gpt-test", "input": messages, "stream": true})).unwrap()
}

fn summaries(sent: &Value) -> Vec<String> {
    sent["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["content"].as_str())
        .filter(|text| text.starts_with(SUMMARY_HEADER))
        .map(str::to_string)
        .collect()
}

fn sse_events() -> Vec<String> {
    vec![
        "event: message_start\ndata: {\"type\":\"message_start\"}\n\n".to_string(),
        "event: content_block_delta\ndata: {\"delta\":{\"text\":\"hi\"}}\n\n".to_string(),
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string(),
    ]
}

#[test]
fn small_requests_pass_through_byte_for_byte_with_headers() {
    let fake = Fake::start(|_| Reply::Json(200, json!({"ok": true})));
    let proxy = start_proxy(&fake.url(), &[]);
    let sent = body(&session(2));
    let response = request(
        proxy.port,
        "POST",
        "/v1/messages?beta=true",
        ANTHROPIC,
        &sent,
    );
    assert_eq!(response.status, 200);
    assert_eq!(response.json(), json!({"ok": true}));
    assert!(response
        .headers
        .iter()
        .any(|(name, value)| name == "x-upstream" && value == "fake"));
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].target, "/v1/messages?beta=true");
    assert_eq!(
        seen[0].body, sent,
        "unmodified requests are forwarded verbatim"
    );
    assert_eq!(
        seen[0].header("x-api-key"),
        Some("sk-ant-synthetic-test-key")
    );
    assert_eq!(seen[0].header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(seen[0].header("accept-encoding"), Some("identity"));
}

#[test]
fn over_threshold_requests_are_compacted_streamed_and_reused() {
    let fake = Fake::start(|_| Reply::Sse(sse_events()));
    let proxy = start_proxy(
        &fake.url(),
        &[
            "--threshold",
            "2000",
            "--keep-recent",
            "1",
            "--evidence-max-bytes",
            "0",
        ],
    );
    // Small closing turns leave headroom under the threshold after the last
    // compaction, so the follow-up below is a pure prefix reuse.
    let mut messages = session(12);
    for i in 12..15 {
        messages.push(json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": format!("tu_{i}"), "name": "bash", "input": {"command": "ls"}}
        ]}));
        messages.push(json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": format!("tu_{i}"), "content": "ok"}
        ]}));
    }
    let response = request(
        proxy.port,
        "POST",
        "/v1/messages",
        ANTHROPIC,
        &body(&messages),
    );
    assert_eq!(response.status, 200);
    assert_eq!(
        String::from_utf8(response.body).unwrap(),
        sse_events().concat()
    );

    let first = fake.seen()[0].json();
    let found = summaries(&first);
    assert_eq!(found.len(), 1);
    assert!(!found[0].contains("XXXX"), "long tool results are dropped");
    assert!(
        found[0].contains("[bash] {\"command\":\"pytest -x "),
        "tool calls become signatures"
    );
    // Several threshold crossings were replayed; each pass dropped the
    // previous summary instead of nesting it. The oldest steps' words may
    // be carried forward, but their tool calls are gone.
    assert_eq!(found[0].matches(SUMMARY_HEADER).count(), 1);
    assert!(!found[0].contains("\"pytest -x 0\""));
    assert_eq!(
        first["messages"][0], messages[0],
        "the head is kept verbatim"
    );
    assert_eq!(first["system"], "You are a coding agent.");

    // The client resends its original history plus one step; the proxy
    // substitutes the same summary, so the provider sees a stable prefix.
    let mut grown = messages.clone();
    grown.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Done."}]}));
    grown.push(json!({"role": "user", "content": "thanks"}));
    let response = request(proxy.port, "POST", "/v1/messages", ANTHROPIC, &body(&grown));
    assert_eq!(response.status, 200);
    let second = fake.seen()[1].json();
    assert_eq!(summaries(&second), found);
    assert_eq!(second["messages"].as_array().unwrap().last(), grown.last());

    let status = request(proxy.port, "GET", "/gobstopper/status", &[], b"").json();
    assert_eq!(status["compacted"], 1);
    assert_eq!(status["matched"], 1);
    assert_eq!(status["threshold_tokens"], 2000);
}

#[test]
fn a_length_rejection_is_retried_compacted() {
    let fake = Fake::start(|recorded| {
        if summaries(&recorded.json()).is_empty() {
            Reply::Json(
                400,
                json!({"type": "error", "error": {"type": "invalid_request_error", "message": "prompt is too long: 250000 tokens > 200000 maximum"}}),
            )
        } else {
            Reply::Json(200, json!({"ok": "retried"}))
        }
    });
    let proxy = start_proxy(
        &fake.url(),
        &["--threshold", "1000000", "--keep-recent", "1"],
    );
    let response = request(
        proxy.port,
        "POST",
        "/v1/messages",
        ANTHROPIC,
        &body(&session(8)),
    );
    assert_eq!(response.status, 200);
    assert_eq!(response.json(), json!({"ok": "retried"}));
    assert_eq!(fake.seen().len(), 2);
}

#[test]
fn other_client_errors_are_relayed_unchanged() {
    let error = json!({"type": "error", "error": {"type": "invalid_request_error", "message": "max_tokens: must be positive"}});
    let reply = error.clone();
    let fake = Fake::start(move |_| Reply::Json(400, reply.clone()));
    let proxy = start_proxy(&fake.url(), &["--threshold", "1000000"]);
    let response = request(
        proxy.port,
        "POST",
        "/v1/messages",
        ANTHROPIC,
        &body(&session(8)),
    );
    assert_eq!(response.status, 400);
    assert_eq!(response.json(), error);
    assert_eq!(fake.seen().len(), 1);
}

#[test]
fn a_rejected_compaction_falls_back_to_the_original_request() {
    let fake = Fake::start(|recorded| {
        if summaries(&recorded.json()).is_empty() {
            Reply::Json(200, json!({"ok": "original"}))
        } else {
            Reply::Json(
                400,
                json!({"type": "error", "error": {"type": "invalid_request_error", "message": "messages.1: unexpected block"}}),
            )
        }
    });
    let proxy = start_proxy(&fake.url(), &["--threshold", "2000", "--keep-recent", "1"]);
    let sent = body(&session(12));
    let response = request(proxy.port, "POST", "/v1/messages", ANTHROPIC, &sent);
    assert_eq!(response.status, 200);
    assert_eq!(response.json(), json!({"ok": "original"}));
    let seen = fake.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[1].body, sent,
        "the fallback resends the client's bytes"
    );
}

#[test]
fn shadow_mode_and_malformed_bodies_forward_the_original_bytes() {
    let fake = Fake::start(|_| Reply::Json(200, json!({})));
    let proxy = start_proxy(&fake.url(), &["--threshold", "2000", "--shadow"]);
    let sent = body(&session(12));
    request(proxy.port, "POST", "/v1/messages", ANTHROPIC, &sent);
    request(proxy.port, "POST", "/v1/messages", ANTHROPIC, b"{not json");
    let seen = fake.seen();
    assert_eq!(seen[0].body, sent);
    assert_eq!(seen[1].body, b"{not json");
}

#[test]
fn codex_requests_route_to_the_chatgpt_upstream_and_compact() {
    let anthropic = Fake::start(|_| Reply::Json(200, json!({"from": "anthropic"})));
    let chatgpt = Fake::start(|_| Reply::Json(200, json!({"from": "chatgpt"})));
    let proxy = start_proxy_with(
        &anthropic.url(),
        &chatgpt.url(),
        &["--threshold", "2000", "--keep-recent", "1"],
    );
    let mut input = vec![
        json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Fix the build."}]}),
    ];
    for n in 0..10 {
        input.push(
            json!({"type": "reasoning", "encrypted_content": format!("enc{n}"), "summary": []}),
        );
        input.push(json!({"type": "function_call", "call_id": format!("c{n}"), "name": "shell", "arguments": "{\"command\":[\"make\"]}"}));
        input.push(json!({"type": "function_call_output", "call_id": format!("c{n}"), "output": "O".repeat(3000)}));
    }
    let sent = serde_json::to_vec(
        &json!({"model": "gpt-test", "instructions": "be brief", "input": input, "stream": true}),
    )
    .unwrap();
    let headers = [
        ("content-type", "application/json"),
        ("authorization", "Bearer synthetic"),
        ("chatgpt-account-id", "acct"),
    ];
    let response = request(
        proxy.port,
        "POST",
        "/backend-api/codex/responses",
        &headers,
        &sent,
    );
    assert_eq!(response.json(), json!({"from": "chatgpt"}));
    assert!(anthropic.seen().is_empty());
    let seen = chatgpt.seen()[0].json();
    let items = seen["input"].as_array().unwrap();
    assert!(items.len() < input.len());
    assert!(items[1]["content"][0]["text"]
        .as_str()
        .unwrap()
        .starts_with(SUMMARY_HEADER));
    assert_eq!(seen["instructions"], "be brief");
    assert_eq!(
        chatgpt.seen()[0].header("authorization"),
        Some("Bearer synthetic")
    );
}

#[test]
fn chat_completions_route_to_openai_and_compact() {
    let anthropic = Fake::start(|_| Reply::Json(200, json!({"from": "anthropic"})));
    let openai = Fake::start(|_| Reply::Json(200, json!({"from": "openai"})));
    let proxy = start_proxy_inner(
        &anthropic.url(),
        &openai.url(),
        &anthropic.url(),
        &["--threshold", "2000", "--keep-recent", "1"],
        &[],
        Stdio::null(),
    );
    let messages = chat_session(10);
    let sent = chat_body(&messages);
    for target in ["/v1/chat/completions", "/chat/completions"] {
        let response = request(proxy.port, "POST", target, OPENAI, &sent);
        assert_eq!(response.json(), json!({"from": "openai"}), "{target}");
    }
    assert!(anthropic.seen().is_empty());
    let seen = openai.seen()[0].json();
    let sent_messages = seen["messages"].as_array().unwrap();
    assert!(sent_messages.len() < messages.len());
    assert_eq!(
        seen["messages"][0], messages[0],
        "the head is kept verbatim"
    );
    assert!(seen["messages"][2]["content"]
        .as_str()
        .unwrap()
        .starts_with(SUMMARY_HEADER));
    // The kept tail is whole turns: assistant calls stay with their tools.
    assert_eq!(sent_messages.last().unwrap()["role"], "tool");
    assert_eq!(
        openai.seen()[0].header("authorization"),
        Some("Bearer synthetic")
    );
}

#[test]
fn only_loopback_hosts_are_served_and_upstream_failures_are_reported() {
    let proxy = start_proxy("http://127.0.0.1:9", &[]);
    let refused = request(
        proxy.port,
        "GET",
        "/v1/models",
        &[("host", "attacker.example")],
        b"",
    );
    assert_eq!(refused.status, 403);
    let failed = request(proxy.port, "GET", "/v1/models", ANTHROPIC, b"");
    assert_eq!(failed.status, 502);
    assert_eq!(failed.json()["type"], "error");
}

#[test]
fn the_stats_ledger_records_each_request_and_survives_a_restart() {
    let dir = std::env::temp_dir().join(format!("gobstopper-stats-test-{}", std::process::id()));
    let stats = dir.join("proxy-stats.jsonl");
    let stats_str = stats.to_str().unwrap();
    let fake = Fake::start(|_| Reply::Json(200, json!({"ok": true})));

    let proxy = start_proxy_env(
        &fake.url(),
        &["--threshold", "2000", "--keep-recent", "1"],
        &[("GOBSTOPPER_STATS_FILE", stats_str)],
    );
    request(
        proxy.port,
        "POST",
        "/v1/messages",
        ANTHROPIC,
        &body(&session(12)),
    );
    let status = request(proxy.port, "GET", "/gobstopper/status", &[], b"").json();
    assert_eq!(status["stats_file"], stats_str);
    let first_in = status["est_tokens_in"].as_u64().unwrap();
    let first_out = status["est_tokens_out"].as_u64().unwrap();
    assert!(first_in > first_out && first_out > 0);
    assert_eq!(status["all_time_est_tokens_in"], first_in);
    wait_for_stats_records(&stats, 1);
    drop(proxy);

    let lines: Vec<Value> = std::fs::read_to_string(&stats)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["dialect"], "anthropic");
    assert_eq!(lines[0]["est_tokens_in"], first_in);
    assert_eq!(lines[0]["est_tokens_out"], first_out);
    assert_eq!(lines[0]["compacted"], true);
    assert!(lines[0]["ts"].as_str().unwrap().contains('T'));

    // A restarted proxy keeps the all-time totals from the ledger.
    let second = start_proxy_env(&fake.url(), &[], &[("GOBSTOPPER_STATS_FILE", stats_str)]);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let status = loop {
        let status = request(second.port, "GET", "/gobstopper/status", &[], b"").json();
        if status["stats_persistence"]["loading_history"] == false {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "stats history did not finish loading: {status}"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(status["all_time_est_tokens_in"], first_in);
    assert_eq!(status["all_time_est_tokens_out"], first_out);
    drop(second);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Request completion intentionally does not wait for optional disk writes.
/// Observe complete records before forcibly stopping the owned test process.
fn wait_for_stats_records(path: &std::path::Path, expected: usize) {
    wait_for_file_lines(path, expected, "");
}

fn wait_for_file_lines(path: &std::path::Path, expected: usize, tag: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match std::fs::read_to_string(path) {
            Ok(contents) if contents.ends_with('\n') => {
                let count = contents.lines().filter(|line| line.contains(tag)).count();
                if count >= expected {
                    assert_eq!(
                        count, expected,
                        "unexpected number of complete {tag:?} lines"
                    );
                    return;
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("cannot read diagnostic output: {error}"),
        }
        assert!(
            std::time::Instant::now() < deadline,
            "writer did not append {expected} complete {tag:?} lines"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The binary with no inherited `GOBSTOPPER_` settings, reading config from
/// a scratch directory.
fn gobstopper(args: &[&str], home: &std::path::Path) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
    // Read as a pipe would, even from an agent session.
    command.env("HRANESS_AUDIENCE", "quiet");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GOBSTOPPER_") {
            command.env_remove(key);
        }
    }
    command
        .env("XDG_CONFIG_HOME", home)
        .env("XDG_DATA_HOME", home)
        .args(args)
        .output()
        .unwrap()
}

/// A Claude Code transcript of 60 tool steps: a 3,000-character result
/// every third step and 700-character results otherwise.
fn tail_transcript(dir: &std::path::Path) -> std::path::PathBuf {
    let mut records =
        vec![json!({"type": "user", "message": {"role": "user", "content": "the task"}})];
    for i in 0..60 {
        records.push(json!({"type": "assistant", "message": {"id": format!("m{i}"), "role": "assistant", "content": [
            {"type": "text", "text": format!("Step {i} {}", "t".repeat(200))},
            {"type": "tool_use", "id": format!("tu_{i}"), "name": "bash", "input": {"command": format!("cmd {i}")}}
        ]}}));
        let size = if i % 3 == 0 { 3000 } else { 700 };
        records.push(json!({"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": format!("tu_{i}"), "content": "R".repeat(size)}
        ]}}));
    }
    let path = dir.join("session.jsonl");
    let lines: String = records.iter().map(|record| format!("{record}\n")).collect();
    std::fs::write(&path, lines).unwrap();
    path
}

fn scratch_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gobstopper-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_tail_percent_is_bounded_and_reported_in_status() {
    let dir = scratch_dir("tail-flag");
    let transcript = tail_transcript(&dir);
    let transcript = transcript.to_str().unwrap();
    for args in [
        vec!["proxy", "serve", "--port", "0", "--keep-tail-percent", "61"],
        vec!["proxy", "replay", transcript, "--keep-tail-percent", "61"],
    ] {
        let output = gobstopper(&args, &dir);
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("61 is not in 0..=60"));
    }
    let proxy = start_proxy("http://127.0.0.1:9", &["--keep-tail-percent", "60"]);
    let status = request(proxy.port, "GET", "/gobstopper/status", &[], b"").json();
    assert_eq!(status["keep_tail_percent"], 60);
    // Since v0.7.3 the default is 0; the old default of 40 is still a flag away.
    let default = start_proxy("http://127.0.0.1:9", &[]);
    let status = request(default.port, "GET", "/gobstopper/status", &[], b"").json();
    assert_eq!(status["keep_tail_percent"], 0);
    let old = start_proxy("http://127.0.0.1:9", &["--keep-tail-percent", "40"]);
    let status = request(old.port, "GET", "/gobstopper/status", &[], b"").json();
    assert_eq!(status["keep_tail_percent"], 40);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_1m_threshold_below_the_base_is_rejected_and_the_default_is_reported() {
    let dir = scratch_dir("threshold-1m-flag");
    let output = gobstopper(
        &[
            "proxy",
            "serve",
            "--port",
            "0",
            "--threshold",
            "128000",
            "--threshold-1m",
            "127999",
        ],
        &dir,
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("--threshold-1m 127999 is below --threshold 128000"));
    let _ = std::fs::remove_dir_all(&dir);

    let unset = start_proxy("http://127.0.0.1:9", &["--threshold", "300000"]);
    let status = request(unset.port, "GET", "/gobstopper/status", &[], b"").json();
    assert_eq!(
        (&status["threshold_tokens"], &status["threshold_1m_tokens"]),
        (&json!(300_000), &json!(300_000))
    );
    assert_eq!(status["requests_1m"], 0);
    let set = start_proxy("http://127.0.0.1:9", &["--threshold-1m", "128000"]);
    let status = request(set.port, "GET", "/gobstopper/status", &[], b"").json();
    assert_eq!(
        status["threshold_1m_tokens"], 128_000,
        "equal turns the split off"
    );
}

/// Headers of an Anthropic request that declares the 1M-token window.
const ANTHROPIC_1M: &[(&str, &str)] = &[
    ("content-type", "application/json"),
    ("anthropic-version", "2023-06-01"),
    ("x-api-key", "sk-ant-synthetic-test-key"),
    (
        "anthropic-beta",
        "interleaved-thinking-2025-05-14, context-1m-2025-08-07",
    ),
];

#[test]
fn a_1m_request_is_compacted_at_the_1m_threshold_without_a_raise() {
    let dir = scratch_dir("threshold-1m-log");
    let log = dir.join("proxy.log");
    let fake = Fake::start(|_| Reply::Sse(sse_events()));
    let proxy = start_proxy_logged(
        &fake.url(),
        &[
            "--threshold",
            "2000",
            "--threshold-1m",
            "8000",
            "--keep-recent",
            "1",
        ],
        &log,
    );
    // Over the base threshold, under the 1M one: sent byte for byte.
    let under = body(&session(12));
    let response = request(proxy.port, "POST", "/v1/messages", ANTHROPIC_1M, &under);
    assert_eq!(response.status, 200);
    assert_eq!(fake.seen()[0].body, under);

    // Over the 1M threshold: compacted to fit it, not the base threshold.
    let over = body(&session(40));
    let response = request(proxy.port, "POST", "/v1/messages", ANTHROPIC_1M, &over);
    assert_eq!(response.status, 200);
    let sent = fake.seen()[1].body.len() as u64;
    assert!(sent < over.len() as u64);
    assert!(sent / 4 > 2000, "sized for the 1M threshold: {sent} bytes");

    let status = request(proxy.port, "GET", "/gobstopper/status", &[], b"").json();
    assert_eq!(status["requests_1m"], 2);
    assert_eq!(status["compacted"], 1);
    wait_for_file_lines(&log, 1, "compacted ~");
    drop(proxy);
    let lines = std::fs::read_to_string(&log).unwrap();
    let compacted: Vec<&str> = lines
        .lines()
        .filter(|l| l.contains("compacted ~"))
        .collect();
    assert_eq!(compacted.len(), 1, "{lines}");
    assert!(!compacted[0].contains("threshold raised"), "{lines}");
    assert!(!compacted[0].contains("still over threshold"), "{lines}");
    assert!(
        !lines.contains("context-1m"),
        "header values are never logged"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_request_under_a_raised_threshold_is_logged_and_recorded() {
    let dir = scratch_dir("raised-threshold-log");
    let (log, stats) = (dir.join("proxy.log"), dir.join("proxy-stats.jsonl"));
    let fake = Fake::start(|_| Reply::Sse(sse_events()));
    let url = fake.url();
    let proxy = start_proxy_inner(
        &url,
        &url,
        &url,
        &["--threshold", "2000"],
        &[("GOBSTOPPER_STATS_FILE", stats.to_str().unwrap())],
        Stdio::from(std::fs::File::create(&log).unwrap()),
    );
    // A head of about 3,000 estimated tokens raises the threshold to about
    // 4,000, so a request of about 3,500 is over 2,000 but sent unchanged.
    let mut messages = session(1);
    messages[0] = json!({"role": "user", "content": "Y".repeat(12_000)});
    let sent = body(&messages);
    let response = request(proxy.port, "POST", "/v1/messages", ANTHROPIC, &sent);
    assert_eq!(response.status, 200);
    assert_eq!(fake.seen()[0].body, sent);
    wait_for_stats_records(&stats, 1);
    wait_for_file_lines(&log, 1, "sent unchanged at ~");
    drop(proxy);

    let lines = std::fs::read_to_string(&log).unwrap();
    let unchanged: Vec<&str> = lines
        .lines()
        .filter(|l| l.contains("sent unchanged at ~"))
        .collect();
    assert_eq!(unchanged.len(), 1, "{lines}");
    assert!(
        unchanged[0].contains("under the threshold raised to ~4k by a large verbatim head"),
        "{lines}"
    );
    assert!(!lines.contains("YYYY"), "content is never logged: {lines}");
    let record: Value = serde_json::from_str(
        std::fs::read_to_string(&stats)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(record["compacted"], false, "{record}");
    let threshold = record["threshold_tokens"].as_u64().unwrap();
    assert!(
        threshold > record["est_tokens_in"].as_u64().unwrap(),
        "{record}"
    );
    assert!(threshold > 2000, "{record}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_request_with_nothing_to_compact_is_logged_as_such() {
    let dir = scratch_dir("nothing-to-compact-log");
    let log = dir.join("proxy.log");
    let fake = Fake::start(|_| Reply::Sse(sse_events()));
    let url = fake.url();
    let proxy = start_proxy_inner(
        &url,
        &url,
        &url,
        &["--threshold", "2000"],
        &[],
        Stdio::from(std::fs::File::create(&log).unwrap()),
    );
    // A short head and one long recent turn: over 2,000 with no older turn to
    // summarize, so the request goes out unchanged over an unraised threshold.
    let messages = vec![
        json!({"role": "user", "content": "task"}),
        json!({"role": "assistant", "content": "Z".repeat(12_000)}),
        json!({"role": "user", "content": "continue"}),
    ];
    let sent = body(&messages);
    let response = request(proxy.port, "POST", "/v1/messages", ANTHROPIC, &sent);
    assert_eq!(response.status, 200);
    assert_eq!(fake.seen()[0].body, sent);
    wait_for_file_lines(&log, 1, "sent unchanged at ~");
    drop(proxy);

    let lines = std::fs::read_to_string(&log).unwrap();
    let unchanged: Vec<&str> = lines
        .lines()
        .filter(|l| l.contains("sent unchanged at ~"))
        .collect();
    assert_eq!(unchanged.len(), 1, "{lines}");
    assert!(
        unchanged[0].contains("over the ~2k threshold with nothing to compact"),
        "{lines}"
    );
    assert!(!lines.contains("ZZZZ"), "content is never logged: {lines}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_strict_refusal_names_the_selected_threshold() {
    let fake = Fake::start(|_| Reply::Json(200, json!({"ok": true})));
    let proxy = start_proxy(
        &fake.url(),
        &["--threshold", "1000", "--threshold-1m", "2000", "--strict"],
    );
    let messages = vec![
        json!({"role": "user", "content": "Fix the failing test."}),
        json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": "tu_0", "name": "bash", "input": {"command": "cat log"}}
        ]}),
        json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "tu_0", "content": "R".repeat(40_000)}
        ]}),
    ];
    for (headers, threshold) in [(ANTHROPIC_1M, 2000), (ANTHROPIC, 1000)] {
        let response = request(
            proxy.port,
            "POST",
            "/v1/messages",
            headers,
            &body(&messages),
        );
        assert_eq!(response.status, 400);
        let error = response.json();
        assert_eq!(error["error"]["type"], "gobstopper_over_budget");
        let message = error["error"]["message"].as_str().unwrap();
        assert!(
            message.ends_with(&format!("over the {threshold} token threshold")),
            "{message}"
        );
    }
    assert!(
        fake.seen().is_empty(),
        "a refused request never reaches the provider"
    );
}

#[test]
fn log_lines_and_ledger_records_carry_sizes_and_the_window_but_no_content() {
    let dir = scratch_dir("window-log");
    let (log, stats) = (dir.join("proxy.log"), dir.join("proxy-stats.jsonl"));
    let fake = Fake::start(|_| Reply::Sse(sse_events()));
    let url = fake.url();
    let proxy = start_proxy_inner(
        &url,
        &url,
        &url,
        &[
            "--threshold",
            "2000",
            "--threshold-1m",
            "8000",
            "--keep-recent",
            "1",
        ],
        &[("GOBSTOPPER_STATS_FILE", stats.to_str().unwrap())],
        Stdio::from(std::fs::File::create(&log).unwrap()),
    );
    // A base-window compaction, its prefix reuse, then a 1M-window one.
    let mut messages = session(12);
    for i in 12..15 {
        messages.push(json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": format!("tu_{i}"), "name": "bash", "input": {"command": "ls"}}
        ]}));
        messages.push(json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": format!("tu_{i}"), "content": "ok"}
        ]}));
    }
    let mut grown = messages.clone();
    grown.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Done."}]}));
    grown.push(json!({"role": "user", "content": "thanks"}));
    for (headers, messages) in [
        (ANTHROPIC, &messages),
        (ANTHROPIC, &grown),
        (ANTHROPIC_1M, &session(40)),
    ] {
        let response = request(proxy.port, "POST", "/v1/messages", headers, &body(messages));
        assert_eq!(response.status, 200);
    }
    let status = request(proxy.port, "GET", "/gobstopper/status", &[], b"").json();
    assert_eq!(
        (
            &status["compacted"],
            &status["matched"],
            &status["requests_1m"]
        ),
        (&json!(2), &json!(1), &json!(1))
    );
    wait_for_stats_records(&stats, 3);
    wait_for_file_lines(&log, 3, "window=");
    drop(proxy);

    let lines = std::fs::read_to_string(&log).unwrap();
    let startup = format!(
        "threshold 2000 tokens, threshold_1m 8000 tokens, keep_recent 1, keep_tail_percent 0, carry_max_chars {}, calibrate on, result_max_chars",
        CliffConfig::default().carry_max_chars
    );
    assert!(
        lines.contains(&startup),
        "the startup line shows both thresholds, the tail percent and the carry: {lines}"
    );
    let tagged: Vec<&str> = lines.lines().filter(|l| l.contains("window=")).collect();
    assert_eq!(tagged.len(), 3, "{lines}");
    for (line, (kind, window)) in tagged.iter().zip([
        ("compacted ~", "window=base"),
        ("reused compacted prefix", "window=base"),
        ("compacted ~", "window=1m"),
    ]) {
        assert!(line.contains(kind) && line.contains(window), "{line}");
        assert!(
            line.contains("(head ~")
                && line.contains(", summary ~")
                && line.contains(", tail ~")
                && line.contains(" chars)"),
            "{line}"
        );
    }
    for content in [
        "Fix the failing",
        "pytest",
        "XXXX",
        "Step ",
        "coding agent",
        "thanks",
        "sk-ant",
        "context-1m",
        "interleaved",
    ] {
        assert!(!lines.contains(content), "{content} logged: {lines}");
    }

    let records: Vec<Value> = std::fs::read_to_string(&stats)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let windows: Vec<&Value> = records.iter().map(|record| &record["window"]).collect();
    assert_eq!(windows, [&json!("base"), &json!("base"), &json!("1m")]);
    for record in &records {
        let size = |key: &str| {
            record[key]
                .as_u64()
                .unwrap_or_else(|| panic!("{key}: {record}"))
        };
        let parts = size("est_head_tokens") + size("est_summary_tokens") + size("est_tail_tokens");
        assert!(size("est_summary_tokens") > 0, "{record}");
        assert!(parts > 0 && parts <= size("est_tokens_out"), "{record}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn proxy_status_prints_no_null_for_an_older_server() {
    let dir = scratch_dir("status-text");
    // A 0.4.1 server: no threshold_1m_tokens, keep_tail_percent or requests_1m.
    let old = TcpListener::bind("127.0.0.1:0").unwrap();
    let listen_port = old.local_addr().unwrap().port();
    let port = listen_port.to_string();
    let server = std::thread::spawn(move || {
        let (stream, _) = old.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let (line, _) = read_head(&mut reader).unwrap();
        let body = serde_json::to_vec(&json!({
            "name": "gobstopper-proxy", "version": "0.4.1", "port": listen_port,
            "threshold_tokens": 128_000, "keep_recent": 3, "result_max_chars": 500,
            "keep_thinking": true, "shadow": false, "strict": false,
            "store_entries": 0, "store_chars": 0, "requests": 5, "compacted": 1,
            "matched": 3, "reactive_retries": 0, "upstream_errors": 0, "fail_open": 0,
            "est_tokens_in": 100, "est_tokens_out": 50,
            "all_time_est_tokens_in": 100, "all_time_est_tokens_out": 50,
            "stats_file": null, "active_connections": 1, "uptime_secs": 9
        }))
        .unwrap();
        let mut out = stream;
        write!(
            out,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        out.write_all(&body).unwrap();
        line
    });
    let output = gobstopper(&["proxy", "status", "--port", &port], &dir);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("null"), "{text}");
    assert!(
        text.starts_with(&format!(
            "gobstopper proxy on 127.0.0.1:{port}: threshold 128000 tokens, keep_recent 3\nrequests 5, compacted 1,"
        )),
        "{text}"
    );
    assert_eq!(server.join().unwrap(), "GET /gobstopper/status HTTP/1.1");

    let fake = Fake::start(|_| Reply::Json(200, json!({"ok": true})));
    let current = start_proxy(
        &fake.url(),
        &["--threshold-1m", "300000", "--carry-max-chars", "7000"],
    );
    let port = current.port.to_string();
    let output = gobstopper(&["proxy", "status", "--port", &port], &dir);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains(": threshold 128000 tokens, threshold_1m 300000 tokens, keep_recent 3, keep_tail_percent 0, carry_max_chars 7000\nrequests 0 (0 with a 1M window), compacted 0,"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn replay_at_zero_tail_percent_matches_the_reference_report() {
    let dir = scratch_dir("tail-replay");
    let transcript = tail_transcript(&dir);
    let replay = |percent: &str, carry: &str| -> Value {
        let output = gobstopper(
            &[
                "proxy",
                "replay",
                transcript.to_str().unwrap(),
                "--threshold",
                "6000",
                "--fixed-tokens",
                "1000",
                "--keep-tail-percent",
                percent,
                "--carry-max-chars",
                carry,
                "--evidence-max-bytes",
                "0",
                "--json",
            ],
            &dir,
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };
    // Recorded with gobstopper 0.4.0, before the tail budget and the carry
    // existed.
    let reference = replay("0", "0");
    for (field, value) in [
        ("requests", 61),
        ("compacted", 8),
        ("reused_prefix", 43),
        ("over_threshold_after", 0),
        ("peak_est_tokens_out", 5830),
        ("last_est_tokens_out", 3238),
        ("total_est_tokens_in", 952_007),
        ("total_est_tokens_out", 253_588),
        ("pairing_violations", 0),
    ] {
        assert_eq!(reference[field], value, "{field}");
    }
    let first = &reference["compactions"][0];
    for (field, value) in [
        ("request", 10),
        ("est_tokens_before", 6197),
        ("est_tokens_after", 2943),
        ("messages_before", 21),
        ("messages_after", 8),
    ] {
        assert_eq!(first[field], value, "{field}");
    }
    // The flag reaches the engine: a 60% budget keeps more and compacts
    // more often on the same transcript.
    let tail = replay("60", "0");
    assert_eq!(tail["compacted"], 10);
    assert_eq!(tail["pairing_violations"], 0);
    assert!(tail["total_est_tokens_out"].as_u64() > reference["total_est_tokens_out"].as_u64());
    let carry_chars = |report: &Value| -> Vec<u64> {
        report["compactions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["carry_chars"].as_u64().unwrap())
            .collect()
    };
    assert!(carry_chars(&reference).iter().all(|&chars| chars == 0));
    // The carry flag reaches the engine too: from the second compaction on,
    // each summary shows the earlier steps' visible text.
    let carried = replay("0", "24000");
    let shown = carry_chars(&carried);
    assert_eq!(shown[0], 0);
    assert!(
        shown.len() >= 2 && shown[1..].iter().all(|&chars| chars > 0),
        "{shown:?}"
    );
    let text = gobstopper(
        &[
            "proxy",
            "replay",
            transcript.to_str().unwrap(),
            "--threshold",
            "6000",
            "--carry-max-chars",
            "24000",
        ],
        &dir,
    );
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(
        text.contains(", keep_tail_percent 0, carry_max_chars 24000, "),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A session of tool steps and, every fourth step, a visible reply and a
/// human instruction; every carried word holds `CARRYMARK`.
fn talk_session(turns: usize) -> Vec<Value> {
    let mut messages = vec![json!({"role": "user", "content": "Fix the failing test."})];
    for i in 0..turns {
        if i % 4 == 3 {
            messages.push(json!({"role": "assistant", "content": [
                {"type": "text", "text": format!("CARRYMARK reply {i}: part {i} is done.")}
            ]}));
            messages.push(json!({"role": "user", "content": format!("CARRYMARK instruction {i}: take part {} next.", i + 1)}));
            continue;
        }
        messages.push(json!({"role": "assistant", "content": [
            {"type": "text", "text": format!("CARRYMARK step {i}: running the tests.")},
            {"type": "tool_use", "id": format!("tu_{i}"), "name": "bash", "input": {"command": format!("pytest -x {i}")}}
        ]}));
        messages.push(json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": format!("tu_{i}"), "content": "X".repeat(1500)}
        ]}));
    }
    messages
}

#[test]
fn a_carry_is_sent_upstream_and_only_its_size_is_logged() {
    const LABEL: &str = "Earlier in this session (oldest first; older text omitted):";
    let dir = scratch_dir("carry-log");
    let fake = Fake::start(|_| Reply::Sse(sse_events()));
    let url = fake.url();
    let messages = talk_session(40);
    let mut grown = messages.clone();
    grown.push(
        json!({"role": "assistant", "content": [{"type": "text", "text": "CARRYMARK done."}]}),
    );
    grown.push(json!({"role": "user", "content": "CARRYMARK thanks"}));
    let mut shown = Vec::new();
    for carry in ["24000", "0"] {
        let (log, stats) = (
            dir.join(format!("proxy-{carry}.log")),
            dir.join(format!("proxy-stats-{carry}.jsonl")),
        );
        let proxy = start_proxy_inner(
            &url,
            &url,
            &url,
            &[
                "--threshold",
                "3000",
                "--keep-recent",
                "1",
                "--carry-max-chars",
                carry,
            ],
            &[("GOBSTOPPER_STATS_FILE", stats.to_str().unwrap())],
            Stdio::from(std::fs::File::create(&log).unwrap()),
        );
        for messages in [&messages, &grown] {
            let response = request(
                proxy.port,
                "POST",
                "/v1/messages",
                ANTHROPIC,
                &body(messages),
            );
            assert_eq!(response.status, 200);
        }
        let status = request(proxy.port, "GET", "/gobstopper/status", &[], b"").json();
        assert_eq!(status["carry_max_chars"], carry.parse::<u64>().unwrap());
        assert_eq!(
            (&status["compacted"], &status["matched"]),
            (&json!(1), &json!(1))
        );
        let port = proxy.port.to_string();
        let text = gobstopper(&["proxy", "status", "--port", &port], &dir).stdout;
        let text = String::from_utf8(text).unwrap();
        assert!(
            text.contains(&format!(", keep_tail_percent 0, carry_max_chars {carry}\n")),
            "{text}"
        );
        wait_for_stats_records(&stats, 2);
        wait_for_file_lines(&log, 2, "window=");
        drop(proxy);

        // The provider gets the carried words; the log and the ledger get
        // their size only.
        let sent = fake.seen().last().unwrap().json();
        let summary = summaries(&sent).pop().unwrap();
        assert_eq!(summary.contains(LABEL), carry != "0", "{summary}");
        let lines = std::fs::read_to_string(&log).unwrap();
        assert!(
            lines.contains(&format!("carry_max_chars {carry}")),
            "{lines}"
        );
        let tagged: Vec<&str> = lines.lines().filter(|l| l.contains("window=")).collect();
        assert_eq!(tagged.len(), 2, "{lines}");
        let logged: Vec<u64> = tagged
            .iter()
            .map(|line| {
                let (_, rest) = line.split_once(", carry ").unwrap();
                rest.split_once(" chars)").unwrap().0.parse().unwrap()
            })
            .collect();
        let ledger = std::fs::read_to_string(&stats).unwrap();
        let recorded: Vec<u64> = ledger
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .map(|record| record["carry_chars"].as_u64().unwrap())
            .collect();
        assert_eq!(logged, recorded);
        for content in [
            "CARRYMARK",
            "Earlier in this session",
            "Fix the failing",
            "XXXX",
        ] {
            assert!(!lines.contains(content), "{content} logged: {lines}");
            assert!(!ledger.contains(content), "{content} recorded: {ledger}");
        }
        shown.push(logged);
    }
    // With the carry on, both requests send a carried section; with it at
    // 0 neither does.
    assert!(shown[0].iter().all(|&chars| chars > 0), "{shown:?}");
    assert_eq!(shown[1], [0, 0]);

    // 0 is accepted and a negative size is not.
    let output = gobstopper(
        &["proxy", "serve", "--port", "0", "--carry-max-chars", "-1"],
        &dir,
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The estimate a fresh engine gives an Anthropic body.
fn estimate(body: &[u8]) -> u64 {
    estimate_as(body, Dialect::Anthropic)
}

/// The estimate a fresh engine gives a body of `dialect`.
fn estimate_as(body: &[u8], dialect: Dialect) -> u64 {
    use gobstopper_adapters::request::Engine;
    let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(body) else {
        panic!("not a JSON object");
    };
    Engine::new(CliffConfig {
        threshold_tokens: u64::MAX / 8,
        ..CliffConfig::default()
    })
    .prepare(map, dialect)
    .map_or(0, |ctx| ctx.est_tokens_in)
}

/// Events whose `message_start` reports half again the estimate of `sent`.
fn usage_events(sent: &[u8]) -> Vec<String> {
    let reported = estimate(sent) * 3 / 2;
    vec![
        format!("event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"usage\":{{\"input_tokens\":2,\"cache_read_input_tokens\":{}}}}}}}\n\n", reported - 2),
        "event: content_block_delta\ndata: {\"delta\":{\"text\":\"hi\"}}\n\n".to_string(),
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string(),
    ]
}

/// The proxy reads usage after the client has the whole response, so wait
/// until `samples` samples are recorded (or give up after five seconds).
fn status_after_samples(port: u16, samples: u64) -> Value {
    for _ in 0..100 {
        let status = request(port, "GET", "/gobstopper/status", &[], b"").json();
        if status["calibrations"][0]["samples"].as_u64() >= Some(samples) {
            return status;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    request(port, "GET", "/gobstopper/status", &[], b"").json()
}

#[test]
fn provider_usage_calibrates_the_threshold_and_no_calibrate_keeps_the_estimate() {
    let threshold = 8_000u64;
    // A request estimated between the calibrated threshold (8,000 / 1.5)
    // and the threshold: only a calibrated proxy compacts it.
    let target = (4..40)
        .map(|turns| body(&session(turns)))
        .find(|sent| (5_600..7_800).contains(&estimate(sent)))
        .expect("a session sized between the thresholds");
    let warm = body(&session(4));
    assert!(estimate(&warm) >= 1_000);

    let run = |extra: &[&str]| {
        let fake = Fake::start(|recorded| Reply::Sse(usage_events(&recorded.body)));
        let mut args = vec!["--threshold", "8000", "--keep-recent", "1"];
        args.extend_from_slice(extra);
        let proxy = start_proxy(&fake.url(), &args);
        let calibrating = !extra.contains(&"--no-calibrate");
        for i in 0..5 {
            let response = request(proxy.port, "POST", "/v1/messages", ANTHROPIC, &warm);
            assert_eq!(response.status, 200);
            // The client receives the provider's bytes unchanged.
            assert_eq!(
                String::from_utf8(response.body).unwrap(),
                usage_events(&warm).concat()
            );
            if calibrating {
                status_after_samples(proxy.port, i + 1);
            }
        }
        let response = request(proxy.port, "POST", "/v1/messages", ANTHROPIC, &target);
        assert_eq!(response.status, 200);
        let status = if calibrating {
            status_after_samples(proxy.port, 6)
        } else {
            request(proxy.port, "GET", "/gobstopper/status", &[], b"").json()
        };
        (fake.seen().last().unwrap().body.clone(), status)
    };

    let (sent, status) = run(&[]);
    assert_ne!(sent, target, "the calibrated proxy compacts");
    assert!(estimate(&sent) <= threshold * 1000 / 1500 + 1);
    assert_eq!(status["calibrate"], true);
    let row = &status["calibrations"][0];
    assert_eq!(row["model"], "claude-test");
    assert_eq!(row["samples"], 6);
    assert!(
        (1.49..=1.5).contains(&row["ratio"].as_f64().unwrap()),
        "{row}"
    );
    assert_eq!(status["compacted"], 1);

    let (sent, status) = run(&["--no-calibrate"]);
    assert_eq!(
        sent, target,
        "without calibration the estimate is under the threshold"
    );
    assert_eq!(status["calibrate"], false);
    assert_eq!(status["calibrations"], json!([]));
    assert_eq!(status["compacted"], 0);
}

#[test]
fn malformed_or_missing_usage_is_ignored_and_json_usage_is_read() {
    let fake = Fake::start(|recorded| {
        if recorded.json()["max_tokens"] == 1 {
            Reply::Json(
                200,
                json!({"usage": {"input_tokens": estimate(&recorded.body) * 5 / 4}}),
            )
        } else {
            Reply::Json(200, json!({"usage": {"input_tokens": "many"}, "ok": true}))
        }
    });
    let proxy = start_proxy(&fake.url(), &[]);
    let warm = body(&session(4));
    let response = request(proxy.port, "POST", "/v1/messages", ANTHROPIC, &warm);
    assert_eq!(
        response.json(),
        json!({"usage": {"input_tokens": "many"}, "ok": true})
    );
    // Give a (wrong) late sample time to land before checking there is none.
    std::thread::sleep(Duration::from_millis(300));
    let status = request(proxy.port, "GET", "/gobstopper/status", &[], b"").json();
    assert_eq!(status["calibrations"], json!([]));

    let mut json_body: Value = serde_json::from_slice(&warm).unwrap();
    json_body["max_tokens"] = json!(1);
    let json_body = serde_json::to_vec(&json_body).unwrap();
    let response = request(proxy.port, "POST", "/v1/messages", ANTHROPIC, &json_body);
    assert_eq!(response.status, 200);
    let status = status_after_samples(proxy.port, 1);
    assert_eq!(status["calibrations"][0]["samples"], 1);
    assert_eq!(
        status["calibrations"][0]["ratio"], 1.0,
        "one sample is not applied"
    );
    let measured = status["calibrations"][0]["measured_ratio"]
        .as_f64()
        .unwrap();
    assert!((1.24..=1.25).contains(&measured), "{measured}");
}

/// A Responses stream whose `response.completed` reports half again the
/// estimate of `sent`.
fn responses_usage_events(sent: &[u8]) -> Vec<String> {
    let reported = estimate_as(sent, Dialect::Responses) * 3 / 2;
    vec![
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n".to_string(),
        format!("event: response.completed\ndata: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":{reported},\"output_tokens\":3}}}}}}\n\n"),
    ]
}

#[test]
fn openai_dialects_calibrate_from_json_and_streamed_usage() {
    let threshold = 8_000u64;
    let warm = chat_session(4);

    // Chat Completions: the JSON reply's `usage.prompt_tokens`.
    let chat_warm = chat_body(&warm);
    assert!(estimate_as(&chat_warm, Dialect::ChatCompletions) >= 1_000);
    let chat_target = (4..40)
        .map(|turns| chat_body(&chat_session(turns)))
        .find(|sent| (5_600..7_800).contains(&estimate_as(sent, Dialect::ChatCompletions)))
        .expect("a session sized between the thresholds");
    let chat = Fake::start(|recorded| {
        Reply::Json(
            200,
            json!({"usage": {"prompt_tokens":
                estimate_as(&recorded.body, Dialect::ChatCompletions) * 3 / 2}}),
        )
    });
    // A 40% tail keeps the compacted request above the 1,000-token sampling
    // floor, so the compacted reply is the sixth sample.
    let proxy = start_proxy(
        &chat.url(),
        &[
            "--threshold",
            "8000",
            "--keep-recent",
            "1",
            "--keep-tail-percent",
            "40",
        ],
    );
    for i in 0..5 {
        let response = request(
            proxy.port,
            "POST",
            "/v1/chat/completions",
            OPENAI,
            &chat_warm,
        );
        assert_eq!(response.status, 200);
        status_after_samples(proxy.port, i + 1);
    }
    let response = request(
        proxy.port,
        "POST",
        "/v1/chat/completions",
        OPENAI,
        &chat_target,
    );
    assert_eq!(response.status, 200);
    let sent = chat.seen().last().unwrap().body.clone();
    assert_ne!(sent, chat_target, "the calibrated proxy compacts");
    assert!(estimate_as(&sent, Dialect::ChatCompletions) <= threshold * 1000 / 1500 + 1);
    let status = status_after_samples(proxy.port, 6);
    let row = &status["calibrations"][0];
    assert_eq!(row["model"], "gpt-test");
    assert_eq!(row["samples"], 6);
    assert!(
        (1.49..=1.5).contains(&row["ratio"].as_f64().unwrap()),
        "{row}"
    );
    drop(proxy);

    // Responses: the terminal stream event's `response.usage.input_tokens`,
    // from a backend-api stream that sends no content type, as ChatGPT does.
    let responses_warm = responses_body(&warm);
    assert!(estimate_as(&responses_warm, Dialect::Responses) >= 1_000);
    let responses_target = (4..40)
        .map(|turns| responses_body(&chat_session(turns)))
        .find(|sent| (5_600..7_800).contains(&estimate_as(sent, Dialect::Responses)))
        .expect("a session sized between the thresholds");
    let responses = Fake::start(|recorded| Reply::SseBare(responses_usage_events(&recorded.body)));
    let proxy = start_proxy(
        &responses.url(),
        &[
            "--threshold",
            "8000",
            "--keep-recent",
            "1",
            "--keep-tail-percent",
            "40",
        ],
    );
    for i in 0..5 {
        let response = request(
            proxy.port,
            "POST",
            "/backend-api/codex/responses",
            OPENAI,
            &responses_warm,
        );
        assert_eq!(response.status, 200);
        // The client receives the provider's bytes unchanged.
        assert_eq!(
            String::from_utf8(response.body).unwrap(),
            responses_usage_events(&responses_warm).concat()
        );
        status_after_samples(proxy.port, i + 1);
    }
    let response = request(
        proxy.port,
        "POST",
        "/backend-api/codex/responses",
        OPENAI,
        &responses_target,
    );
    assert_eq!(response.status, 200);
    let sent = responses.seen().last().unwrap().body.clone();
    assert_ne!(sent, responses_target, "the calibrated proxy compacts");
    let status = status_after_samples(proxy.port, 6);
    let row = &status["calibrations"][0];
    assert_eq!(row["samples"], 6);
    assert!(
        (1.49..=1.5).contains(&row["ratio"].as_f64().unwrap()),
        "{row}"
    );
}

fn data_cli(proxy: &ProxyProcess, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_gobstopper"))
        .env("GOBSTOPPER_DATA_DIR", &proxy.data_root)
        .env("HRANESS_AUDIENCE", "quiet")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn finished_requests(proxy: &ProxyProcess, expected: usize) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let rows = data_cli(proxy, &["data", "requests"]);
        if rows.as_array().is_some_and(|r| {
            r.len() == expected && r.iter().all(|row| row["finished_at_ms"].is_number())
        }) {
            return rows;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "terminal observations did not arrive: {rows}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn scoped_reservation_changes_actual_wire_request_then_expires_without_leaking_capability() {
    let fake = Fake::start(|_| {
        Reply::Json(
            200,
            json!({"type":"message","role":"assistant","stop_reason":"end_turn","content":[],"usage":{"input_tokens":100,"output_tokens":20}}),
        )
    });
    let proxy = start_proxy(&fake.url(), &["--threshold", "1000", "--no-calibrate"]);
    let scope = data_cli(
        &proxy,
        &[
            "context",
            "create",
            "--context-window",
            "100000",
            "--port",
            &proxy.port.to_string(),
        ],
    );
    let capability = scope["scope"].as_str().unwrap();
    let target = format!("/__gobstopper/s/{capability}/v1/messages");
    let original = body(&session(20));
    assert_eq!(
        request(proxy.port, "POST", &target, ANTHROPIC, &original).status,
        200
    );
    assert_ne!(
        fake.seen()[0].json()["messages"],
        serde_json::from_slice::<Value>(&original).unwrap()["messages"]
    );
    let reservation = data_cli(
        &proxy,
        &[
            "context",
            "reserve",
            "--scope",
            capability,
            "--tokens",
            "50000",
            "--requests",
            "2",
        ],
    );
    assert_eq!(reservation["effective_input_tokens"], 50000);
    for _ in 0..2 {
        assert_eq!(
            request(proxy.port, "POST", &target, ANTHROPIC, &original).status,
            200
        );
    }
    assert_eq!(fake.seen()[1].body, original);
    assert_eq!(fake.seen()[2].body, original);
    assert_eq!(
        request(proxy.port, "POST", &target, ANTHROPIC, &original).status,
        200
    );
    assert_ne!(
        fake.seen()[3].json()["messages"],
        serde_json::from_slice::<Value>(&original).unwrap()["messages"]
    );
    for wire in fake.seen() {
        assert_eq!(wire.target, "/v1/messages");
        assert!(wire.header("x-gobstopper-scope").is_none());
    }
    let rows = finished_requests(&proxy, 4);
    assert!(rows
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["outcome"] == "success"));
    let export = Command::new(env!("CARGO_BIN_EXE_gobstopper"))
        .env("GOBSTOPPER_DATA_DIR", &proxy.data_root)
        .args(["data", "export"])
        .output()
        .unwrap();
    assert!(export.status.success());
    let exported = String::from_utf8(export.stdout).unwrap();
    for private in [
        capability,
        "Fix the failing test.",
        "sk-ant-synthetic-test-key",
        "pytest -x",
    ] {
        assert!(!exported.contains(private));
    }
    data_cli(&proxy, &["context", "close", "--scope", capability]);
    assert_eq!(
        request(proxy.port, "POST", &target, ANTHROPIC, &original).status,
        400
    );
}

#[test]
fn live_observations_join_retries_without_counting_them_as_separate_requests() {
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let n = Arc::clone(&count);
    let fake = Fake::start(move |_| {
        if n.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            Reply::Json(
                400,
                json!({"error":{"type":"invalid_request_error","message":"prompt is too long"}}),
            )
        } else {
            Reply::Json(
                200,
                json!({"type":"message","role":"assistant","stop_reason":"end_turn","content":[],"usage":{"input_tokens":100,"cache_read_input_tokens":900,"output_tokens":50}}),
            )
        }
    });
    let proxy = start_proxy(&fake.url(), &["--threshold", "10000", "--no-calibrate"]);
    assert_eq!(
        request(
            proxy.port,
            "POST",
            "/v1/messages",
            ANTHROPIC,
            &body(&session(30))
        )
        .status,
        200
    );
    let rows = finished_requests(&proxy, 2);
    let rows = rows.as_array().unwrap();
    assert_eq!(
        rows[0]["identity"]["request_id"],
        rows[1]["identity"]["request_id"]
    );
    assert_ne!(
        rows[0]["identity"]["attempt_id"],
        rows[1]["identity"]["attempt_id"]
    );
    let success = rows.iter().find(|row| row["outcome"] == "success").unwrap();
    assert_eq!(success["usage"]["input_tokens"]["value"], 1000);
    assert!(success["generation"].is_null());
    assert_eq!(
        rows.iter()
            .filter(|row| row["outcome"] == "refused")
            .count(),
        1
    );
}

#[test]
fn live_streaming_metrics_require_terminal_event_and_capture_final_usage() {
    let fake = Fake::start(|_| {
        Reply::Sse(vec![
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n".into(),
        "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"SYNTHETIC_PRIVATE_OUTPUT\"}}\n\n".into(),
        "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":250}}\n\n".into(),
        "data: {\"type\":\"message_stop\"}\n\n".into(),
    ])
    });
    let proxy = start_proxy(&fake.url(), &[]);
    assert_eq!(
        request(
            proxy.port,
            "POST",
            "/v1/messages",
            ANTHROPIC,
            &body(&session(1))
        )
        .status,
        200
    );
    let rows = finished_requests(&proxy, 1);
    assert_eq!(rows[0]["usage"]["output_tokens"]["value"], 250);
    assert!(rows[0]["first_output_ms"].is_number());
    assert_eq!(rows[0]["outcome"], "success");
    assert!(!rows.to_string().contains("SYNTHETIC_PRIVATE_OUTPUT"));
    let fake = Fake::start(|_| Reply::Sse(vec!["data: {\"type\":\"message_start\"}\n\n".into()]));
    let proxy = start_proxy(&fake.url(), &[]);
    request(
        proxy.port,
        "POST",
        "/v1/messages",
        ANTHROPIC,
        &body(&session(1)),
    );
    let rows = finished_requests(&proxy, 1);
    assert_eq!(rows[0]["outcome"], "interrupted");
    assert!(rows[0]["usage"].is_null());
}

#[test]
fn owned_service_drain_rejects_new_inference_and_resume_restores_it() {
    let fake = Fake::start(|_| {
        Reply::Json(
            200,
            json!({"type":"message","content":[],"stop_reason":"end_turn"}),
        )
    });
    let proxy = start_proxy(&fake.url(), &["--service-id", "owned-service-test"]);
    let endpoint = "/gobstopper/service/drain";
    let owned = &[("x-gobstopper-service-id", "owned-service-test")];
    assert_eq!(request(proxy.port, "POST", endpoint, &[], b"").status, 403);
    assert_eq!(
        request(
            proxy.port,
            "POST",
            endpoint,
            &[
                ("x-gobstopper-service-id", "owned-service-test"),
                ("origin", "https://unrelated.invalid")
            ],
            b""
        )
        .status,
        403
    );
    assert_eq!(
        request(proxy.port, "POST", endpoint, owned, b"").json()["drained"],
        true
    );
    assert_eq!(
        request(
            proxy.port,
            "POST",
            "/v1/messages",
            ANTHROPIC,
            &body(&session(1))
        )
        .status,
        503
    );
    assert!(fake.seen().is_empty());
    assert_eq!(
        request(proxy.port, "GET", "/gobstopper/status", &[], b"").json()["draining"],
        true
    );
    assert_eq!(
        request(proxy.port, "POST", "/gobstopper/service/resume", owned, b"").json()["drained"],
        false
    );
    assert_eq!(
        request(
            proxy.port,
            "POST",
            "/v1/messages",
            ANTHROPIC,
            &body(&session(1))
        )
        .status,
        200
    );
}

#[test]
fn service_drain_preserves_active_inference() {
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let gate = Arc::new(std::sync::Barrier::new(2));
    let upstream_gate = Arc::clone(&gate);
    let fake = Fake::start(move |_| {
        started_tx.send(()).unwrap();
        upstream_gate.wait();
        Reply::Json(
            200,
            json!({"type":"message","content":[],"stop_reason":"end_turn"}),
        )
    });
    let proxy = start_proxy(&fake.url(), &["--service-id", "active-service-test"]);
    let port = proxy.port;
    for route in ["/v1/messages", "/backend-api/codex/responses/compact"] {
        let inference =
            std::thread::spawn(move || request(port, "POST", route, ANTHROPIC, &body(&session(1))));
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            request(
                port,
                "POST",
                "/gobstopper/service/drain",
                &[("x-gobstopper-service-id", "active-service-test")],
                b""
            )
            .status,
            409
        );
        assert_eq!(
            request(port, "GET", "/gobstopper/status", &[], b"").json()["draining"],
            false
        );
        gate.wait();
        assert_eq!(inference.join().unwrap().status, 200);
    }
}

#[test]
fn explicit_capacity_rejects_an_immutable_oversized_head_before_upstream() {
    let fake = Fake::start(|_| Reply::Json(200, json!({})));
    let proxy = start_proxy(&fake.url(), &["--context-window", "5000"]);
    let oversized = json!({"model":"test","max_tokens":100,"messages":[{"role":"user","content":"x".repeat(100_000)}]});
    let response = request(
        proxy.port,
        "POST",
        "/v1/messages",
        ANTHROPIC,
        &serde_json::to_vec(&oversized).unwrap(),
    );
    assert_eq!(response.status, 400);
    assert_eq!(
        response.json()["error"]["type"],
        "gobstopper_capacity_exceeded"
    );
    assert!(fake.seen().is_empty());
}

#[test]
fn explicit_capacity_applies_to_nonlength_original_body_fallback() {
    let fake = Fake::start(|_| {
        Reply::Json(
            400,
            json!({"error":{"type":"invalid_request_error","message":"synthetic schema rejection"}}),
        )
    });
    let proxy = start_proxy(
        &fake.url(),
        &[
            "--threshold",
            "2000",
            "--keep-recent",
            "1",
            "--evidence-max-bytes",
            "0",
            "--context-window",
            "8000",
        ],
    );
    let mut payload = serde_json::from_slice::<Value>(&body(&session(40))).unwrap();
    payload["max_tokens"] = json!(100);
    assert_eq!(
        request(
            proxy.port,
            "POST",
            "/v1/messages",
            ANTHROPIC,
            &serde_json::to_vec(&payload).unwrap()
        )
        .status,
        400
    );
    assert_eq!(
        fake.seen().len(),
        1,
        "original oversized input must not be retried"
    );
}
