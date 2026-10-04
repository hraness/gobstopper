//! Diagnostic output cannot hold a forwarding or control thread on stderr I/O.
use crate::proxy_observations::{FailureCode, FailureHistory};
use serde_json::{json, Value};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, OnceLock};

const QUEUE_LIMIT: usize = 256;
const LINE_LIMIT: usize = 16 * 1024;

#[derive(Default)]
struct State {
    dropped: AtomicU64,
    failures: AtomicU64,
    written: AtomicU64,
    history: FailureHistory,
}

struct Logger {
    sender: Option<SyncSender<Vec<u8>>>,
    state: Arc<State>,
}

impl Logger {
    fn start(mut writer: impl Write + Send + 'static) -> Self {
        let (sender, receiver) = sync_channel::<Vec<u8>>(QUEUE_LIMIT);
        let state = Arc::new(State::default());
        let worker_state = Arc::clone(&state);
        let worker = std::thread::Builder::new()
            .name("proxy-diagnostics".into())
            .spawn(move || {
                for bytes in receiver {
                    // Never retry an uncertain write: a prefix may already be present.
                    if writer
                        .write_all(&bytes)
                        .and_then(|()| writer.flush())
                        .is_err()
                    {
                        worker_state.history.record(FailureCode::StorageWrite);
                        worker_state.failures.fetch_add(1, Ordering::Release);
                    } else {
                        worker_state.history.recover();
                        worker_state.written.fetch_add(1, Ordering::Release);
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

    fn write(&self, message: &str) {
        if message.len() >= LINE_LIMIT {
            self.state.history.record(FailureCode::RecordLimit);
            self.state.dropped.fetch_add(1, Ordering::Release);
            return;
        }
        let mut bytes = Vec::with_capacity(message.len() + 1);
        bytes.extend_from_slice(message.as_bytes());
        bytes.push(b'\n');
        let failure = match &self.sender {
            Some(sender) => match sender.try_send(bytes) {
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
            self.state.dropped.fetch_add(1, Ordering::Release);
        }
    }

    fn status(&self) -> Value {
        let dropped_lines = self.state.dropped.load(Ordering::Acquire);
        let write_failures = self.state.failures.load(Ordering::Acquire);
        let written_lines = self.state.written.load(Ordering::Acquire);
        let (last_failure_at_ms, last_failure_code, last_recovery_at_ms) =
            self.state.history.fields();
        json!({
            "dropped_lines": dropped_lines,
            "write_failures": write_failures,
            "written_lines": written_lines,
            "queue_limit": QUEUE_LIMIT,
            "last_failure_at_ms": last_failure_at_ms,
            "last_failure_code": last_failure_code,
            "last_recovery_at_ms": last_recovery_at_ms,
        })
    }
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

pub(super) fn startup(port: u16) {
    // A full stdout pipe or stalled launchd log must not hold the accept loop.
    // Preserve the banner on stdout for callers that discover an ephemeral port.
    let _ = std::thread::Builder::new()
        .name("proxy-startup-output".into())
        .spawn(move || {
            let mut stdout = std::io::stdout();
            let _ = writeln!(
                stdout,
                "gobstopper proxy listening on http://127.0.0.1:{port}"
            )
            .and_then(|()| stdout.flush());
            crate::ux::next_hint(&format!(
            "export ANTHROPIC_BASE_URL=http://127.0.0.1:{port} in the shell that starts Claude Code"
        ));
        });
}

pub(super) fn write(message: &str) {
    LOGGER
        .get_or_init(|| Logger::start(std::io::stderr()))
        .write(message);
}

pub(super) fn status() -> Value {
    LOGGER
        .get()
        .map_or_else(|| json!({"started": false}), Logger::status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    struct BlockOnce {
        entered: mpsc::Sender<()>,
        release: Option<mpsc::Receiver<()>>,
    }
    impl Write for BlockOnce {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(release) = self.release.take() {
                self.entered.send(()).unwrap();
                release.recv_timeout(Duration::from_secs(10)).unwrap();
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn stalled_stderr_and_full_queue_never_hold_the_caller() {
        let (entered, blocked) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let logger = Logger::start(BlockOnce {
            entered,
            release: Some(resume),
        });
        logger.write("first");
        blocked.recv_timeout(Duration::from_secs(3)).unwrap();
        let started = Instant::now();
        for _ in 0..QUEUE_LIMIT + 3 {
            logger.write("next");
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(logger.status()["dropped_lines"], 3);
        release.send(()).unwrap();
        while logger.state.written.load(Ordering::Acquire) < QUEUE_LIMIT as u64 + 1 {
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    struct FailOnce(bool);

    impl Write for FailOnce {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if !self.0 {
                self.0 = true;
                return Err(std::io::Error::other("private writer failure"));
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn diagnostics_distinguish_historical_failure_from_recovery() {
        let logger = Logger::start(FailOnce(false));
        assert!(logger.status()["last_failure_at_ms"].is_null());
        logger.write("first");
        let deadline = Instant::now() + Duration::from_secs(5);
        while logger.status()["last_failure_at_ms"].is_null() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        let failed = logger.status();
        assert_eq!(failed["last_failure_code"], "storage_write_failed");
        assert!(!failed.to_string().contains("private writer failure"));
        logger.write("second");
        while logger.status()["last_recovery_at_ms"].is_null() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        let recovered = logger.status();
        assert_eq!(recovered["write_failures"], 1);
        assert_eq!(recovered["written_lines"], 1);
        assert!(
            recovered["last_recovery_at_ms"].as_u64().unwrap()
                >= failed["last_failure_at_ms"].as_u64().unwrap()
        );
    }

    #[test]
    fn oversized_lines_are_dropped_before_entering_the_queue() {
        let logger = Logger::start(std::io::sink());
        logger.write(&"x".repeat(LINE_LIMIT));
        assert_eq!(logger.status()["dropped_lines"], 1);
        assert_eq!(logger.status()["written_lines"], 0);
    }
}
