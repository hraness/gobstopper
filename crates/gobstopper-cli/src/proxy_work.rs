//! Bounded optional work. Timing out abandons a result, never its resource lease.
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

pub(super) const BODY_BUDGET: usize = 256 * 1024 * 1024;
pub(super) const TRANSFORM_LIMIT: usize = 16 * 1024 * 1024;
#[derive(Debug)]
pub(super) struct MemoryExhausted;
impl std::fmt::Display for MemoryExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("request memory is temporarily full; retry shortly")
    }
}
impl std::error::Error for MemoryExhausted {}
const WORKERS: usize = 4;

#[derive(Clone)]
pub(super) struct MemoryBudget {
    used: Arc<AtomicUsize>,
    limit: usize,
}

impl MemoryBudget {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            used: Arc::new(AtomicUsize::new(0)),
            limit,
        }
    }
    pub(super) fn lease(&self) -> MemoryLease {
        MemoryLease {
            budget: self.clone(),
            bytes: 0,
        }
    }
    pub(super) fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }
}

pub(super) struct MemoryLease {
    budget: MemoryBudget,
    bytes: usize,
}

impl MemoryLease {
    pub(super) fn grow(&mut self, bytes: usize) -> bool {
        if self
            .budget
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|next| *next <= self.budget.limit)
            })
            .is_err()
        {
            return false;
        }
        self.bytes += bytes;
        true
    }
}

impl Drop for MemoryLease {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[derive(Default)]
struct State {
    active: AtomicUsize,
    unavailable: AtomicU64,
    timed_out: AtomicU64,
    failed: AtomicU64,
}

#[derive(Default)]
pub(super) struct Workers {
    state: Arc<State>,
}

pub(super) struct Job {
    state: Arc<State>,
    _memory: MemoryLease,
}

#[derive(Debug)]
pub(super) enum Failure {
    Timeout,
    Panicked,
    Spawn,
}

impl Workers {
    /// Obtain both leases before cloning input. A timed-out thread retains them
    /// until its real exit, so stalled work cannot cause unbounded accumulation.
    pub(super) fn acquire(&self, memory: &MemoryBudget, bytes: usize) -> Option<Job> {
        if bytes > TRANSFORM_LIMIT
            || self
                .state
                .active
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                    (n < WORKERS).then_some(n + 1)
                })
                .is_err()
        {
            self.state.unavailable.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let mut job = Job {
            state: Arc::clone(&self.state),
            _memory: memory.lease(),
        };
        if !job._memory.grow(bytes) {
            self.state.unavailable.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        Some(job)
    }
    pub(super) fn status(&self) -> Value {
        json!({"active":self.state.active.load(Ordering::Acquire),"worker_limit":WORKERS,
            "input_byte_limit":TRANSFORM_LIMIT,"unavailable":self.state.unavailable.load(Ordering::Relaxed),
            "timed_out":self.state.timed_out.load(Ordering::Relaxed),"failed":self.state.failed.load(Ordering::Relaxed)})
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.state.active.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Job {
    pub(super) fn run<T: Send + 'static>(
        self,
        timeout: Duration,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, Failure> {
        let state = Arc::clone(&self.state);
        let (sender, receiver) = mpsc::sync_channel(1);
        let spawn = std::thread::Builder::new()
            .name("gobstopper-transform".into())
            .spawn(move || {
                let _custody = self;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
                // A timed-out receiver cannot use this returned value. Optional
                // transforms defer publication until acceptance; callers with
                // database effects separately handle uncertain commits.
                let _ = sender.send(result);
            });
        if spawn.is_err() {
            state.failed.fetch_add(1, Ordering::Relaxed);
            return Err(Failure::Spawn);
        }
        match receiver.recv_timeout(timeout) {
            Ok(Ok(result)) => Ok(result),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                state.timed_out.fetch_add(1, Ordering::Relaxed);
                Err(Failure::Timeout)
            }
            Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                state.failed.fetch_add(1, Ordering::Relaxed);
                Err(Failure::Panicked)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn memory_budget_is_atomic_and_released_on_every_exit() {
        let memory = MemoryBudget::new(100);
        let mut first = memory.lease();
        let mut second = memory.lease();
        assert!(first.grow(60));
        assert!(!second.grow(41));
        assert!(second.grow(40));
        assert!(!first.grow(usize::MAX));
        drop(first);
        assert_eq!(memory.used(), 40);
        drop(second);
        assert_eq!(memory.used(), 0);
    }

    #[test]
    fn timed_out_jobs_keep_worker_and_byte_leases_until_they_really_exit() {
        let workers = Workers::default();
        let memory = MemoryBudget::new(100);
        let mut releases = Vec::new();
        for _ in 0..WORKERS {
            let (release, receive) = mpsc::channel();
            let result =
                workers
                    .acquire(&memory, 10)
                    .unwrap()
                    .run(Duration::from_millis(20), move || {
                        receive.recv().unwrap();
                        42
                    });
            assert!(matches!(result, Err(Failure::Timeout)));
            releases.push(release);
        }
        assert!(workers.acquire(&memory, 1).is_none());
        assert_eq!(memory.used(), 40);
        assert_eq!(workers.status()["active"], WORKERS);
        for release in releases {
            release.send(()).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while memory.used() != 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(memory.used(), 0);
        assert!(workers.acquire(&memory, 10).is_some());
    }

    #[test]
    fn failed_work_and_oversized_inputs_do_not_leak_capacity() {
        let workers = Workers::default();
        let memory = MemoryBudget::new(100);
        assert!(workers.acquire(&memory, TRANSFORM_LIMIT + 1).is_none());
        assert!(workers.acquire(&memory, 101).is_none());
        let result = workers
            .acquire(&memory, 10)
            .unwrap()
            .run(Duration::from_secs(1), || {
                panic!("injected optional transform panic")
            });
        assert!(matches!(result, Err(Failure::Panicked)));
        let deadline = Instant::now() + Duration::from_secs(3);
        while memory.used() != 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(memory.used(), 0);
        assert_eq!(workers.status()["active"], 0);
    }
}
