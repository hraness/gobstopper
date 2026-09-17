//! Session discovery: locate Codex rollouts and Claude Code sessions on
//! disk and classify them by recency.

use gobstopper_core::model::{SessionHandle, UsageSample};
use gobstopper_core::Provider;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{claude, codex};

/// Where to look for provider state. Overridable because managed
/// runtimes (e.g. oompa profiles) relocate these roots.
#[derive(Debug, Clone)]
pub struct Roots {
    /// `~/.codex` or `$CODEX_HOME`.
    pub codex_home: PathBuf,
    /// `~/.claude` or `$CLAUDE_CONFIG_DIR`-style root.
    pub claude_home: PathBuf,
}

impl Roots {
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let codex_home = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        let claude_home = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"));
        Self {
            codex_home,
            claude_home,
        }
    }
}

/// A discovered session plus its cheap usage read.
#[derive(Debug, Clone)]
pub struct Discovered {
    pub handle: SessionHandle,
    pub usage: UsageSample,
}

fn age_secs(path: &Path) -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|t| now.saturating_sub(t.as_secs()))
        .unwrap_or(u64::MAX)
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, out, depth - 1);
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
}

/// Sessions younger than this are worth reporting by default.
const DEFAULT_MAX_AGE_SECS: u64 = 7 * 24 * 3600;

fn handle_for(
    provider: Provider,
    path: PathBuf,
    meta: (Option<String>, Option<PathBuf>),
    age: u64,
) -> SessionHandle {
    let fallback_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".into());
    SessionHandle {
        provider,
        session_id: meta.0.unwrap_or(fallback_id),
        path,
        cwd: meta.1,
        age_secs: age,
    }
}

/// Find all sessions under the configured roots, newest first.
/// `max_age_secs` bounds reported age; `0` reports everything.
pub fn discover(roots: &Roots, max_age_secs: u64) -> Vec<Discovered> {
    discover_cached(roots, max_age_secs, &mut DiscoveryCache::default())
}

#[derive(Default)]
pub struct DiscoveryCache(std::collections::HashMap<(Provider, PathBuf), CachedSession>);
struct CachedSession {
    fingerprint: (u64, Option<SystemTime>, u64, u64, i64, i64),
    sampled: std::time::Instant,
    meta: (Option<String>, Option<PathBuf>),
    usage: UsageSample,
}
impl DiscoveryCache {
    fn inspect(
        &mut self,
        provider: Provider,
        path: &Path,
    ) -> ((Option<String>, Option<PathBuf>), UsageSample) {
        let metadata = fs::metadata(path).ok();
        let mut fingerprint = (
            metadata.as_ref().map(|m| m.len()).unwrap_or(0),
            metadata.as_ref().and_then(|m| m.modified().ok()),
            0,
            0,
            0,
            0,
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if let Some(meta) = &metadata {
                fingerprint.2 = meta.dev();
                fingerprint.3 = meta.ino();
                fingerprint.4 = meta.ctime();
                fingerprint.5 = meta.ctime_nsec();
            }
        }
        let key = (provider, path.to_path_buf());
        if let Some(entry) = self.0.get(&key) {
            if entry.fingerprint == fingerprint && entry.sampled.elapsed().as_secs() < 60 {
                return (entry.meta.clone(), entry.usage);
            }
        }
        let (meta, usage) = match provider {
            Provider::Codex => (codex::scan_meta(path), codex::scan_usage(path)),
            Provider::ClaudeCode => (claude::scan_meta(path), claude::scan_usage(path)),
        };
        if self.0.len() >= 4096 {
            self.0.clear();
        }
        self.0.insert(
            key,
            CachedSession {
                fingerprint,
                sampled: std::time::Instant::now(),
                meta: meta.clone(),
                usage,
            },
        );
        (meta, usage)
    }
}

pub fn discover_cached(
    roots: &Roots,
    max_age_secs: u64,
    cache: &mut DiscoveryCache,
) -> Vec<Discovered> {
    let limit = if max_age_secs == 0 {
        u64::MAX
    } else {
        max_age_secs
    };
    let mut found = Vec::new();

    let mut codex_files = Vec::new();
    collect_jsonl(&roots.codex_home.join("sessions"), &mut codex_files, 4);
    for path in codex_files {
        let age = age_secs(&path);
        if age > limit {
            continue;
        }
        let (meta, usage) = cache.inspect(Provider::Codex, &path);
        let handle = handle_for(Provider::Codex, path, meta, age);
        found.push(Discovered { usage, handle });
    }

    let mut claude_files = Vec::new();
    collect_jsonl(&roots.claude_home.join("projects"), &mut claude_files, 3);
    for path in claude_files {
        let age = age_secs(&path);
        if age > limit {
            continue;
        }
        let (meta, usage) = cache.inspect(Provider::ClaudeCode, &path);
        let handle = handle_for(Provider::ClaudeCode, path, meta, age);
        found.push(Discovered { usage, handle });
    }

    found.sort_by_key(|d| d.handle.age_secs);
    found
}

/// Identify the provider from file content, not filename: Codex records
/// carry `payload`/`ordinal` envelopes (`session_meta` first), Claude
/// lines carry `sessionId`/`uuid`. Returns `None` when neither matches.
pub fn sniff_provider(path: &Path) -> Option<Provider> {
    use std::io::{BufRead, BufReader};
    let file = fs::File::open(path).ok()?;
    for line in BufReader::new(file).lines().take(24) {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if v.get("payload").is_some()
            && (v.get("ordinal").is_some()
                || v.get("type").and_then(serde_json::Value::as_str) == Some("session_meta"))
        {
            return Some(Provider::Codex);
        }
        if v.get("sessionId").is_some() || v.get("session_id").is_some() {
            return Some(Provider::ClaudeCode);
        }
    }
    None
}

/// Cheap lookup: only files whose *name* contains `query` get a meta/usage
/// scan. Both providers embed the session id in the filename (Codex
/// `rollout-<ts>-<uuid>.jsonl`, Claude `<uuid>.jsonl`), so a name filter
/// misses nothing the id-prefix match would hit.
pub fn find(roots: &Roots, query: &str) -> Vec<Discovered> {
    let mut paths = Vec::new();
    collect_jsonl(&roots.codex_home.join("sessions"), &mut paths, 4);
    collect_jsonl(&roots.claude_home.join("projects"), &mut paths, 3);
    let mut found = Vec::new();
    for path in paths {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        if !name.contains(query) {
            continue;
        }
        let codex =
            name.starts_with("rollout-") || path.components().any(|c| c.as_os_str() == ".codex");
        let (provider, meta, usage) = if codex {
            (
                Provider::Codex,
                codex::scan_meta(&path),
                codex::scan_usage(&path),
            )
        } else {
            (
                Provider::ClaudeCode,
                claude::scan_meta(&path),
                claude::scan_usage(&path),
            )
        };
        let handle = handle_for(provider, path.clone(), meta, age_secs(&path));
        if handle.session_id.starts_with(query) || name.contains(query) {
            found.push(Discovered { handle, usage });
        }
    }
    found
}

/// Parse a discovered session fully.
pub fn load(d: &Discovered) -> Result<gobstopper_core::Transcript, crate::AdapterError> {
    match d.handle.provider {
        Provider::Codex => codex::load(d.handle.clone()),
        Provider::ClaudeCode => claude::load(d.handle.clone()),
    }
}

pub const fn default_max_age_secs() -> u64 {
    DEFAULT_MAX_AGE_SECS
}
