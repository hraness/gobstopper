//! Best-effort, content-free collection. Transport never depends on telemetry.
use crate::session_data::{
    self as data, Envelope, Event, FinishReason, Identity, OpaqueId, Outcome, Quantity,
    RequestTimings, Source, SourceKind, Store, Usage,
};
use gobstopper_adapters::request::Dialect;
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering},
    Arc, Mutex, OnceLock, TryLockError,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAX_PENDING_EVENTS: usize = 1024;
const MAX_FLUSH_EVENTS: usize = 256;
const PROXY_STORE_WAIT: Duration = Duration::from_millis(25);

#[derive(Clone, Copy)]
#[repr(u8)]
pub(crate) enum FailureCode {
    IdentityUnavailable = 1,
    WorkerSpawn = 2,
    WorkerPanic = 3,
    StorageOpen = 4,
    StorageWrite = 5,
    StorageRead = 6,
    QueueContended = 7,
    QueueFull = 8,
    WorkerStopped = 9,
    InvalidEvent = 10,
    RecordLimit = 11,
}

#[derive(Default)]
pub(crate) struct FailureHistory {
    failure: AtomicU64,
    recovery: AtomicU64,
    recovery_pending: AtomicBool,
}

impl FailureHistory {
    pub(crate) fn record(&self, code: FailureCode) {
        let timestamp = data::now_ms().clamp(1, u64::MAX >> 8);
        let _ = self
            .failure
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |prior| {
                Some((timestamp.max(prior >> 8) << 8) | code as u64)
            });
        self.recovery_pending.store(true, Ordering::Release);
    }
    pub(crate) fn recover(&self) {
        if self.recovery_pending.swap(false, Ordering::AcqRel) {
            let failure = self.failure.load(Ordering::Acquire);
            self.recovery
                .fetch_max(data::now_ms().max(failure >> 8), Ordering::Release);
        }
    }
    pub(crate) fn fields(&self) -> (Option<u64>, Option<&'static str>, Option<u64>) {
        let failure = self.failure.load(Ordering::Acquire);
        let recovery = self.recovery.load(Ordering::Acquire);
        let code = match failure as u8 {
            1 => Some("identity_unavailable"),
            2 => Some("worker_spawn_failed"),
            3 => Some("worker_panicked"),
            4 => Some("storage_open_failed"),
            5 => Some("storage_write_failed"),
            6 => Some("storage_read_failed"),
            7 => Some("queue_contended"),
            8 => Some("queue_full"),
            9 => Some("worker_stopped"),
            10 => Some("invalid_event"),
            11 => Some("record_limit"),
            _ => None,
        };
        (
            (failure != 0).then_some(failure >> 8),
            code,
            (recovery != 0).then_some(recovery),
        )
    }
}

struct RecorderState {
    store: Option<Store>,
    path: Option<PathBuf>,
    next_retry: Option<Instant>,
    backoff: Duration,
}

struct RecorderShared {
    pending: Mutex<VecDeque<Envelope>>,
    pending_count: AtomicUsize,
    namespace: OnceLock<data::IdentityNamespace>,
    tool_namespace: Option<data::IdentityNamespace>,
    source: Option<Source>,
    runtime: Option<OpaqueId>,
    enabled: bool,
    failures: AtomicU64,
    dropped: AtomicU64,
    recoveries: AtomicU64,
    history: FailureHistory,
    // 0: disabled/unopened, 1: available, 2: degraded. Status never waits on I/O.
    health: AtomicU8,
    wake: Arc<RecorderWake>,
    #[cfg(test)]
    hooks: Mutex<StorageHooks>,
}
#[cfg(test)]
#[derive(Default)]
struct StorageHooks {
    open: Option<Box<dyn FnOnce() + Send>>,
    append: Option<Box<dyn FnOnce() + Send>>,
}
pub struct Recorder {
    shared: Arc<RecorderShared>,
    worker: Option<RecorderWorker>,
}
#[derive(Default)]
struct RecorderWake {
    stopped: AtomicBool,
    notified: AtomicBool,
    thread: OnceLock<thread::Thread>,
}
impl RecorderWake {
    fn notify(&self) {
        self.notified.store(true, Ordering::Release);
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
    }
}
struct RecorderWorker {
    wake: Arc<RecorderWake>,
    thread: Option<JoinHandle<()>>,
}
impl RecorderWorker {
    fn start(shared: Arc<RecorderShared>, mut state: RecorderState) -> std::io::Result<Self> {
        let wake = Arc::clone(&shared.wake);
        let thread = thread::Builder::new()
            .name("gobstopper-session-data".into())
            .spawn(move || {
                let _ = shared.wake.thread.set(thread::current());
                // The SQLite connection stays on this stack, including during
                // errors, shutdown and unwind. Shared-state destruction cannot
                // move database close/checkpoint work to a request thread.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    loop {
                        if shared.wake.stopped.load(Ordering::Acquire) {
                            // Best effort only; never join this writer from Drop.
                            for _ in 0..MAX_PENDING_EVENTS.div_ceil(MAX_FLUSH_EVENTS) {
                                let before = shared.pending_count.load(Ordering::Acquire);
                                if before == 0 {
                                    break;
                                }
                                shared.flush(&mut state, true);
                                if shared.pending_count.load(Ordering::Acquire) >= before {
                                    break;
                                }
                            }
                            break;
                        }
                        shared.flush(&mut state, false);
                        let delay = shared.next_flush_delay(&state);
                        if shared.wake.stopped.load(Ordering::Acquire)
                            || shared.wake.notified.swap(false, Ordering::AcqRel)
                        {
                            continue;
                        }
                        match delay {
                            Some(delay) => thread::park_timeout(delay),
                            None => thread::park(),
                        }
                    }
                }));
                if result.is_err() {
                    shared.history.record(FailureCode::WorkerPanic);
                    shared.health.store(2, Ordering::Release);
                    shared.failures.fetch_add(1, Ordering::Release);
                }
                shared.wake.stopped.store(true, Ordering::Release);
                let mut pending = shared.pending.lock().unwrap_or_else(|e| e.into_inner());
                let unsaved = pending.len();
                pending.clear();
                shared.pending_count.store(0, Ordering::Release);
                shared.dropped.fetch_add(unsaved as u64, Ordering::Relaxed);
                drop(pending);
                drop(state);
            })?;
        Ok(Self {
            wake,
            thread: Some(thread),
        })
    }
    fn is_running(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
    }
}
impl Drop for RecorderWorker {
    fn drop(&mut self) {
        self.wake.stopped.store(true, Ordering::Release);
        self.wake.notify();
        // Dropping JoinHandle detaches; a blocked disk cannot hold shutdown.
        drop(self.thread.take());
    }
}
impl Recorder {
    pub fn disabled() -> Self {
        Self::open_path(false, None)
    }
    pub fn open(enabled: bool) -> Self {
        if !enabled {
            return Self::disabled();
        }
        Self::open_path(enabled, data::default_path().ok())
    }
    fn open_path(enabled: bool, path: Option<PathBuf>) -> Self {
        Self::open_impl(
            enabled,
            path,
            #[cfg(test)]
            StorageHooks::default(),
        )
    }
    fn open_impl(enabled: bool, path: Option<PathBuf>, #[cfg(test)] hooks: StorageHooks) -> Self {
        let setup = if enabled {
            OpaqueId::random()
                .and_then(|runtime| Ok((runtime, data::IdentityNamespace::random()?)))
                .ok()
        } else {
            None
        };
        let (runtime, tool_namespace) = match setup {
            Some((runtime, key)) => (Some(runtime), Some(key)),
            None => (None, None),
        };
        let source = runtime.as_ref().map(|id| Source {
            kind: SourceKind::LiveProxy,
            id: id.clone(),
            profile: "gobstopper-proxy-v2".into(),
        });
        let unavailable = enabled && source.is_none();
        let history = FailureHistory::default();
        if unavailable {
            history.record(FailureCode::IdentityUnavailable);
        }
        let shared = Arc::new(RecorderShared {
            pending: Mutex::new(VecDeque::new()),
            pending_count: AtomicUsize::new(0),
            namespace: OnceLock::new(),
            tool_namespace,
            source,
            runtime,
            enabled,
            failures: AtomicU64::new(u64::from(unavailable)),
            dropped: AtomicU64::new(0),
            recoveries: AtomicU64::new(0),
            history,
            health: AtomicU8::new(if unavailable { 2 } else { 0 }),
            wake: Arc::new(RecorderWake::default()),
            #[cfg(test)]
            hooks: Mutex::new(hooks),
        });
        let worker = if shared.source.is_some() {
            match RecorderWorker::start(
                Arc::clone(&shared),
                RecorderState {
                    store: None,
                    path,
                    next_retry: None,
                    backoff: Duration::from_secs(1),
                },
            ) {
                Ok(worker) => Some(worker),
                Err(_) => {
                    shared.history.record(FailureCode::WorkerSpawn);
                    shared.health.store(2, Ordering::Release);
                    shared.failures.fetch_add(1, Ordering::Release);
                    shared.wake.stopped.store(true, Ordering::Release);
                    None
                }
            }
        } else {
            None
        };
        Self { shared, worker }
    }
    pub fn status(&self) -> Value {
        let write_failures = self.shared.failures.load(Ordering::Acquire);
        let health = self.shared.health.load(Ordering::Acquire);
        let pending = self.shared.pending_count.load(Ordering::Acquire);
        let running = self.worker.as_ref().is_some_and(RecorderWorker::is_running);
        let (last_failure_at_ms, last_failure_code, last_recovery_at_ms) =
            self.shared.history.fields();
        json!({"enabled":self.shared.enabled,"available":health == 1 && running,
            "degraded":health == 2 || (self.shared.enabled && !running),
            "background_retry":running,"write_failures":write_failures,
            "recoveries":self.shared.recoveries.load(Ordering::Relaxed),"pending_events":pending,
            "dropped_events":self.shared.dropped.load(Ordering::Relaxed),"pending_limit":MAX_PENDING_EVENTS,
            "runtime_id":self.shared.runtime,"content_recorded":false,
            "last_failure_at_ms":last_failure_at_ms,"last_failure_code":last_failure_code,
            "last_recovery_at_ms":last_recovery_at_ms})
    }
    pub fn request_id(&self) -> Option<OpaqueId> {
        self.shared
            .source
            .as_ref()
            .and_then(|_| OpaqueId::random().ok())
    }
    /// Provider-native session ID only. An unopened store has no persistent
    /// identity namespace, so early requests retain an explicitly unknown session.
    pub fn session_id(&self, provider: &str, native: Option<&str>) -> Option<OpaqueId> {
        if !matches!(provider, "codex" | "claude_code") {
            return None;
        }
        let native = native
            .filter(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))?;
        let key = serde_json::to_string(&(provider, native)).ok()?;
        self.shared
            .namespace
            .get()
            .map(|namespace| namespace.opaque("native-session", &key))
    }
    fn emit(&self, identity: Identity, event: Event) {
        self.emit_many(std::iter::once((identity, event)));
    }
    fn emit_many(&self, events: impl IntoIterator<Item = (Identity, Event)>) {
        self.emit_many_at(
            events
                .into_iter()
                .map(|(identity, event)| (identity, event, None)),
        );
    }
    fn emit_many_at(&self, events: impl IntoIterator<Item = (Identity, Event, Option<u64>)>) {
        let Some(source) = &self.shared.source else {
            return;
        };
        let mut pending = match self.shared.pending.try_lock() {
            Ok(pending) => pending,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => {
                self.shared
                    .dropped
                    .fetch_add(events.into_iter().count() as u64, Ordering::Relaxed);
                self.shared.history.record(FailureCode::QueueContended);
                return;
            }
        };
        if self.shared.wake.stopped.load(Ordering::Acquire) {
            self.shared
                .dropped
                .fetch_add(events.into_iter().count() as u64, Ordering::Relaxed);
            self.shared.history.record(FailureCode::WorkerStopped);
            return;
        }
        for (identity, event, observed_at_ms) in events {
            let envelope =
                Envelope::new(source.clone(), identity, event).and_then(|mut envelope| {
                    if let Some(observed_at_ms) = observed_at_ms {
                        envelope.observed_at_ms = observed_at_ms;
                    }
                    envelope.validate()?;
                    Ok(envelope)
                });
            match envelope {
                Ok(envelope) => {
                    if pending.len() < MAX_PENDING_EVENTS {
                        pending.push_back(envelope);
                        self.shared.pending_count.fetch_add(1, Ordering::Release);
                    } else {
                        self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                        self.shared.history.record(FailureCode::QueueFull);
                    }
                }
                Err(_) => {
                    self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                    self.shared.history.record(FailureCode::InvalidEvent);
                    self.shared.failures.fetch_add(1, Ordering::Release);
                }
            }
        }
        drop(pending);
        self.shared.wake.notify();
    }
    pub fn start<'a>(
        &'a self,
        request: Option<OpaqueId>,
        session: Option<OpaqueId>,
        dialect: Dialect,
        model: Option<&str>,
    ) -> Attempt<'a> {
        self.start_at(request, session, dialect, model, Instant::now())
    }
    pub fn start_at<'a>(
        &'a self,
        request: Option<OpaqueId>,
        session: Option<OpaqueId>,
        dialect: Dialect,
        model: Option<&str>,
        started: Instant,
    ) -> Attempt<'a> {
        let identity = request.and_then(|request| {
            OpaqueId::random().ok().map(|attempt| Identity {
                runtime_id: self.shared.runtime.clone(),
                session_id: session,
                request_id: Some(request),
                attempt_id: Some(attempt),
                tool_id: None,
            })
        });
        if let Some(identity) = &identity {
            let provider = match dialect {
                Dialect::Anthropic => "anthropic",
                Dialect::Responses => "responses",
                Dialect::ChatCompletions => "chat_completions",
            };
            let observed_at_ms = data::now_ms().saturating_sub(elapsed_ms(started.elapsed()));
            self.emit_many_at(std::iter::once((
                identity.clone(),
                Event::RequestStarted {
                    provider: provider.into(),
                    model: model.filter(|m| data::safe_label(m)).map(String::from),
                },
                Some(observed_at_ms),
            )));
        }
        Attempt {
            recorder: self,
            identity,
            started,
            timings: None,
            finished: false,
        }
    }
}
impl RecorderShared {
    fn next_flush_delay(&self, state: &RecorderState) -> Option<Duration> {
        if state.store.is_some() && self.pending_count.load(Ordering::Acquire) == 0 {
            return None;
        }
        Some(state.next_retry.map_or(Duration::ZERO, |retry| {
            retry.saturating_duration_since(Instant::now())
        }))
    }
    /// Only the background owner calls this. Failed batches keep stable event
    /// IDs for idempotent retry, with backoff even when requests keep arriving.
    fn flush(&self, state: &mut RecorderState, shutdown: bool) {
        if self.source.is_none() {
            return;
        }
        if !shutdown && state.next_retry.is_some_and(|time| Instant::now() < time) {
            return;
        }
        let result = (|| -> Result<(), FailureCode> {
            if state.store.is_none() {
                #[cfg(test)]
                {
                    let hook = self.hooks.lock().unwrap().open.take();
                    if let Some(hook) = hook {
                        hook();
                    }
                }
                let path = match &state.path {
                    Some(path) => path.clone(),
                    None => data::default_path().map_err(|_| FailureCode::StorageOpen)?,
                };
                let store = Store::open_with_busy_timeout(&path, PROXY_STORE_WAIT)
                    .map_err(|_| FailureCode::StorageOpen)?;
                let _ = self.namespace.set(store.identity_namespace());
                state.store = Some(store);
            }
            let batch: Vec<_> = self
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .take(MAX_FLUSH_EVENTS)
                .cloned()
                .collect();
            if !batch.is_empty() {
                #[cfg(test)]
                {
                    let hook = self.hooks.lock().unwrap().append.take();
                    if let Some(hook) = hook {
                        hook();
                    }
                }
                state
                    .store
                    .as_mut()
                    .expect("opened store")
                    .append_batch(&batch)
                    .map_err(|_| FailureCode::StorageWrite)?;
                let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
                pending.drain(..batch.len());
                self.pending_count.fetch_sub(batch.len(), Ordering::Release);
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                if self.health.load(Ordering::Relaxed) == 2 {
                    self.recoveries.fetch_add(1, Ordering::Relaxed);
                }
                state.next_retry = None;
                state.backoff = Duration::from_secs(1);
                self.history.recover();
                self.health.store(1, Ordering::Release);
            }
            Err(code) => {
                self.history.record(code);
                state.next_retry = Some(Instant::now() + state.backoff);
                state.backoff = (state.backoff * 2).min(Duration::from_secs(60));
                self.health.store(2, Ordering::Release);
                self.failures.fetch_add(1, Ordering::Release);
            }
        }
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        // Stop is asynchronous. The writer retains its SQLite handle and tries
        // the finite pending queue when storage becomes responsive.
        drop(self.worker.take());
    }
}
fn elapsed_ms(duration: Duration) -> u64 {
    duration.as_millis().min(31 * 86_400_000) as u64
}

pub struct Attempt<'a> {
    recorder: &'a Recorder,
    identity: Option<Identity>,
    pub started: Instant,
    timings: Option<RequestTimings>,
    finished: bool,
}
impl Attempt<'_> {
    pub fn begin_upstream(&mut self) -> Instant {
        let started = Instant::now();
        self.timings = Some(RequestTimings {
            preparation_ms: elapsed_ms(started.saturating_duration_since(self.started)),
            upstream_headers_ms: 0,
            transform_ms: None,
        });
        started
    }
    pub fn end_upstream(&mut self, started: Instant) {
        if let Some(timings) = &mut self.timings {
            timings.upstream_headers_ms = elapsed_ms(started.elapsed());
        }
    }
    pub fn context(&self, event: Event) {
        if let Some(identity) = &self.identity {
            self.recorder.emit(identity.clone(), event);
        }
    }
    pub fn finish(
        &mut self,
        outcome: Outcome,
        status: Option<u16>,
        metrics: Option<&StreamMetrics>,
    ) {
        let reason = match outcome {
            Outcome::Success => FinishReason::Completed,
            Outcome::Error => FinishReason::ProviderError,
            Outcome::Refused => FinishReason::ProviderRefused,
            Outcome::Cancelled => FinishReason::ClientDisconnected,
            Outcome::Timeout => FinishReason::UpstreamTimeout,
            Outcome::Interrupted => FinishReason::ProxyInterrupted,
            Outcome::Unknown => FinishReason::ParserUncertain,
        };
        self.finish_recorded(outcome, reason, status, metrics);
    }
    pub fn finish_reason(
        &mut self,
        reason: FinishReason,
        status: Option<u16>,
        metrics: Option<&StreamMetrics>,
    ) {
        self.finish_recorded(reason.outcome(), reason, status, metrics);
    }
    fn finish_recorded(
        &mut self,
        outcome: Outcome,
        reason: FinishReason,
        status: Option<u16>,
        metrics: Option<&StreamMetrics>,
    ) {
        if self.finished {
            return;
        }
        self.finished = true;
        if let Some(identity) = &self.identity {
            let duration_ms = elapsed_ms(self.started.elapsed());
            let timings = self.timings.map(|mut timings| {
                timings.preparation_ms = timings.preparation_ms.min(duration_ms);
                timings.upstream_headers_ms = timings
                    .upstream_headers_ms
                    .min(duration_ms.saturating_sub(timings.preparation_ms));
                timings.transform_ms = timings.transform_ms.map(|n| n.min(timings.preparation_ms));
                timings
            });
            let mut events = vec![(
                identity.clone(),
                Event::RequestFinished {
                    outcome,
                    http_status: status,
                    duration_ms: Some(duration_ms),
                    first_output_ms: metrics
                        .and_then(|m| m.first_output_ms)
                        .map(|n| n.min(duration_ms)),
                    usage: metrics.and_then(|m| m.usage.clone()),
                    reason: Some(reason),
                    timings,
                    // Provider usage does not supply a matching generation interval.
                    generation: None,
                },
            )];
            if let (Some(metrics), Some(namespace), Some(attempt)) = (
                metrics,
                self.recorder.shared.tool_namespace.as_ref(),
                identity.attempt_id.as_ref(),
            ) {
                for (call_id, name) in metrics.observed_tools() {
                    let mut tool_identity = identity.clone();
                    tool_identity.tool_id =
                        Some(namespace.opaque("proxy-tool", &format!("{}:{call_id}", attempt.0)));
                    events.push((
                        tool_identity,
                        Event::ToolObserved {
                            tool_name: name,
                            stage: data::ToolStage::Requested,
                            outcome: Outcome::Unknown,
                        },
                    ));
                }
                self.recorder
                    .shared
                    .dropped
                    .fetch_add(metrics.dropped_tools, Ordering::Relaxed);
            }
            self.recorder.emit_many(events);
        }
    }
}
impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        self.finish(Outcome::Interrupted, None, None);
    }
}

/// Incremental SSE metadata parser. Stores at most one bounded event, discards
/// content immediately, and reads final usage even after multi-megabyte output.
pub struct StreamMetrics {
    dialect: Dialect,
    event_stream: bool,
    buffer: Vec<u8>,
    discard: bool,
    started: Instant,
    pub first_output_ms: Option<u64>,
    pub usage: Option<Usage>,
    pub terminal: bool,
    pub failed: bool,
    pub refused: bool,
    pub uncertain: bool,
    protocol_observed: bool,
    tools: BTreeMap<String, Option<String>>,
    chat_tools: BTreeMap<(u64, u64), ChatTool>,
    dropped_tools: u64,
}
#[derive(Default)]
struct ChatTool {
    id: Option<String>,
    name: String,
    invalid_name: bool,
}
const MAX_OBSERVED_TOOLS: usize = 128;
impl StreamMetrics {
    pub fn new(dialect: Dialect, event_stream: bool, started: Instant) -> Self {
        Self {
            dialect,
            event_stream,
            buffer: Vec::new(),
            discard: false,
            started,
            first_output_ms: None,
            usage: None,
            terminal: false,
            failed: false,
            refused: false,
            uncertain: false,
            protocol_observed: false,
            tools: BTreeMap::new(),
            chat_tools: BTreeMap::new(),
            dropped_tools: 0,
        }
    }
    pub fn observe(&mut self, bytes: &[u8]) {
        const LIMIT: usize = 4 * 1024 * 1024;
        if !self.event_stream {
            if self.buffer.len().saturating_add(bytes.len()) > LIMIT {
                self.discard = true;
                self.uncertain = true;
                self.buffer.clear();
            }
            if !self.discard {
                self.buffer.extend_from_slice(bytes);
            }
            return;
        }
        // SSE data lines may be split anywhere by the network. Multiline events
        // are reassembled according to SSE; a large event is skipped as a unit.
        for byte in bytes {
            if self.discard {
                self.buffer.push(*byte);
                if self.buffer.len() > 4 {
                    self.buffer.remove(0);
                }
                if self.buffer.ends_with(b"\n\n") || self.buffer.ends_with(b"\r\n\r\n") {
                    self.discard = false;
                    self.buffer.clear();
                }
                continue;
            }
            self.buffer.push(*byte);
            if self.buffer.ends_with(b"\n\n") || self.buffer.ends_with(b"\r\n\r\n") {
                let frame = std::mem::take(&mut self.buffer);
                let lines: Vec<&[u8]> = frame
                    .split(|b| *b == b'\n')
                    .filter_map(|line| {
                        line.strip_prefix(b"data:")
                            .map(|line| line.strip_prefix(b" ").unwrap_or(line).trim_ascii_end())
                    })
                    .collect();
                let data = lines.join(&b'\n');
                if data == b"[DONE]" {
                    if self.dialect == Dialect::ChatCompletions {
                        self.terminal = true;
                        self.protocol_observed = true;
                    } else {
                        self.uncertain = true;
                    }
                    continue;
                }
                if !data.is_empty() {
                    match serde_json::from_slice::<Value>(&data) {
                        Ok(event) => self.event(&event),
                        Err(_) => self.uncertain = true,
                    }
                }
            } else if self.buffer.len() > LIMIT {
                // Keep the possible delimiter prefix across the size boundary.
                // Otherwise a newline at LIMIT+1 can consume the next usage frame.
                self.buffer = self.buffer.split_off(self.buffer.len().saturating_sub(3));
                self.discard = true;
                self.uncertain = true;
            }
        }
    }
    pub fn finish_json(&mut self) {
        if !self.event_stream && !self.discard {
            match serde_json::from_slice::<Value>(&self.buffer) {
                Ok(value) => self.event(&value),
                Err(_) => self.uncertain = true,
            }
        } else if self.event_stream && (!self.buffer.trim_ascii().is_empty() || self.discard) {
            self.uncertain = true;
        }
        self.buffer.clear();
    }
    pub fn completion_reason(&self) -> FinishReason {
        if self.failed {
            FinishReason::ProviderError
        } else if self.refused {
            FinishReason::ProviderRefused
        } else if self.uncertain {
            FinishReason::ParserUncertain
        } else if self.terminal {
            FinishReason::Completed
        } else if self.protocol_observed {
            FinishReason::MissingTerminal
        } else {
            FinishReason::ParserUncertain
        }
    }
    fn event(&mut self, event: &Value) {
        if !event.is_object() {
            self.uncertain = true;
            return;
        }
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        self.protocol_observed |= match self.dialect {
            Dialect::Anthropic => matches!(
                kind,
                "message"
                    | "message_start"
                    | "message_delta"
                    | "message_stop"
                    | "content_block_start"
                    | "content_block_delta"
                    | "content_block_stop"
                    | "ping"
                    | "error"
            ),
            Dialect::Responses => {
                kind.starts_with("response.")
                    || kind == "error"
                    || event.get("object").and_then(Value::as_str) == Some("response")
            }
            Dialect::ChatCompletions => {
                event.get("choices").is_some_and(Value::is_array)
                    || event
                        .get("object")
                        .and_then(Value::as_str)
                        .is_some_and(|s| matches!(s, "chat.completion" | "chat.completion.chunk"))
            }
        };
        let usage = match self.dialect {
            Dialect::Anthropic => event
                .pointer("/message/usage")
                .or_else(|| event.get("usage")),
            Dialect::Responses => event
                .pointer("/response/usage")
                .or_else(|| event.get("usage")),
            Dialect::ChatCompletions => event.get("usage"),
        };
        if let Some(usage) = usage {
            self.merge_usage(usage);
            if self.dialect == Dialect::Anthropic && kind == "message_start" {
                // message_start's output counter is provisional; an interrupted
                // stream must not turn that initial zero into completed usage.
                if let Some(usage) = &mut self.usage {
                    usage.output_tokens = None;
                }
                if self.usage.as_ref() == Some(&Usage::default()) {
                    self.usage = None;
                }
            }
        }
        let nonempty = |v: Option<&Value>| v.and_then(Value::as_str).is_some_and(|s| !s.is_empty());
        self.refused |=
            match self.dialect {
                Dialect::Anthropic => {
                    event
                        .get("stop_reason")
                        .or_else(|| event.pointer("/delta/stop_reason"))
                        .and_then(Value::as_str)
                        == Some("refusal")
                }
                Dialect::Responses => {
                    (matches!(kind, "response.refusal.delta" | "response.refusal.done")
                        && (nonempty(event.get("delta")) || nonempty(event.get("refusal"))))
                        || event
                            .pointer("/response/output")
                            .or_else(|| event.get("output"))
                            .and_then(Value::as_array)
                            .is_some_and(|output| {
                                output.iter().any(|item| {
                                    item.get("content").and_then(Value::as_array).is_some_and(
                                        |blocks| {
                                            blocks.iter().any(|block| {
                                                block.get("type").and_then(Value::as_str)
                                                    == Some("refusal")
                                            })
                                        },
                                    )
                                })
                            })
                }
                Dialect::ChatCompletions => event
                    .get("choices")
                    .and_then(Value::as_array)
                    .is_some_and(|choices| {
                        choices.iter().any(|choice| {
                            choice.get("finish_reason").and_then(Value::as_str)
                                == Some("content_filter")
                                || nonempty(choice.pointer("/delta/refusal"))
                                || nonempty(choice.pointer("/message/refusal"))
                        })
                    }),
            };
        let output =
            match self.dialect {
                Dialect::Anthropic => {
                    kind == "content_block_delta"
                        && ["text", "thinking", "partial_json"]
                            .iter()
                            .any(|key| nonempty(event.get("delta").and_then(|v| v.get(key))))
                }
                Dialect::Responses => {
                    matches!(
                        kind,
                        "response.output_text.delta"
                            | "response.reasoning_text.delta"
                            | "response.reasoning_summary_text.delta"
                            | "response.function_call_arguments.delta"
                            | "response.custom_tool_call_input.delta"
                    ) && nonempty(event.get("delta"))
                }
                Dialect::ChatCompletions => event
                    .get("choices")
                    .and_then(Value::as_array)
                    .is_some_and(|choices| {
                        choices.iter().any(|c| {
                            ["content", "reasoning_content"]
                                .iter()
                                .any(|key| nonempty(c.get("delta").and_then(|v| v.get(key))))
                                || c.pointer("/delta/tool_calls")
                                    .and_then(Value::as_array)
                                    .is_some_and(|calls| {
                                        calls.iter().any(|call| {
                                            nonempty(call.pointer("/function/arguments"))
                                        })
                                    })
                        })
                    }),
            };
        if output && self.first_output_ms.is_none() {
            self.first_output_ms =
                Some(self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
        }
        let failed_status = |value: &Value| {
            value
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| {
                    matches!(status, "failed" | "incomplete" | "cancelled" | "canceled")
                })
        };
        let has_error = |value: &Value| value.get("error").is_some_and(|error| !error.is_null());
        if matches!(kind, "error" | "response.failed" | "response.incomplete")
            || has_error(event)
            || failed_status(event)
            || event
                .get("response")
                .is_some_and(|response| has_error(response) || failed_status(response))
        {
            self.failed = true;
            self.terminal = true;
        }
        if matches!(
            kind,
            "message_stop" | "response.completed" | "response.done"
        ) {
            self.terminal = true;
        }
        if !self.event_stream {
            let recognized = match self.dialect {
                Dialect::Anthropic => {
                    kind == "message"
                        && event.get("content").is_some_and(Value::is_array)
                        && nonempty(event.get("stop_reason"))
                }
                Dialect::Responses => {
                    event.get("object").and_then(Value::as_str) == Some("response")
                        && event.get("status").and_then(Value::as_str) == Some("completed")
                }
                Dialect::ChatCompletions => {
                    event.get("object").and_then(Value::as_str) == Some("chat.completion")
                        && event
                            .get("choices")
                            .and_then(Value::as_array)
                            .is_some_and(|choices| {
                                !choices.is_empty()
                                    && choices
                                        .iter()
                                        .all(|choice| nonempty(choice.get("finish_reason")))
                            })
                }
            };
            self.terminal |= recognized;
        }
        self.collect_tools(event, kind);
    }
    fn add_tool(&mut self, id: Option<&str>, name: Option<&str>) {
        let Some(id) = id.filter(|id| !id.is_empty() && id.len() <= 512) else {
            return;
        };
        if !self.tools.contains_key(id)
            && self.tools.len() + self.chat_tools.len() >= MAX_OBSERVED_TOOLS
        {
            self.dropped_tools = self.dropped_tools.saturating_add(1);
            return;
        }
        let name = name.filter(|name| data::safe_label(name)).map(String::from);
        self.tools
            .entry(id.to_owned())
            .and_modify(|old| {
                if name.is_some() {
                    *old = name.clone();
                }
            })
            .or_insert(name);
    }
    fn collect_tools(&mut self, event: &Value, kind: &str) {
        match self.dialect {
            Dialect::Anthropic => {
                let mut items = Vec::new();
                if kind == "content_block_start" {
                    if let Some(item) = event.get("content_block") {
                        items.push(item);
                    }
                }
                if !self.event_stream {
                    if let Some(content) = event.get("content").and_then(Value::as_array) {
                        items.extend(content);
                    }
                }
                for item in items {
                    if matches!(
                        item.get("type").and_then(Value::as_str),
                        Some("tool_use" | "server_tool_use")
                    ) {
                        self.add_tool(
                            item.get("id").and_then(Value::as_str),
                            item.get("name").and_then(Value::as_str),
                        );
                    }
                }
            }
            Dialect::Responses => {
                let mut items = Vec::new();
                if matches!(
                    kind,
                    "response.output_item.added" | "response.output_item.done"
                ) {
                    if let Some(item) = event.get("item") {
                        items.push(item);
                    }
                }
                if let Some(output) = event
                    .pointer("/response/output")
                    .or_else(|| event.get("output"))
                    .and_then(Value::as_array)
                {
                    items.extend(output);
                }
                for item in items {
                    if matches!(
                        item.get("type").and_then(Value::as_str),
                        Some("function_call" | "custom_tool_call")
                    ) {
                        self.add_tool(
                            item.get("call_id")
                                .or_else(|| item.get("id"))
                                .and_then(Value::as_str),
                            item.get("name").and_then(Value::as_str),
                        );
                    }
                }
            }
            Dialect::ChatCompletions => {
                if let Some(choices) = event.get("choices").and_then(Value::as_array) {
                    for (choice_position, choice) in choices.iter().enumerate() {
                        let choice_id = choice
                            .get("index")
                            .and_then(Value::as_u64)
                            .unwrap_or(choice_position as u64);
                        if let Some(calls) = choice
                            .pointer("/message/tool_calls")
                            .and_then(Value::as_array)
                        {
                            for call in calls {
                                self.add_tool(
                                    call.get("id").and_then(Value::as_str),
                                    call.pointer("/function/name").and_then(Value::as_str),
                                );
                            }
                        }
                        if let Some(calls) = choice
                            .pointer("/delta/tool_calls")
                            .and_then(Value::as_array)
                        {
                            for call in calls {
                                let Some(index) = call.get("index").and_then(Value::as_u64) else {
                                    continue;
                                };
                                let key = (choice_id, index);
                                if !self.chat_tools.contains_key(&key)
                                    && self.tools.len() + self.chat_tools.len()
                                        >= MAX_OBSERVED_TOOLS
                                {
                                    self.dropped_tools = self.dropped_tools.saturating_add(1);
                                    continue;
                                }
                                let entry = self.chat_tools.entry(key).or_default();
                                if let Some(id) = call
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .filter(|id| !id.is_empty() && id.len() <= 512)
                                {
                                    entry.id = Some(id.to_owned());
                                }
                                if let Some(part) =
                                    call.pointer("/function/name").and_then(Value::as_str)
                                {
                                    if entry.name.len().saturating_add(part.len()) > 128 {
                                        entry.name.clear();
                                        entry.invalid_name = true;
                                    } else if !entry.invalid_name {
                                        entry.name.push_str(part);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    fn observed_tools(&self) -> BTreeMap<String, Option<String>> {
        let mut tools = self.tools.clone();
        for tool in self.chat_tools.values() {
            if let Some(id) = &tool.id {
                tools.entry(id.clone()).or_insert_with(|| {
                    (!tool.invalid_name && data::safe_label(&tool.name)).then(|| tool.name.clone())
                });
            }
        }
        tools
    }
    fn merge_usage(&mut self, raw: &Value) {
        if !raw.is_object() {
            return;
        }
        let mut usage = self.usage.clone().unwrap_or_default();
        let number = |value: &Value| {
            value
                .as_u64()
                .filter(|n| *n <= data::MAX_COUNTER)
                .map(Quantity::reported)
        };
        let set = |dst: &mut Option<Quantity>, value: Option<&Value>| {
            if let Some(value) = value {
                *dst = number(value);
            }
        };
        match self.dialect {
            Dialect::Anthropic => {
                set(
                    &mut usage.cache_read_tokens,
                    raw.get("cache_read_input_tokens"),
                );
                set(
                    &mut usage.cache_write_tokens,
                    raw.get("cache_creation_input_tokens"),
                );
                if let Some(input) = raw.get("input_tokens") {
                    usage.input_tokens = number(input)
                        .and_then(|input| {
                            input
                                .value
                                .checked_add(usage.cache_read_tokens.map_or(0, |q| q.value))
                        })
                        .and_then(|n| {
                            n.checked_add(usage.cache_write_tokens.map_or(0, |q| q.value))
                        })
                        .filter(|n| *n <= data::MAX_COUNTER)
                        .map(Quantity::reported);
                    if ["cache_read_input_tokens", "cache_creation_input_tokens"]
                        .iter()
                        .any(|key| raw.get(key).is_some_and(|v| number(v).is_none()))
                    {
                        usage.input_tokens = None;
                    }
                }
                set(&mut usage.output_tokens, raw.get("output_tokens"));
            }
            Dialect::Responses => {
                set(&mut usage.input_tokens, raw.get("input_tokens"));
                set(&mut usage.output_tokens, raw.get("output_tokens"));
                set(
                    &mut usage.cache_read_tokens,
                    raw.pointer("/input_tokens_details/cached_tokens"),
                );
                set(
                    &mut usage.reasoning_tokens,
                    raw.pointer("/output_tokens_details/reasoning_tokens"),
                );
            }
            Dialect::ChatCompletions => {
                set(&mut usage.input_tokens, raw.get("prompt_tokens"));
                set(&mut usage.output_tokens, raw.get("completion_tokens"));
                set(
                    &mut usage.cache_read_tokens,
                    raw.pointer("/prompt_tokens_details/cached_tokens"),
                );
                set(
                    &mut usage.reasoning_tokens,
                    raw.pointer("/completion_tokens_details/reasoning_tokens"),
                );
            }
        }
        // Invalid provider counters must not discard the attempt's terminal
        // event or poison the storage retry queue. Unknown usage remains absent.
        self.usage = (usage != Usage::default() && usage.validate()).then_some(usage);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_free_finish_reasons_distinguish_refusal_error_missing_terminal_and_uncertainty() {
        for (dialect, stream, bytes, reason) in [
            (Dialect::Anthropic, false, br#"{"type":"message","content":[],"stop_reason":"refusal"}"#.as_slice(), FinishReason::ProviderRefused),
            (Dialect::Responses, true, b"data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"PRIVATE_ERROR\"}}}\n\n".as_slice(), FinishReason::ProviderError),
            (Dialect::Anthropic, true, b"data: {\"type\":\"message_start\"}\n\n".as_slice(), FinishReason::MissingTerminal),
            (Dialect::Responses, true, b"data: PRIVATE_UNPARSEABLE_PAYLOAD\n\n".as_slice(), FinishReason::ParserUncertain),
            (Dialect::Responses, false, br#"{"usage":{},"unrelated":"PRIVATE_PAYLOAD"}"#.as_slice(), FinishReason::ParserUncertain),
            (Dialect::ChatCompletions, false, br#"{"object":"chat.completion","choices":[{"finish_reason":"stop","message":{"refusal":"PRIVATE_REFUSAL"}}]}"#.as_slice(), FinishReason::ProviderRefused),
            (Dialect::Responses, true, b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"error\":null}}\n\n".as_slice(), FinishReason::Completed),
        ] {
            let mut tap = StreamMetrics::new(dialect, stream, Instant::now());
            for part in bytes.chunks(3) {
                tap.observe(part);
            }
            tap.finish_json();
            assert_eq!(tap.completion_reason(), reason);
        }
    }

    #[test]
    fn failure_history_keeps_recovery_time_instead_of_latest_healthy_write_time() {
        let history = FailureHistory::default();
        assert_eq!(history.fields(), (None, None, None));
        history.record(FailureCode::StorageWrite);
        let failed = history.fields();
        assert_eq!(failed.1, Some("storage_write_failed"));
        assert!(failed.0.is_some());
        assert_eq!(failed.2, None);
        history.recover();
        let recovered = history.fields();
        assert!(recovered.2.unwrap() >= failed.0.unwrap());
        thread::sleep(Duration::from_millis(5));
        history.recover();
        assert_eq!(history.fields(), recovered);
        history.record(FailureCode::QueueFull);
        assert_eq!(history.fields().1, Some("queue_full"));
        history.recover();
        assert!(history.fields().2.unwrap() >= recovered.2.unwrap());
    }

    #[test]
    fn provider_session_namespace_matches_native_imports_without_cross_provider_aliases() {
        let temp = Temp::new();
        let recorder = Recorder::open_path(true, Some(temp.path()));
        wait_for_idle_recorder(&recorder);
        let native = "11111111-1111-4111-8111-111111111111";
        let codex = recorder.session_id("codex", Some(native)).unwrap();
        let claude = recorder.session_id("claude_code", Some(native)).unwrap();
        assert_ne!(codex, claude);
        let store = Store::open(&temp.path()).unwrap();
        assert_eq!(
            codex,
            store.opaque(
                "native-session",
                &serde_json::to_string(&("codex", native)).unwrap()
            )
        );
        assert_eq!(recorder.session_id("codex", Some(native)), Some(codex));
        assert_eq!(recorder.session_id("unrecognized", Some(native)), None);
        assert_eq!(recorder.session_id("codex", Some("\n")), None);
        assert_eq!(recorder.session_id("codex", None), None);
    }

    #[test]
    fn anthropic_final_output_survives_long_stream_and_arbitrary_chunking() {
        let mut tap = StreamMetrics::new(Dialect::Anthropic, true, Instant::now());
        let start=b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":90,\"output_tokens\":0}}}\n\n";
        for part in start.chunks(3) {
            tap.observe(part);
        }
        assert!(tap.first_output_ms.is_none());
        for _ in 0..2000 {
            tap.observe(b"data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n");
        }
        tap.observe(b"data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":2000}}\n\ndata: {\"type\":\"message_stop\"}\n\n");
        assert_eq!(tap.usage.as_ref().unwrap().input_tokens.unwrap().value, 100);
        assert_eq!(
            tap.usage.as_ref().unwrap().output_tokens.unwrap().value,
            2000
        );
        assert!(tap.first_output_ms.is_some());
        assert!(tap.terminal);
        assert!(tap.buffer.len() < 10);
    }
    #[test]
    fn responses_failure_and_missing_usage_are_not_success_or_zero() {
        let mut tap = StreamMetrics::new(Dialect::Responses, true, Instant::now());
        tap.observe(b"data: {\"type\":\"response.incomplete\"}\n\n");
        assert!(tap.failed);
        assert!(tap.terminal);
        assert!(tap.usage.is_none());
    }
    #[test]
    fn chat_stream_carries_reasoning_and_cache_subsets() {
        let mut tap = StreamMetrics::new(Dialect::ChatCompletions, true, Instant::now());
        tap.observe(b"data: {\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":50,\"prompt_tokens_details\":{\"cached_tokens\":60},\"completion_tokens_details\":{\"reasoning_tokens\":40}}}\r\n\r\ndata: [DONE]\r\n\r\n");
        assert_eq!(tap.usage.unwrap().reasoning_tokens.unwrap().value, 40);
        assert!(tap.terminal);
    }

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "gobstopper-proxy-observations-{}",
                OpaqueId::random().unwrap().0
            ));
            data::prepare_private_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> PathBuf {
            self.0.join("events.sqlite3")
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn no_usage_attempt(recorder: &Recorder) {
        let identity = Identity {
            runtime_id: recorder.shared.runtime.clone(),
            request_id: recorder.request_id(),
            attempt_id: OpaqueId::random().ok(),
            ..Identity::default()
        };
        recorder.emit_many([
            (
                identity.clone(),
                Event::RequestStarted {
                    provider: "responses".into(),
                    model: Some("test-model".into()),
                },
            ),
            (
                identity,
                Event::RequestFinished {
                    outcome: Outcome::Success,
                    http_status: Some(200),
                    duration_ms: Some(0),
                    first_output_ms: None,
                    usage: None,
                    generation: None,
                    reason: Some(FinishReason::Completed),
                    timings: None,
                },
            ),
        ]);
    }

    fn wait_for_idle_recorder(recorder: &Recorder) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = recorder.status();
            if status["pending_events"] == 0 && status["available"] == true {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "idle recorder did not recover: {status}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn eventually(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "recorder condition did not converge"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn stalled_hook() -> (
        Box<dyn FnOnce() + Send>,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (entered, observed) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        (
            Box::new(move || {
                entered.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(5)).unwrap();
            }),
            observed,
            release,
        )
    }

    fn stalled_storage_never_blocks_callers(open: bool) {
        let temp = Temp::new();
        let (hook, entered, release) = stalled_hook();
        let (open_hook, open_entered, open_release) = stalled_hook();
        let hooks = if open {
            StorageHooks {
                open: Some(hook),
                append: None,
            }
        } else {
            StorageHooks {
                open: Some(open_hook),
                append: Some(hook),
            }
        };
        let started = Instant::now();
        let recorder = Recorder::open_impl(true, Some(temp.path()), hooks);
        assert!(started.elapsed() < Duration::from_millis(500));
        if !open {
            open_entered.recv_timeout(Duration::from_secs(5)).unwrap();
            no_usage_attempt(&recorder);
            assert_eq!(recorder.status()["pending_events"], 2);
            assert_eq!(recorder.status()["dropped_events"], 0);
            open_release.send(()).unwrap();
        }
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let shared = Arc::downgrade(&recorder.shared);
        let started = Instant::now();
        no_usage_attempt(&recorder);
        assert_eq!(
            recorder.status()["pending_events"],
            if open { 2 } else { 4 }
        );
        assert_eq!(recorder.status()["dropped_events"], 0);
        assert!(recorder.request_id().is_some());
        // Drop cannot join the owner of a stalled open or append.
        drop(recorder);
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(
            shared.upgrade().is_some(),
            "blocked worker lost its custody"
        );
        release.send(()).unwrap();
        eventually(|| shared.upgrade().is_none());
        assert_eq!(
            Store::open(&temp.path()).unwrap().status().unwrap().events,
            if open { 2 } else { 4 }
        );
    }

    #[test]
    fn recorder_stalled_open_never_blocks_start_requests_status_or_drop() {
        stalled_storage_never_blocks_callers(true);
    }

    #[test]
    fn recorder_stalled_append_never_blocks_requests_status_or_drop() {
        stalled_storage_never_blocks_callers(false);
    }

    #[test]
    fn recorder_shutdown_persists_more_than_one_batch_without_joining_writer() {
        let temp = Temp::new();
        let (hook, entered, release) = stalled_hook();
        let recorder = Recorder::open_impl(
            true,
            Some(temp.path()),
            StorageHooks {
                open: Some(hook),
                append: None,
            },
        );
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let count = MAX_PENDING_EVENTS;
        recorder.emit_many((0..count).map(|_| {
            (
                Identity {
                    runtime_id: recorder.shared.runtime.clone(),
                    request_id: Some(OpaqueId::random().unwrap()),
                    attempt_id: Some(OpaqueId::random().unwrap()),
                    ..Identity::default()
                },
                Event::RequestStarted {
                    provider: "responses".into(),
                    model: None,
                },
            )
        }));
        assert_eq!(recorder.status()["pending_events"], count);
        let shared = Arc::downgrade(&recorder.shared);
        drop(recorder);
        release.send(()).unwrap();
        eventually(|| shared.upgrade().is_none());
        assert_eq!(
            Store::open(&temp.path()).unwrap().status().unwrap().events,
            count as u64
        );
    }

    #[test]
    fn invalid_timestamp_override_drops_only_that_event_and_does_not_poison_flushes() {
        let temp = Temp::new();
        let recorder = Recorder::open_path(true, Some(temp.path()));
        wait_for_idle_recorder(&recorder);
        let identity = Identity {
            runtime_id: recorder.shared.runtime.clone(),
            request_id: Some(OpaqueId::random().unwrap()),
            attempt_id: Some(OpaqueId::random().unwrap()),
            ..Identity::default()
        };
        recorder.emit_many_at(std::iter::once((
            identity,
            Event::RequestStarted {
                provider: "responses".into(),
                model: None,
            },
            Some(8_640_000_000_000_001),
        )));
        assert_eq!(recorder.status()["dropped_events"], 1);
        assert_eq!(recorder.status()["last_failure_code"], "invalid_event");
        no_usage_attempt(&recorder);
        wait_for_idle_recorder(&recorder);
        let store = Store::open(&temp.path()).unwrap();
        let events = store.events(&data::Query::default()).unwrap();
        assert_eq!(events.len(), 2);
        assert!(events
            .iter()
            .all(|event| event.observed_at_ms <= 8_640_000_000_000_000));
        assert_eq!(store.status().unwrap().incomplete_attempts, 0);
        assert_eq!(recorder.status()["dropped_events"], 1);
    }

    #[test]
    fn recorder_contended_queue_drops_events_without_blocking_status_or_drop() {
        let temp = Temp::new();
        let recorder = Recorder::open_path(true, Some(temp.path()));
        wait_for_idle_recorder(&recorder);
        let shared = Arc::clone(&recorder.shared);
        let pending = shared.pending.lock().unwrap();
        let started = Instant::now();
        no_usage_attempt(&recorder);
        assert_eq!(recorder.status()["dropped_events"], 2);
        assert_eq!(recorder.status()["pending_events"], 0);
        assert_eq!(recorder.status()["available"], true);
        assert_eq!(recorder.status()["degraded"], false);
        assert_eq!(recorder.status()["last_failure_code"], "queue_contended");
        assert!(recorder.status()["last_failure_at_ms"].is_number());
        drop(recorder);
        assert!(started.elapsed() < Duration::from_millis(500));
        drop(pending);
        let weak = Arc::downgrade(&shared);
        drop(shared);
        eventually(|| weak.upgrade().is_none());
    }

    #[test]
    fn recorder_worker_panic_is_degraded_and_future_events_are_dropped() {
        let temp = Temp::new();
        let recorder = Recorder::open_impl(
            true,
            Some(temp.path()),
            StorageHooks {
                open: Some(Box::new(|| panic!("injected storage worker failure"))),
                append: None,
            },
        );
        eventually(|| recorder.status()["background_retry"] == false);
        assert_eq!(recorder.status()["available"], false);
        assert_eq!(recorder.status()["degraded"], true);
        assert_eq!(recorder.status()["write_failures"], 1);
        no_usage_attempt(&recorder);
        assert_eq!(recorder.status()["dropped_events"], 2);
        assert_eq!(recorder.status()["pending_events"], 0);
    }

    #[cfg(unix)]
    #[test]
    fn recorder_recovers_initial_open_failure_without_changing_runtime_or_resetting_data() {
        use std::os::unix::fs::PermissionsExt;
        let temp = Temp::new();
        std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        let recorder = Recorder::open_path(true, Some(temp.path()));
        let runtime = recorder.status()["runtime_id"].clone();
        assert!(runtime.is_string());
        assert_eq!(recorder.status()["available"], false);
        no_usage_attempt(&recorder);
        eventually(|| recorder.status()["write_failures"].as_u64().unwrap() >= 1);
        assert_eq!(recorder.status()["pending_events"], 2);
        assert!(recorder.status()["write_failures"].as_u64().unwrap() >= 1);
        assert!(recorder
            .session_id("codex", Some("native-session"))
            .is_none());
        std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        wait_for_idle_recorder(&recorder);
        assert_eq!(recorder.status()["runtime_id"], runtime);
        assert_eq!(recorder.status()["available"], true);
        assert_eq!(recorder.status()["pending_events"], 0);
        assert_eq!(recorder.status()["recoveries"], 1);
        let store = Store::open(&temp.path()).unwrap();
        let records = store.events(&data::Query::default()).unwrap();
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|r| r.identity.session_id.is_none()));
        assert_eq!(
            recorder.session_id("codex", Some("native-session")),
            Some(store.opaque(
                "native-session",
                &serde_json::to_string(&("codex", "native-session")).unwrap()
            ))
        );
    }

    #[test]
    fn recorder_recovers_busy_batch_while_idle_without_new_requests() {
        let temp = Temp::new();
        let recorder = Recorder::open_path(true, Some(temp.path()));
        wait_for_idle_recorder(&recorder);
        let connection = rusqlite::Connection::open(temp.path()).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        no_usage_attempt(&recorder);
        eventually(|| recorder.status()["write_failures"].as_u64().unwrap() >= 1);
        assert!(recorder.status()["write_failures"].as_u64().unwrap() >= 1);
        assert_eq!(recorder.status()["available"], false);
        assert_eq!(recorder.status()["pending_events"], 2);
        let failed = recorder.status();
        assert_eq!(failed["last_failure_code"], "storage_write_failed");
        let failed_at = failed["last_failure_at_ms"].as_u64().unwrap();
        assert!(failed["last_recovery_at_ms"].is_null());
        connection.execute_batch("ROLLBACK").unwrap();
        // No request, explicit flush, or retry-clock manipulation follows release.
        wait_for_idle_recorder(&recorder);
        assert_eq!(recorder.status()["recoveries"], 1);
        assert_eq!(recorder.status()["pending_events"], 0);
        assert_eq!(recorder.status()["degraded"], false);
        assert!(recorder.status()["last_recovery_at_ms"].as_u64().unwrap() >= failed_at);
        assert_eq!(
            recorder.status()["last_failure_code"],
            "storage_write_failed"
        );
        assert_eq!(
            Store::open(&temp.path()).unwrap().status().unwrap().events,
            2
        );
    }

    #[test]
    fn recorder_drains_more_than_one_batch_without_another_request() {
        let temp = Temp::new();
        let recorder = Recorder::open_path(true, Some(temp.path()));
        let count = MAX_FLUSH_EVENTS + 17;
        recorder.emit_many((0..count).map(|_| {
            (
                Identity {
                    runtime_id: recorder.shared.runtime.clone(),
                    request_id: Some(OpaqueId::random().unwrap()),
                    attempt_id: Some(OpaqueId::random().unwrap()),
                    ..Identity::default()
                },
                Event::RequestStarted {
                    provider: "responses".into(),
                    model: None,
                },
            )
        }));
        wait_for_idle_recorder(&recorder);
        assert_eq!(
            Store::open(&temp.path()).unwrap().status().unwrap().events,
            count as u64
        );
        assert_eq!(recorder.status()["write_failures"], 0);
        assert_eq!(recorder.status()["dropped_events"], 0);
    }

    #[test]
    fn recorder_status_is_nonblocking_and_concurrent_tail_drains_without_new_requests() {
        let temp = Temp::new();
        let (hook, entered, release) = stalled_hook();
        let recorder = Recorder::open_impl(
            true,
            Some(temp.path()),
            StorageHooks {
                open: None,
                append: Some(hook),
            },
        );
        no_usage_attempt(&recorder);
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        no_usage_attempt(&recorder);
        let status = recorder.status();
        assert_eq!(status["pending_events"], 4);
        assert_eq!(status["write_failures"], 0);
        release.send(()).unwrap();
        wait_for_idle_recorder(&recorder);
        assert_eq!(
            Store::open(&temp.path()).unwrap().status().unwrap().events,
            4
        );
    }

    #[test]
    fn recorder_detaches_worker_which_flushes_exact_pending_events_before_releasing_storage() {
        let disabled = Recorder::disabled();
        assert_eq!(disabled.status()["background_retry"], false);
        assert!(disabled.worker.is_none());
        let temp = Temp::new();
        drop(Store::open(&temp.path()).unwrap());
        let (hook, entered, release) = stalled_hook();
        let recorder = Recorder::open_impl(
            true,
            Some(temp.path()),
            StorageHooks {
                open: Some(hook),
                append: None,
            },
        );
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(recorder.status()["background_retry"], true);
        let shared = Arc::downgrade(&recorder.shared);
        let connection = rusqlite::Connection::open(temp.path()).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        no_usage_attempt(&recorder);
        assert_eq!(recorder.status()["pending_events"], 2);
        assert_eq!(recorder.status()["dropped_events"], 0);
        release.send(()).unwrap();
        eventually(|| recorder.status()["write_failures"].as_u64().unwrap() >= 1);
        let pending: std::collections::BTreeSet<_> = recorder
            .shared
            .pending
            .lock()
            .unwrap()
            .iter()
            .map(|event| event.event_id.clone())
            .collect();
        assert_eq!(pending.len(), 2);
        connection.execute_batch("ROLLBACK").unwrap();
        // Drop wakes the worker and returns. The worker owns the final flush
        // and SQLite destruction without changing queued identities.
        drop(recorder);
        eventually(|| shared.upgrade().is_none());
        assert!(
            shared.upgrade().is_none(),
            "recorder worker retained shared state"
        );
        let saved: std::collections::BTreeSet<_> = Store::open(&temp.path())
            .unwrap()
            .events(&data::Query::default())
            .unwrap()
            .into_iter()
            .map(|event| event.event_id)
            .collect();
        assert_eq!(saved, pending);
    }

    #[cfg(unix)]
    #[test]
    fn recorder_queue_is_bounded_and_drops_are_counted() {
        use std::os::unix::fs::PermissionsExt;
        let temp = Temp::new();
        std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        let recorder = Recorder::open_path(true, Some(temp.path()));
        recorder.emit_many((0..MAX_PENDING_EVENTS + 2).map(|_| {
            (
                Identity {
                    runtime_id: recorder.shared.runtime.clone(),
                    request_id: Some(OpaqueId::random().unwrap()),
                    attempt_id: Some(OpaqueId::random().unwrap()),
                    ..Identity::default()
                },
                Event::RequestStarted {
                    provider: "responses".into(),
                    model: None,
                },
            )
        }));
        assert_eq!(recorder.status()["pending_events"], MAX_PENDING_EVENTS);
        assert_eq!(recorder.status()["dropped_events"], 2);
        std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        wait_for_idle_recorder(&recorder);
        assert_eq!(recorder.status()["pending_events"], 0);
        assert_eq!(
            Store::open(&temp.path()).unwrap().status().unwrap().events,
            MAX_PENDING_EVENTS as u64
        );
    }

    #[test]
    fn null_errors_and_nested_terminal_states_are_classified_semantically() {
        for (payload, terminal, failed) in [
            (
                json!({"type":"response.completed","response":{"status":"completed","error":null}}),
                true,
                false,
            ),
            (
                json!({"type":"response.done","response":{"status":"incomplete","error":null}}),
                true,
                true,
            ),
            (
                json!({"type":"response.done","response":{"status":"failed","error":{"code":"test"}}}),
                true,
                true,
            ),
            (
                json!({"object":"response","status":"completed","error":null}),
                true,
                false,
            ),
            (
                json!({"object":"response","status":"in_progress"}),
                false,
                false,
            ),
            (json!({"hello":"unrelated-json","usage":{}}), false, false),
        ] {
            let mut tap = StreamMetrics::new(Dialect::Responses, false, Instant::now());
            tap.observe(&serde_json::to_vec(&payload).unwrap());
            tap.finish_json();
            assert_eq!(tap.terminal, terminal, "{payload}");
            assert_eq!(tap.failed, failed, "{payload}");
        }
    }

    #[test]
    fn invalid_usage_remains_absent_and_does_not_prevent_request_completion() {
        let temp = Temp::new();
        let recorder = Recorder::open_path(true, Some(temp.path()));
        let mut attempt = recorder.start(recorder.request_id(), None, Dialect::Responses, None);
        let mut tap = StreamMetrics::new(Dialect::Responses, true, attempt.started);
        for usage in [
            json!({}),
            json!({"input_tokens":"not-a-counter","output_tokens":-1}),
            json!({"input_tokens":4,"output_tokens":1,"output_tokens_details":{"reasoning_tokens":50}}),
            json!({"input_tokens":u64::MAX}),
        ] {
            tap.event(&json!({"type":"response.completed","response":{"status":"completed","usage":usage}}));
            assert!(tap.usage.is_none());
        }
        attempt.finish(Outcome::Success, Some(200), Some(&tap));
        wait_for_idle_recorder(&recorder);
        let records = Store::open(&temp.path())
            .unwrap()
            .events(&data::Query::default())
            .unwrap();
        assert!(matches!(
            records.last().unwrap().event,
            Event::RequestFinished { usage: None, .. }
        ));
        assert_eq!(recorder.status()["dropped_events"], 0);
    }

    #[test]
    fn live_tool_requests_are_deduplicated_private_and_have_unknown_execution() {
        let temp = Temp::new();
        let recorder = Recorder::open_path(true, Some(temp.path()));
        let mut attempt = recorder.start(recorder.request_id(), None, Dialect::Responses, None);
        let mut tap = StreamMetrics::new(Dialect::Responses, true, attempt.started);
        for kind in ["response.output_item.added", "response.output_item.done"] {
            tap.event(&json!({"type":kind,"item":{"type":"custom_tool_call","call_id":"private-provider-tool-id","name":"exec","input":"NEVER_STORE_THIS_PROGRAM"}}));
        }
        tap.event(&json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":100,"output_tokens":20,"output_tokens_details":{"reasoning_tokens":10}}}}));
        attempt.finish(Outcome::Success, Some(200), Some(&tap));
        wait_for_idle_recorder(&recorder);
        let store = Store::open(&temp.path()).unwrap();
        let events = store.events(&data::Query::default()).unwrap();
        let tools = data::metrics::tools(&events);
        assert_eq!(tools.len(), 1);
        assert!(tools[0].requested);
        assert!(!tools[0].terminal);
        assert_eq!(tools[0].outcome, None);
        assert_eq!(tools[0].name.as_deref(), Some("exec"));
        assert!(tools[0].session_id.is_none());
        assert!(tools[0].attempt_id.is_some());
        let encoded = serde_json::to_string(&events).unwrap();
        assert!(!encoded.contains("private-provider-tool-id"));
        assert!(!encoded.contains("NEVER_STORE_THIS_PROGRAM"));
        assert_eq!(recorder.status()["dropped_events"], 0);
    }

    #[test]
    fn anthropic_and_chat_tool_requests_and_response_output_timing_are_observed() {
        let mut anthropic = StreamMetrics::new(Dialect::Anthropic, true, Instant::now());
        anthropic.event(&json!({"type":"content_block_start","content_block":{"type":"tool_use","id":"tool-1","name":"read_file","input":{"secret":"ignored"}}}));
        assert_eq!(
            anthropic.observed_tools().get("tool-1"),
            Some(&Some("read_file".into()))
        );
        let mut chat = StreamMetrics::new(Dialect::ChatCompletions, true, Instant::now());
        chat.event(&json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"read_","arguments":"ignored"}}]}}]}));
        chat.event(&json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"file","arguments":"ignored"}}]}}]}));
        assert_eq!(
            chat.observed_tools().get("call-1"),
            Some(&Some("read_file".into()))
        );
        for kind in [
            "response.output_text.delta",
            "response.reasoning_text.delta",
            "response.reasoning_summary_text.delta",
        ] {
            let mut responses = StreamMetrics::new(Dialect::Responses, true, Instant::now());
            responses.event(&json!({"type":kind,"delta":"observed-output"}));
            assert!(responses.first_output_ms.is_some());
        }
    }

    #[test]
    fn oversized_event_delimiter_boundary_preserves_following_final_usage() {
        let mut tap = StreamMetrics::new(Dialect::Responses, true, Instant::now());
        tap.observe(&vec![b'x'; 4 * 1024 * 1024]);
        tap.observe(b"\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":42,\"output_tokens\":7}}}\n\n");
        assert!(tap.terminal);
        assert_eq!(tap.usage.unwrap().output_tokens.unwrap().value, 7);
    }

    #[test]
    fn interrupted_anthropic_stream_does_not_report_initial_output_zero_as_total() {
        let mut tap = StreamMetrics::new(Dialect::Anthropic, true, Instant::now());
        tap.event(&json!({"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":0}}}));
        assert_eq!(tap.usage.as_ref().unwrap().input_tokens.unwrap().value, 10);
        assert!(tap.usage.as_ref().unwrap().output_tokens.is_none());
        assert!(!tap.terminal);
    }
}
