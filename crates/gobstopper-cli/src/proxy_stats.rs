//! Optional legacy JSONL accounting must never wait on disk in request threads.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, OnceLock};

const QUEUE_LIMIT: usize = 1024;
const MAX_RECORD: usize = 128 * 1024;

#[derive(Default)]
struct State {
    prior: OnceLock<(u64, u64)>,
    loading: AtomicBool,
    available: AtomicBool,
    dropped: AtomicU64,
    failures: AtomicU64,
    history_incomplete: AtomicBool,
}

pub(super) struct StatsLog {
    pub(super) path: Option<PathBuf>,
    sender: Option<SyncSender<Vec<u8>>>,
    state: Arc<State>,
}

impl StatsLog {
    pub(super) fn open() -> Self {
        let path = match std::env::var_os("GOBSTOPPER_STATS_FILE") {
            Some(value) if value == "off" => None,
            Some(value) => Some(PathBuf::from(value)),
            None => std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
                .map(|p| p.join("gobstopper/proxy-stats.jsonl")),
        };
        Self::at(path)
    }

    pub(super) fn at(path: Option<PathBuf>) -> Self {
        let state = Arc::new(State::default());
        let Some(file) = path.as_ref().cloned() else {
            return Self {
                path,
                sender: None,
                state,
            };
        };
        state.loading.store(true, Ordering::Release);
        let (sender, receiver) = sync_channel(QUEUE_LIMIT);
        let worker_state = Arc::clone(&state);
        let spawned = std::thread::Builder::new()
            .name("gobstopper-stats".into())
            .spawn(move || write_loop(file, receiver, worker_state));
        if spawned.is_err() {
            state.loading.store(false, Ordering::Release);
            state.history_incomplete.store(true, Ordering::Release);
            state.failures.fetch_add(1, Ordering::Relaxed);
            return Self {
                path,
                sender: None,
                state,
            };
        }
        Self {
            path,
            sender: Some(sender),
            state,
        }
    }

    pub(super) fn enqueue(&self, value: &Value) {
        if self.path.is_none() {
            return;
        }
        let mut bytes = serde_json::to_vec(value).unwrap_or_default();
        if bytes.is_empty() || bytes.len() >= MAX_RECORD {
            self.state.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        bytes.push(b'\n');
        if self
            .sender
            .as_ref()
            .is_none_or(|s| s.try_send(bytes).is_err())
        {
            self.state.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(super) fn totals(&self, current_in: u64, current_out: u64) -> (u64, u64) {
        let (prior_in, prior_out) = self.state.prior.get().copied().unwrap_or_default();
        (
            prior_in.saturating_add(current_in),
            prior_out.saturating_add(current_out),
        )
    }

    pub(super) fn status(&self) -> Value {
        json!({
            "enabled": self.path.is_some(),
            "available": self.state.available.load(Ordering::Acquire),
            "loading_history": self.state.loading.load(Ordering::Acquire),
            "history_incomplete": self.state.history_incomplete.load(Ordering::Acquire),
            "dropped_events": self.state.dropped.load(Ordering::Relaxed),
            "write_failures": self.state.failures.load(Ordering::Relaxed),
            "pending_limit": QUEUE_LIMIT,
        })
    }
}

/// Scan a growing/possibly damaged file with bounded per-record memory. The
/// snapshot ends at the original file length so concurrent appends cannot make
/// startup scan forever or count this process's queued records twice.
fn history(reader: impl std::io::Read, state: &State) {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    let mut oversized = false;
    let mut total_in = 0u64;
    let mut total_out = 0u64;
    loop {
        let buffer = match reader.fill_buf() {
            Ok([]) => break,
            Err(_) => {
                state.history_incomplete.store(true, Ordering::Release);
                break;
            }
            Ok(buffer) => buffer,
        };
        let newline = buffer.iter().position(|b| *b == b'\n');
        let count = newline.map_or(buffer.len(), |n| n + 1);
        if !oversized {
            if line.len().saturating_add(count) > MAX_RECORD {
                oversized = true;
                state.history_incomplete.store(true, Ordering::Release);
                line.clear();
            } else {
                line.extend_from_slice(&buffer[..count]);
            }
        }
        reader.consume(count);
        if newline.is_some() {
            if !oversized {
                if let Ok(value) = serde_json::from_slice::<Value>(&line) {
                    if let (Some(input), Some(output)) = (
                        value["est_tokens_in"].as_u64(),
                        value["est_tokens_out"].as_u64(),
                    ) {
                        total_in = total_in.saturating_add(input);
                        total_out = total_out.saturating_add(output);
                    } else {
                        state.history_incomplete.store(true, Ordering::Release);
                    }
                } else {
                    state.history_incomplete.store(true, Ordering::Release);
                }
            }
            line.clear();
            oversized = false;
        }
    }
    if !line.is_empty() || oversized {
        state.history_incomplete.store(true, Ordering::Release);
    }
    let _ = state.prior.set((total_in, total_out));
}

fn write_loop(path: PathBuf, receiver: Receiver<Vec<u8>>, state: Arc<State>) {
    match std::fs::File::open(&path) {
        Ok(file) => match file.metadata() {
            Ok(meta) if meta.is_file() => {
                use std::io::Read;
                history(file.take(meta.len()), &state);
            }
            _ => state.history_incomplete.store(true, Ordering::Release),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => state.history_incomplete.store(true, Ordering::Release),
    }
    state.loading.store(false, Ordering::Release);
    let mut writer = None;
    for record in receiver {
        if writer.is_none() {
            writer = path.parent().and_then(|parent| {
                std::fs::create_dir_all(parent).ok()?;
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .read(true)
                    .append(true)
                    .open(&path)
                    .ok()?;
                let meta = file.metadata().ok()?;
                if !meta.is_file() {
                    return None;
                }
                // Preserve interrupted bytes as a separate malformed line so
                // the next complete event remains independently readable.
                if meta.len() > 0 {
                    use std::io::{Read, Seek, SeekFrom};
                    file.seek(SeekFrom::End(-1)).ok()?;
                    let mut last = [0];
                    file.read_exact(&mut last).ok()?;
                    if last[0] != b'\n' {
                        file.write_all(b"\n").ok()?;
                    }
                }
                Some(file)
            });
        }
        if let Some(file) = writer.as_mut() {
            if file.write_all(&record).is_ok() {
                state.available.store(true, Ordering::Release);
                continue;
            }
        }
        // A partial append has an uncertain result: never replay it. Fresh
        // records may recover later, with explicit loss counters for visibility.
        writer = None;
        state.available.store(false, Ordering::Release);
        state.failures.fetch_add(1, Ordering::Relaxed);
        state.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "gobstopper-stats-{}",
                crate::proxy_agent::unique_id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn blocked_writer_has_bounded_memory_and_does_not_wait_for_disk() {
        let (sender, receiver) = sync_channel(1);
        let log = StatsLog {
            path: Some("unused".into()),
            sender: Some(sender),
            state: Arc::default(),
        };
        let started = Instant::now();
        for _ in 0..1000 {
            log.enqueue(&json!({"est_tokens_in": 2}));
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(log.status()["dropped_events"], 999);
        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn malformed_oversized_and_unfinished_records_do_not_lose_later_history() {
        let mut bytes = vec![b'x'; MAX_RECORD * 2];
        bytes.extend_from_slice(
            b"\nnot json\n{\"est_tokens_in\":3,\"est_tokens_out\":2}\n{\"est_tokens_in\":100}",
        );
        let state = State::default();
        history(bytes.as_slice(), &state);
        assert_eq!(state.prior.get(), Some(&(3, 2)));
        assert!(state.history_incomplete.load(Ordering::Acquire));
    }

    #[test]
    fn startup_history_and_append_are_eventually_available() {
        let dir = TestDirectory::new();
        let path = dir.path().join("stats.jsonl");
        std::fs::write(&path, b"{\"est_tokens_in\":7,\"est_tokens_out\":4}\n").unwrap();
        let log = StatsLog::at(Some(path.clone()));
        log.enqueue(&json!({"est_tokens_in":3,"est_tokens_out":2}));
        let deadline = Instant::now() + Duration::from_secs(3);
        while log.status()["available"] != true && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(log.status()["available"], true);
        assert_eq!(log.status()["loading_history"], false);
        assert_eq!(log.totals(3, 2), (10, 6));
        assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 2);
    }

    #[test]
    fn disconnected_writer_and_overlarge_record_count_loss() {
        let (sender, receiver) = sync_channel(1);
        drop(receiver);
        let log = StatsLog {
            path: Some("unused".into()),
            sender: Some(sender),
            state: Arc::default(),
        };
        log.enqueue(&json!({"x": "x".repeat(MAX_RECORD)}));
        log.enqueue(&json!({"est_tokens_in":1}));
        assert_eq!(log.status()["dropped_events"], 2);
        assert_eq!(log.totals(u64::MAX, 1), (u64::MAX, 1));
    }

    #[test]
    fn failed_storage_recovers_without_replaying_uncertain_records() {
        let dir = TestDirectory::new();
        let parent = dir.path().join("blocked");
        std::fs::write(&parent, b"file blocking directory").unwrap();
        let path = parent.join("stats.jsonl");
        let log = StatsLog::at(Some(path.clone()));
        log.enqueue(&json!({"est_tokens_in":5}));
        let deadline = Instant::now() + Duration::from_secs(3);
        while log.status()["write_failures"] == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(log.status()["write_failures"], 1);
        std::fs::remove_file(&parent).unwrap();
        std::fs::create_dir(&parent).unwrap();
        std::fs::write(&path, b"{\"partial\":").unwrap();
        log.enqueue(&json!({"est_tokens_in":9}));
        while log.status()["available"] != true && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(log.status()["available"], true);
        let bytes = std::fs::read_to_string(path).unwrap();
        let records: Vec<Value> = bytes
            .lines()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect();
        assert_eq!(records, vec![json!({"est_tokens_in":9})]);
        assert_eq!(log.status()["dropped_events"], 1);
    }
    #[test]
    fn maximum_accepted_record_is_counted_after_restart() {
        let (sender, receiver) = sync_channel(1);
        let log = StatsLog {
            path: Some("unused".into()),
            sender: Some(sender),
            state: Arc::default(),
        };
        let mut value = json!({"est_tokens_in":7,"est_tokens_out":4,"padding":""});
        let overhead = serde_json::to_vec(&value).unwrap().len();
        value["padding"] = json!("x".repeat(MAX_RECORD - overhead - 1));
        log.enqueue(&value);
        let bytes = receiver.try_recv().unwrap();
        assert_eq!(bytes.len(), MAX_RECORD);
        let state = State::default();
        history(bytes.as_slice(), &state);
        assert_eq!(state.prior.get(), Some(&(7, 4)));
        assert!(!state.history_incomplete.load(Ordering::Acquire));
        value["padding"] = json!("x".repeat(MAX_RECORD - overhead));
        log.enqueue(&value);
        assert!(receiver.try_recv().is_err());
        assert_eq!(log.status()["dropped_events"], 1);
    }
}
