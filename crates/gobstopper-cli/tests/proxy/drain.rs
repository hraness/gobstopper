//! Exercise admission through the real proxy, including an already flowing SSE.

use super::*;
use std::io::Read;
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::Instant;

const SERVICE: &str = "drain-http-regression";
const OWNER: &str = "http-drain-owner-0123456789";

/// Run a supervised-process restart against its saved state, without installing
/// an OS service or touching the user's service directory.
fn seeded_restart(stage: &str, damaged: bool) -> (ProxyProcess, std::path::PathBuf) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "gobstopper-drain-restart-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let executable = std::path::Path::new(env!("CARGO_BIN_EXE_gobstopper"))
        .canonicalize()
        .unwrap();
    let service = "aabbcc0123456789";
    let platform = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let manifest = json!({
        "schema": 1, "platform": platform, "service_id": service,
        "state_dir": root, "executable": executable, "serve_args": [],
        "port": port, "definition_sha256": "fixture-definition", "rendered_definition": "fixture"
    });
    std::fs::write(root.join("manifest.json"), manifest.to_string()).unwrap();
    let journal = json!({
        "schema": 1, "service_id": service, "executable": executable,
        "definition_sha256": "fixture-definition", "port": port, "pid": 42,
        "instance_id": "prior-process", "owner": OWNER, "epoch": 3,
        "protocol": 1, "stage": stage
    });
    std::fs::write(
        root.join("drain-operation.json"),
        if damaged {
            "{".into()
        } else {
            journal.to_string()
        },
    )
    .unwrap();
    let log = root.join("stderr.log");
    let mut command = Command::new(&executable);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GOBSTOPPER_") {
            command.env_remove(key);
        }
    }
    command
        .env("HRANESS_AUDIENCE", "quiet")
        .env("XDG_CONFIG_HOME", &root)
        .env("GOBSTOPPER_DATA_DIR", root.join("data"))
        .env("GOBSTOPPER_STATS_FILE", "off")
        .args(["proxy", "serve", "--port", &port.to_string()])
        .args(["--service-id", service, "--service-state-dir"])
        .arg(&root)
        .args(["--no-keep-awake", "--no-session-data"])
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap());
    drop(listener);
    let child = command.spawn().unwrap();
    (
        ProxyProcess {
            data_root: root,
            child,
            port,
        },
        log,
    )
}

#[test]
fn leased_drain_restart_refuses_unresolved_stop_or_damaged_journal() {
    for stage in [
        "commit_intent",
        "committed",
        "stop_started",
        "stop_acknowledged",
        "damaged",
    ] {
        let damaged = stage == "damaged";
        let (mut proxy, log) = seeded_restart(stage, damaged);
        let deadline = Instant::now() + Duration::from_secs(10);
        let exit = loop {
            if let Some(status) = proxy.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "{stage}: restart did not refuse startup"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(!exit.success(), "{stage}: unsafe restart succeeded");
        let error = std::fs::read_to_string(log).unwrap();
        assert!(
            error.to_lowercase().contains(if damaged {
                "drain journal is damaged"
            } else {
                "unresolved service stop prevents startup inference"
            }),
            "{stage}: {error}"
        );
        assert!(TcpStream::connect(("127.0.0.1", proxy.port)).is_err());
        assert!(proxy.data_root.join("drain-operation.json").exists());
    }
}

#[test]
fn leased_drain_restart_after_waiting_does_not_inherit_old_controller() {
    let (mut proxy, log) = seeded_restart("waiting", false);
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", proxy.port)).is_err() {
        assert!(
            proxy.child.try_wait().unwrap().is_none(),
            "{}",
            std::fs::read_to_string(&log).unwrap()
        );
        assert!(
            Instant::now() < deadline,
            "waiting restart did not become ready"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let status = live(proxy.port);
    assert_eq!(status["drain_control"]["startup_guard"], true);
    assert_eq!(status["draining"], false);
    assert_eq!(status["drain_control"]["phase"], "open");
    assert_ne!(status["instance_id"], "prior-process");
    assert!(proxy.data_root.join("drain-operation.json").exists());
}

fn live(port: u16) -> Value {
    request(port, "GET", "/gobstopper/status", &[], b"").json()
}

fn owner_request(status: &Value, wait: u64) -> Value {
    json!({
        "instance_id": status["instance_id"], "pid": status["pid"],
        "owner": OWNER, "epoch": status["drain_control"]["epoch"], "wait_secs": wait,
    })
}

fn control(port: u16, action: &str, body: &Value) -> Response {
    request(
        port,
        "POST",
        &format!("/gobstopper/service/drain-lease/{action}"),
        &[("x-gobstopper-service-id", SERVICE)],
        &serde_json::to_vec(body).unwrap(),
    )
}

fn until_status(port: u16, predicate: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = live(port);
        if predicate(&status) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "unexpected final status: {status}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Owns a single stream; completion and cleanup are bounded even if an assertion fails.
struct HeldStream {
    port: u16,
    release: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl HeldStream {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let (release, resume) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("fixture accept: {e}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let (_, headers) = read_head(&mut reader).unwrap();
            let _ = read_body(&mut reader, &headers);
            stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n").unwrap();
            let first = "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"first retained chunk\"}}\n\n";
            write!(stream, "{:x}\r\n{first}\r\n", first.len()).unwrap();
            stream.flush().unwrap();
            let _ = resume.recv_timeout(Duration::from_secs(10));
            let last = "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"last retained chunk\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n";
            let _ = write!(stream, "{:x}\r\n{last}\r\n0\r\n\r\n", last.len());
        });
        Self {
            port,
            release,
            worker: Some(worker),
        }
    }
}

impl Drop for HeldStream {
    fn drop(&mut self) {
        let _ = self.release.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[test]
fn leased_drain_preserves_flowing_stream_and_fences_final_stop() {
    let upstream = HeldStream::start();
    let proxy = start_proxy(
        &format!("http://127.0.0.1:{}", upstream.port),
        &[
            "--service-id",
            SERVICE,
            "--no-keep-awake",
            "--no-session-data",
        ],
    );
    let mut stream = TcpStream::connect(("127.0.0.1", proxy.port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let body = body(&session(1));
    write!(stream, "POST /v1/messages HTTP/1.1\r\nhost: 127.0.0.1:{}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", proxy.port, body.len()).unwrap();
    stream.write_all(&body).unwrap();
    let mut received = Vec::new();
    while !String::from_utf8_lossy(&received).contains("first retained chunk") {
        let mut chunk = [0; 4096];
        let n = stream.read(&mut chunk).unwrap();
        assert_ne!(n, 0, "stream ended before its first event");
        received.extend_from_slice(&chunk[..n]);
    }
    let status = live(proxy.port);
    assert_eq!(status["keep_awake"]["active_inference"], 1);
    let mut owner = owner_request(&status, 600);
    let acquired = control(proxy.port, "acquire", &owner);
    assert_eq!(acquired.status, 200);
    owner["epoch"] = acquired.json()["epoch"].clone();
    assert_eq!(control(proxy.port, "commit", &owner).status, 409);
    for route in [
        "/v1/messages",
        "/v1/responses",
        "/backend-api/codex/responses/compact",
    ] {
        let blocked = request(proxy.port, "POST", route, ANTHROPIC, &body);
        assert_eq!(blocked.status, 503);
        assert!(blocked
            .headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("retry-after") && v == "1"));
    }
    assert_eq!(
        request(
            proxy.port,
            "POST",
            "/gobstopper/service/resume",
            &[("x-gobstopper-service-id", SERVICE)],
            b""
        )
        .status,
        409
    );
    assert_eq!(live(proxy.port)["drain_control"]["phase"], "waiting");
    upstream.release.send(()).unwrap();
    stream.read_to_end(&mut received).unwrap();
    let received = String::from_utf8(received).unwrap();
    assert!(received.starts_with("HTTP/1.1 200"));
    assert!(received.contains("last retained chunk"));
    assert!(received.contains("message_stop"));
    until_status(proxy.port, |s| s["keep_awake"]["active_inference"] == 0);
    assert_eq!(
        control(proxy.port, "commit", &owner).json()["phase"],
        "committed"
    );
    assert_eq!(control(proxy.port, "commit", &owner).status, 200);
    assert_eq!(control(proxy.port, "release", &owner).status, 409);
    assert_eq!(
        request(proxy.port, "POST", "/v1/messages", ANTHROPIC, &body).status,
        503
    );
}

#[test]
fn leased_drain_deadline_reopens_without_its_controller() {
    let fake = Fake::start(|_| Reply::Json(200, json!({"type":"message","content":[]})));
    let proxy = start_proxy(
        &fake.url(),
        &[
            "--service-id",
            SERVICE,
            "--no-keep-awake",
            "--no-session-data",
        ],
    );
    let mut owner = owner_request(&live(proxy.port), 1);
    let acquired = control(proxy.port, "acquire", &owner);
    assert_eq!(acquired.status, 200);
    owner["epoch"] = acquired.json()["epoch"].clone();
    assert_eq!(acquired.json()["phase"], "waiting");
    assert!(fake.seen().is_empty());
    let status = until_status(proxy.port, |s| s["drain_control"]["phase"] == "open");
    assert_eq!(status["draining"], false);
    assert!(status["drain_control"].get("owner").is_none());
    assert_eq!(control(proxy.port, "renew", &owner).status, 409);
    assert_eq!(control(proxy.port, "commit", &owner).status, 409);
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
    assert_eq!(fake.seen().len(), 1);
}

#[test]
fn leased_drain_rejects_stale_identity_owner_and_epoch() {
    let fake = Fake::start(|_| Reply::Json(200, json!({})));
    let proxy = start_proxy(
        &fake.url(),
        &[
            "--service-id",
            SERVICE,
            "--no-keep-awake",
            "--no-session-data",
        ],
    );
    let status = live(proxy.port);
    let mut owner = owner_request(&status, 600);
    let mut wrong = owner.clone();
    wrong["instance_id"] = json!("different-incarnation");
    assert_eq!(control(proxy.port, "acquire", &wrong).status, 409);
    wrong = owner.clone();
    wrong["pid"] = json!(status["pid"].as_u64().unwrap() + 1);
    assert_eq!(control(proxy.port, "acquire", &wrong).status, 409);
    assert_eq!(live(proxy.port)["drain_control"]["phase"], "open");
    let first = control(proxy.port, "acquire", &owner);
    assert_eq!(first.status, 200);
    owner["epoch"] = first.json()["epoch"].clone();
    wrong = owner.clone();
    wrong["owner"] = json!("another-operation-123456789");
    assert_eq!(control(proxy.port, "release", &wrong).status, 409);
    assert_eq!(control(proxy.port, "release", &owner).status, 200);
    let mut successor = owner_request(&live(proxy.port), 600);
    let second = control(proxy.port, "acquire", &successor);
    assert_eq!(second.status, 200);
    successor["epoch"] = second.json()["epoch"].clone();
    for action in ["release", "renew", "commit"] {
        assert_eq!(control(proxy.port, action, &owner).status, 409);
    }
    assert_eq!(live(proxy.port)["drain_control"]["phase"], "waiting");
    assert_eq!(control(proxy.port, "release", &successor).status, 200);
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
fn leased_drain_allows_existing_requests_provider_retry_to_finish() {
    let (first_seen, first_started) = mpsc::channel();
    let (retry_seen, retry_started) = mpsc::channel();
    let (reject_first, first_gate) = mpsc::channel();
    let (finish_retry, retry_gate) = mpsc::channel();
    let first_gate = Mutex::new(first_gate);
    let retry_gate = Mutex::new(retry_gate);
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let fake = Fake::start(move |_| {
        if attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            first_seen.send(()).unwrap();
            first_gate
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            Reply::Json(
                400,
                json!({"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 250000 tokens > 200000 maximum"}}),
            )
        } else {
            retry_seen.send(()).unwrap();
            retry_gate
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            Reply::Json(200, json!({"ok":"retry completed"}))
        }
    });
    let proxy = start_proxy(
        &fake.url(),
        &[
            "--service-id",
            SERVICE,
            "--no-keep-awake",
            "--no-session-data",
            "--threshold",
            "1000000",
            "--keep-recent",
            "1",
        ],
    );
    std::thread::scope(|threads| {
        let inference = threads.spawn(|| {
            request(
                proxy.port,
                "POST",
                "/v1/messages",
                ANTHROPIC,
                &body(&session(8)),
            )
        });
        first_started.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut owner = owner_request(&live(proxy.port), 600);
        let acquired = control(proxy.port, "acquire", &owner);
        assert_eq!(acquired.status, 200);
        owner["epoch"] = acquired.json()["epoch"].clone();
        reject_first.send(()).unwrap();
        retry_started.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(live(proxy.port)["keep_awake"]["active_inference"], 1);
        assert_eq!(control(proxy.port, "commit", &owner).status, 409);
        finish_retry.send(()).unwrap();
        let response = inference.join().unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.json(), json!({"ok":"retry completed"}));
        until_status(proxy.port, |s| s["keep_awake"]["active_inference"] == 0);
        assert_eq!(control(proxy.port, "release", &owner).status, 200);
    });
    assert_eq!(fake.seen().len(), 2);
}
