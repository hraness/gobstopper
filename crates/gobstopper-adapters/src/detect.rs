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

/// Closed, numeric discovery coverage. Success of a caller is not proof that
/// every configured source was readable or fit within the scan limits.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderDiscovery {
    pub provider: Provider,
    pub source_state: &'static str,
    pub scanned: usize,
    pub selected: usize,
    pub invalid_records: usize,
    pub io_errors: usize,
    pub omitted: usize,
    pub truncated: bool,
}

impl ProviderDiscovery {
    pub fn new(provider: Provider) -> Self {
        Self {
            provider,
            source_state: "available",
            scanned: 0,
            selected: 0,
            invalid_records: 0,
            io_errors: 0,
            omitted: 0,
            truncated: false,
        }
    }
}

#[derive(Debug)]
pub struct DiscoveryResult {
    pub sessions: Vec<Discovered>,
    pub providers: Vec<ProviderDiscovery>,
}

/// One canonical store/session binding shared by reports and retained evidence.
/// This resolves metadata only; it never reads provider content.
pub fn source_identity(handle: &SessionHandle) -> std::io::Result<String> {
    let path = fs::canonicalize(&handle.path)?;
    if !fs::metadata(&path)?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "source is not a regular file",
        ));
    }
    Ok(crate::copy::sha256(
        &serde_json::to_vec(&(handle.provider, &handle.session_id, path))
            .map_err(std::io::Error::other)?,
    ))
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
    collect_jsonl_status(
        dir,
        out,
        depth,
        &mut ProviderDiscovery::new(Provider::Codex),
        &mut 0,
    );
}

fn collect_jsonl_status(
    dir: &Path,
    out: &mut Vec<PathBuf>,
    depth: usize,
    status: &mut ProviderDiscovery,
    visited: &mut usize,
) {
    const MAX_ENTRIES: usize = 100_000;
    if depth == 0 || *visited >= MAX_ENTRIES {
        status.truncated = true;
        status.omitted += 1;
        return;
    }
    match fs::symlink_metadata(dir) {
        Ok(metadata) if metadata.is_dir() => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if *visited == 0 {
                status.source_state = "missing";
            } else {
                status.io_errors += 1;
            }
            return;
        }
        _ => {
            status.source_state = "unavailable";
            status.io_errors += 1;
            return;
        }
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => {
            status.io_errors += 1;
            status.source_state = "unavailable";
            return;
        }
    };
    for entry in entries {
        if *visited >= MAX_ENTRIES {
            status.truncated = true;
            status.omitted += 1;
            break;
        }
        *visited += 1;
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                status.io_errors += 1;
                continue;
            }
        };
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(_) => {
                status.io_errors += 1;
                continue;
            }
        };
        let path = entry.path();
        if kind.is_dir() {
            collect_jsonl_status(&path, out, depth - 1, status, visited);
        } else if kind.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        } else if kind.is_symlink() {
            status.omitted += 1;
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
    discover_cached(
        roots,
        max_age_secs,
        &mut DiscoveryCache::default(),
        None,
        false,
    )
}

/// Per-process cache of bounded session metadata and usage scans, keyed
/// by provider and path. An entry is served again while the file's
/// fingerprint is unchanged: length, mtime and, on Unix, device, inode
/// and ctime. Those identity fields are the same evidence the watch
/// loop's persisted `settled` fingerprints rely on, so an identified
/// entry has no time bound; an unchanged transcript is not reread every
/// pass. Without identity fields (no metadata, or a non-Unix host) the
/// entry expires after [`DiscoveryCache::SAMPLE_TTL_SECS`] seconds.
#[derive(Default)]
pub struct DiscoveryCache {
    files: std::collections::HashMap<(Provider, PathBuf), CachedSession>,
}
struct CachedSession {
    fingerprint: (u64, Option<SystemTime>, u64, u64, i64, i64),
    /// True when the fingerprint carries device, inode and ctime, so an
    /// equal fingerprint alone shows the bytes are unchanged.
    identified: bool,
    sampled: std::time::Instant,
    meta: (Option<String>, Option<PathBuf>),
    usage: UsageSample,
}
/// Serializable form of one [`CachedSession`]. Path and usage are the same
/// information class the provider's own directory listing already exposes.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct DiscoveryCacheRow {
    pub path: PathBuf,
    pub len: u64,
    pub mtime: Option<(i64, u32)>,
    pub dev: u64,
    pub ino: u64,
    pub ctime: i64,
    pub ctime_nanos: i64,
    pub identified: bool,
    pub sampled_unix: i64,
    pub meta_id: Option<String>,
    pub meta_path: Option<PathBuf>,
    pub usage: UsageSample,
}

/// On-disk snapshot written by watcher lanes beside their watch state; a
/// cold one-shot read consumes it to skip content rescans of unchanged
/// files. `schema` changes evict the whole snapshot rather than merge.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct DiscoveryCacheFile {
    pub schema: String,
    pub provider: String,
    pub written_unix: u64,
    #[serde(default)]
    pub entries: Vec<DiscoveryCacheRow>,
}

impl DiscoveryCacheFile {
    pub const SCHEMA: &'static str = "gobstopper.discovery-cache.v1";
}

impl DiscoveryCache {
    /// Validity bound for entries whose fingerprint lacks identity fields.
    const SAMPLE_TTL_SECS: u64 = 60;
    /// Entries kept before the cache is dropped and rebuilt; a corpus
    /// larger than this bound rescans once per pass.
    const MAX_ENTRIES: usize = 4096;

    /// Rows for `provider` in a form that survives a process restart.
    /// Persisted discovery is advisory only: a stale or corrupt snapshot
    /// simply costs a rescan, exactly like a cold start.
    pub fn persist_rows(&self, provider: Provider) -> Vec<DiscoveryCacheRow> {
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        self.files
            .iter()
            .filter(|((p, _), _)| *p == provider)
            .map(|((_, path), e)| DiscoveryCacheRow {
                path: path.clone(),
                len: e.fingerprint.0,
                mtime: e
                    .fingerprint
                    .1
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| (d.as_secs() as i64, d.subsec_nanos())),
                dev: e.fingerprint.2,
                ino: e.fingerprint.3,
                ctime: e.fingerprint.4,
                ctime_nanos: e.fingerprint.5,
                identified: e.identified,
                sampled_unix: epoch - e.sampled.elapsed().as_secs() as i64,
                meta_id: e.meta.0.clone(),
                meta_path: e.meta.1.clone(),
                usage: e.usage,
            })
            .collect()
    }

    /// Merge rows from a persisted snapshot. Non-identified rows expire
    /// against [`DiscoveryCache::SAMPLE_TTL_SECS`] measured from the
    /// recorded wall-clock sample, never from load time.
    pub fn merge_persisted(&mut self, provider: Provider, rows: Vec<DiscoveryCacheRow>) {
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        for row in rows {
            if self.files.len() >= Self::MAX_ENTRIES {
                break;
            }
            let mtime = row
                .mtime
                .map(|(s, n)| std::time::UNIX_EPOCH + std::time::Duration::new(s as u64, n));
            let age = (epoch - row.sampled_unix).max(0) as u64;
            let sampled = std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(age))
                .unwrap_or_else(std::time::Instant::now);
            self.files.insert(
                (provider, row.path),
                CachedSession {
                    fingerprint: (row.len, mtime, row.dev, row.ino, row.ctime, row.ctime_nanos),
                    identified: row.identified,
                    sampled,
                    meta: (row.meta_id, row.meta_path),
                    usage: row.usage,
                },
            );
        }
    }

    fn inspect(
        &mut self,
        provider: Provider,
        path: &Path,
    ) -> ((Option<String>, Option<PathBuf>), UsageSample) {
        self.inspect_at(provider, path, std::time::Instant::now())
    }

    fn inspect_at(
        &mut self,
        provider: Provider,
        path: &Path,
        now: std::time::Instant,
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
        let mut identified = false;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if let Some(meta) = &metadata {
                fingerprint.2 = meta.dev();
                fingerprint.3 = meta.ino();
                fingerprint.4 = meta.ctime();
                fingerprint.5 = meta.ctime_nsec();
                identified = true;
            }
        }
        let key = (provider, path.to_path_buf());
        if let Some(entry) = self.files.get(&key) {
            let fresh = entry.identified
                || now.saturating_duration_since(entry.sampled).as_secs() < Self::SAMPLE_TTL_SECS;
            if entry.fingerprint == fingerprint && fresh {
                return (entry.meta.clone(), entry.usage);
            }
        }
        let (meta, usage) = match provider {
            Provider::Codex => (codex::scan_meta(path), codex::scan_usage(path)),
            Provider::ClaudeCode => (claude::scan_meta(path), claude::scan_usage(path)),
        };
        if self.files.len() >= Self::MAX_ENTRIES {
            self.files.clear();
        }
        self.files.insert(
            key,
            CachedSession {
                fingerprint,
                identified,
                sampled: now,
                meta: meta.clone(),
                usage,
            },
        );
        (meta, usage)
    }
}

/// `provider` scopes the scan to one provider so `watch --provider
/// claude_code` never touches Codex roots (and vice versa). When
/// `context_only` is set, providers may return a `context_tokens`-only
/// `UsageSample` — lifetime counters can be left zero.
pub fn discover_cached(
    roots: &Roots,
    max_age_secs: u64,
    cache: &mut DiscoveryCache,
    provider: Option<Provider>,
    context_only: bool,
) -> Vec<Discovered> {
    discover_cached_with_status(roots, max_age_secs, cache, provider, context_only).sessions
}

pub fn discover_cached_with_status(
    roots: &Roots,
    max_age_secs: u64,
    cache: &mut DiscoveryCache,
    provider: Option<Provider>,
    _context_only: bool,
) -> DiscoveryResult {
    let limit = if max_age_secs == 0 {
        u64::MAX
    } else {
        max_age_secs
    };
    let mut found = Vec::new();
    let mut providers = Vec::new();

    if provider.is_none_or(|p| p == Provider::Codex) {
        let mut codex_files = Vec::new();
        let mut status = ProviderDiscovery::new(Provider::Codex);
        collect_jsonl_status(
            &roots.codex_home.join("sessions"),
            &mut codex_files,
            4,
            &mut status,
            &mut 0,
        );
        status.scanned = codex_files.len();
        for path in codex_files {
            let age = age_secs(&path);
            if age == u64::MAX {
                status.io_errors += 1;
                continue;
            }
            if age > limit {
                continue;
            }
            let (meta, usage) = cache.inspect(Provider::Codex, &path);
            status.invalid_records += usize::from(meta.0.is_none());
            status.selected += 1;
            let handle = handle_for(Provider::Codex, path, meta, age);
            found.push(Discovered { usage, handle });
        }
        providers.push(status);
    }

    if provider.is_none_or(|p| p == Provider::ClaudeCode) {
        let mut claude_files = Vec::new();
        let mut status = ProviderDiscovery::new(Provider::ClaudeCode);
        collect_jsonl_status(
            &roots.claude_home.join("projects"),
            &mut claude_files,
            3,
            &mut status,
            &mut 0,
        );
        status.scanned = claude_files.len();
        // Provider-reported ownership beats file mtime: a session open
        // in a TUI can go quiet for minutes — `live_sessions` marks it
        // active anyway so `auto` keeps delegating instead of rewriting
        // a file the provider still owns.
        let live = claude::live_sessions(&roots.claude_home);
        for path in claude_files {
            let age = age_secs(&path);
            if age == u64::MAX {
                status.io_errors += 1;
                continue;
            }
            if age > limit {
                // Quiet files may still belong to a live provider. Inspect
                // only bounded metadata before excluding them; nonlive old
                // files never receive a usage/history scan.
                if live.is_empty()
                    || !claude::scan_meta(&path)
                        .0
                        .as_ref()
                        .is_some_and(|id| live.contains_key(id))
                {
                    continue;
                }
            }
            let (meta, usage) = cache.inspect(Provider::ClaudeCode, &path);
            status.invalid_records += usize::from(meta.0.is_none());
            status.selected += 1;
            let mut handle = handle_for(Provider::ClaudeCode, path, meta, age);
            if live.contains_key(&handle.session_id) {
                handle.age_secs = 0;
            }
            found.push(Discovered { usage, handle });
        }
        providers.push(status);
    }

    found.sort_by_key(|d| d.handle.age_secs);
    DiscoveryResult {
        sessions: found,
        providers,
    }
}

/// Identify the provider from file content, not filename: Codex records
/// carry typed `payload` envelopes, optionally with `ordinal`; an export
/// can start after session_meta or contain usage records alone. Claude
/// lines carry `sessionId`/`uuid`. Returns `None` when neither matches.
pub fn sniff_provider(path: &Path) -> Option<Provider> {
    for v in crate::payload::head_records(path, 24) {
        let ty = v.get("type").and_then(serde_json::Value::as_str);
        if v.get("payload").is_some()
            && (v.get("ordinal").is_some()
                || matches!(
                    ty,
                    Some(
                        "session_meta"
                            | "response_item"
                            | "compacted"
                            | "token_usage_record"
                            | "event_msg"
                            | "turn_context"
                    )
                ))
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
    let mut codex_paths = Vec::new();
    let mut claude_paths = Vec::new();
    collect_jsonl(&roots.codex_home.join("sessions"), &mut codex_paths, 4);
    collect_jsonl(&roots.claude_home.join("projects"), &mut claude_paths, 3);
    let mut found = Vec::new();
    // The configured root already establishes the provider. Do not
    // discard that identity and guess from a filename or default-home
    // component: managed homes and detached filenames need neither.
    let paths = codex_paths
        .into_iter()
        .map(|path| (Provider::Codex, path))
        .chain(
            claude_paths
                .into_iter()
                .map(|path| (Provider::ClaudeCode, path)),
        );
    for (provider, path) in paths {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        if !name.contains(query) {
            continue;
        }
        let (meta, usage) = if provider == Provider::Codex {
            (codex::scan_meta(&path), codex::scan_usage(&path))
        } else {
            (claude::scan_meta(&path), claude::scan_usage(&path))
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

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "gobstopper-discovery-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_unavailable_and_scan_limits_are_explicit() {
        let fixture = Fixture::new();
        let mut paths = Vec::new();
        let mut status = ProviderDiscovery::new(Provider::Codex);
        collect_jsonl_status(
            &fixture.0.join("missing"),
            &mut paths,
            4,
            &mut status,
            &mut 0,
        );
        assert_eq!(status.source_state, "missing");
        let file = fixture.0.join("file");
        fs::write(&file, b"synthetic").unwrap();
        let mut status = ProviderDiscovery::new(Provider::Codex);
        collect_jsonl_status(&file, &mut paths, 4, &mut status, &mut 0);
        assert_eq!(status.source_state, "unavailable");
        assert_eq!(status.io_errors, 1);
        let mut status = ProviderDiscovery::new(Provider::Codex);
        collect_jsonl_status(&fixture.0, &mut paths, 0, &mut status, &mut 0);
        assert!(status.truncated);
        assert_eq!(status.omitted, 1);
        let mut status = ProviderDiscovery::new(Provider::Codex);
        collect_jsonl_status(&fixture.0, &mut paths, 4, &mut status, &mut 100_000);
        assert!(status.truncated);
        assert!(paths.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn canonical_identity_unifies_aliases_but_discovery_does_not_follow_symlinks() {
        let fixture = Fixture::new();
        let file = fixture.0.join("source.jsonl");
        fs::write(&file, b"synthetic").unwrap();
        let alias = fixture.0.join("alias.jsonl");
        std::os::unix::fs::symlink(&file, &alias).unwrap();
        let mut handle = SessionHandle {
            provider: Provider::Codex,
            session_id: "id".into(),
            path: file,
            cwd: None,
            age_secs: 0,
        };
        let identity = source_identity(&handle).unwrap();
        handle.path = alias;
        assert_eq!(source_identity(&handle).unwrap(), identity);
        handle.session_id = "foreign".into();
        assert_ne!(source_identity(&handle).unwrap(), identity);
        let mut paths = Vec::new();
        let mut status = ProviderDiscovery::new(Provider::Codex);
        collect_jsonl_status(&fixture.0, &mut paths, 4, &mut status, &mut 0);
        assert_eq!(paths.len(), 1);
        assert_eq!(status.omitted, 1);
    }

    #[test]
    fn quiet_live_claude_session_survives_age_filter_without_scanning_old_usage() {
        let fixture = Fixture::new();
        let home = fixture.0.join("claude");
        let project = home.join("projects/synthetic");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(home.join("sessions")).unwrap();
        for session in ["live", "old"] {
            let path = project.join(format!("{session}.jsonl"));
            fs::write(&path, format!("{{\"sessionId\":\"{session}\",\"uuid\":\"a1\",\"type\":\"assistant\",\"message\":{{\"usage\":{{\"input_tokens\":50,\"output_tokens\":2}}}}}}\n")).unwrap();
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(
                    fs::FileTimes::new()
                        .set_modified(SystemTime::now() - std::time::Duration::from_secs(3600)),
                )
                .unwrap();
        }
        fs::write(
            home.join("sessions/live.json"),
            format!(
                "{{\"pid\":{},\"sessionId\":\"live\",\"status\":\"idle\"}}",
                std::process::id()
            ),
        )
        .unwrap();
        let roots = Roots {
            codex_home: fixture.0.join("codex"),
            claude_home: home,
        };
        let mut cache = DiscoveryCache::default();
        let result =
            discover_cached_with_status(&roots, 60, &mut cache, Some(Provider::ClaudeCode), true);
        assert_eq!(result.sessions.len(), 1);
        assert_eq!(result.sessions[0].handle.session_id, "live");
        assert!(result.sessions[0].handle.is_active());
        assert_eq!(result.sessions[0].usage.reported_context(), Some(52));
        assert_eq!(result.providers[0].scanned, 2);
        assert_eq!(result.providers[0].selected, 1);
        // Only selected live files enter the usage cache; the old nonlive
        // file's pre-filter read is bounded session metadata only.
        assert_eq!(cache.files.len(), 1);
        assert!(cache
            .files
            .keys()
            .all(|(_, path)| path.ends_with("live.jsonl")));
    }

    #[cfg(unix)]
    #[test]
    fn identified_cache_entries_outlive_the_sample_interval_until_the_file_changes() {
        use std::io::Write;
        let fixture = Fixture::new();
        let path = fixture.0.join("rollout.jsonl");
        let token_count = concat!(
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{",
            "\"last_token_usage\":{\"input_tokens\":100,\"output_tokens\":5},",
            "\"total_token_usage\":{\"input_tokens\":100,\"cached_input_tokens\":0},",
            "\"model_context_window\":258400}}}\n"
        );
        fs::write(
            &path,
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"sess-1\",\"cwd\":\"/work\"}}}}\n{token_count}"
            ),
        )
        .unwrap();
        let mut cache = DiscoveryCache::default();
        let now = std::time::Instant::now();
        let (meta, usage) = cache.inspect_at(Provider::Codex, &path, now);
        assert_eq!(meta.0.as_deref(), Some("sess-1"));
        assert_eq!(usage.reported_context(), Some(105));
        let key = (Provider::Codex, path.clone());
        let entry = cache.files.get_mut(&key).unwrap();
        assert!(entry.identified);
        entry.meta.0 = Some("cached-marker".into());
        // Hours past the sample interval an unchanged file is served from
        // the cache: no metadata or usage rescan touches the transcript.
        let later = now + std::time::Duration::from_secs(3 * 3600);
        let (meta, cached) = cache.inspect_at(Provider::Codex, &path, later);
        assert_eq!(meta.0.as_deref(), Some("cached-marker"));
        assert_eq!(cached, usage);
        // Any fingerprint difference rescans the file.
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(token_count.as_bytes())
            .unwrap();
        let (meta, rescanned) = cache.inspect_at(Provider::Codex, &path, later);
        assert_eq!(meta.0.as_deref(), Some("sess-1"));
        assert_eq!(rescanned, usage);
        assert_eq!(cache.files.len(), 1);
    }

    #[test]
    fn unidentified_cache_entries_expire_after_the_sample_interval() {
        let fixture = Fixture::new();
        let missing = fixture.0.join("missing.jsonl");
        let mut cache = DiscoveryCache::default();
        let now = std::time::Instant::now();
        let (meta, _) = cache.inspect_at(Provider::Codex, &missing, now);
        assert_eq!(meta, (None, None));
        let key = (Provider::Codex, missing.clone());
        let entry = cache.files.get_mut(&key).unwrap();
        assert!(!entry.identified);
        entry.meta.0 = Some("cached-marker".into());
        let within = now + std::time::Duration::from_secs(DiscoveryCache::SAMPLE_TTL_SECS - 1);
        let (meta, _) = cache.inspect_at(Provider::Codex, &missing, within);
        assert_eq!(meta.0.as_deref(), Some("cached-marker"));
        let expired = now + std::time::Duration::from_secs(DiscoveryCache::SAMPLE_TTL_SECS);
        let (meta, _) = cache.inspect_at(Provider::Codex, &missing, expired);
        assert_eq!(meta, (None, None));
    }
}
