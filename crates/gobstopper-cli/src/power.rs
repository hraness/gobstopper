//! Process-owned idle-sleep inhibition while model requests are in flight.
//!
//! The last request releases the assertion. Operating-system ownership (or
//! the Linux helper's EOF-bound stdin) also releases it if the proxy dies.
//! Failures are observable but never interfere with forwarding a request.

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const RETRY_DELAY: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct Power {
    inner: Arc<Mutex<State>>,
}

struct State {
    enabled: bool,
    active: usize,
    assertion: Option<platform::Assertion>,
    last_error: Option<String>,
    retry_after: Option<Instant>,
}

pub struct Guard {
    inner: Arc<Mutex<State>>,
}

impl Power {
    pub fn new(enabled: bool) -> Self {
        Self {
            inner: Arc::new(Mutex::new(State {
                enabled,
                active: 0,
                assertion: None,
                last_error: None,
                retry_after: None,
            })),
        }
    }

    /// Call only for a validated model-inference request, after rejecting
    /// local budget errors. Keep the guard through all upstream attempts.
    pub fn acquire(&self) -> Guard {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        state.active = state.active.saturating_add(1);
        if state.enabled {
            if let Some(assertion) = state.assertion.as_mut() {
                if !assertion.alive() {
                    state.assertion = None;
                    state.last_error = Some("sleep-inhibition helper exited".into());
                    state.retry_after = Some(Instant::now() + RETRY_DELAY);
                }
            }
            if state.assertion.is_none() && state.retry_after.is_none_or(|at| Instant::now() >= at)
            {
                match platform::Assertion::acquire() {
                    Ok(assertion) => {
                        state.assertion = Some(assertion);
                        state.last_error = None;
                        state.retry_after = None;
                    }
                    Err(error) => {
                        state.last_error = Some(error);
                        state.retry_after = Some(Instant::now() + RETRY_DELAY);
                    }
                }
            }
        }
        Guard {
            inner: Arc::clone(&self.inner),
        }
    }

    pub fn status(&self) -> Value {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if state.assertion.as_mut().is_some_and(|a| !a.alive()) {
            state.assertion = None;
            state.last_error = Some("sleep-inhibition helper exited".into());
            state.retry_after = Some(Instant::now() + RETRY_DELAY);
        }
        json!({
            "enabled": state.enabled,
            "backend": platform::NAME,
            "active_inference": state.active,
            "held": state.assertion.is_some(),
            "last_error": state.last_error,
            "scope": if cfg!(target_os = "linux") { "logind-idle-action" } else { "idle-system-sleep" },
        })
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        state.active = state.active.saturating_sub(1);
        if state.active == 0 {
            // Drop while holding the lock: another request cannot begin a new
            // assertion until the old one has actually been released.
            state.assertion = None;
        }
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
    // serialized by Power's mutex and Windows power handles are thread-safe.
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
        assert_eq!(power.status()["held"], true, "{}", power.status());
        drop(first);
        assert_eq!(power.status()["held"], true);
        std::thread::sleep(Duration::from_secs(2));
        drop(second);
        assert_eq!(power.status()["held"], false);
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
