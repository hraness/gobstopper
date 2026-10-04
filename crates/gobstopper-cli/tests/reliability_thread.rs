#![cfg(unix)]

use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct World {
    root: PathBuf,
    proxy: Option<Child>,
    stop: Arc<AtomicBool>,
    upstream: Option<JoinHandle<()>>,
}

impl World {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "gobstopper-reliability-thread-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(root.join("config")).unwrap();
        Self {
            root,
            proxy: None,
            stop: Arc::new(AtomicBool::new(false)),
            upstream: None,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/opt/homebrew/bin")
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("xdg-data"))
            .env("GOBSTOPPER_DATA_DIR", self.root.join("observations"))
            .env("GOBSTOPPER_STATS_FILE", "off")
            .env("HRANESS_NO_UPDATE", "1")
            .env("HRANESS_AUDIENCE", "quiet");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn start(&mut self, expected: Vec<u8>) -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let upstream_port = listener.local_addr().unwrap().port();
        let stop = Arc::clone(&self.stop);
        self.upstream = Some(std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("fake upstream accept: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(line.starts_with("POST /v1/responses "));
                let mut length = None;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = Some(value.trim().parse::<usize>().unwrap());
                        }
                    }
                }
                let length = length.unwrap();
                assert!(length < 4096);
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                assert_eq!(body, expected);
                std::thread::sleep(Duration::from_millis(15));
                let response = serde_json::to_vec(&json!({
                    "id": "response-fixture", "object": "response", "status": "completed",
                    "model": "gpt-fixture", "output": [],
                    "usage": {"input_tokens": 100, "output_tokens": 20}
                }))
                .unwrap();
                write!(reader.get_mut(), "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).unwrap();
                reader.get_mut().write_all(&response).unwrap();
            }
        }));
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        let upstream = format!("http://127.0.0.1:{upstream_port}");
        let mut command = self.command();
        command
            .args([
                "proxy",
                "serve",
                "--port",
                &port.to_string(),
                "--openai-upstream",
                &upstream,
                "--no-keep-awake",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        drop(reservation);
        self.proxy = Some(command.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            assert!(self.proxy.as_mut().unwrap().try_wait().unwrap().is_none());
            if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream.write_all(b"GET /gobstopper/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
                let mut response = String::new();
                stream.read_to_string(&mut response).unwrap();
                if response.starts_with("HTTP/1.1 200")
                    && self.json(&["proxy", "status", "--port", &port.to_string(), "--json"])
                        ["observations"]["available"]
                        == true
                {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "isolated proxy did not become ready"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        port
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if let Some(mut proxy) = self.proxy.take() {
            if proxy.try_wait().ok().flatten().is_none() {
                let _ = Command::new("/bin/kill")
                    .args(["-TERM", &proxy.id().to_string()])
                    .status();
                let deadline = Instant::now() + Duration::from_secs(2);
                while proxy.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                if proxy.try_wait().ok().flatten().is_none() {
                    let _ = proxy.kill();
                }
            }
            let _ = proxy.wait();
        }
        self.stop.store(true, Ordering::Release);
        if let Some(upstream) = self.upstream.take() {
            let result = upstream.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn records_diagnoses_reports_archives_and_recovers_without_changing_original_data() {
    let mut world = World::new();
    let body = serde_json::to_vec(&json!({
        "model": "gpt-fixture", "input": [{"role":"user","content":"PUBLIC_TEST_INPUT"}]
    }))
    .unwrap();
    let port = world.start(body.clone());
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(stream, "POST /v1/responses HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nsession_id: native-fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
    stream.write_all(&body).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    let deadline = Instant::now() + Duration::from_secs(8);
    let request = loop {
        let requests = world.json(&["data", "requests"]);
        if let Some(request) = requests
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["incomplete"] == false)
        {
            break request.clone();
        }
        assert!(Instant::now() < deadline, "observation did not commit");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(request["outcome"], "success");
    assert_eq!(request["reason"], "completed");
    assert_eq!(request["source"]["profile"], "gobstopper-proxy-v2");
    let duration = request["duration_ms"].as_u64().unwrap();
    let preparation = request["timings"]["preparation_ms"].as_u64().unwrap();
    let headers = request["timings"]["upstream_headers_ms"].as_u64().unwrap();
    assert!(headers >= 10);
    assert!(preparation + headers <= duration);
    let metrics = world.json(&["data", "metrics"]);
    assert_eq!(metrics["cohorts"][0]["attempts"], 1);
    assert_eq!(metrics["cohorts"][0]["reported_input_tokens"], "100");
    assert_eq!(metrics["cohorts"][0]["reported_output_tokens"], "20");
    let status = world.json(&["data", "status"]);
    assert_eq!(world.json(&["data", "check"])["ok"], true);
    let archive = world.root.join("archive");
    let archived = world.json(&[
        "data",
        "archive",
        "--output",
        archive.to_str().unwrap(),
        "--segment-events",
        "2",
    ]);
    assert_eq!(archived["complete"], true);
    assert_eq!(archived["event_count"], status["events"]);
    assert_eq!(
        world.json(&["data", "archive-check", archive.to_str().unwrap()]),
        archived
    );
    let restored = world.root.join("restored");
    let manifest = fs::read_to_string(archive.join("manifest.jsonl")).unwrap();
    for row in manifest.lines().skip(1) {
        let row: Value = serde_json::from_str(row).unwrap();
        let file = archive.join(row["file"].as_str().unwrap());
        world.json(&[
            "data",
            "--state-dir",
            restored.to_str().unwrap(),
            "import",
            file.to_str().unwrap(),
        ]);
    }
    assert_eq!(
        world.json(&["data", "--state-dir", restored.to_str().unwrap(), "check"])["ok"],
        true
    );
    assert_eq!(
        world.json(&["data", "--state-dir", restored.to_str().unwrap(), "metrics"]),
        metrics
    );
    assert_eq!(world.json(&["data", "status"])["events"], status["events"]);
    let log = world.root.join("xdg-data/gobstopper/events.jsonl");
    fs::create_dir_all(log.parent().unwrap()).unwrap();
    let legacy = b"{\"schema\":\"gobstopper/compaction-events-v1\",\"provider\":\"devin\",\"ts\":1,\"session_id\":\"historical\",\"prompt\":\"PRIVATE_HISTORY_SENTINEL\"}\n";
    fs::write(&log, legacy).unwrap();
    let diagnostics = world.json(&["events", "--diagnostics", "--json"]);
    assert_eq!(diagnostics["diagnostics_only"], true);
    assert_eq!(diagnostics["evidence_eligible"], false);
    assert!(!diagnostics.to_string().contains("PRIVATE_HISTORY_SENTINEL"));
    assert!(!world.run(&["events", "--json"]).status.success());
    assert_eq!(fs::read(&log).unwrap(), legacy);
}
