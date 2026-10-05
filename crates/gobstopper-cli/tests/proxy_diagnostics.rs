#[path = "../src/session_data/schema.rs"]
#[allow(dead_code)]
mod schema;

use schema::{Envelope, Event, FinishReason, RequestTimings};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const LEGACY_FINISH: &str = r#"{"kind":"request_finished","outcome":"interrupted","http_status":200,"duration_ms":250,"first_output_ms":null,"usage":null,"generation":null}"#;

fn old_envelope() -> Envelope {
    serde_json::from_value(json!({
        "schema_version":1, "event_id":"a".repeat(64),
        "source":{"kind":"live_proxy","id":"b".repeat(64),"profile":"gobstopper-proxy-v1"},
        "observed_at_ms":1000,
        "identity":{"runtime_id":null,"session_id":null,"request_id":"c".repeat(64),"attempt_id":"d".repeat(64),"tool_id":null},
        "event":serde_json::from_str::<Value>(LEGACY_FINISH).unwrap()
    })).unwrap()
}

#[test]
fn request_finished_new_fields_are_optional_and_preserve_old_serialized_bytes() {
    let envelope = old_envelope();
    envelope.validate().unwrap();
    assert!(matches!(
        envelope.event,
        Event::RequestFinished {
            reason: None,
            timings: None,
            ..
        }
    ));
    assert_eq!(
        serde_json::to_string(&envelope.event).unwrap(),
        LEGACY_FINISH
    );
    let mut value = serde_json::to_value(&envelope).unwrap();
    value["event"]["reason"] = json!("PRIVATE_PROVIDER_ERROR");
    assert!(serde_json::from_value::<Envelope>(value).is_err());
    let mut value = serde_json::to_value(&envelope).unwrap();
    value["event"]["timings"] =
        json!({"preparation_ms":0,"upstream_headers_ms":0,"headers":"PRIVATE_HEADER"});
    assert!(serde_json::from_value::<Envelope>(value).is_err());
}

#[test]
fn finish_reason_must_agree_with_its_outcome() {
    let mut envelope = old_envelope();
    if let Event::RequestFinished {
        outcome, reason, ..
    } = &mut envelope.event
    {
        *outcome = schema::Outcome::Success;
        *reason = Some(FinishReason::UpstreamTimeout);
    }
    assert!(envelope.validate().is_err());
    if let Event::RequestFinished { outcome, .. } = &mut envelope.event {
        *outcome = schema::Outcome::Timeout;
    }
    envelope.validate().unwrap();
    envelope.source.profile = "gobstopper-proxy-v2".into();
    envelope.validate().unwrap();
}

#[test]
fn completed_reason_cannot_qualify_an_http_error_as_success() {
    let mut envelope = old_envelope();
    if let Event::RequestFinished {
        outcome,
        reason,
        http_status,
        ..
    } = &mut envelope.event
    {
        *outcome = schema::Outcome::Success;
        *reason = Some(FinishReason::Completed);
        *http_status = Some(500);
    }
    assert!(envelope.validate().is_err());
    if let Event::RequestFinished { reason, .. } = &mut envelope.event {
        *reason = None;
    }
    envelope.validate().unwrap();
}

#[test]
fn generation_spans_cannot_exceed_request_duration_or_reported_output() {
    let mut envelope = old_envelope();
    for (duration, output) in [(251, 5), (250, 11)] {
        if let Event::RequestFinished {
            generation, usage, ..
        } = &mut envelope.event
        {
            *generation = Some(schema::Generation {
                duration_ms: duration,
                output_tokens: output,
            });
            *usage = Some(schema::Usage {
                output_tokens: Some(schema::Quantity::reported(10)),
                ..schema::Usage::default()
            });
        }
        assert!(envelope.validate().is_err());
    }
}

#[test]
fn request_timings_are_integer_spans_bounded_by_the_same_total() {
    let mut envelope = old_envelope();
    envelope.event = Event::RequestFinished {
        outcome: schema::Outcome::Success,
        http_status: Some(200),
        duration_ms: Some(100),
        first_output_ms: None,
        usage: None,
        generation: None,
        reason: Some(FinishReason::Completed),
        timings: Some(RequestTimings {
            preparation_ms: 10,
            upstream_headers_ms: 90,
            transform_ms: Some(3),
        }),
    };
    envelope.validate().unwrap();
    let decoded: Envelope =
        serde_json::from_slice(&serde_json::to_vec(&envelope).unwrap()).unwrap();
    assert_eq!(decoded, envelope);
    for timings in [
        RequestTimings {
            preparation_ms: 101,
            upstream_headers_ms: 0,
            transform_ms: None,
        },
        RequestTimings {
            preparation_ms: 60,
            upstream_headers_ms: 41,
            transform_ms: None,
        },
        RequestTimings {
            preparation_ms: 5,
            upstream_headers_ms: 0,
            transform_ms: Some(6),
        },
        RequestTimings {
            preparation_ms: u64::MAX,
            upstream_headers_ms: 1,
            transform_ms: None,
        },
    ] {
        if let Event::RequestFinished {
            timings: measured, ..
        } = &mut envelope.event
        {
            *measured = Some(timings);
        }
        assert!(envelope.validate().is_err());
    }
    if let Event::RequestFinished {
        duration_ms,
        timings,
        ..
    } = &mut envelope.event
    {
        *duration_ms = None;
        *timings = Some(RequestTimings::default());
    }
    assert!(envelope.validate().is_err());
}

#[derive(Clone)]
struct Recorded {
    target: String,
    body: Vec<u8>,
}

struct Fake {
    port: u16,
    seen: Arc<Mutex<Vec<Recorded>>>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Fake {
    fn start(respond: impl Fn(&mut TcpStream, &Recorded) + Send + 'static) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let worker = thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("fake listener failed: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).unwrap_or(0) == 0 {
                    continue;
                }
                let target = first.split_whitespace().nth(1).unwrap().to_owned();
                let mut length = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap();
                        }
                    }
                }
                assert!(length <= 16 * 1024 * 1024);
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let record = Recorded { target, body };
                log.lock().unwrap().push(record.clone());
                respond(&mut stream, &record);
            }
        });
        Self {
            port,
            seen,
            stopped,
            worker: Some(worker),
        }
    }
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn records(&self) -> Vec<Recorded> {
        self.seen.lock().unwrap().clone()
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn reply(stream: &mut TcpStream, status: u16, content_type: &str, bytes: &[u8], length: usize) {
    write!(stream, "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nx-private-header: PRIVATE_HEADER\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n").unwrap();
    stream.write_all(bytes).unwrap();
}

fn isolated_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GOBSTOPPER_") {
            command.env_remove(key);
        }
    }
    command
        .env("HRANESS_AUDIENCE", "quiet")
        .env("HRANESS_NO_UPDATE", "1");
    command
}

struct Proxy {
    child: Child,
    root: PathBuf,
    port: u16,
}
impl Proxy {
    fn start(upstream: &str, extra: &[&str]) -> Self {
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        let root = std::env::temp_dir().join(format!(
            "gobstopper-diagnostics-{}",
            schema::OpaqueId::random().unwrap().0
        ));
        let mut command = isolated_command();
        command
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("GOBSTOPPER_DATA_DIR", root.join("data"))
            .env("GOBSTOPPER_STATS_FILE", "off")
            .args([
                "proxy",
                "serve",
                "--port",
                &port.to_string(),
                "--no-keep-awake",
                "--no-calibrate",
                "--anthropic-upstream",
                upstream,
                "--openai-upstream",
                upstream,
                "--chatgpt-upstream",
                upstream,
            ])
            .args(extra)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        drop(reservation);
        let mut proxy = Self {
            child: command.spawn().unwrap(),
            root,
            port,
        };
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Ok((200, body)) = exchange(port, "GET", "/gobstopper/status", &[], b"") {
                let status: Value = serde_json::from_slice(&body).unwrap();
                if status["observations"]["available"] == true {
                    break;
                }
            }
            assert!(
                proxy.child.try_wait().unwrap().is_none(),
                "fixture proxy exited"
            );
            assert!(
                Instant::now() < deadline,
                "fixture proxy never became ready"
            );
            thread::sleep(Duration::from_millis(10));
        }
        proxy
    }
    fn finishes(&self, expected: usize) -> Vec<Envelope> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let output = isolated_command()
                .env("XDG_CONFIG_HOME", self.root.join("config"))
                .env("GOBSTOPPER_DATA_DIR", self.root.join("data"))
                .args(["data", "export"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let lines: Vec<Value> = output
                .stdout
                .split(|b| *b == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_slice(line).unwrap())
                .collect();
            let events: Vec<Envelope> = lines[1..lines.len() - 1]
                .iter()
                .map(|row| serde_json::from_value::<Envelope>(row.clone()).unwrap())
                .filter(|row| matches!(row.event, Event::RequestFinished { .. }))
                .collect();
            for row in &events {
                row.validate().unwrap();
            }
            if events.len() == expected {
                return events;
            }
            assert!(events.len() <= expected);
            assert!(
                Instant::now() < deadline,
                "finishing observations did not arrive"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn send_request(
    stream: &mut TcpStream,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<()> {
    write!(stream, "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", body.len())?;
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    stream.write_all(b"\r\n")?;
    stream.write_all(body)
}

fn exchange(
    port: u16,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<(u16, Vec<u8>)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    send_request(&mut stream, method, target, headers, body)?;
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes)?;
    let boundary = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&bytes[..boundary]);
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    let wire = &bytes[boundary + 4..];
    if !head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        return Ok((status, wire.to_vec()));
    }
    let mut body = Vec::new();
    let mut reader = BufReader::new(wire);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let length = usize::from_str_radix(line.trim(), 16).unwrap();
        if length == 0 {
            break;
        }
        let mut chunk = vec![0; length + 2];
        reader.read_exact(&mut chunk)?;
        assert_eq!(&chunk[length..], b"\r\n");
        body.extend_from_slice(&chunk[..length]);
    }
    Ok((status, body))
}

fn payload(target: &str) -> Vec<u8> {
    let mut value = json!({"model":"test","max_tokens":12,"stream":false});
    value[if target.ends_with("/responses") {
        "input"
    } else {
        "messages"
    }] = json!([{"role":"user","content":"PRIVATE_REQUEST"}]);
    serde_json::to_vec(&value).unwrap()
}

fn compacting_payload() -> Vec<u8> {
    let mut messages = vec![json!({"role":"user","content":"immutable head"})];
    for _ in 0..20 {
        messages.push(json!({"role":"assistant","content":"x".repeat(2000)}));
        messages.push(json!({"role":"user","content":"continue"}));
    }
    serde_json::to_vec(&json!({"model":"test","max_tokens":12,"messages":messages})).unwrap()
}

fn finished_reason(row: &Envelope) -> FinishReason {
    match row.event {
        Event::RequestFinished {
            reason: Some(reason),
            ..
        } => reason,
        _ => panic!("missing content-free finish reason"),
    }
}

fn assert_timings(row: &Envelope) {
    match row.event {
        Event::RequestFinished {
            duration_ms: Some(total),
            timings: Some(timings),
            ..
        } => {
            assert!(
                timings
                    .preparation_ms
                    .checked_add(timings.upstream_headers_ms)
                    .unwrap()
                    <= total
            );
            assert_eq!(timings.transform_ms, None);
        }
        _ => panic!("missing measured request spans"),
    }
}

#[test]
fn provider_failures_and_parser_uncertainty_preserve_bytes_without_replay_or_content() {
    let cases = vec![
        ("/v1/messages", 400, "application/json", br#"{"error":{"type":"invalid_request_error","message":"PRIVATE_ERROR"}}"#.to_vec(), FinishReason::ProviderRefused),
        ("/v1/messages", 500, "application/json", br#"{"error":{"message":"PRIVATE_ERROR"}}"#.to_vec(), FinishReason::ProviderError),
        ("/v1/messages", 200, "application/json", br#"{"type":"message","content":[{"type":"text","text":"PRIVATE_REFUSAL"}],"stop_reason":"refusal"}"#.to_vec(), FinishReason::ProviderRefused),
        ("/v1/responses", 200, "text/event-stream", b"data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"PRIVATE_ERROR\"}}}\n\n".to_vec(), FinishReason::ProviderError),
        ("/v1/messages", 200, "text/event-stream", b"data: {\"type\":\"message_start\"}\n\n".to_vec(), FinishReason::MissingTerminal),
        ("/v1/messages", 200, "text/event-stream", b"data: PRIVATE_UNPARSEABLE_OUTPUT\n\n".to_vec(), FinishReason::ParserUncertain),
        ("/v1/messages", 200, "application/json", b"PRIVATE_UNPARSEABLE_OUTPUT".to_vec(), FinishReason::ParserUncertain),
    ];
    let replies = cases.clone();
    let counter = AtomicUsize::new(0);
    let fake = Fake::start(move |stream, _| {
        let index = counter.fetch_add(1, Ordering::AcqRel);
        let (_, status, kind, body, _) = &replies[index];
        reply(stream, *status, kind, body, body.len());
    });
    let proxy = Proxy::start(&fake.url(), &[]);
    for (i, (target, status, _, bytes, reason)) in cases.iter().enumerate() {
        let original = payload(target);
        let (received, body) = exchange(
            proxy.port,
            "POST",
            target,
            &[("authorization", "PRIVATE_AUTHORIZATION")],
            &original,
        )
        .unwrap();
        assert_eq!(received, *status);
        assert_eq!(&body, bytes);
        let records = fake.records();
        assert_eq!(records.len(), i + 1);
        assert_eq!(records[i].body, original);
        let events = proxy.finishes(i + 1);
        assert_eq!(finished_reason(&events[i]), *reason);
        assert_timings(&events[i]);
        let serialized = serde_json::to_string(&events).unwrap();
        assert!(!serialized.contains("PRIVATE_"));
        assert!(!serialized.contains("authorization"));
        assert!(!serialized.contains("headers\""));
    }
}

#[test]
fn truncated_rejection_is_not_replayed_even_after_compaction() {
    let error = br#"{"error":{"message":"prompt is too long: PRIVATE_ERROR"}}"#.to_vec();
    let reply_body = error.clone();
    let fake = Fake::start(move |stream, _| {
        reply(
            stream,
            400,
            "application/json",
            &reply_body,
            reply_body.len() + 20,
        )
    });
    let proxy = Proxy::start(&fake.url(), &["--threshold", "1000", "--keep-recent", "1"]);
    let original = compacting_payload();
    let (status, bytes) = exchange(proxy.port, "POST", "/v1/messages", &[], &original).unwrap();
    assert_eq!(status, 400);
    assert_eq!(bytes, error);
    let records = fake.records();
    assert_eq!(records.len(), 1);
    assert_ne!(records[0].body, original);
    let events = proxy.finishes(1);
    assert_eq!(finished_reason(&events[0]), FinishReason::UpstreamTruncated);
    assert_timings(&events[0]);
    assert!(!serde_json::to_string(&events)
        .unwrap()
        .contains("PRIVATE_ERROR"));
}

#[test]
fn rejection_larger_than_sampling_limit_relays_its_prefix_and_tail_unchanged_once() {
    let bytes =
        serde_json::to_vec(&json!({"error":{"message":"x".repeat(4 * 1024 * 1024 + 64)}})).unwrap();
    let sent = bytes.clone();
    let fake = Fake::start(move |stream, _| {
        stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n").unwrap();
        stream.write_all(&sent).unwrap();
    });
    let proxy = Proxy::start(&fake.url(), &["--threshold", "1000", "--keep-recent", "1"]);
    let original = compacting_payload();
    let (status, received) = exchange(proxy.port, "POST", "/v1/messages", &[], &original).unwrap();
    assert_eq!(status, 400);
    assert_eq!(received, bytes);
    let records = fake.records();
    assert_eq!(records.len(), 1);
    assert_ne!(records[0].body, original);
    let events = proxy.finishes(1);
    assert_eq!(finished_reason(&events[0]), FinishReason::ProviderRefused);
    assert_timings(&events[0]);
}

#[test]
fn upstream_eof_before_headers_finishes_once_without_replay() {
    let fake = Fake::start(|_, _| {});
    let proxy = Proxy::start(&fake.url(), &[]);
    let (status, bytes) = exchange(
        proxy.port,
        "POST",
        "/v1/messages",
        &[],
        &payload("/v1/messages"),
    )
    .unwrap();
    assert_eq!(status, 502);
    assert!(!String::from_utf8(bytes).unwrap().contains("curl:"));
    let events = proxy.finishes(1);
    assert_eq!(
        finished_reason(&events[0]),
        FinishReason::UpstreamReadFailed
    );
    assert_eq!(fake.records().len(), 1);
    assert_timings(&events[0]);
}

#[test]
fn provider_header_wait_is_measured_as_header_wait_not_preparation() {
    let bytes = br#"{"type":"message","content":[],"stop_reason":"end_turn"}"#.to_vec();
    let reply_body = bytes.clone();
    let fake = Fake::start(move |stream, _| {
        thread::sleep(Duration::from_millis(120));
        reply(
            stream,
            200,
            "application/json",
            &reply_body,
            reply_body.len(),
        );
    });
    let proxy = Proxy::start(&fake.url(), &[]);
    assert_eq!(
        exchange(
            proxy.port,
            "POST",
            "/v1/messages",
            &[],
            &payload("/v1/messages")
        )
        .unwrap(),
        (200, bytes)
    );
    let events = proxy.finishes(1);
    assert_eq!(finished_reason(&events[0]), FinishReason::Completed);
    assert_timings(&events[0]);
    if let Event::RequestFinished {
        timings: Some(timings),
        ..
    } = events[0].event
    {
        assert!(timings.upstream_headers_ms >= 100);
    } else {
        panic!("header interval is absent");
    }
}

#[test]
fn client_disconnect_is_not_misattributed_to_upstream_and_never_replayed() {
    let (entered, observed) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let fake = Fake::start(move |stream, _| {
        entered.send(()).unwrap();
        resume.recv_timeout(Duration::from_secs(5)).unwrap();
        if stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .is_err()
        {
            return;
        }
        let frame = format!(
            "data: {{\"type\":\"content_block_delta\",\"delta\":{{\"text\":\"{}\"}}}}\n\n",
            "x".repeat(64 * 1024)
        );
        for _ in 0..128 {
            if stream.write_all(frame.as_bytes()).is_err() {
                break;
            }
        }
    });
    let proxy = Proxy::start(&fake.url(), &[]);
    let mut client = TcpStream::connect(("127.0.0.1", proxy.port)).unwrap();
    send_request(
        &mut client,
        "POST",
        "/v1/messages",
        &[],
        &payload("/v1/messages"),
    )
    .unwrap();
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    client.shutdown(Shutdown::Both).unwrap();
    drop(client);
    release.send(()).unwrap();
    let events = proxy.finishes(1);
    assert_eq!(
        finished_reason(&events[0]),
        FinishReason::ClientDisconnected
    );
    if let Event::RequestFinished { outcome, .. } = events[0].event {
        assert_eq!(outcome, schema::Outcome::Cancelled);
    }
    assert_eq!(fake.records().len(), 1);
    assert_timings(&events[0]);
}

#[test]
fn only_explicit_provider_sessions_are_hashed_and_unknown_requests_stay_unknown() {
    let fake = Fake::start(|stream, record| {
        let body = if record.target.ends_with("/responses") {
            br#"{"object":"response","status":"completed"}"#.as_slice()
        } else {
            br#"{"type":"message","content":[],"stop_reason":"end_turn"}"#.as_slice()
        };
        reply(stream, 200, "application/json", body, body.len());
    });
    let proxy = Proxy::start(&fake.url(), &[]);
    let native = "11111111-1111-4111-8111-111111111111";
    let mut identities = Vec::new();
    for (i, headers) in [
        vec![("session_id", native)],
        vec![
            ("thread-id", native),
            ("session-id", "PRIVATE_CACHE_AFFINITY"),
        ],
        vec![
            ("session-id", "PRIVATE_CACHE_AFFINITY"),
            ("x-client-request-id", native),
        ],
        vec![("x-session-id", native)],
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            exchange(
                proxy.port,
                "POST",
                "/v1/responses",
                headers,
                &payload("/v1/responses")
            )
            .unwrap()
            .0,
            200
        );
        identities.push(proxy.finishes(i + 1)[i].identity.session_id.clone());
    }
    assert!(identities[0].is_some());
    assert_eq!(identities[0], identities[1]);
    assert_eq!(identities[2], None);
    assert_eq!(identities[3], None);
    assert_eq!(
        exchange(
            proxy.port,
            "POST",
            "/v1/messages",
            &[("session_id", native)],
            &payload("/v1/messages")
        )
        .unwrap()
        .0,
        200
    );
    assert_eq!(proxy.finishes(5)[4].identity.session_id, None);
    let device = "a".repeat(64);
    let current = json!({"device_id":device,"account_uuid":"","session_id":native}).to_string();
    let legacy = format!("user_{device}_account__session_{native}");
    let mut claude = Vec::new();
    for (i, user_id) in [current, legacy, native.to_string()].iter().enumerate() {
        let mut body: Value = serde_json::from_slice(&payload("/v1/messages")).unwrap();
        body["metadata"] = json!({"user_id":user_id});
        assert_eq!(
            exchange(
                proxy.port,
                "POST",
                "/v1/messages",
                &[],
                &serde_json::to_vec(&body).unwrap()
            )
            .unwrap()
            .0,
            200
        );
        claude.push(proxy.finishes(i + 6)[i + 5].identity.session_id.clone());
    }
    assert!(claude[0].is_some());
    assert_eq!(claude[0], claude[1]);
    assert_ne!(identities[0], claude[0]);
    assert_eq!(claude[2], None);
    let events = proxy.finishes(8);
    let serialized = serde_json::to_string(&events).unwrap();
    assert!(!serialized.contains(native));
    assert!(!serialized.contains("PRIVATE_"));
    assert_eq!(fake.records().len(), 8);
}
