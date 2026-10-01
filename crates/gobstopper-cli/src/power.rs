//! Process-owned idle-sleep inhibition while model requests are in flight.
//!
//! Request guards notify one background owner to acquire or release the
//! assertion. Native operations may remain pending or stalled, but request
//! threads never wait for them. Operating-system ownership (or the Linux
//! helper's EOF-bound stdin) also releases the assertion if the proxy dies.

use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

const RETRY_DELAY: Duration = Duration::from_secs(30);
const PROBE_INTERVAL: Duration = Duration::from_secs(1);
const STALLED_AFTER_MS: u64 = 2_000;
const DISABLED: u8 = 0;
const IDLE: u8 = 1;
const ACQUIRING: u8 = 2;
const HELD: u8 = 3;
const CHECKING: u8 = 4;
const RELEASING: u8 = 5;
const BACKOFF: u8 = 6;
const UNAVAILABLE: u8 = 7;

#[derive(Clone)]
pub struct Power {
    inner: Arc<State>,
}

struct State {
    enabled: bool,
    active: AtomicUsize,
    held: AtomicBool,
    phase: AtomicU8,
    phase_since_ms: AtomicU64,
    started: Instant,
    worker_alive: AtomicBool,
    last_error: Mutex<Option<String>>,
    wake: OnceLock<Thread>,
}

pub struct Guard {
    inner: Arc<State>,
}

trait Assertion {
    fn alive(&mut self) -> bool;
}
impl Assertion for platform::Assertion {
    fn alive(&mut self) -> bool {
        platform::Assertion::alive(self)
    }
}

impl State {
    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis().min(u64::MAX as u128) as u64
    }
    fn phase(&self, phase: u8) {
        self.phase_since_ms
            .store(self.elapsed_ms(), Ordering::Release);
        self.phase.store(phase, Ordering::Release);
    }
    fn error(&self, error: Option<String>) {
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = error;
    }
    fn wake(&self) {
        if let Some(worker) = self.wake.get() {
            // park/unpark coalesces notifications; there is no per-request
            // thread, queued work item, native call, or worker join.
            worker.unpark();
        }
    }
}

impl Power {
    pub fn new(enabled: bool) -> Self {
        Self::with_backend(enabled, platform::Assertion::acquire)
    }

    fn with_backend<A: Assertion + 'static>(
        enabled: bool,
        acquire: impl FnMut() -> Result<A, String> + Send + 'static,
    ) -> Self {
        let inner = Arc::new(State {
            enabled,
            active: AtomicUsize::new(0),
            held: AtomicBool::new(false),
            phase: AtomicU8::new(if enabled { IDLE } else { DISABLED }),
            phase_since_ms: AtomicU64::new(0),
            started: Instant::now(),
            worker_alive: AtomicBool::new(enabled),
            last_error: Mutex::new(None),
            wake: OnceLock::new(),
        });
        if enabled {
            let weak = Arc::downgrade(&inner);
            match thread::Builder::new()
                .name("gobstopper-power".into())
                .spawn(move || {
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_worker(&weak, acquire);
                    }));
                    if let Some(state) = weak.upgrade() {
                        state.worker_alive.store(false, Ordering::Release);
                        state.held.store(false, Ordering::Release);
                        if outcome.is_err() {
                            state
                                .error(Some("sleep-inhibition worker stopped unexpectedly".into()));
                        }
                        state.phase(UNAVAILABLE);
                    }
                }) {
                Ok(worker) => {
                    // A detached single owner performs all native cleanup. Its
                    // Weak reference cannot keep Power/Guard alive in a cycle.
                    let _ = inner.wake.set(worker.thread().clone());
                }
                Err(error) => {
                    inner.worker_alive.store(false, Ordering::Release);
                    inner.error(Some(format!(
                        "cannot start sleep-inhibition worker: {error}"
                    )));
                    inner.phase(UNAVAILABLE);
                }
            }
        }
        Self { inner }
    }

    /// Call for an admitted logical inference request. Keep the guard through
    /// context preflight and all upstream attempts; local rejection drops it.
    pub fn acquire(&self) -> Guard {
        self.inner.active.fetch_add(1, Ordering::AcqRel);
        self.inner.wake();
        Guard {
            inner: Arc::clone(&self.inner),
        }
    }

    pub fn status(&self) -> Value {
        self.snapshot(self.inner.elapsed_ms())
    }

    fn snapshot(&self, now_ms: u64) -> Value {
        let state = &self.inner;
        let active = state.active.load(Ordering::Acquire);
        let held = state.held.load(Ordering::Acquire);
        let phase = state.phase.load(Ordering::Acquire);
        let elapsed = now_ms.saturating_sub(state.phase_since_ms.load(Ordering::Acquire));
        let (error, details_pending) = match state.last_error.try_lock() {
            Ok(error) => (error.clone(), false),
            Err(std::sync::TryLockError::Poisoned(error)) => (error.into_inner().clone(), false),
            Err(std::sync::TryLockError::WouldBlock) => (None, true),
        };
        json!({
            "enabled": state.enabled,
            "backend": platform::NAME,
            "active_inference": active,
            "held": held,
            "last_error": error,
            "details_pending": details_pending,
            "worker_alive": state.worker_alive.load(Ordering::Acquire),
            "pending": held != (state.enabled && active > 0) || matches!(phase, ACQUIRING | RELEASING),
            "stalled": matches!(phase, ACQUIRING | CHECKING | RELEASING) && elapsed >= STALLED_AFTER_MS,
            "phase_elapsed_ms": elapsed,
            "phase": match phase { DISABLED => "disabled", IDLE => "idle", ACQUIRING => "acquiring", HELD => "held", CHECKING => "checking", RELEASING => "releasing", BACKOFF => "retry_wait", _ => "unavailable" },
            "scope": if cfg!(target_os = "linux") { "logind-idle-action" } else { "idle-system-sleep" },
        })
    }
}

impl Drop for Power {
    fn drop(&mut self) {
        // Wake before releasing our reference. A racing worker may observe it
        // for one more iteration; its bounded idle poll then sees Weak expire.
        self.inner.wake();
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.inner.active.fetch_sub(1, Ordering::AcqRel);
        self.inner.wake();
    }
}

fn release<A: Assertion>(state: &State, assertion: &mut Option<A>) {
    if assertion.is_some() {
        state.phase(RELEASING);
        drop(assertion.take());
        state.held.store(false, Ordering::Release);
    }
}

fn run_worker<A: Assertion>(weak: &Weak<State>, mut acquire: impl FnMut() -> Result<A, String>) {
    let mut assertion: Option<A> = None;
    let mut retry_after = None;
    loop {
        let Some(state) = weak.upgrade() else {
            // No Power or request Guard remains. Native destruction happens
            // here, on the owner thread, even if the final drop was a request.
            drop(assertion);
            return;
        };
        if state.active.load(Ordering::Acquire) == 0 {
            release(&state, &mut assertion);
            state.phase(IDLE);
        } else {
            if let Some(current) = assertion.as_mut() {
                state.phase(CHECKING);
                if !current.alive() {
                    release(&state, &mut assertion);
                    state.error(Some("sleep-inhibition helper exited".into()));
                    retry_after = Some(Instant::now() + RETRY_DELAY);
                }
            }
            if assertion.is_none() && retry_after.is_none_or(|at| Instant::now() >= at) {
                state.phase(ACQUIRING);
                match acquire() {
                    Ok(acquired) => {
                        assertion = Some(acquired);
                        state.held.store(true, Ordering::Release);
                        state.error(None);
                        retry_after = None;
                    }
                    Err(error) => {
                        state.error(Some(error));
                        retry_after = Some(Instant::now() + RETRY_DELAY);
                    }
                }
            }
            // Requests may finish during a slow native operation. Never keep
            // its late result merely because an earlier snapshot was active.
            if state.active.load(Ordering::Acquire) == 0 {
                release(&state, &mut assertion);
                state.phase(IDLE);
            } else {
                state.phase(if assertion.is_some() { HELD } else { BACKOFF });
            }
        }
        drop(state);
        thread::park_timeout(PROBE_INTERVAL);
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{c_char, c_void};
    pub const NAME: &str = "macos-iokit";
    type CFStringRef = *const c_void;
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            text: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFRelease(value: *const c_void);
    }
    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            level: u32,
            name: CFStringRef,
            assertion_id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(assertion_id: u32) -> i32;
    }
    pub struct Assertion(u32);
    impl Assertion {
        pub fn acquire() -> Result<Self, String> {
            // SAFETY: literal strings are terminated UTF-8; CF objects are
            // checked for null, kept alive for the call, and released once.
            unsafe {
                let kind = CFStringCreateWithCString(
                    std::ptr::null(),
                    c"PreventUserIdleSystemSleep".as_ptr(),
                    0x08000100,
                );
                let name = CFStringCreateWithCString(
                    std::ptr::null(),
                    c"Gobstopper active inference".as_ptr(),
                    0x08000100,
                );
                if kind.is_null() || name.is_null() {
                    if !kind.is_null() {
                        CFRelease(kind);
                    }
                    if !name.is_null() {
                        CFRelease(name);
                    }
                    return Err("cannot allocate idle-sleep assertion name".into());
                }
                let mut id = 0;
                let result = IOPMAssertionCreateWithName(kind, 255, name, &mut id);
                CFRelease(kind);
                CFRelease(name);
                if result == 0 {
                    Ok(Self(id))
                } else {
                    Err(format!("IOKit idle-sleep assertion failed ({result})"))
                }
            }
        }
        pub fn alive(&mut self) -> bool {
            true
        }
    }
    impl Drop for Assertion {
        fn drop(&mut self) {
            // SAFETY: this value exclusively owns the successfully created ID.
            unsafe {
                IOPMAssertionRelease(self.0);
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use std::ffi::c_void;
    pub const NAME: &str = "windows-power-request";
    #[repr(C)]
    struct DetailedReason {
        module: *mut c_void,
        resource_id: u32,
        string_count: u32,
        strings: *mut *mut u16,
    }
    #[repr(C)]
    union Reason {
        simple: *const u16,
        detailed: std::mem::ManuallyDrop<DetailedReason>,
    }
    #[repr(C)]
    struct ReasonContext {
        version: u32,
        flags: u32,
        reason: Reason,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn PowerCreateRequest(context: *const ReasonContext) -> *mut c_void;
        fn PowerSetRequest(handle: *mut c_void, request_type: u32) -> i32;
        fn PowerClearRequest(handle: *mut c_void, request_type: u32) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    // Store the process-owned handle as its pointer-sized integer. Calls are
    // serialized by the single power worker; request threads never use it.
    pub struct Assertion(usize);
    impl Assertion {
        pub fn acquire() -> Result<Self, String> {
            let name: Vec<u16> = "Gobstopper active inference\0".encode_utf16().collect();
            let reason = ReasonContext {
                version: 0,
                flags: 1,
                reason: Reason {
                    simple: name.as_ptr(),
                },
            };
            // SAFETY: REASON_CONTEXT matches the Windows ABI and the UTF-16
            // string remains live through creation. Only valid handles close.
            unsafe {
                let handle = PowerCreateRequest(&reason);
                if handle.is_null() || handle as isize == -1 {
                    return Err("Windows could not create an idle-sleep request".into());
                }
                // PowerRequestSystemRequired = 1; display-required is 0.
                if PowerSetRequest(handle, 1) == 0 {
                    CloseHandle(handle);
                    return Err("Windows refused idle-sleep inhibition".into());
                }
                Ok(Self(handle as usize))
            }
        }
        pub fn alive(&mut self) -> bool {
            true
        }
    }
    impl Drop for Assertion {
        fn drop(&mut self) {
            // SAFETY: this value exclusively owns the live power request handle.
            unsafe {
                PowerClearRequest(self.0 as *mut c_void, 1);
                CloseHandle(self.0 as *mut c_void);
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::process::{Child, ChildStdin, Command, Stdio};
    pub const NAME: &str = "linux-systemd-inhibit";
    pub struct Assertion {
        child: Child,
        stdin: Option<ChildStdin>,
    }
    impl Assertion {
        pub fn acquire() -> Result<Self, String> {
            let mut child = Command::new("/usr/bin/systemd-inhibit")
                .args([
                    "--what=idle",
                    "--mode=block",
                    "--who=Gobstopper",
                    "--why=Active inference",
                    "/bin/cat",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|_| "systemd idle inhibition is unavailable".to_string())?;
            let stdin = child.stdin.take();
            let mut assertion = Self { child, stdin };
            // This does not prove logind accepted the request. Status checks
            // report helpers that exited; ordinary request forwarding proceeds.
            if !assertion.alive() {
                return Err("systemd idle inhibition was refused".into());
            }
            Ok(assertion)
        }
        pub fn alive(&mut self) -> bool {
            matches!(self.child.try_wait(), Ok(None))
        }
    }
    impl Drop for Assertion {
        fn drop(&mut self) {
            // Closing stdin releases cat even after abrupt proxy termination.
            self.stdin.take();
            // Bound our cleanup if the helper is unhealthy. The owned helper
            // process, never a discovered PID, is the only process killed.
            for _ in 0..10 {
                if !self.alive() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod platform {
    pub const NAME: &str = "unsupported";
    pub struct Assertion;
    impl Assertion {
        pub fn acquire() -> Result<Self, String> {
            Err("idle-sleep inhibition is unsupported on this platform".into())
        }
        pub fn alive(&mut self) -> bool {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Receiver, Sender};

    fn eventually(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "power worker did not settle");
            thread::sleep(Duration::from_millis(2));
        }
    }

    struct Gate {
        entered: Sender<()>,
        release: Receiver<()>,
    }
    impl Gate {
        fn wait(self) {
            let _ = self.entered.send(());
            // A failing fixture still allows the detached worker to clean up.
            let _ = self.release.recv_timeout(Duration::from_secs(5));
        }
    }
    fn gate() -> (Gate, Receiver<()>, Sender<()>) {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        (
            Gate {
                entered: entered_tx,
                release: release_rx,
            },
            entered_rx,
            release_tx,
        )
    }
    struct FakeAssertion {
        drops: Arc<AtomicUsize>,
        checking: Option<Gate>,
        releasing: Option<Gate>,
    }
    impl Assertion for FakeAssertion {
        fn alive(&mut self) -> bool {
            if let Some(gate) = self.checking.take() {
                gate.wait();
            }
            true
        }
    }
    impl Drop for FakeAssertion {
        fn drop(&mut self) {
            if let Some(gate) = self.releasing.take() {
                gate.wait();
            }
            self.drops.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn stalled_acquisition_never_blocks_requests_or_publishes_an_idle_assertion() {
        let (gate, entered, release) = gate();
        let mut gate = Some(gate);
        let drops = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&drops);
        let power = Power::with_backend(true, move || {
            if let Some(gate) = gate.take() {
                gate.wait();
            }
            Ok(FakeAssertion {
                drops: Arc::clone(&observed),
                checking: None,
                releasing: None,
            })
        });
        let first = power.acquire();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let started = Instant::now();
        for _ in 0..100 {
            let guard = power.acquire();
            assert_eq!(power.status()["active_inference"], 2);
            drop(guard);
        }
        drop(first);
        assert!(started.elapsed() < Duration::from_secs(1));
        let status = power.snapshot(power.inner.elapsed_ms() + STALLED_AFTER_MS);
        assert_eq!(status["phase"], "acquiring");
        assert_eq!(status["stalled"], true);
        assert_eq!(status["pending"], true);
        assert_eq!(status["active_inference"], 0);
        release.send(()).unwrap();
        eventually(|| drops.load(Ordering::Acquire) == 1 && power.status()["held"] == false);
        let next = power.acquire();
        eventually(|| power.status()["held"] == true);
        drop(next);
        eventually(|| drops.load(Ordering::Acquire) == 2);
    }

    #[test]
    fn stalled_liveness_probe_does_not_block_last_request_release() {
        let (gate, entered, release) = gate();
        let mut gate = Some(gate);
        let drops = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&drops);
        let power = Power::with_backend(true, move || {
            Ok(FakeAssertion {
                drops: Arc::clone(&observed),
                checking: gate.take(),
                releasing: None,
            })
        });
        let first = power.acquire();
        eventually(|| power.status()["held"] == true);
        let second = power.acquire();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let started = Instant::now();
        drop(first);
        drop(second);
        let status = power.snapshot(power.inner.elapsed_ms() + STALLED_AFTER_MS);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(status["phase"], "checking");
        assert_eq!(status["stalled"], true);
        assert_eq!(status["active_inference"], 0);
        release.send(()).unwrap();
        eventually(|| drops.load(Ordering::Acquire) == 1 && power.status()["held"] == false);
    }

    #[test]
    fn stalled_native_release_allows_a_new_request_and_serializes_reacquisition() {
        let (gate, entered, release) = gate();
        let mut gate = Some(gate);
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let drops = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&drops);
        let power = Power::with_backend(true, move || {
            counted.fetch_add(1, Ordering::AcqRel);
            Ok(FakeAssertion {
                drops: Arc::clone(&observed),
                checking: None,
                releasing: gate.take(),
            })
        });
        let first = power.acquire();
        eventually(|| power.status()["held"] == true);
        drop(first);
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let started = Instant::now();
        let next = power.acquire();
        let status = power.snapshot(power.inner.elapsed_ms() + STALLED_AFTER_MS);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(status["phase"], "releasing");
        assert_eq!(status["stalled"], true);
        assert_eq!(status["pending"], true);
        assert_eq!(status["active_inference"], 1);
        assert_eq!(calls.load(Ordering::Acquire), 1);
        release.send(()).unwrap();
        eventually(|| calls.load(Ordering::Acquire) == 2 && power.status()["phase"] == "held");
        assert_eq!(drops.load(Ordering::Acquire), 1);
        drop(next);
        eventually(|| drops.load(Ordering::Acquire) == 2);
    }

    #[test]
    fn power_owner_has_no_arc_cycle_and_outlives_only_active_guards() {
        let drops = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&drops);
        let power = Power::with_backend(true, move || {
            Ok(FakeAssertion {
                drops: Arc::clone(&observed),
                checking: None,
                releasing: None,
            })
        });
        let weak = Arc::downgrade(&power.inner);
        let guard = power.acquire();
        eventually(|| power.status()["held"] == true);
        drop(power);
        assert_eq!(drops.load(Ordering::Acquire), 0);
        assert!(weak.upgrade().is_some());
        drop(guard);
        eventually(|| drops.load(Ordering::Acquire) == 1 && weak.upgrade().is_none());
    }

    #[test]
    fn worker_failure_is_cached_without_request_thread_restart_or_native_work() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        let power = Power::with_backend(true, move || -> Result<FakeAssertion, String> {
            observed.fetch_add(1, Ordering::AcqRel);
            panic!("injected native backend panic");
        });
        let first = power.acquire();
        eventually(|| power.status()["phase"] == "unavailable");
        for _ in 0..100 {
            drop(power.acquire());
        }
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert_eq!(power.status()["active_inference"], 1);
        assert_eq!(power.status()["worker_alive"], false);
        assert_eq!(power.status()["held"], false);
        assert!(power.status()["last_error"]
            .as_str()
            .unwrap()
            .contains("stopped unexpectedly"));
        drop(first);
    }

    #[test]
    fn unavailable_backend_backs_off_even_under_request_churn() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        let power = Power::with_backend(true, move || -> Result<FakeAssertion, String> {
            observed.fetch_add(1, Ordering::AcqRel);
            Err("injected native permission error".into())
        });
        let first = power.acquire();
        eventually(|| power.status()["phase"] == "retry_wait");
        for _ in 0..100 {
            drop(power.acquire());
        }
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert_eq!(power.status()["held"], false);
        assert_eq!(power.status()["pending"], true);
        assert_eq!(
            power.status()["last_error"],
            "injected native permission error"
        );
        drop(first);
        eventually(|| power.status()["phase"] == "idle");
    }

    #[test]
    fn error_snapshot_and_request_guards_never_wait_on_optional_details_lock() {
        let power = Power::new(false);
        let _details = power.inner.last_error.lock().unwrap();
        let guard = power.acquire();
        assert_eq!(power.status()["details_pending"], true);
        drop(guard);
        assert_eq!(power.status()["active_inference"], 0);
    }
    #[test]
    fn disabled_tracks_overlapping_requests_without_an_assertion() {
        let power = Power::new(false);
        let first = power.acquire();
        let second = power.acquire();
        assert_eq!(power.status()["active_inference"], 2);
        assert_eq!(power.status()["held"], false);
        drop(first);
        assert_eq!(power.status()["active_inference"], 1);
        drop(second);
        assert_eq!(power.status()["active_inference"], 0);
    }
    #[test]
    fn request_unwind_releases_its_activity() {
        let power = Power::new(false);
        let _ = std::panic::catch_unwind(|| {
            let _guard = power.acquire();
            panic!("synthetic request failure");
        });
        assert_eq!(power.status()["active_inference"], 0);
        assert_eq!(power.status()["held"], false);
    }
    #[test]
    #[ignore = "native host qualification: inspect pmset/powercfg/logind while held"]
    fn native_assertion_lives_only_until_last_request_finishes() {
        let power = Power::new(true);
        let first = power.acquire();
        let second = power.acquire();
        eventually(|| power.status()["held"] == true);
        assert_eq!(power.status()["held"], true, "{}", power.status());
        drop(first);
        assert_eq!(power.status()["held"], true);
        std::thread::sleep(Duration::from_secs(2));
        drop(second);
        eventually(|| power.status()["held"] == false);
    }
    #[test]
    #[ignore = "private child fixture for native process-death qualification"]
    fn native_crash_child() {
        if std::env::var_os("GOBSTOPPER_POWER_TEST_CHILD").is_none() {
            return;
        }
        use std::io::{Read, Write};
        let power = Power::new(true);
        let _guard = power.acquire();
        eventually(|| power.status()["held"] == true);
        assert_eq!(power.status()["held"], true, "{}", power.status());
        println!("gobstopper-power-ready");
        std::io::stdout().flush().unwrap();
        let _ = std::io::stdin().read_exact(&mut [0u8; 1]);
    }

    #[test]
    #[cfg(target_os = "macos")]
    #[ignore = "native macOS qualification: creates and terminates its own test child"]
    fn macos_assertion_is_visible_and_released_after_process_death() {
        use std::io::{BufRead, BufReader};
        use std::process::{Child, Command, Stdio};
        struct OwnedChild(Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                if matches!(self.0.try_wait(), Ok(None)) {
                    let _ = self.0.kill();
                }
                let _ = self.0.wait();
            }
        }
        let mut child = OwnedChild(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "power::tests::native_crash_child",
                    "--nocapture",
                ])
                .env("GOBSTOPPER_POWER_TEST_CHILD", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let pid = child.0.id();
        let stdout = child.0.stdout.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if line == "gobstopper-power-ready" {
                    let _ = sender.send(());
                }
            }
        });
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("owned assertion child ready");
        let held = || {
            let output = Command::new("/usr/bin/pmset")
                .args(["-g", "assertions"])
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8_lossy(&output.stdout).lines().any(|line| {
                line.contains(&format!("pid {pid}("))
                    && line.contains("PreventUserIdleSystemSleep")
                    && line.contains("Gobstopper active inference")
            })
        };
        assert!(
            held(),
            "the owned assertion must be visible in macOS power state"
        );
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        reader.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while held() {
            assert!(
                Instant::now() < deadline,
                "macOS retained the dead process assertion"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
