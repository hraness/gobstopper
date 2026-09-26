//! `gobstopper proxy` end to end: the real binary between a raw HTTP client
//! and a fake upstream, over real sockets and the system curl.

use gobstopper_adapters::request::SUMMARY_HEADER;
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
    match responder(&recorded) {
        Reply::Json(status, value) => {
            let body = serde_json::to_vec(&value).unwrap();
            let head = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\nx-upstream: fake\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            out.write_all(head.as_bytes()).unwrap();
            out.write_all(&body).unwrap();
        }
        Reply::Sse(events) => {
            out.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n")
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
    child: Child,
    port: u16,
}

impl Drop for ProxyProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
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
    let mut command = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
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
    ProxyProcess { child, port }
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
    let proxy = start_proxy(&fake.url(), &["--threshold", "2000", "--keep-recent", "1"]);
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
    // previous summary, so the oldest steps are gone rather than nested.
    assert!(!found[0].contains("Step 0:"));
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
    let mut messages = vec![
        json!({"role": "system", "content": "you are an agent"}),
        json!({"role": "user", "content": "Fix the bug."}),
    ];
    for n in 0..10 {
        messages.push(json!({"role": "assistant", "content": format!("step {n}"),
            "tool_calls": [{"id": format!("call_{n}"), "type": "function",
                "function": {"name": "bash", "arguments": format!("{{\"cmd\":\"s{n}\"}}")}}]}));
        messages.push(json!({"role": "tool", "tool_call_id": format!("call_{n}"),
            "content": "R".repeat(3000)}));
    }
    let sent =
        serde_json::to_vec(&json!({"model": "gpt-test", "messages": messages, "stream": true}))
            .unwrap();
    let headers = [
        ("content-type", "application/json"),
        ("authorization", "Bearer synthetic"),
    ];
    for target in ["/v1/chat/completions", "/chat/completions"] {
        let response = request(proxy.port, "POST", target, &headers, &sent);
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
    let status = request(second.port, "GET", "/gobstopper/status", &[], b"").json();
    assert_eq!(status["all_time_est_tokens_in"], first_in);
    assert_eq!(status["all_time_est_tokens_out"], first_out);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The binary with no inherited `GOBSTOPPER_` settings, reading config from
/// a scratch directory.
fn gobstopper(args: &[&str], home: &std::path::Path) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
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
    drop(proxy);

    let lines = std::fs::read_to_string(&log).unwrap();
    assert!(
        lines.contains(
            "threshold 2000 tokens, threshold_1m 8000 tokens, keep_recent 1, keep_tail_percent 40, result_max_chars"
        ),
        "the startup line shows both thresholds and the tail percent: {lines}"
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
            line.contains("(head ~") && line.contains(", summary ~") && line.contains(", tail ~"),
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
    let port = old.local_addr().unwrap().port().to_string();
    let server = std::thread::spawn(move || {
        let (stream, _) = old.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let (line, _) = read_head(&mut reader).unwrap();
        let body = serde_json::to_vec(&json!({
            "name": "gobstopper-proxy", "version": "0.4.1", "port": 8260,
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
    let current = start_proxy(&fake.url(), &["--threshold-1m", "300000"]);
    let port = current.port.to_string();
    let output = gobstopper(&["proxy", "status", "--port", &port], &dir);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains(": threshold 128000 tokens, threshold_1m 300000 tokens, keep_recent 3, keep_tail_percent 40\nrequests 0 (0 with a 1M window), compacted 0,"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn replay_at_zero_tail_percent_matches_the_reference_report() {
    let dir = scratch_dir("tail-replay");
    let transcript = tail_transcript(&dir);
    let replay = |percent: &str| -> Value {
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
    // Recorded with gobstopper 0.4.0, before the tail budget existed.
    let reference = replay("0");
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
    let tail = replay("60");
    assert_eq!(tail["compacted"], 10);
    assert_eq!(tail["pairing_violations"], 0);
    assert!(tail["total_est_tokens_out"].as_u64() > reference["total_est_tokens_out"].as_u64());
    let _ = std::fs::remove_dir_all(&dir);
}
