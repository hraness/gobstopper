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

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Context;

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

/// Process-wide prompt→response cache shared by every apple feature.
/// `watch` re-evaluates an unchanged transcript each poll interval; the
/// model inputs (excerpts of immutable lines plus the tail) are identical
/// while nothing new has been appended, so a bounded cache turns repeat
/// evaluations into zero model calls. The schema text is part of the key
/// so distinct request shapes never collide. Bounded at 64 entries —
/// a stale entry only means a regenerated response, never a wrong one.
/// `GOBSTOPPER_APPLE_CACHE=0` disables reads (writes still land).
const CACHE_MAX: usize = 64;

pub(crate) fn cache_get(prompt: &str, schema: &serde_json::Value) -> Option<serde_json::Value> {
    if std::env::var("GOBSTOPPER_APPLE_CACHE").as_deref() == Ok("0") {
        return None;
    }
    response_cache()
        .lock()
        .ok()?
        .get(&cache_key(prompt, schema))
        .cloned()
}

pub(crate) fn cache_put(prompt: &str, schema: &serde_json::Value, value: &serde_json::Value) {
    if let Ok(mut c) = response_cache().lock() {
        if c.len() >= CACHE_MAX {
            c.clear();
        }
        c.insert(cache_key(prompt, schema), value.clone());
    }
}

fn response_cache() -> &'static std::sync::Mutex<std::collections::HashMap<u64, serde_json::Value>>
{
    static CACHE: OnceLock<std::sync::Mutex<std::collections::HashMap<u64, serde_json::Value>>> =
        OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn cache_key(prompt: &str, schema: &serde_json::Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    schema.to_string().hash(&mut h);
    prompt.hash(&mut h);
    h.finish()
}

/// Head+tail window of a record: errors tend to sit at the end of tool
/// output, so the tail is kept alongside the opening context.
pub(crate) fn excerpt(line: &str, max_bytes: usize) -> String {
    if line.len() <= max_bytes {
        return line.to_string();
    }
    let head = (max_bytes * 3) / 4;
    let tail = max_bytes - head;
    format!(
        "{} …[{} bytes elided]… {}",
        head_bytes(line, head),
        line.len(),
        tail_bytes(line, tail)
    )
}

fn head_bytes(s: &str, n: usize) -> &str {
    let mut end = s.len().min(n);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn tail_bytes(s: &str, n: usize) -> &str {
    let mut start = s.len().saturating_sub(n);
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// Pull the given JSONL line numbers out of a transcript file, each
/// bounded to `item_bytes`, until the excerpt budget is spent.
pub(crate) fn read_excerpts(
    path: &Path,
    wanted: &[usize],
    item_bytes: usize,
    total_bytes: usize,
) -> anyhow::Result<Vec<(usize, String)>> {
    let file =
        std::fs::File::open(path).with_context(|| format!("open transcript {}", path.display()))?;
    let wanted: HashSet<usize> = wanted.iter().copied().collect();
    let last = wanted.iter().copied().max().unwrap_or(0);
    let mut out = Vec::new();
    let mut budget = total_bytes;
    let mut line = String::new();
    let mut reader = BufReader::new(file);
    let mut idx = 0usize;
    while idx <= last && reader.read_line(&mut line)? > 0 {
        if wanted.contains(&idx) && budget > 256 {
            let e = excerpt(line.trim_end(), item_bytes.min(budget));
            budget = budget.saturating_sub(e.len());
            out.push((idx, e));
        }
        line.clear();
        idx += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The response cache is a process-wide static — serialize these tests
    // so one cannot evict the other's entries mid-assertion.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn cache_round_trip_and_schema_isolation() {
        let _g = LOCK.lock().unwrap();
        let schema_a = serde_json::json!({"kind": "a"});
        let schema_b = serde_json::json!({"kind": "b"});
        let v = serde_json::json!({"digest": {}});
        cache_put("cache-test-prompt-1", &schema_a, &v);
        assert_eq!(cache_get("cache-test-prompt-1", &schema_a), Some(v));
        assert_eq!(cache_get("cache-test-prompt-1", &schema_b), None);
        assert_eq!(cache_get("cache-test-prompt-2", &schema_a), None);
    }

    #[test]
    fn cache_stays_bounded() {
        let _g = LOCK.lock().unwrap();
        let schema = serde_json::json!({});
        for i in 0..(CACHE_MAX + 16) {
            cache_put(&format!("bound-{i}"), &schema, &serde_json::json!(i));
        }
        assert!(response_cache().lock().unwrap().len() <= CACHE_MAX);
    }
}
