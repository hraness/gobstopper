//! Real-socket regressions with deliberately small limits, without user state.
use super::*;
use std::sync::mpsc;

fn test_proxy(threshold: u64, threshold_1m: u64) -> Proxy {
    let mut proxy = tests::test_proxy(threshold, threshold_1m);
    // A routing regression must never send fixture traffic to a public provider.
    proxy.anthropic = "http://127.0.0.1:9".into();
    proxy.openai = "http://127.0.0.1:9".into();
    proxy.chatgpt = "http://127.0.0.1:9".into();
    proxy
}

fn pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    (client, listener.accept().unwrap().0)
}

fn dispatch(proxy: &Arc<Proxy>) -> TcpStream {
    let (client, server) = pair();
    dispatch_connection(server, Arc::clone(proxy), 2, |job| {
        std::thread::Builder::new().spawn(job).map(|_| ())
    });
    client
}

fn send(client: &mut TcpStream, method: &str, path: &str, body: &[u8]) {
    write!(client, "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nanthropic-version: 2023-06-01\r\nx-gobstopper-service-id: test-service\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
    client.write_all(body).unwrap();
}

fn response(mut client: TcpStream) -> (u16, Value) {
    let mut bytes = Vec::new();
    client.read_to_end(&mut bytes).unwrap();
    let mut head = BufReader::new(bytes.as_slice());
    let (status, _, _) = read_response_head(&mut head).unwrap();
    let at = find(&bytes, b"\r\n\r\n").unwrap();
    (
        status,
        serde_json::from_slice(&bytes[at + 4..]).unwrap_or(Value::Null),
    )
}

fn exchange(proxy: &Arc<Proxy>, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
    let mut client = dispatch(proxy);
    send(&mut client, method, path, body);
    response(client)
}

#[test]
fn resilience_saturated_inference_preserves_health_and_drain_without_replay() {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let mut proxy = test_proxy(128_000, 256_000);
    proxy.anthropic = format!("http://{}", upstream.local_addr().unwrap());
    proxy.service_id = Some("test-service".into());
    let proxy = Arc::new(proxy);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let provider = std::thread::spawn(move || {
        let mut sockets = Vec::new();
        for _ in 0..2 {
            let (mut socket, _) = upstream.accept().unwrap();
            let mut wire = Wire {
                stream: &mut socket,
                buffer: Vec::new(),
                deadline: Instant::now() + Duration::from_secs(5),
            };
            let mut request = read_request_head(&mut wire).unwrap();
            read_request_body(&mut wire, &mut request, MAX_BODY_BYTES, None).unwrap();
            assert_eq!(request.path(), "/v1/messages");
            sockets.push(socket);
            started_tx.send(()).unwrap();
        }
        release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        for mut socket in sockets {
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        }
        upstream.set_nonblocking(true).unwrap();
        assert!(
            matches!(upstream.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "a rejected request or an existing inference was replayed"
        );
    });
    let mut clients = Vec::new();
    for _ in 0..2 {
        let mut client = dispatch(&proxy);
        send(
            &mut client,
            "POST",
            "/v1/messages",
            br#"{"model":"test","messages":[{"role":"user","content":"hello"}]}"#,
        );
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        clients.push(client);
    }
    assert_eq!(proxy.inference.load(Ordering::Acquire), 2);
    assert_eq!(exchange(&proxy, "POST", "/v1/messages", b"{}").0, 503);
    let (code, ready) = exchange(&proxy, "GET", READY_PATH, b"");
    assert_eq!(code, 200);
    assert_eq!(ready["active_inference"], 2);
    assert_eq!(exchange(&proxy, "GET", STATUS_PATH, b"").0, 200);
    assert_eq!(
        exchange(&proxy, "POST", "/gobstopper/service/drain", b"").0,
        409
    );
    let lease = json!({"owner":"test-owner-0123456789", "epoch":0, "wait_secs":60, "instance_id":proxy.instance_id, "pid":std::process::id()});
    let (code, leased) = exchange(
        &proxy,
        "POST",
        "/gobstopper/service/drain-lease/acquire",
        &serde_json::to_vec(&lease).unwrap(),
    );
    assert_eq!(code, 200);
    assert_eq!(leased["active_inference"], 2);
    assert_eq!(exchange(&proxy, "GET", READY_PATH, b"").0, 503);
    release_tx.send(()).unwrap();
    for client in clients {
        assert_eq!(response(client).0, 200);
    }
    provider.join().unwrap();
}

#[test]
fn resilience_readiness_does_not_wait_for_optional_state_or_admission() {
    let proxy = Arc::new(test_proxy(128_000, 256_000));
    let _calibrations = proxy.calibrations.lock().unwrap();
    let _context = proxy.control.state.lock().unwrap();
    assert_eq!(exchange(&proxy, "GET", READY_PATH, b"").0, 200);
    let _admission = proxy.admission.lock().unwrap();
    let (code, ready) = exchange(&proxy, "GET", READY_PATH, b"");
    assert_eq!(code, 503);
    assert_eq!(ready["ready"], false);
    assert_eq!(ready["instance_id"], proxy.instance_id);
}

#[test]
fn resilience_slow_headers_do_not_block_other_connections_and_have_total_deadline() {
    let proxy = Arc::new(test_proxy(128_000, 256_000));
    let mut slow = dispatch(&proxy);
    slow.write_all(b"GET /gobstopper/ready HTTP/1.1\r\nHost:")
        .unwrap();
    assert_eq!(exchange(&proxy, "GET", READY_PATH, b"").0, 200);
    let (mut client, mut server) = pair();
    let writer = std::thread::spawn(move || {
        for _ in 0..20 {
            if client.write_all(b"x").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let started = Instant::now();
    let mut wire = Wire {
        stream: &mut server,
        buffer: Vec::new(),
        deadline: started + Duration::from_millis(100),
    };
    assert!(read_request_head(&mut wire).is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(server);
    writer.join().unwrap();
    drop(slow);
}

#[test]
fn resilience_worker_spawn_failure_releases_socket_and_capacity() {
    let proxy = Arc::new(test_proxy(128_000, 256_000));
    let (mut client, server) = pair();
    dispatch_connection(server, Arc::clone(&proxy), 2, |job| {
        drop(job);
        Err(std::io::Error::other("injected thread resource exhaustion"))
    });
    assert_eq!(proxy.active.load(Ordering::Acquire), 0);
    assert_eq!(client.read(&mut [0]).unwrap(), 0);
    assert_eq!(exchange(&proxy, "GET", READY_PATH, b"").0, 200);
}

#[test]
fn resilience_absolute_capacity_never_writes_or_spawns_on_accept_thread() {
    let proxy = Arc::new(test_proxy(128_000, 256_000));
    proxy.active.store(2 + CONTROL_RESERVE, Ordering::Release);
    let (mut client, server) = pair();
    dispatch_connection(server, Arc::clone(&proxy), 2, |_| {
        panic!("must not spawn over hard socket cap")
    });
    assert_eq!(client.read(&mut [0]).unwrap(), 0);
    assert_eq!(proxy.active.load(Ordering::Acquire), 2 + CONTROL_RESERVE);
    proxy.active.store(0, Ordering::Release);
    assert_eq!(exchange(&proxy, "GET", READY_PATH, b"").0, 200);
}

fn stall_transforms(proxy: &Proxy) -> Vec<mpsc::Sender<()>> {
    let mut releases = Vec::new();
    for _ in 0..4 {
        let (release, receive) = mpsc::channel();
        let job = proxy.transforms.acquire(&proxy.memory, 0).unwrap();
        assert!(matches!(
            job.run(Duration::from_millis(10), move || {
                receive.recv_timeout(Duration::from_secs(10)).unwrap();
            }),
            Err(work::Failure::Timeout)
        ));
        releases.push(release);
    }
    releases
}

#[test]
fn resilience_stalled_transform_pool_forwards_exact_original_once() {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let original = br#"{ "model": "test", "messages": [{"role":"user","content":"hello"}] }"#;
    let mut proxy = test_proxy(128_000, 256_000);
    proxy.anthropic = format!("http://{}", upstream.local_addr().unwrap());
    let releases = stall_transforms(&proxy);
    let proxy = Arc::new(proxy);
    let provider = std::thread::spawn(move || {
        let (mut socket, _) = upstream.accept().unwrap();
        let mut wire = Wire {
            stream: &mut socket,
            buffer: Vec::new(),
            deadline: Instant::now() + Duration::from_secs(5),
        };
        let mut request = read_request_head(&mut wire).unwrap();
        read_request_body(&mut wire, &mut request, MAX_BODY_BYTES, None).unwrap();
        assert_eq!(request.body, original);
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .unwrap();
        upstream
    });
    let started = Instant::now();
    assert_eq!(exchange(&proxy, "POST", "/v1/messages", original).0, 200);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(exchange(&proxy, "GET", READY_PATH, b"").0, 200);
    assert_eq!(proxy.stats.fail_open.load(Ordering::Relaxed), 1);
    let upstream = provider.join().unwrap();
    upstream.set_nonblocking(true).unwrap();
    assert!(matches!(upstream.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    for release in releases {
        release.send(()).unwrap();
    }
}

#[test]
fn resilience_unavailable_transform_cannot_bypass_explicit_capacity() {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let mut proxy = test_proxy(128_000, 256_000);
    proxy.context_window = Some(200_000);
    proxy.anthropic = format!("http://{}", upstream.local_addr().unwrap());
    let releases = stall_transforms(&proxy);
    let proxy = Arc::new(proxy);
    let (status, body) = exchange(
        &proxy,
        "POST",
        "/v1/messages",
        br#"{"messages":[{"role":"user","content":"hello"}]}"#,
    );
    assert_eq!(status, 503);
    assert_eq!(body["error"]["type"], "proxy_transform_unavailable");
    upstream.set_nonblocking(true).unwrap();
    assert!(matches!(upstream.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    for release in releases {
        release.send(()).unwrap();
    }
}

#[test]
fn resilience_unavailable_transform_cannot_bypass_strict_policy() {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let mut proxy = test_proxy(2000, 256_000);
    proxy.engine = Arc::new(Engine::new(CliffConfig {
        threshold_tokens: 2000,
        strict: true,
        ..CliffConfig::default()
    }));
    proxy.anthropic = format!("http://{}", upstream.local_addr().unwrap());
    let releases = stall_transforms(&proxy);
    let proxy = Arc::new(proxy);
    let original = serde_json::to_vec(&json!({
        "messages": [{"role":"user","content":"large unchecked input ".repeat(1000)}]
    }))
    .unwrap();
    let started = Instant::now();
    let (status, body) = exchange(&proxy, "POST", "/v1/messages", &original);
    assert_eq!(status, 503);
    assert_eq!(body["error"]["type"], "proxy_transform_unavailable");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(exchange(&proxy, "GET", READY_PATH, b"").0, 200);
    upstream.set_nonblocking(true).unwrap();
    assert!(matches!(upstream.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    for release in releases {
        release.send(()).unwrap();
    }
}

#[test]
fn resilience_unavailable_transform_preserves_strict_shadow_forwarding() {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let mut proxy = test_proxy(2000, 256_000);
    proxy.engine = Arc::new(Engine::new(CliffConfig {
        threshold_tokens: 2000,
        strict: true,
        ..CliffConfig::default()
    }));
    proxy.shadow = true;
    proxy.anthropic = format!("http://{}", upstream.local_addr().unwrap());
    let releases = stall_transforms(&proxy);
    let proxy = Arc::new(proxy);
    let original = serde_json::to_vec(&json!({
        "messages": [{"role":"user","content":"large unchecked input ".repeat(1000)}]
    }))
    .unwrap();
    let expected = original.clone();
    let provider = std::thread::spawn(move || {
        let (mut socket, _) = upstream.accept().unwrap();
        let mut wire = Wire {
            stream: &mut socket,
            buffer: Vec::new(),
            deadline: Instant::now() + Duration::from_secs(5),
        };
        let mut request = read_request_head(&mut wire).unwrap();
        read_request_body(&mut wire, &mut request, MAX_BODY_BYTES, None).unwrap();
        assert_eq!(request.body, expected);
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .unwrap();
        upstream
    });
    assert_eq!(exchange(&proxy, "POST", "/v1/messages", &original).0, 200);
    let upstream = provider.join().unwrap();
    upstream.set_nonblocking(true).unwrap();
    assert!(matches!(upstream.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    for release in releases {
        release.send(()).unwrap();
    }
}

#[test]
fn resilience_full_body_memory_rejects_upload_without_disabling_health() {
    let mut proxy = test_proxy(128_000, 256_000);
    proxy.memory = work::MemoryBudget::new(10);
    let proxy = Arc::new(proxy);
    let mut client = dispatch(&proxy);
    client
        .write_all(b"POST /v1/messages HTTP/1.1\r\nHost: localhost\r\nContent-Length: 11\r\n\r\n")
        .unwrap();
    let (code, body) = response(client);
    assert_eq!(code, 503);
    assert_eq!(body["error"]["type"], "proxy_overloaded");
    assert_eq!(proxy.memory.used(), 0);
    assert_eq!(exchange(&proxy, "GET", READY_PATH, b"").0, 200);
}

#[test]
fn resilience_chunked_body_reserves_each_chunk_and_releases_after_failure() {
    let (mut client, mut server) = pair();
    client.write_all(b"POST /v1/messages HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n8\r\n12345678\r\n8\r\n").unwrap();
    let mut wire = Wire {
        stream: &mut server,
        buffer: Vec::new(),
        deadline: Instant::now() + Duration::from_secs(1),
    };
    let mut request = read_request_head(&mut wire).unwrap();
    let memory = work::MemoryBudget::new(12);
    let mut lease = memory.lease();
    let error =
        read_request_body(&mut wire, &mut request, MAX_BODY_BYTES, Some(&mut lease)).unwrap_err();
    assert!(error.is::<work::MemoryExhausted>());
    assert_eq!(memory.used(), 8);
    drop(lease);
    assert_eq!(memory.used(), 0);
}

#[test]
fn resilience_late_transform_result_cannot_publish_a_compacted_prefix() {
    let proxy = test_proxy(2000, 256_000);
    let mut messages = vec![json!({"role":"user","content":"inspect this system"})];
    for n in 0..15 {
        let id = format!("read-{n}");
        messages.push(json!({"role":"assistant","content":[{"type":"tool_use","id":id,"name":"Read","input":{"file_path":"source.rs"}}]}));
        messages.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":"unchanged diagnostic ".repeat(300)}]}));
    }
    let body = serde_json::to_vec(&json!({"messages":messages})).unwrap();
    let engine = Arc::clone(&proxy.engine);
    let (release, receive) = mpsc::channel();
    let (finished, observed) = mpsc::channel();
    let job = proxy.transforms.acquire(&proxy.memory, body.len()).unwrap();
    let result = job.run(Duration::from_millis(10), move || {
        receive.recv_timeout(Duration::from_secs(5)).unwrap();
        let Value::Object(body) = serde_json::from_slice(&body).unwrap() else {
            unreachable!()
        };
        let prepared = engine
            .prepare_deferred(body, Dialect::Anthropic, 2000, 1000, "late", None)
            .unwrap();
        finished.send(prepared.compacted).unwrap();
        prepared
    });
    assert!(matches!(result, Err(work::Failure::Timeout)));
    assert!(proxy.memory.used() > 0);
    release.send(()).unwrap();
    assert!(observed.recv_timeout(Duration::from_secs(5)).unwrap());
    let deadline = Instant::now() + Duration::from_secs(3);
    while proxy.memory.used() != 0 && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(proxy.memory.used(), 0);
    assert_eq!(proxy.engine.store_stats().0, 0);
}
