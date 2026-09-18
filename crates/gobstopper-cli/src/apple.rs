//! Shared Apple on-device bridge plumbing for gobstopper's local-model
//! features (the `apple` scorer and `apple` digest writer).
//!
//! One persistent `apple-foundation` bridge process serves every feature in
//! the process: the on-device model is serial anyway, so requests queue
//! in-process rather than fanning out. Resolution order for the bridge
//! binary mirrors the scorer docs: `GOBSTOPPER_APPLE_BRIDGE` → sibling of
//! the gobstopper binary → `~/.local/share/gobstopper/apple-bridge`
//! (auto-built via swiftc when absent).
//!
//!   GOBSTOPPER_APPLE_BRIDGE     - explicit bridge binary path
//!   GOBSTOPPER_APPLE_TIMEOUT_MS - 180000 (first request pays model warm-up)

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Bridge binary resolution. The managed share path rebuilds automatically
/// when the embedded bridge source changes; env/sibling paths are
/// user-managed and used as-is.
pub(crate) fn resolve_bridge() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("GOBSTOPPER_APPLE_BRIDGE") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.parent()?.join("apple-bridge");
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    let installed = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".local/share/gobstopper/apple-bridge"))?;
    apple_foundation::ensure_bridge(&installed).ok()
}

pub(crate) fn timeout_ms() -> u64 {
    std::env::var("GOBSTOPPER_APPLE_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(180_000)
}

/// Live availability check against the bridge binary. Prints the model's
/// reason on stderr when unavailable so every caller reports once.
pub(crate) fn available(bridge: &Path) -> bool {
    match apple_foundation::check(&[bridge.to_string_lossy().into_owned()]) {
        Ok(a) if a.available => true,
        Ok(a) => {
            eprintln!(
                "apple: model unavailable ({})",
                a.reason.as_deref().unwrap_or("unknown")
            );
            false
        }
        Err(e) => {
            eprintln!("apple: availability check failed: {e:#}");
            false
        }
    }
}

/// The process-wide bridge. Spawned once on first use; the client kills
/// and lazily respawns it after timeouts or process death.
pub(crate) fn shared_bridge(
    bridge: &Path,
    timeout_ms: u64,
) -> Option<&'static apple_foundation::Bridge> {
    static BRIDGE: OnceLock<Option<apple_foundation::Bridge>> = OnceLock::new();
    BRIDGE
        .get_or_init(|| {
            apple_foundation::Bridge::with_options(
                &[bridge.to_string_lossy().into_owned()],
                apple_foundation::Options {
                    request_timeout: std::time::Duration::from_millis(timeout_ms),
                    max_pending: 8,
                },
            )
            .ok()
        })
        .as_ref()
}
