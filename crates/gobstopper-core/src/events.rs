//! Compaction telemetry: numeric-only records appended to a local JSONL
//! log (schema `gobstopper/compaction-events-v1`) for downstream
//! consumers such as aicharts and oompa.
//!
//! Hard rule: these records carry counts, durations, closed enum-like
//! strings, and identifiers only. Transcript content, file paths, and
//! raw provider messages are FORBIDDEN in these records.

use crate::Provider;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Accounting parsed from one retained byte sequence. This is provider-record
/// evidence, not a billing measurement or a guarantee about a resumed context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenObservation {
    pub source_sha256: String,
    pub source_identity_sha256: String,
    pub snapshot_manifest_sha256: Option<String>,
    pub context_state: crate::model::ContextState,
    pub context_tokens: Option<u64>,
    pub estimated_context_tokens: u64,
    pub lifetime_scope: crate::model::LifetimeScope,
    pub lifetime_input_tokens: Option<u64>,
    pub lifetime_cached_tokens: Option<u64>,
}

fn valid_hash(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl TokenObservation {
    pub fn is_valid(&self) -> bool {
        valid_hash(&self.source_sha256)
            && valid_hash(&self.source_identity_sha256)
            && self
                .snapshot_manifest_sha256
                .as_deref()
                .is_none_or(valid_hash)
            && (self.context_state == crate::model::ContextState::Reported)
                == self.context_tokens.is_some()
            && match (
                self.lifetime_scope,
                self.lifetime_input_tokens,
                self.lifetime_cached_tokens,
            ) {
                (crate::model::LifetimeScope::Full, Some(input), Some(cached)) => cached <= input,
                (
                    crate::model::LifetimeScope::Absent | crate::model::LifetimeScope::Partial,
                    None,
                    None,
                ) => true,
                _ => false,
            }
    }
}

/// One compaction telemetry record — the unit written to `events.jsonl`.
///
/// Numeric-only by design; see the module docs for what must never
/// appear here. `action`, `outcome`, and `error_code` are closed vocab
/// strings, not freeform text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionEvent {
    /// Always `gobstopper/compaction-events-v1`.
    pub schema: String,
    /// Unix seconds when the event was recorded.
    pub ts: u64,
    /// Provider that owns the session.
    pub provider: crate::Provider,
    /// Session identifier — callers pass a real id locally or a
    /// keyed/pseudonymous digest for telemetry export.
    pub session_id: String,
    /// Strategy id that produced the plan (e.g. "auto", "elide").
    pub strategy: String,
    /// "provider_compact" | "transcript_compact" | "none"
    pub action: String,
    /// "applied" | "planned" | "failed" | "skipped"
    pub outcome: String,
    /// Effective trigger the decision was evaluated against.
    pub trigger_tokens: u64,
    /// Context estimate before the compaction.
    pub context_tokens_before: u64,
    /// Context estimate after the compaction.
    pub context_tokens_after: u64,
    /// `context_tokens_before - context_tokens_after`, saturating.
    pub est_reclaimed_tokens: u64,
    /// Transcript items covered by elision/digest.
    pub items_covered: u64,
    /// Wall time of the evaluate+apply pass.
    pub duration_ms: u64,
    /// Closed error/decision code on failure or qualified outcomes
    /// (e.g. "io", "provider_rejected", "unresolved_context") — never a
    /// freeform message.
    pub error_code: Option<String>,
    /// Hash of the canonical provider/store/session identity; never a path.
    /// Absent for legacy events whose precise source was not retained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_identity_sha256: Option<String>,
    /// Vault object holding the exact pre-compaction bytes, when the
    /// compaction path preserved them. Hash identifier only — never
    /// content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_before_sha256: Option<String>,
    /// Vault object holding the post-compaction bytes, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_after_sha256: Option<String>,
    /// Evidence parsed from the corresponding retained bytes. Legacy numeric
    /// estimates above remain readable but do not qualify as observed savings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_observation: Option<TokenObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_observation: Option<TokenObservation>,
    /// Realized retention: heuristic checks bound to the before-state,
    /// scored against the after-state. `total`/`retained` (literal)/
    /// `lexical` (≥75% token coverage). Absent when unmeasured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_retained: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_lexical: Option<u64>,
}

impl CompactionEvent {
    /// Schema tag written into every record.
    pub const SCHEMA: &'static str = "gobstopper/compaction-events-v1";

    /// Build an event for one compaction pass. `schema` and `ts` are
    /// filled automatically; `est_reclaimed_tokens` is derived from the
    /// before/after context estimates.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Provider,
        session_id: impl Into<String>,
        strategy: impl Into<String>,
        action: impl Into<String>,
        outcome: impl Into<String>,
        trigger_tokens: u64,
        context_tokens_before: u64,
        context_tokens_after: u64,
        items_covered: u64,
        duration_ms: u64,
        error_code: Option<String>,
    ) -> Self {
        Self {
            schema: Self::SCHEMA.to_string(),
            ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            provider,
            session_id: session_id.into(),
            strategy: strategy.into(),
            action: action.into(),
            outcome: outcome.into(),
            trigger_tokens,
            context_tokens_before,
            context_tokens_after,
            est_reclaimed_tokens: context_tokens_before.saturating_sub(context_tokens_after),
            items_covered,
            duration_ms,
            error_code,
            source_identity_sha256: None,
            snapshot_before_sha256: None,
            snapshot_after_sha256: None,
            before_observation: None,
            after_observation: None,
            retention_total: None,
            retention_retained: None,
            retention_lexical: None,
        }
    }

    /// A same-source, retained-byte-bound reduction in positive provider
    /// context reports, in tokens. Zero/reset/unknown, legacy estimates, and
    /// unapplied plans remain unqualified. This is never billed-token savings.
    pub fn recorded_context_reduction_tokens(&self) -> Option<u64> {
        if self.outcome != "applied" || self.error_code.is_some() {
            return None;
        }
        let (before, after) = (
            self.before_observation.as_ref()?,
            self.after_observation.as_ref()?,
        );
        if !before.is_valid()
            || !after.is_valid()
            || before.source_identity_sha256 != after.source_identity_sha256
            || self.source_identity_sha256.as_ref() != Some(&before.source_identity_sha256)
            || before.snapshot_manifest_sha256.is_none()
            || after.snapshot_manifest_sha256.is_none()
            || before.snapshot_manifest_sha256 != self.snapshot_before_sha256
            || after.snapshot_manifest_sha256 != self.snapshot_after_sha256
        {
            return None;
        }
        let (before, after) = (before.context_tokens?, after.context_tokens?);
        (before > 0 && after > 0).then_some(before.saturating_sub(after))
    }
}

fn valid_event(event: &CompactionEvent) -> bool {
    let valid_digest = |digest: &Option<String>| digest.as_deref().is_none_or(valid_hash);
    event.schema == CompactionEvent::SCHEMA
        && valid_digest(&event.source_identity_sha256)
        && valid_digest(&event.snapshot_before_sha256)
        && valid_digest(&event.snapshot_after_sha256)
        && event
            .before_observation
            .as_ref()
            .is_none_or(TokenObservation::is_valid)
        && event
            .after_observation
            .as_ref()
            .is_none_or(TokenObservation::is_valid)
        && match (
            event.retention_total,
            event.retention_retained,
            event.retention_lexical,
        ) {
            (None, None, None) => true,
            (Some(total), Some(literal), Some(lexical)) => literal <= total && lexical <= total,
            _ => false,
        }
        && !event.session_id.is_empty()
        && event.session_id.len() <= 256
        && event
            .session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && !event.strategy.is_empty()
        && event.strategy.len() <= 128
        && event
            .strategy
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
        && matches!(
            event.action.as_str(),
            "provider_compact" | "transcript_compact" | "none"
        )
        && matches!(
            event.outcome.as_str(),
            "applied" | "planned" | "failed" | "skipped" | "blocked"
        )
        && event.error_code.as_deref().is_none_or(|code| {
            matches!(
                code,
                "io" | "provider_rejected"
                    | "apply_failed"
                    | "verification_failed"
                    | "custody_unavailable"
                    | "unattributed_provider_hook"
                    | "unresolved_context"
                    | "native_unqualified"
                    | "spawn_failed"
                    | "parent_thread"
                    | "provider_noop"
                    | "quota_limited"
            )
        })
        && event.est_reclaimed_tokens
            == event
                .context_tokens_before
                .saturating_sub(event.context_tokens_after)
}

/// Live-log size cap before rotation. Post-dedupe growth is roughly
/// 150KiB/day, so the live file spans about two months per generation.
const ROTATE_BYTES: u64 = 8 * 1024 * 1024;

/// Path of the previous generation (`events.jsonl` → `events.1.jsonl`),
/// matching the monitor's `observations.1.jsonl` convention.
fn rotated_path(log_path: &Path) -> PathBuf {
    log_path.with_extension("1.jsonl")
}

/// Single-generation rotation: an oversize live log becomes
/// `events.1.jsonl`, dropping an older regular-file generation atomically.
/// A failed rename leaves the admitted open file available for append. This is
/// best-effort telemetry, not a transaction across concurrent rotations: parent
/// directories must remain stable and owner-controlled.
#[cfg(unix)]
fn rotate_if_oversize(log_path: &Path, file: &std::fs::File) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let opened = file.metadata()?;
    if opened.len() <= ROTATE_BYTES {
        return Ok(false);
    }
    let named = std::fs::symlink_metadata(log_path)?;
    if !named.is_file() || named.dev() != opened.dev() || named.ino() != opened.ino() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "compaction event log identity changed",
        ));
    }
    let rotated = rotated_path(log_path);
    match std::fs::symlink_metadata(&rotated) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "compaction event generation is not a regular file",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(std::fs::rename(log_path, rotated).is_ok())
}

#[cfg(unix)]
fn open_event_append(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "compaction event log is not a regular file",
        ));
    }
    Ok(file)
}

/// Append one event as a JSONL line, creating parent dirs as needed. The log
/// leaf must be a regular file; new logs have private permissions. Parent
/// directory ownership and stability are the caller's responsibility.
pub fn append_event(log_path: &Path, event: &CompactionEvent) -> std::io::Result<()> {
    if !valid_event(event) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "compaction event violates schema bounds",
        ));
    }
    #[cfg(unix)]
    {
        let mut line = serde_json::to_string(event).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "compaction event serialization failed",
            )
        })?;
        line.push('\n');
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Admission precedes rotation; a FIFO cannot block open, and a symlink
        // cannot redirect either the append or the rotation's source lookup.
        let mut file = open_event_append(log_path)?;
        if rotate_if_oversize(log_path, &file)? {
            file = open_event_append(log_path)?;
        }
        file.write_all(line.as_bytes())
    }
    #[cfg(not(unix))]
    {
        let _ = log_path;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "bounded event log appends are unsupported on this platform",
        ))
    }
}

/// Default telemetry log: `$XDG_DATA_HOME/gobstopper/events.jsonl`,
/// falling back to `~/.local/share/gobstopper/events.jsonl`.
pub fn default_log_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("gobstopper").join("events.jsonl")
}

/// Read every event in the log, oldest generation first so a rotation
/// boundary does not silently truncate history. Blank and unparseable
/// lines are skipped so one torn write does not lose the rest.
pub fn read_events(log_path: &Path) -> std::io::Result<Vec<CompactionEvent>> {
    const MAX_LOG_BYTES: u64 = 128 * 1024 * 1024;
    const MAX_LINE_BYTES: usize = 16 * 1024;
    let mut events = Vec::new();
    let rotated = rotated_path(log_path);
    for path in [rotated.as_path(), log_path] {
        let file = match open_event_log(path, MAX_LOG_BYTES) {
            Ok(file) => file,
            // The previous generation is optional; the live log keeps
            // the original missing-file error contract.
            Err(e) if path == log_path => return Err(e),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let bytes = bounded_log_bytes(file, MAX_LOG_BYTES)?;
        for line in bytes.split(|byte| *byte == b'\n') {
            if line.len() > MAX_LINE_BYTES || line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if let Some(event) = serde_json::from_slice(line).ok().filter(valid_event) {
                events.push(event);
            }
        }
    }
    Ok(events)
}

fn open_event_log(path: &Path, max_bytes: u64) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "compaction event log is not a regular file",
            ));
        }
        if metadata.len() > max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "compaction event log exceeds byte limit",
            ));
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, max_bytes);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "bounded event log reads are unsupported on this platform",
        ))
    }
}

fn bounded_log_bytes(file: std::fs::File, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    // Metadata is only an early rejection; an appending writer cannot bypass
    // the actual byte cap or make a single line allocate without a bound.
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "compaction event log exceeds byte limit",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reader_fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gobstopper-event-reader-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn event_reader_caps_actual_bytes_after_admission() {
        let dir = reader_fixture("growth");
        let path = dir.join("events.jsonl");
        std::fs::write(&path, b"{}\n").unwrap();
        let file = open_event_log(&path, 8).unwrap();
        // Deterministic interleaving: the admitted inode grows before read.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"0123456789abcdef\n")
            .unwrap();
        let error = bounded_log_bytes(file, 8).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "compaction event log exceeds byte limit");
        assert!(open_event_log(&path, 8).is_err());
        std::fs::write(&path, b"12345678").unwrap();
        assert_eq!(
            bounded_log_bytes(open_event_log(&path, 8).unwrap(), 8).unwrap(),
            b"12345678"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn event_reader_refuses_fifo_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;
        let dir = reader_fixture("fifo");
        let path = dir.join("events.jsonl");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: the owned CString is NUL-terminated and remains alive for
        // this call; mkfifo only creates the new private fixture path.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let reader_path = path.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let _ = tx.send(read_events(&reader_path));
        });
        let result = rx.recv_timeout(std::time::Duration::from_secs(1));
        if result.is_err() {
            // Unblock a regressed blocking open before joining; never leak a
            // test thread. Nonblocking writer open returns if no reader exists.
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&path);
        }
        reader.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(
            result
                .expect("FIFO reader must refuse promptly")
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[cfg(unix)]
    #[test]
    fn event_reader_rejects_symlink_and_unsafe_rotated_generation() {
        let dir = reader_fixture("symlink");
        let path = dir.join("events.jsonl");
        let target = dir.join("target");
        std::fs::write(&target, b"").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(read_events(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"").unwrap();
        std::os::unix::fs::symlink(&target, rotated_path(&path)).unwrap();
        assert!(read_events(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn event_append_refuses_fifo_without_effects_or_waiting() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;
        let dir = reader_fixture("append-fifo");
        let path = dir.join("events.jsonl");
        let previous = rotated_path(&path);
        std::fs::write(&previous, b"previous generation").unwrap();
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: the owned CString remains NUL-terminated and alive during
        // this call; the only created FIFO is in the private fixture directory.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let writer_path = path.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let _ = tx.send(append_event(&writer_path, &observed_event()));
        });
        let result = rx.recv_timeout(std::time::Duration::from_secs(1));
        // Keep the nonblocking reader alive until join if a regression needs
        // its blocked open released. The small event cannot fill the pipe.
        let unblock = result.is_err().then(|| {
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&path)
                .unwrap()
        });
        writer.join().unwrap();
        drop(unblock);
        assert!(result.expect("FIFO append must refuse promptly").is_err());
        assert_eq!(std::fs::read(&previous).unwrap(), b"previous generation");
        assert!(!std::fs::symlink_metadata(&path).unwrap().is_file());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn event_append_rejects_redirected_rotation_without_effects() {
        let dir = reader_fixture("append-symlink");
        let path = dir.join("events.jsonl");
        let target = dir.join("target");
        let previous = rotated_path(&path);
        let original = vec![b'x'; ROTATE_BYTES as usize + 1];
        std::fs::write(&target, &original).unwrap();
        std::fs::write(&previous, b"previous generation").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(append_event(&path, &observed_event()).is_err());
        assert_eq!(std::fs::read_link(&path).unwrap(), target);
        assert_eq!(std::fs::read(&target).unwrap(), original);
        assert_eq!(std::fs::read(&previous).unwrap(), b"previous generation");

        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&target, &path).unwrap();
        std::fs::remove_file(&previous).unwrap();
        std::os::unix::fs::symlink("missing-target", &previous).unwrap();
        assert!(append_event(&path, &observed_event()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            std::fs::read_link(&previous).unwrap(),
            Path::new("missing-target")
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn event_rotation_refuses_a_replaced_admitted_inode() {
        let dir = reader_fixture("append-identity");
        let path = dir.join("events.jsonl");
        let admitted = open_event_append(&path).unwrap();
        admitted.set_len(ROTATE_BYTES + 1).unwrap();
        std::fs::rename(&path, dir.join("admitted")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        assert_eq!(
            rotate_if_oversize(&path, &admitted).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
        assert!(!rotated_path(&path).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn observed_event() -> CompactionEvent {
        use crate::model::{ContextState, LifetimeScope};
        let observation = |source: char, manifest: char, context| TokenObservation {
            source_sha256: source.to_string().repeat(64),
            source_identity_sha256: "a".repeat(64),
            snapshot_manifest_sha256: Some(manifest.to_string().repeat(64)),
            context_state: ContextState::Reported,
            context_tokens: Some(context),
            estimated_context_tokens: 0,
            lifetime_scope: LifetimeScope::Absent,
            lifetime_input_tokens: None,
            lifetime_cached_tokens: None,
        };
        let mut event = CompactionEvent::new(
            Provider::Codex,
            "s",
            "auto",
            "provider_compact",
            "applied",
            10,
            900,
            500,
            0,
            1,
            None,
        );
        event.source_identity_sha256 = Some("a".repeat(64));
        event.snapshot_before_sha256 = Some("b".repeat(64));
        event.snapshot_after_sha256 = Some("c".repeat(64));
        event.before_observation = Some(observation('d', 'b', 100));
        event.after_observation = Some(observation('e', 'c', 40));
        event
    }

    #[test]
    fn context_reduction_requires_complete_positive_same_source_evidence() {
        let event = observed_event();
        assert!(valid_event(&event));
        assert_eq!(event.recorded_context_reduction_tokens(), Some(60));
        // Deliberately different legacy scalar estimates cannot affect the result.
        assert_eq!(event.est_reclaimed_tokens, 400);
        let mutate: [fn(&mut CompactionEvent); 7] = [
            |e| e.outcome = "planned".into(),
            |e| e.error_code = Some("unresolved_context".into()),
            |e| e.after_observation = None,
            |e| e.snapshot_before_sha256 = None,
            |e| e.after_observation.as_mut().unwrap().source_identity_sha256 = "f".repeat(64),
            |e| {
                e.after_observation
                    .as_mut()
                    .unwrap()
                    .snapshot_manifest_sha256 = Some("f".repeat(64))
            },
            |e| e.after_observation.as_mut().unwrap().context_tokens = Some(0),
        ];
        for mutation in mutate {
            let mut changed = event.clone();
            mutation(&mut changed);
            assert_eq!(changed.recorded_context_reduction_tokens(), None);
        }
        for state in [
            crate::model::ContextState::Absent,
            crate::model::ContextState::Unknown,
            crate::model::ContextState::Reset,
        ] {
            let mut changed = event.clone();
            let observation = changed.after_observation.as_mut().unwrap();
            observation.context_state = state;
            observation.context_tokens = None;
            assert!(observation.is_valid());
            assert_eq!(changed.recorded_context_reduction_tokens(), None);
        }
    }

    #[test]
    fn retention_counts_and_observations_reject_invalid_denominators() {
        let mut event = observed_event();
        event.retention_total = Some(0);
        event.retention_retained = Some(1);
        event.retention_lexical = Some(0);
        assert!(!valid_event(&event));
        event.retention_retained = Some(0);
        assert!(valid_event(&event));
        event.retention_lexical = None;
        assert!(!valid_event(&event));
        let before = event.before_observation.as_mut().unwrap();
        before.source_sha256 = "sensitive arbitrary content".into();
        assert!(!before.is_valid());
    }

    #[test]
    fn event_json_round_trip() {
        let event = CompactionEvent::new(
            Provider::Codex,
            "sess-1",
            "elide",
            "transcript_compact",
            "applied",
            250_000,
            260_000,
            40_000,
            12,
            42,
            None,
        );
        assert_eq!(event.schema, CompactionEvent::SCHEMA);
        assert!(event.ts > 0);
        assert_eq!(event.est_reclaimed_tokens, 220_000);

        let json = serde_json::to_string(&event).unwrap();
        let back: CompactionEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.schema, "gobstopper/compaction-events-v1");
        assert_eq!(back.provider, Provider::Codex);
        assert_eq!(back.session_id, "sess-1");
        assert_eq!(back.strategy, "elide");
        assert_eq!(back.action, "transcript_compact");
        assert_eq!(back.outcome, "applied");
        assert_eq!(back.trigger_tokens, 250_000);
        assert_eq!(back.context_tokens_before, 260_000);
        assert_eq!(back.context_tokens_after, 40_000);
        assert_eq!(back.est_reclaimed_tokens, 220_000);
        assert_eq!(back.items_covered, 12);
        assert_eq!(back.duration_ms, 42);
        assert_eq!(back.error_code, None);

        // Provider serializes snake_case like the rest of the model.
        assert!(json.contains("\"provider\":\"codex\""));

        let failed = CompactionEvent::new(
            Provider::ClaudeCode,
            "sess-2",
            "sawtooth",
            "provider_compact",
            "failed",
            175_000,
            180_000,
            180_000,
            0,
            5,
            Some("provider_rejected".to_string()),
        );
        let back: CompactionEvent =
            serde_json::from_str(&serde_json::to_string(&failed).unwrap()).unwrap();
        assert_eq!(back.error_code.as_deref(), Some("provider_rejected"));
        assert_eq!(back.provider, Provider::ClaudeCode);
        // Saturating: after > before yields zero reclaimed, not underflow.
        assert_eq!(back.est_reclaimed_tokens, 0);
    }

    #[test]
    fn evidence_fields_are_additive_and_backward_compatible() {
        let mut event = CompactionEvent::new(
            Provider::Devin,
            "sess-9",
            "auto",
            "provider_compact",
            "applied",
            250_000,
            300_000,
            150_000,
            0,
            120,
            None,
        );
        // Absent fields serialize nothing — old consumers see v1 shape.
        let bare = serde_json::to_string(&event).unwrap();
        assert!(!bare.contains("retention_total"));
        assert!(!bare.contains("snapshot_before_sha256"));

        event.snapshot_before_sha256 = Some("ab".repeat(32));
        event.snapshot_after_sha256 = Some("cd".repeat(32));
        event.retention_total = Some(32);
        event.retention_retained = Some(13);
        event.retention_lexical = Some(27);
        let json = serde_json::to_string(&event).unwrap();
        let back: CompactionEvent = serde_json::from_str(&json).unwrap();
        assert!(valid_event(&back));
        assert_eq!(back.retention_lexical, Some(27));
        assert_eq!(back.snapshot_before_sha256.as_deref().unwrap().len(), 64);

        // Pre-field records (the shipped v1 lines) still deserialize.
        let legacy = CompactionEvent::new(
            Provider::Codex,
            "s",
            "elide",
            "transcript_compact",
            "applied",
            1_000,
            1_200,
            300,
            0,
            5,
            None,
        );
        let mut legacy_json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&legacy).unwrap()).unwrap();
        for key in [
            "snapshot_before_sha256",
            "snapshot_after_sha256",
            "retention_total",
            "retention_retained",
            "retention_lexical",
        ] {
            legacy_json.as_object_mut().unwrap().remove(key);
        }
        let back: CompactionEvent = serde_json::from_value(legacy_json).unwrap();
        assert!(valid_event(&back));
        assert_eq!(back.retention_total, None);
    }

    #[test]
    fn append_and_read_events_jsonl() {
        let unique = format!(
            "gobstopper-events-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        // Nested path proves append_event creates parent dirs.
        let log = dir.join("nested").join("events.jsonl");

        let e1 = CompactionEvent::new(
            Provider::Codex,
            "a",
            "elide",
            "transcript_compact",
            "applied",
            1_000,
            1_200,
            300,
            4,
            10,
            None,
        );
        let e2 = CompactionEvent::new(
            Provider::ClaudeCode,
            "b",
            "sawtooth",
            "provider_compact",
            "skipped",
            1_000,
            900,
            900,
            0,
            0,
            None,
        );
        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&log).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let mut invalid = e1.clone();
        invalid.session_id = "/private/session/path".to_string();
        assert!(append_event(&log, &invalid).is_err());
        let pristine = std::fs::read(&log).unwrap();
        for value in [
            "private-response-sentinel".into(),
            "A".repeat(64),
            "a".repeat(65),
            "x".repeat(100_000),
        ] {
            for field in 0..3 {
                let mut invalid = e1.clone();
                match field {
                    0 => invalid.source_identity_sha256 = Some(value.clone()),
                    1 => invalid.snapshot_before_sha256 = Some(value.clone()),
                    _ => invalid.snapshot_after_sha256 = Some(value.clone()),
                }
                let error = append_event(&log, &invalid).unwrap_err();
                assert!(!error.to_string().contains(&value));
                assert_eq!(std::fs::read(&log).unwrap(), pristine);
            }
        }

        // A torn write / foreign line is skipped, not fatal.
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        f.write_all(b"this is not json\n\n").unwrap();
        drop(f);

        let events = read_events(&log).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].session_id, "a");
        assert_eq!(events[1].session_id, "b");
        assert_eq!(events[1].outcome, "skipped");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn oversize_log_rotates_and_read_events_stitches_generations() {
        let unique = format!(
            "gobstopper-rotate-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("events.jsonl");

        let old = CompactionEvent::new(
            Provider::Codex,
            "old-gen",
            "elide",
            "transcript_compact",
            "applied",
            1_000,
            1_200,
            300,
            4,
            10,
            None,
        );
        append_event(&log, &old).unwrap();
        // Pad past the 8MiB cap so the next append must rotate.
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        f.write_all(vec![b'x'; (ROTATE_BYTES + 1) as usize].as_slice())
            .unwrap();
        drop(f);

        let new = CompactionEvent::new(
            Provider::Codex,
            "new-gen",
            "elide",
            "transcript_compact",
            "applied",
            1_000,
            1_200,
            300,
            4,
            10,
            None,
        );
        append_event(&log, &new).unwrap();

        let rotated = dir.join("events.1.jsonl");
        assert!(rotated.exists());
        assert!(std::fs::metadata(&rotated).unwrap().len() > ROTATE_BYTES);

        let events = read_events(&log).unwrap();
        // The padding is one giant unparseable line — skipped — so only
        // the two real events survive, oldest generation first.
        assert_eq!(
            events
                .iter()
                .map(|e| e.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["old-gen", "new-gen"]
        );

        // Under-cap logs never rotate.
        let small = dir.join("small.jsonl");
        append_event(&small, &new).unwrap();
        assert!(!dir.join("small.1.jsonl").exists());

        std::fs::remove_dir_all(&dir).ok();
    }
}
