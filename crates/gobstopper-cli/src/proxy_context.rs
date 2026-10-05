//! Scoped policy I/O cannot occupy forwarding threads indefinitely. A lost
//! consume result is never replayed: its reservation may have been consumed.
use super::{work, ContextAccess};
use crate::context::{Control, Decision};
use crate::proxy_observations::{FailureCode, FailureHistory};
use gobstopper_adapters::request::EvidenceObservation;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(super) const TIMEOUT: Duration = Duration::from_secs(2);
const QUEUE_LIMIT: usize = 64;
const MAX_OBSERVATIONS: usize = 128;
const MAX_SOURCE_BYTES: usize = 1024;
const MAX_EVICTED: usize = 2048;

#[derive(Debug)]
pub(super) enum Failure {
    Unavailable,
    Invalid,
}

pub(super) struct Policy {
    pub(super) base: u64,
    pub(super) window: Option<u64>,
}

impl ContextAccess {
    pub(super) fn consume(
        self: &Arc<Self>,
        memory: &work::MemoryBudget,
        body: &[u8],
        scope: &str,
        policy: Policy,
        timeout: Duration,
    ) -> Result<Decision, Failure> {
        if scope.len() != 64 || !scope.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Failure::Invalid);
        }
        let deadline = Instant::now() + timeout;
        // This pool is distinct from optional transforms. Timed-out work keeps
        // both leases until the actual database operation finishes.
        let job = self
            .jobs
            .acquire(memory, body.len())
            .ok_or(Failure::Unavailable)?;
        let access = Arc::clone(self);
        let scope = scope.to_owned();
        let body = body.to_owned();
        job.run(
            deadline.saturating_duration_since(Instant::now()),
            move || {
                let control = access.get().ok_or(Failure::Unavailable)?;
                let output = serde_json::from_slice::<Value>(&body)
                    .ok()
                    .and_then(|value| value.as_object().map(super::requested_output))
                    .unwrap_or(32_000);
                if Instant::now() >= deadline {
                    return Err(Failure::Unavailable);
                }
                control
                    .consume(&scope, policy.base, policy.window, output)
                    .map_err(|error| {
                        if error.downcast_ref::<rusqlite::Error>().is_some() {
                            Failure::Unavailable
                        } else {
                            Failure::Invalid
                        }
                    })
            },
        )
        .map_err(|_| Failure::Unavailable)?
    }

    pub(super) fn observe(
        &self,
        scope: &str,
        evidence: Vec<EvidenceObservation>,
        evicted: &[String],
        decision: &Decision,
    ) {
        if !decision.adaptive {
            return;
        }
        let Some(control) = self.current() else {
            self.observer.state.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let observations: Vec<_> = evidence.into_iter().rev().take(MAX_OBSERVATIONS).collect();
        // Provider IDs are arbitrary input. Cap retained metadata as well as
        // queue length; no request content or IDs are printed in diagnostics.
        if scope.len() != 64
            || observations.iter().any(|entry| {
                entry.source_id.len() > MAX_SOURCE_BYTES
                    || entry.content_digest.len() > 64
                    || entry.kind.len() > 64
            })
            || evicted
                .iter()
                .take(MAX_EVICTED)
                .any(|digest| digest.len() > 64)
        {
            self.observer.state.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let mut observations = observations;
        observations.reverse();
        self.observer.enqueue(Observation {
            control,
            scope: scope.to_owned(),
            observations,
            evicted: evicted.iter().take(MAX_EVICTED).cloned().collect(),
            decision: decision.clone(),
        });
    }
}

struct Observation {
    control: Arc<Control>,
    scope: String,
    observations: Vec<EvidenceObservation>,
    evicted: Vec<String>,
    decision: Decision,
}

#[derive(Default)]
struct State {
    queued: AtomicUsize,
    dropped: AtomicU64,
    failures: AtomicU64,
    processed: AtomicU64,
    rescues: AtomicU64,
    history: FailureHistory,
}

pub(super) struct Observer {
    sender: Option<SyncSender<Observation>>,
    state: Arc<State>,
}

impl Default for Observer {
    fn default() -> Self {
        Self::start(|batch| {
            batch.control.observe(
                &batch.scope,
                &batch.observations,
                &batch.evicted,
                &batch.decision,
            )
        })
    }
}

impl Observer {
    fn start(mut write: impl FnMut(Observation) -> anyhow::Result<bool> + Send + 'static) -> Self {
        let (sender, receiver) = sync_channel(QUEUE_LIMIT);
        let state = Arc::new(State::default());
        let worker_state = Arc::clone(&state);
        let worker = std::thread::Builder::new()
            .name("proxy-context-observer".into())
            .spawn(move || {
                for batch in receiver {
                    worker_state.queued.fetch_sub(1, Ordering::Relaxed);
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| write(batch))) {
                        Ok(Ok(rescued)) => {
                            worker_state.history.recover();
                            worker_state.processed.fetch_add(1, Ordering::Release);
                            if rescued {
                                worker_state.rescues.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        _ => {
                            // A failed commit has an uncertain result; never retry
                            // this batch or hold up a request on its account.
                            worker_state.history.record(FailureCode::StorageWrite);
                            worker_state.failures.fetch_add(1, Ordering::Release);
                            worker_state.dropped.fetch_add(1, Ordering::Release);
                        }
                    }
                }
            });
        if worker.is_err() {
            state.history.record(FailureCode::WorkerSpawn);
            state.failures.fetch_add(1, Ordering::Release);
        }
        Self {
            sender: worker.ok().map(|_| sender),
            state,
        }
    }

    fn enqueue(&self, batch: Observation) {
        self.state.queued.fetch_add(1, Ordering::Relaxed);
        let failure = match &self.sender {
            Some(sender) => match sender.try_send(batch) {
                Ok(()) => None,
                Err(std::sync::mpsc::TrySendError::Full(_)) => Some(FailureCode::QueueFull),
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    Some(FailureCode::WorkerStopped)
                }
            },
            None => Some(FailureCode::WorkerStopped),
        };
        if let Some(code) = failure {
            self.state.history.record(code);
            self.state.queued.fetch_sub(1, Ordering::Relaxed);
            self.state.dropped.fetch_add(1, Ordering::Release);
        }
    }

    pub(super) fn status(&self) -> Value {
        let dropped = self.state.dropped.load(Ordering::Acquire);
        let write_failures = self.state.failures.load(Ordering::Acquire);
        let processed = self.state.processed.load(Ordering::Acquire);
        let (last_failure_at_ms, last_failure_code, last_recovery_at_ms) =
            self.state.history.fields();
        json!({
            "queued": self.state.queued.load(Ordering::Relaxed),
            "queue_limit": QUEUE_LIMIT,
            "dropped": dropped,
            "write_failures": write_failures,
            "processed": processed,
            "rescues": self.state.rescues.load(Ordering::Relaxed),
            "last_failure_at_ms": last_failure_at_ms,
            "last_failure_code": last_failure_code,
            "last_recovery_at_ms": last_recovery_at_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::mpsc;

    #[test]
    fn cold_context_store_is_not_reported_as_a_storage_failure() {
        let mut proxy = super::super::tests::test_proxy(128_000, 256_000);
        assert_eq!(proxy.status()["context_control"]["state"], "disabled");
        assert!(proxy.status()["context_control"]["error"].is_null());
        let root = std::env::temp_dir().join(format!(
            "gobstopper-cold-context-{}",
            crate::proxy_agent::unique_id()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("blocked"), b"not a directory").unwrap();
        proxy.control = Arc::new(ContextAccess::new(Some(
            root.join("blocked/context.sqlite3"),
        )));
        assert_eq!(proxy.status()["context_control"]["state"], "uninitialized");
        assert!(proxy.status()["context_control"]["error"].is_null());
        assert!(proxy.control.get().is_none());
        assert_eq!(proxy.status()["context_control"]["state"], "unavailable");
        assert_eq!(
            proxy.status()["context_control"]["error"],
            "context_control_unavailable"
        );
        drop(proxy);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn fixture() -> (PathBuf, Arc<ContextAccess>, String) {
        let root = std::env::temp_dir().join(format!(
            "gobstopper-scoped-io-{}",
            crate::proxy_agent::unique_id()
        ));
        let access = Arc::new(ContextAccess::new(Some(root.join("context.sqlite3"))));
        let scope = access
            .get()
            .unwrap()
            .create(Some(500_000), None, 32_000, true, None)
            .unwrap();
        (root, access, scope)
    }

    fn until(mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(done(), "bounded worker did not finish after release");
    }

    #[test]
    fn scoped_io_database_stall_times_out_without_releasing_worker_or_memory_early() {
        let (root, access, scope) = fixture();
        access
            .get()
            .unwrap()
            .reserve(&scope, 200_000, 10, 60)
            .unwrap();
        let blocker = rusqlite::Connection::open(root.join("context.sqlite3")).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let memory = work::MemoryBudget::new(1000);
        for _ in 0..4 {
            let started = Instant::now();
            assert!(matches!(
                access.consume(
                    &memory,
                    b"{}",
                    &scope,
                    Policy {
                        base: 128_000,
                        window: None
                    },
                    Duration::from_millis(100)
                ),
                Err(Failure::Unavailable)
            ));
            assert!(started.elapsed() < Duration::from_secs(1));
        }
        assert_eq!(access.jobs.status()["active"], 4);
        assert_eq!(memory.used(), 8);
        let started = Instant::now();
        assert!(matches!(
            access.consume(
                &memory,
                b"{}",
                &scope,
                Policy {
                    base: 128_000,
                    window: None
                },
                Duration::from_secs(2)
            ),
            Err(Failure::Unavailable)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        blocker.execute_batch("ROLLBACK").unwrap();
        drop(blocker);
        until(|| memory.used() == 0 && access.jobs.status()["active"] == 0);
        let remaining = access
            .get()
            .unwrap()
            .status(&scope)
            .unwrap()
            .remaining_requests;
        // Uncertain consumes may finish once; none is replayed or restored.
        assert!((6..=10).contains(&remaining));
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(
            access
                .get()
                .unwrap()
                .status(&scope)
                .unwrap()
                .remaining_requests,
            remaining
        );
        drop(access);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn batch(access: &ContextAccess, scope: &str) -> Observation {
        let control = access.get().unwrap();
        let decision = control.status(scope).unwrap();
        Observation {
            control,
            scope: scope.to_owned(),
            observations: Vec::new(),
            evicted: Vec::new(),
            decision,
        }
    }

    #[test]
    fn scoped_io_stalled_observation_writer_has_bounded_nonblocking_queue() {
        let (root, access, scope) = fixture();
        let (entered, started) = mpsc::channel();
        let (release, receive) = mpsc::channel();
        let mut receive = Some(receive);
        let observer = Observer::start(move |_| {
            if let Some(receive) = receive.take() {
                entered.send(()).unwrap();
                receive.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            Ok(false)
        });
        observer.enqueue(batch(&access, &scope));
        started.recv_timeout(Duration::from_secs(1)).unwrap();
        let batches: Vec<_> = (0..=QUEUE_LIMIT).map(|_| batch(&access, &scope)).collect();
        let started = Instant::now();
        for batch in batches {
            observer.enqueue(batch);
        }
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(observer.status()["queued"], QUEUE_LIMIT);
        assert_eq!(observer.status()["dropped"], 1);
        release.send(()).unwrap();
        until(|| observer.status()["processed"] == QUEUE_LIMIT + 1);
        drop(observer);
        drop(access);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scoped_io_observation_errors_are_counted_without_replaying_uncertain_writes() {
        let (root, access, scope) = fixture();
        let observer = Observer::start(|_| anyhow::bail!("injected uncertain commit"));
        observer.enqueue(batch(&access, &scope));
        until(|| observer.status()["write_failures"] == 1 && observer.status()["dropped"] == 1);
        assert_eq!(observer.status()["dropped"], 1);
        assert_eq!(observer.status()["queued"], 0);
        assert_eq!(observer.status()["processed"], 0);
        let status = observer.status();
        assert_eq!(status["last_failure_code"], "storage_write_failed");
        assert!(status["last_failure_at_ms"].as_u64().unwrap() > 0);
        assert!(status["last_recovery_at_ms"].is_null());
        assert!(!status.to_string().contains("injected uncertain commit"));
        drop(observer);
        drop(access);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scoped_io_rejects_invalid_scope_and_excessive_observation_metadata() {
        let (root, access, scope) = fixture();
        assert!(matches!(
            access.consume(
                &work::MemoryBudget::new(100),
                b"{}",
                "bad",
                Policy {
                    base: 128_000,
                    window: None
                },
                TIMEOUT
            ),
            Err(Failure::Invalid)
        ));
        assert_eq!(access.jobs.status()["active"], 0);
        let decision = access.get().unwrap().status(&scope).unwrap();
        access.observe(
            &scope,
            vec![EvidenceObservation {
                source_id: "x".repeat(MAX_SOURCE_BYTES + 1),
                content_digest: "a".repeat(64),
                kind: "text".into(),
            }],
            &[],
            &decision,
        );
        assert_eq!(access.observer.status()["dropped"], 1);
        assert_eq!(access.observer.status()["queued"], 0);
        drop(access);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scoped_io_stalled_sqlite_returns_retryable_error_without_blocking_readiness_or_sending_inference(
    ) {
        use super::super::{dispatch_connection, Proxy};
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};

        fn send(proxy: &Arc<Proxy>, request: &[u8]) -> TcpStream {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            client
                .set_write_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            dispatch_connection(listener.accept().unwrap().0, Arc::clone(proxy), 2, |job| {
                std::thread::Builder::new().spawn(job).map(|_| ())
            });
            client.write_all(request).unwrap();
            client
        }
        fn response(mut socket: TcpStream) -> String {
            let mut response = String::new();
            socket.read_to_string(&mut response).unwrap();
            response
        }

        let (root, access, scope) = fixture();
        let blocker = rusqlite::Connection::open(root.join("context.sqlite3")).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let upstream = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let mut proxy = super::super::tests::test_proxy(128_000, 256_000);
        proxy.control = Arc::clone(&access);
        proxy.anthropic = format!("http://{}", upstream.local_addr().unwrap());
        proxy.openai = proxy.anthropic.clone();
        proxy.chatgpt = proxy.anthropic.clone();
        let proxy = Arc::new(proxy);
        let body = r#"{"messages":[{"role":"user","content":"hello"}]}"#;
        let request = format!("POST /v1/messages HTTP/1.1\r\nHost: localhost\r\nanthropic-version: 2023-06-01\r\nx-gobstopper-scope: {scope}\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        let started = Instant::now();
        let pending = send(&proxy, request.as_bytes());
        until(|| access.jobs.status()["active"] == 1);
        let ready = response(send(
            &proxy,
            b"GET /gobstopper/ready HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
        assert!(ready.starts_with("HTTP/1.1 200"), "{ready}");
        let failed = response(pending);
        assert!(failed.starts_with("HTTP/1.1 503"), "{failed}");
        assert!(failed.to_ascii_lowercase().contains("retry-after: 1"));
        assert!(failed.contains("gobstopper_scope_unavailable"));
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(proxy.inference.load(Ordering::Acquire), 0);
        upstream.set_nonblocking(true).unwrap();
        assert!(
            matches!(upstream.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        blocker.execute_batch("ROLLBACK").unwrap();
        drop(blocker);
        until(|| access.jobs.status()["active"] == 0 && proxy.memory.used() == 0);
        drop(proxy);
        drop(access);
        std::fs::remove_dir_all(root).unwrap();
    }
}
