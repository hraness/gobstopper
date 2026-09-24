//! Session discovery: locate Codex rollouts and Claude Code sessions on
//! disk and classify them by recency.

use gobstopper_core::model::{SessionHandle, UsageSample};
use gobstopper_core::Provider;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{claude, codex, devin};

/// Where to look for provider state. Overridable because managed
/// runtimes (e.g. oompa profiles) relocate these roots.
#[derive(Debug, Clone)]
pub struct Roots {
    /// `~/.codex` or `$CODEX_HOME`.
    pub codex_home: PathBuf,
    /// `~/.claude` or `$CLAUDE_CONFIG_DIR`-style root.
    pub claude_home: PathBuf,
    /// Directory containing `sessions.db` and `session_locks/`:
    /// `$DEVIN_DATA_DIR`, else `$XDG_DATA_HOME/devin/cli`, else
    /// `~/.local/share/devin/cli`.
    pub devin_home: PathBuf,
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
        let devin_home = std::env::var_os("DEVIN_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("XDG_DATA_HOME")
                    .map(|p| PathBuf::from(p).join("devin").join("cli"))
            })
            .unwrap_or_else(|| home.join(".local/share/devin/cli"));
        Self {
            codex_home,
            claude_home,
            devin_home,
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
            // Devin sessions live in rows of a shared database, not in
            // per-session files, so they bypass the file cache entirely.
            Provider::Devin => ((None, None), UsageSample::default()),
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

/// `provider` scopes the scan to one provider so `watch --provider
/// claude_code` never opens the Devin store (and vice versa). When
/// `context_only` is set, providers may return a `context_tokens`-only
/// `UsageSample` — lifetime counters can be left zero. Watch passes
/// true: it consumes only context occupancy, and the full usage scan on
/// the shared Devin store reads every message payload.
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
    context_only: bool,
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

    if provider.is_none_or(|p| p == Provider::Devin) {
        let (sessions, status) =
            devin::discover_with_status(&roots.devin_home, limit, context_only);
        found.extend(sessions);
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
        // Devin canonical exports: `session_meta` + `main_chain_id`, or
        // `message_node` records. Checked before Claude because the devin
        // meta record also carries a `session_id` field.
        if (ty == Some("session_meta") && v.get("main_chain_id").is_some())
            || (ty == Some("message_node") && v.get("node_id").is_some())
        {
            return Some(Provider::Devin);
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
    found.extend(devin::find(&roots.devin_home, query));
    found
}

/// Parse a discovered session fully.
pub fn load(d: &Discovered) -> Result<gobstopper_core::Transcript, crate::AdapterError> {
    match d.handle.provider {
        Provider::Codex => codex::load(d.handle.clone()),
        Provider::Devin => devin::load(d.handle.clone()),
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
            devin_home: fixture.0.join("devin"),
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
        assert_eq!(cache.0.len(), 1);
        assert!(cache.0.keys().all(|(_, path)| path.ends_with("live.jsonl")));
    }
}
