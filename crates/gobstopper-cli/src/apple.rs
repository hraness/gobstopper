//! Shared Apple on-device bridge plumbing for gobstopper's local-model
//! features (the `apple` scorer and `apple` digest writer).
//!
//! Each uncached request uses one bounded, owned `--once` process. Requests
//! serialize in-process, with the deadline including queue time. Resolution for
//! an already-installed bridge follows the scorer docs:
//! `GOBSTOPPER_APPLE_BRIDGE` → sibling of
//! the gobstopper binary → `~/.local/share/gobstopper/apple-bridge`
//! (missing binaries cause mechanical fallback; inference never builds tools).
//!
//!   GOBSTOPPER_APPLE_BRIDGE     - explicit bridge binary path
//!   GOBSTOPPER_APPLE_TIMEOUT_MS - 180000 (uncached requests may pay model warm-up)

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Context;
use sha2::{Digest, Sha256};

/// Resolve an installed bridge without invoking a compiler or changing files.
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
    installed.is_file().then_some(installed)
}

pub(crate) fn timeout_ms() -> u64 {
    std::env::var("GOBSTOPPER_APPLE_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(180_000)
        .clamp(100, 600_000)
}

/// Bounded availability check. Provider diagnostics are never echoed.
pub(crate) fn available(bridge: &Path) -> bool {
    let mut command = inference_command(bridge);
    command.arg("--check");
    gobstopper_adapters::plugins::run_bounded(command, Vec::new(), 15_000, 32 * 1024)
        .ok()
        .and_then(|raw| crate::mcp::strict_json(&raw).ok())
        .is_some_and(|value| {
            value.get("available").and_then(serde_json::Value::as_bool) == Some(true)
        })
}

fn inference_command(path: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(path);
    for key in [
        "AI_GATEWAY_API_KEY",
        "GOBSTOPPER_LLM_API_KEY",
        "TYPESAFE_API_KEY",
        "GOBSTOPPER_JEV_API_KEY",
        "GOBSTOPPER_CURL_BEARER",
    ] {
        command.env_remove(key);
    }
    command
}

fn binary_identity(path: &Path) -> anyhow::Result<[u8; 32]> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|_| anyhow::anyhow!("bridge_unavailable"))?;
    const MAX: u64 = 512 * 1024 * 1024;
    let meta = file
        .metadata()
        .map_err(|_| anyhow::anyhow!("bridge_unavailable"))?;
    anyhow::ensure!(
        meta.is_file() && meta.len() <= MAX,
        "bridge_identity_invalid"
    );
    let mut reader = file.take(MAX + 1);
    let mut hasher = Sha256::new();
    let mut count = 0u64;
    let mut bytes = [0u8; 64 * 1024];
    loop {
        let n = reader
            .read(&mut bytes)
            .map_err(|_| anyhow::anyhow!("bridge_identity_invalid"))?;
        if n == 0 {
            break;
        }
        count += n as u64;
        anyhow::ensure!(count <= MAX, "bridge_identity_invalid");
        hasher.update(&bytes[..n]);
    }
    Ok(hasher.finalize().into())
}

/// Stable, owner-controlled executable paths are assumed between hashing and
/// exec. This identifies client bytes, not opaque Apple model weights.
pub(crate) struct Bridge {
    path: PathBuf,
    identity: [u8; 32],
    timeout_ms: u64,
}

impl Bridge {
    pub(crate) fn request(
        &self,
        request: &apple_foundation::Request,
    ) -> anyhow::Result<serde_json::Value> {
        use std::time::{Duration, Instant};
        static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let max = request.max_output_bytes.unwrap_or(16_384);
        anyhow::ensure!(
            !request.prompt.is_empty()
                && request.prompt.len() <= 32_768
                && request
                    .instructions
                    .as_ref()
                    .is_none_or(|s| s.len() <= 4096)
                && (1..=262_144).contains(&max),
            "inference_input_invalid"
        );
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        let _serial = loop {
            match SERIAL.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    anyhow::bail!("inference_custody_unavailable")
                }
                Err(_) if Instant::now() >= deadline => anyhow::bail!("inference_deadline"),
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        };
        anyhow::ensure!(
            binary_identity(&self.path)? == self.identity,
            "bridge_identity_changed"
        );
        let mut message = serde_json::json!({"id":1,"prompt":request.prompt,"expectJson":request.expect_json,"maxOutputBytes":max});
        if let Some(instructions) = &request.instructions {
            message["instructions"] = instructions.clone().into();
        }
        if let Some(schema) = &request.schema {
            message["schema"] = schema.clone();
        }
        let input =
            serde_json::to_vec(&message).map_err(|_| anyhow::anyhow!("inference_input_invalid"))?;
        anyhow::ensure!(input.len() <= 64 * 1024, "inference_input_invalid");
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        anyhow::ensure!(left > 0, "inference_deadline");
        let mut command = inference_command(&self.path);
        command.arg("--once");
        let raw =
            gobstopper_adapters::plugins::run_bounded_inference(command, input, left, max + 4096)
                .map_err(|_| anyhow::anyhow!("inference_process_failed"))?;
        let envelope = crate::mcp::strict_json(&raw)
            .map_err(|_| anyhow::anyhow!("inference_response_invalid"))?;
        anyhow::ensure!(
            envelope.get("id").and_then(serde_json::Value::as_u64) == Some(1)
                && envelope.get("ok").and_then(serde_json::Value::as_bool) == Some(true)
                && envelope.get("error").is_none(),
            "inference_response_invalid"
        );
        let value = envelope
            .get("value")
            .context("inference_response_invalid")?;
        anyhow::ensure!(value.to_string().len() <= max, "inference_response_invalid");
        Ok(value.clone())
    }
}

/// Construct an identified client; no process starts until an uncached request.
pub(crate) fn shared_bridge(bridge: &Path, timeout_ms: u64) -> Option<Bridge> {
    let path = bridge.canonicalize().ok()?;
    let identity = binary_identity(&path).ok()?;
    Some(Bridge {
        path,
        identity,
        timeout_ms: timeout_ms.clamp(100, 600_000),
    })
}

/// Process-local response cache: task, prompt, instructions, schema and
/// identified bridge bytes must match. Opaque Apple model weights are not
/// attested by this identity. At most 64 validated bounded values survive,
/// with oldest-first eviction. GOBSTOPPER_APPLE_CACHE=0 disables both paths.
const CACHE_MAX: usize = 64;
type CacheKey = [u8; 32];

struct CacheEntry {
    sequence: u64,
    value: serde_json::Value,
}

#[derive(Default)]
struct ResponseCache {
    entries: HashMap<CacheKey, CacheEntry>,
    next_sequence: u64,
}

impl ResponseCache {
    fn get(&self, key: &CacheKey) -> Option<serde_json::Value> {
        self.entries.get(key).map(|entry| entry.value.clone())
    }

    fn put(&mut self, key: CacheKey, value: serde_json::Value) {
        if self.entries.len() >= CACHE_MAX && !self.entries.contains_key(&key) {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.sequence)
                .map(|(key, _)| *key)
            {
                self.entries.remove(&oldest);
            }
        }
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.entries.insert(
            key,
            CacheEntry {
                sequence: self.next_sequence,
                value,
            },
        );
    }
}

fn cache_enabled(raw: Option<&str>) -> bool {
    raw != Some("0")
}

fn apple_cache_enabled() -> bool {
    cache_enabled(std::env::var("GOBSTOPPER_APPLE_CACHE").ok().as_deref())
}

pub(crate) fn cache_get(
    bridge: &Bridge,
    task: &str,
    request: &apple_foundation::Request,
) -> Option<serde_json::Value> {
    if !apple_cache_enabled() {
        return None;
    }
    response_cache()
        .lock()
        .ok()?
        .get(&cache_key(bridge, task, request))
}

/// Call only after task-specific response validation. Failed/partial generation
/// never publishes a cache entry.
pub(crate) fn cache_put(
    bridge: &Bridge,
    task: &str,
    request: &apple_foundation::Request,
    value: &serde_json::Value,
) {
    if !apple_cache_enabled() {
        return;
    }
    if let Ok(mut cache) = response_cache().lock() {
        cache.put(cache_key(bridge, task, request), value.clone());
    }
}

fn response_cache() -> &'static std::sync::Mutex<ResponseCache> {
    static CACHE: OnceLock<std::sync::Mutex<ResponseCache>> = OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(ResponseCache::default()))
}

fn cache_key(bridge: &Bridge, task: &str, request: &apple_foundation::Request) -> CacheKey {
    let shape = serde_json::json!([
        "gobstopper-apple-v2",
        task,
        bridge.path,
        bridge.identity,
        request.instructions,
        request.schema,
        request.expect_json,
        request.max_output_bytes
    ]);
    let schema = shape.to_string();
    let mut hasher = Sha256::new();
    for part in [schema.as_bytes(), request.prompt.as_bytes()] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// Head+tail window of a record: errors tend to sit at the end of tool
/// output, so the tail is kept alongside the opening context.
pub(crate) fn excerpt(line: &str, max_bytes: usize) -> String {
    if line.len() <= max_bytes {
        return line.to_string();
    }
    let marker = format!(" …[{} bytes elided]… ", line.len());
    if marker.len() >= max_bytes {
        return head_bytes(line, max_bytes).to_string();
    }
    let available = max_bytes - marker.len();
    let head = (available * 3) / 4;
    let tail = available - head;
    format!(
        "{}{marker}{}",
        head_bytes(line, head),
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
    transcript: &gobstopper_core::Transcript,
    wanted: &[usize],
    item_bytes: usize,
    total_bytes: usize,
) -> anyhow::Result<Vec<(usize, String)>> {
    anyhow::ensure!(
        wanted.len() <= 256 && item_bytes <= 2048 && total_bytes <= 256 * 2048,
        "excerpt_bounds_invalid"
    );
    if wanted.is_empty() || item_bytes == 0 || total_bytes == 0 {
        return Ok(Vec::new());
    }
    use gobstopper_adapters::{claude, codex, devin, transaction};
    use gobstopper_core::Provider;
    let handle = transcript.session.clone();
    let bytes = match handle.provider {
        Provider::Devin => devin::export_bytes(&handle.path, &handle.session_id)
            .map_err(|_| anyhow::anyhow!("excerpt_source_unavailable"))?,
        _ => transaction::read(&handle.path)
            .map_err(|_| anyhow::anyhow!("excerpt_source_unavailable"))?,
    };
    let current = match handle.provider {
        Provider::Codex => codex::load_bytes(handle, &bytes),
        Provider::ClaudeCode => claude::load_bytes(handle, &bytes),
        Provider::Devin => devin::load_bytes(handle, &bytes),
    }
    .map_err(|_| anyhow::anyhow!("excerpt_source_invalid"))?;
    anyhow::ensure!(
        serde_json::to_vec(&current.items)? == serde_json::to_vec(&transcript.items)?,
        "excerpt_source_changed"
    );
    let wanted: HashSet<usize> = wanted.iter().copied().collect();
    anyhow::ensure!(
        wanted.iter().all(
            |line| current.items.iter().any(|item| item.line_index == *line
                && item.elidable_bytes.is_some()
                && item.payload_sha256.is_some())
        ),
        "excerpt_identity_unavailable"
    );
    let mut out = Vec::new();
    let mut budget = total_bytes;
    for (idx, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        if wanted.contains(&idx) && budget > 0 {
            let raw =
                std::str::from_utf8(line).map_err(|_| anyhow::anyhow!("excerpt_source_invalid"))?;
            let e = excerpt(raw.trim_end(), item_bytes.min(budget));
            budget -= e.len();
            out.push((idx, e));
        }
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) fn test_bridge() -> Bridge {
    Bridge {
        path: PathBuf::from("/synthetic/must-not-spawn"),
        identity: [0; 32],
        timeout_ms: 100,
    }
}

#[cfg(test)]
pub(crate) struct BridgeFixture {
    pub root: PathBuf,
    pub bridge: Bridge,
}
#[cfg(test)]
impl BridgeFixture {
    pub(crate) fn new(body: &str) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "gobstopper-inference-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("provider");
        std::fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let bridge = shared_bridge(&path, 3000).unwrap();
        Self { root, bridge }
    }
    pub(crate) fn reply(reply: &str) -> Self {
        Self::new(&format!(
            "cat >/dev/null\nprintf '%s\\n' '{}'",
            reply.replace('\'', "'\\''")
        ))
    }
}
#[cfg(test)]
impl Drop for BridgeFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
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
        let bridge = test_bridge();
        let request = apple_foundation::Request::guided(
            "cache-test-prompt-1",
            serde_json::json!({"kind":"a"}),
        );
        let mut changed = request.clone();
        changed.schema = Some(serde_json::json!({"kind":"b"}));
        let value = serde_json::json!({"digest":{}});
        cache_put(&bridge, "fixture", &request, &value);
        assert_eq!(cache_get(&bridge, "fixture", &request), Some(value));
        assert_eq!(cache_get(&bridge, "fixture", &changed), None);
        assert_eq!(cache_get(&bridge, "other", &request), None);
    }

    #[test]
    fn cache_stays_bounded_and_evicts_oldest_only() {
        let mut cache = ResponseCache::default();
        for i in 0..CACHE_MAX {
            cache.put([i as u8; 32], serde_json::json!(i));
        }
        cache.put([255; 32], serde_json::json!(255));
        assert_eq!(cache.entries.len(), CACHE_MAX);
        assert!(cache.get(&[0; 32]).is_none());
        assert_eq!(cache.get(&[1; 32]), Some(serde_json::json!(1)));
        assert_eq!(cache.get(&[255; 32]), Some(serde_json::json!(255)));
    }

    #[test]
    fn cache_disable_value_applies_to_reads_and_writes() {
        assert!(cache_enabled(None));
        assert!(cache_enabled(Some("1")));
        assert!(cache_enabled(Some("false")));
        assert!(!cache_enabled(Some("0")));
    }

    #[test]
    fn cache_key_is_stable_and_contract_isolated() {
        let bridge = test_bridge();
        let request = apple_foundation::Request::guided("prompt", serde_json::json!({"kind":"a"}));
        let key = cache_key(&bridge, "score", &request);
        assert_eq!(key, cache_key(&bridge, "score", &request));
        let mut changed = request.clone();
        changed.instructions = Some("new instructions".into());
        assert_ne!(key, cache_key(&bridge, "score", &changed));
        assert_ne!(key, cache_key(&bridge, "digest", &request));
        let mut changed_bridge = test_bridge();
        changed_bridge.identity = [1; 32];
        assert_ne!(key, cache_key(&changed_bridge, "score", &request));
    }

    #[test]
    fn bounded_bridge_rejects_malformed_flood_hang_and_private_errors() {
        let request =
            apple_foundation::Request::guided("synthetic", serde_json::json!({"type":"object"}));
        let good = BridgeFixture::reply(r#"{"id":1,"ok":true,"value":{"scores":{"p_0":0.8}}}"#);
        assert_eq!(good.bridge.request(&request).unwrap()["scores"]["p_0"], 0.8);
        for reply in [
            r#"{"id":2,"ok":true,"value":{}}"#,
            r#"{"id":1,"ok":false,"error":{"code":"PRIVATE_SENTINEL"}}"#,
            r#"{"id":1,"id":1,"ok":true,"value":{}}"#,
            "PRIVATE_SENTINEL invalid json",
        ] {
            let fixture = BridgeFixture::reply(reply);
            let error = fixture.bridge.request(&request).unwrap_err().to_string();
            assert!(!error.contains("PRIVATE_SENTINEL"));
            assert_eq!(error, "inference_response_invalid");
        }
        for body in [
            "cat >/dev/null\nexec sleep 10",
            "cat >/dev/null\nyes x | head -c 200000",
        ] {
            let mut fixture = BridgeFixture::new(body);
            fixture.bridge.timeout_ms = 300;
            let start = std::time::Instant::now();
            assert!(fixture.bridge.request(&request).is_err());
            assert!(start.elapsed() < std::time::Duration::from_secs(2));
        }
    }

    #[test]
    fn bridge_timeout_collects_owned_descendant_before_return() {
        let mut fixture = BridgeFixture::new(
            "cat >/dev/null\n(sleep 1; printf survived > \"$0-survived\") &\nsleep 10",
        );
        fixture.bridge.timeout_ms = 300;
        let start = std::time::Instant::now();
        let request = apple_foundation::Request::text("synthetic");
        assert!(fixture.bridge.request(&request).is_err());
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(!fixture.root.join("provider-survived").exists());
    }

    #[test]
    fn excerpts_bind_to_one_normalized_source_and_exact_byte_budgets() {
        let fixture = BridgeFixture::reply("{}");
        let path = fixture.root.join("rollout-synthetic.jsonl");
        let raw = format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"synthetic\"}}}}\n{{\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call_output\",\"call_id\":\"a\",\"output\":\"{}\"}}}}\n", "é".repeat(2048));
        std::fs::write(&path, &raw).unwrap();
        let handle = gobstopper_core::SessionHandle {
            provider: gobstopper_core::Provider::Codex,
            session_id: "synthetic".into(),
            path: path.clone(),
            cwd: None,
            age_secs: 0,
        };
        let transcript = gobstopper_adapters::codex::load_bytes(handle, raw.as_bytes()).unwrap();
        let excerpts = read_excerpts(&transcript, &[1], 101, 101).unwrap();
        assert_eq!(excerpts.len(), 1);
        assert!(excerpts[0].1.len() <= 101);
        assert!(read_excerpts(&transcript, &[0], 100, 100).is_err());
        std::fs::write(&path, raw.replace(&"é".repeat(2048), &"z".repeat(4096))).unwrap();
        assert_eq!(
            read_excerpts(&transcript, &[1], 100, 100)
                .unwrap_err()
                .to_string(),
            "excerpt_source_changed"
        );
        for size in 0..80 {
            assert!(excerpt(&"é".repeat(100), size).len() <= size);
        }
    }
}
