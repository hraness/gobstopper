//! Devin session adapter (`$DEVIN_DATA_DIR/sessions.db`, default
//! `~/.local/share/devin/cli/sessions.db`).
//!
//! Record dialect: the `sessions` table carries one row per session with
//! `main_chain_id` naming the head of the live chain. `message_nodes`
//! rows chain `parent_node_id` and carry `chat_message` — a JSON string in
//! the provider's own message shape (`role`, `content`, `tool_calls`,
//! `tool_call_id`, `thinking`, `metadata.metrics`). Nodes not reachable
//! from `main_chain_id` are dead branches: they occupy store bytes but no
//! context tokens.
//!
//! Because the store is a database rather than a file, all hashing,
//! vault snapshots, and `verify` operate on a canonical *export form*:
//! one JSON object per line — a `session_meta` record followed by
//! `message_node` records ordered by `node_id`. `export_bytes` is the
//! read substrate; the byte identity of a session is the sha256 of its
//! export.
//!
//! Inspection exports and detached byte transformations are available. Direct
//! store/file mutation is disabled because the observed lock probe does not
//! establish provider-compatible lifetime custody. Native `/compact` remains
//! delegated to the provider under its separately qualified control contract.

use gobstopper_core::estimate::estimate_tokens;
use gobstopper_core::model::{ItemKind, SessionHandle, TranscriptItem, UsageSample};
use gobstopper_core::{Provider, Transcript};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::detect::Discovered;
use crate::AdapterError;

/// Devin-owned marker meaning "session is live": while a CLI process has
/// the session open it holds an `flock` on `session_locks/<id>.lock`.
/// Lock files persist after the process exits, so existence alone is not
/// proof. Stale regular files lock cleanly. Unreadable/unsupported lock state
/// conservatively counts as active; an absent lock alone counts as inactive.
/// This probe does not establish provider-compatible mutation custody.
pub fn session_active(root: &Path, session_id: &str) -> bool {
    if session_id.is_empty()
        || session_id.len() > 256
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return true;
    }
    let directory = root.join("session_locks");
    match std::fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.is_dir() => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        _ => return true,
    }
    let path = directory.join(format!("{session_id}.lock"));
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        _ => return true,
    }
    let file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return true;
    }
    fs2::FileExt::try_lock_exclusive(&file).is_err()
}

pub fn db_path(root: &Path) -> PathBuf {
    root.join("sessions.db")
}

/// Whether `path` is the shared provider store rather than a detached
/// export file. This filename distinction is only a parsing hint; it does
/// not establish write custody for either path.
pub fn is_store_path(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == "sessions.db")
}

fn open_readonly(db: &Path) -> Result<Connection, AdapterError> {
    // SQLite may block while opening a FIFO before it can reject its format.
    // Admit only an existing regular leaf; stable owner-controlled parents are
    // required, and SQLite's NOFOLLOW also rejects leaf replacement by a link.
    if !std::fs::symlink_metadata(db)
        .map_err(|source| AdapterError::Io {
            path: db.to_path_buf(),
            source,
        })?
        .is_file()
    {
        return Err(AdapterError::InvalidEdit(
            "Devin store must be a regular file, not a symlink or special file",
        ));
    }
    // Common home/temp aliases (for example macOS /var) are allowed as stable
    // parents. Canonicalize only the parent: resolving the leaf would undo the
    // no-symlink admission if an installer replaced it between these steps.
    let parent = db
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()
        .map_err(|source| AdapterError::Io {
            path: db.to_path_buf(),
            source,
        })?;
    let sqlite_path = parent.join(
        db.file_name()
            .ok_or(AdapterError::InvalidEdit("Devin store has no filename"))?,
    );
    Connection::open_with_flags(
        sqlite_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|e| AdapterError::Io {
        path: db.to_path_buf(),
        source: std::io::Error::other(e),
    })
}

fn rusqlite_io(db: &Path) -> impl Fn(rusqlite::Error) -> AdapterError + '_ {
    move |e| AdapterError::Io {
        path: db.to_path_buf(),
        source: std::io::Error::other(e),
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Every session in the store, newest activity first. `path` on each
/// handle is the shared database; `session_id` selects the row set.
/// `cwd` comes from `sessions.working_directory`; `age_secs` is derived
/// from `last_activity_at` rather than file mtime. `context_only`
/// bounds each session's usage read to its newest rows — callers that
/// need `lifetime_*` counters (report/export) must pass false.
pub fn discover(root: &Path, max_age_secs: u64, context_only: bool) -> Vec<Discovered> {
    let db = db_path(root);
    let Ok(conn) = open_readonly(&db) else {
        return Vec::new();
    };
    let mut stmt = match conn.prepare(
        "SELECT id, working_directory, last_activity_at FROM sessions ORDER BY last_activity_at DESC",
    ) {
        Ok(stmt) => stmt,
        Err(_) => return Vec::new(),
    };
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<i64>>(2)?,
            ))
        })
        .map(|it| it.flatten().collect::<Vec<_>>())
        .unwrap_or_default();
    let limit = if max_age_secs == 0 {
        u64::MAX
    } else {
        max_age_secs
    };
    let now = now_secs();
    let mut found = Vec::new();
    for (session_id, cwd, last_activity) in rows {
        // A live provider lock means the session is owned right now
        // regardless of how stale its last write looks: report age 0 so
        // `is_active()` and `auto`'s live-session delegation both see it.
        let age = if session_active(root, &session_id) {
            0
        } else {
            last_activity
                .map(|t| now.saturating_sub(t.max(0) as u64))
                .unwrap_or(u64::MAX)
        };
        if age > limit {
            continue;
        }
        let usage = if context_only {
            scan_context(&conn, &session_id)
        } else {
            scan_usage(&conn, &session_id)
        };
        found.push(Discovered {
            handle: SessionHandle {
                provider: Provider::Devin,
                session_id,
                path: db.clone(),
                cwd: cwd.map(PathBuf::from),
                age_secs: age,
            },
            usage,
        });
    }
    found
}

/// Cheap lookup by session-id prefix or title substring.
pub fn find(root: &Path, query: &str) -> Vec<Discovered> {
    let db = db_path(root);
    let Ok(conn) = open_readonly(&db) else {
        return Vec::new();
    };
    let mut stmt = match conn.prepare(
        "SELECT id, working_directory, last_activity_at FROM sessions \
         WHERE id LIKE ?1 || '%' OR title LIKE '%' || ?1 || '%' \
         ORDER BY last_activity_at DESC",
    ) {
        Ok(stmt) => stmt,
        Err(_) => return Vec::new(),
    };
    let rows = stmt
        .query_map([query], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<i64>>(2)?,
            ))
        })
        .map(|it| it.flatten().collect::<Vec<_>>())
        .unwrap_or_default();
    let now = now_secs();
    rows.into_iter()
        .map(|(session_id, cwd, last_activity)| Discovered {
            usage: scan_usage(&conn, &session_id),
            handle: SessionHandle {
                provider: Provider::Devin,
                age_secs: if session_active(root, &session_id) {
                    0
                } else {
                    last_activity
                        .map(|t| now.saturating_sub(t.max(0) as u64))
                        .unwrap_or(u64::MAX)
                },
                session_id,
                path: db.clone(),
                cwd: cwd.map(PathBuf::from),
            },
        })
        .collect()
}

/// O(1) content version for watch suppression caching: main-chain head
/// plus node count. A provider append that changes either moves
/// it, so an unchanged fingerprint means the session is byte-identical.
pub fn chain_fingerprint(db: &Path, session_id: &str) -> Option<String> {
    let conn = open_readonly(db).ok()?;
    conn.query_row(
        "SELECT main_chain_id, \
         (SELECT COUNT(*) FROM message_nodes WHERE session_id = ?1) \
         FROM sessions WHERE id = ?1",
        [session_id],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
    )
    .ok()
    .map(|(head, count)| format!("{head}:{count}"))
}

/// One-shot observation for policy surfaces: the session's token
/// accounting plus whether the provider currently holds its lock.
/// `None` when the store or the session row is absent.
pub fn session_observation(root: &Path, session_id: &str) -> Option<(UsageSample, bool)> {
    let db = db_path(root);
    let conn = open_readonly(&db).ok()?;
    conn.query_row("SELECT 1 FROM sessions WHERE id = ?1", [session_id], |_| {
        Ok(())
    })
    .ok()?;
    Some((
        scan_usage(&conn, session_id),
        session_active(root, session_id),
    ))
}

/// Resolve the caller's own session: a provider-locked session whose
/// `working_directory` contains or is contained by `cwd` (longest match
/// wins, then most recently active). When no locked session matches the
/// directory, a single locked session is still a safe answer; ambiguity
/// returns `None` rather than a guess.
pub fn current_session(root: &Path, cwd: &Path) -> Option<String> {
    let db = db_path(root);
    let conn = open_readonly(&db).ok()?;
    let mut stmt = conn
        .prepare(
            "SELECT id, working_directory, last_activity_at FROM sessions \
             ORDER BY last_activity_at DESC",
        )
        .ok()?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<i64>>(2)?,
            ))
        })
        .ok()?
        .flatten()
        .collect::<Vec<_>>();
    let locked: Vec<(String, Option<String>, i64)> = rows
        .into_iter()
        .filter(|(id, _, _)| session_active(root, id))
        .map(|(id, w, last)| (id, w, last.unwrap_or(0)))
        .collect();
    if locked.is_empty() {
        return None;
    }
    locked
        .iter()
        .filter(|(_, w, _)| {
            w.as_deref().is_some_and(|w| {
                let w = Path::new(w);
                cwd.starts_with(w) || w.starts_with(cwd)
            })
        })
        .max_by_key(|(_, w, last)| (w.as_deref().map_or(0, str::len), *last))
        .map(|(id, _, _)| id.clone())
        .or_else(|| (locked.len() == 1).then(|| locked[0].0.clone()))
}

/// Provider-reported token accounting. Mirrors the Claude convention:
/// `input + cache_read + cache_creation + output` on the latest live-chain
/// assistant message approximates context occupancy; lifetime sums the
/// same inputs across every assistant message in the session.
pub fn scan_usage(conn: &Connection, session_id: &str) -> UsageSample {
    with_read_snapshot(conn, || {
        let bytes = export_bytes_conn(conn, Path::new(""), session_id).ok()?;
        let handle = SessionHandle {
            provider: Provider::Devin,
            session_id: session_id.to_string(),
            path: PathBuf::new(),
            cwd: None,
            age_secs: u64::MAX,
        };
        load_bytes(handle, &bytes).ok().map(|t| t.usage)
    })
    .unwrap_or_else(|| {
        let mut usage = UsageSample::default();
        usage.invalidate_context();
        usage
    })
}

/// Pin head and node reads to one SQLite snapshot. A caller's existing
/// transaction already provides that snapshot and is never committed here.
fn with_read_snapshot<T>(conn: &Connection, read: impl FnOnce() -> Option<T>) -> Option<T> {
    let own = conn.is_autocommit();
    if own && conn.execute_batch("BEGIN DEFERRED").is_err() {
        return None;
    }
    let result = read();
    if own && conn.execute_batch("ROLLBACK").is_err() {
        return None;
    }
    result
}

/// Context-only discovery validates the bounded identity graph, then reads at
/// most 32 live-chain payloads. A newer dead sibling is never a context sample.
/// Lifetime counters are unavailable in this deliberately partial payload scan.
fn scan_context(conn: &Connection, session_id: &str) -> UsageSample {
    with_read_snapshot(conn, || {
        let head: Option<i64> = conn.query_row(
            "SELECT main_chain_id FROM sessions WHERE id = ?1", [session_id], |r| r.get(0),
        ).ok()?;
        let mut stmt = conn.prepare(
            "SELECT node_id, parent_node_id FROM message_nodes WHERE session_id = ?1 ORDER BY node_id LIMIT ?2",
        ).ok()?;
        let mut query = stmt.query(rusqlite::params![session_id, (gobstopper_core::validation::MAX_ITEMS + 1) as i64]).ok()?;
        let mut graph = Vec::new();
        while let Some(row) = query.next().ok()? {
            if graph.len() >= gobstopper_core::validation::MAX_ITEMS { return None; }
            graph.push(NodeRow {
                line_index: graph.len(), node_id: row.get(0).ok()?, parent_node_id: row.get(1).ok()?,
                message: Value::Null, num_tokens_preceding: None,
            });
        }
        let live = live_nodes(&graph, head);
        if live.is_empty() { return None; }
        let ids: Vec<_> = graph.iter().rev().filter(|row| live.contains(&row.node_id))
            .take(32).map(|row| row.node_id).collect();
        let mut stmt = conn.prepare(
            "SELECT chat_message, metadata FROM message_nodes WHERE session_id = ?1 AND node_id = ?2",
        ).ok()?;
        let mut usage = UsageSample::default();
        let mut preceding = None;
        let mut payload_bytes = 0u64;
        for id in ids.into_iter().rev() {
            let (raw, meta): (String, Option<String>) = stmt.query_row(
                rusqlite::params![session_id, id], |r| Ok((r.get(0)?, r.get(1)?)),
            ).ok()?;
            payload_bytes = payload_bytes.checked_add(raw.len() as u64)?
                .checked_add(meta.as_ref().map_or(0, |m| m.len() as u64))?;
            if payload_bytes > crate::transaction::max_transcript_bytes() { return None; }
            let message = crate::payload::decode_record(&raw).ok()?;
            if !message.is_object() { return None; }
            if let Some(meta) = meta {
                let meta = crate::payload::decode_record(&meta).ok()?;
                if let Some(n) = meta.get("num_tokens_preceding").and_then(Value::as_u64) { preceding = Some(n); }
            }
            absorb_usage(&message, &mut usage, true);
        }
        if usage.context_state != gobstopper_core::model::ContextState::Reported {
            if let Some(n) = preceding { usage.context_tokens = n; usage.context_state = gobstopper_core::model::ContextState::Unknown; }
        }
        usage.lifetime_input_tokens = 0;
        usage.lifetime_cached_tokens = 0;
        usage.lifetime_scope = gobstopper_core::model::LifetimeScope::Absent;
        Some(usage)
    }).unwrap_or_else(|| { let mut usage = UsageSample::default(); usage.invalidate_context(); usage })
}

/// Deterministic byte identity for one session: a `session_meta` record
/// then one `message_node` record per row, ordered by `node_id`. This is
/// the substrate for hashing, vault snapshots, and `verify` — the SQLite
/// file itself is never treated as the transcript.
pub fn export_bytes(db: &Path, session_id: &str) -> Result<Vec<u8>, AdapterError> {
    let conn = open_readonly(db)?;
    // Pin one WAL snapshot across both queries — otherwise a commit between
    // the meta read and the node scan can tear the exported view.
    conn.execute_batch("BEGIN DEFERRED")
        .map_err(rusqlite_io(db))?;
    let out = export_bytes_conn(&conn, db, session_id);
    let _ = conn.execute_batch("ROLLBACK");
    out
}

/// Export metadata and nodes using the same pinned read transaction.
fn export_bytes_conn(
    conn: &Connection,
    db: &Path,
    session_id: &str,
) -> Result<Vec<u8>, AdapterError> {
    let session = conn
        .query_row(
            "SELECT id, working_directory, created_at, last_activity_at, main_chain_id \
             FROM sessions WHERE id = ?1",
            [session_id],
            |r| {
                Ok(serde_json::json!({
                    "type": "session_meta",
                    "session_id": r.get::<_, String>(0)?,
                    "working_directory": r.get::<_, Option<String>>(1)?,
                    "created_at": r.get::<_, Option<i64>>(2)?,
                    "last_activity_at": r.get::<_, Option<i64>>(3)?,
                    "main_chain_id": r.get::<_, Option<i64>>(4)?,
                }))
            },
        )
        .map_err(rusqlite_io(db))?;
    let mut stmt = conn
        .prepare(
            "SELECT node_id, parent_node_id, chat_message, created_at, metadata \
             FROM message_nodes WHERE session_id = ?1 ORDER BY node_id",
        )
        .map_err(rusqlite_io(db))?;
    let rows = stmt
        .query_map([session_id], |r| {
            Ok(serde_json::json!({
                "type": "message_node",
                "node_id": r.get::<_, i64>(0)?,
                "parent_node_id": r.get::<_, Option<i64>>(1)?,
                "chat_message": r.get::<_, String>(2)?,
                "created_at": r.get::<_, Option<i64>>(3)?,
                "metadata": r.get::<_, Option<String>>(4)?,
            }))
        })
        .map_err(rusqlite_io(db))?;
    let mut out = serde_json::to_vec(&session).unwrap_or_default();
    out.push(b'\n');
    for (index, row) in rows.enumerate() {
        if index >= gobstopper_core::validation::MAX_ITEMS - 1 {
            return Err(AdapterError::InvalidEdit("transcript exceeds record limit"));
        }
        let row = row.map_err(rusqlite_io(db))?;
        // chat_message and metadata are JSON text columns; embed them as
        // values rather than escaped strings so consumers parse once.
        let mut record = row;
        for key in ["chat_message", "metadata"] {
            if let Some(raw) = record.get(key).and_then(Value::as_str) {
                let parsed = crate::payload::decode_record(raw).map_err(|_| {
                    AdapterError::InvalidEdit("Devin store contains invalid or ambiguous JSON")
                })?;
                record[key] = parsed;
            }
        }
        let encoded = serde_json::to_vec(&record).unwrap_or_default();
        if out.len().saturating_add(encoded.len()).saturating_add(1) as u64
            > crate::transaction::max_transcript_bytes()
        {
            return Err(AdapterError::InvalidEdit("transcript exceeds byte limit"));
        }
        out.extend_from_slice(&encoded);
        out.push(b'\n');
    }
    Ok(out)
}

/// Provider-native compaction for an *idle* Devin session, driven over
/// the ACP stdio bridge: `devin acp` → `initialize` → `session/load` →
/// `session/prompt "/compact"`. The bridge interprets the text as the
/// advertised `/compact` command and runs the provider's own compactor
/// (a `file_compactor` summary node lands on the main chain). Print-mode
/// `-p "/compact"` is a silent no-op by contrast — verified empirically.
///
/// The caller should make an idle-session observation before dispatch; this
/// observation does not establish lifetime custody. Provider session/load must
/// reject an already-owned session; qualification of that contract is separate.
///
/// `session/load` on a live-owned session would fork the context the
/// TUI holds. Initialize and session/load are acknowledged sequentially
/// before dispatching /compact. Success needs both its acknowledgment and
/// a terminal notification for the selected session, in either order.
/// The owned process group and reader are collected on every return.
pub fn acp_compact(
    devin_bin: &Path,
    session_id: &str,
    cwd: &Path,
    timeout_secs: u64,
) -> Result<(), AdapterError> {
    acp_compact_in_home(devin_bin, session_id, cwd, timeout_secs, None)
}

const MAX_ACP_FRAME_BYTES: usize = 1024 * 1024;
const MAX_ACP_REQUEST_BYTES: usize = 16 * 1024;
const MAX_ACP_QUEUED_FRAMES: usize = 32;

#[cfg(unix)]
fn acp_nonblocking(pipe: &impl std::os::fd::AsRawFd) -> std::io::Result<()> {
    let fd = pipe.as_raw_fd();
    // SAFETY: the pipe owns this live descriptor throughout both calls.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: F_SETFL updates only this owned pipe's status flags.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn acp_nonblocking<T>(_pipe: &T) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "bounded ACP pipes are unsupported on this platform",
    ))
}

struct AcpChild {
    child: std::process::Child,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Drop for AcpChild {
    fn drop(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        #[cfg(unix)]
        {
            // SAFETY: spawn establishes a new process group with this child's
            // PID. No path reaps the leader before Drop, so its PID cannot be
            // reused for an unrelated group before this signal.
            unsafe {
                libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        // The reader uses a nonblocking owned pipe and checks cancellation,
        // so inherited pipes cannot keep this join waiting for a descendant.
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[derive(Clone, Copy)]
enum AcpReadFailure {
    Io = 1,
    Oversized = 2,
    InvalidUtf8 = 3,
    QueueTimeout = 4,
}

fn acp_read_failure(failure: &std::sync::atomic::AtomicU8) -> Option<&'static str> {
    match failure.load(std::sync::atomic::Ordering::Acquire) {
        0 => None,
        1 => Some("ACP response read failed"),
        2 => Some("ACP response frame exceeds byte limit"),
        3 => Some("ACP response frame is not UTF-8"),
        _ => Some("ACP response queue deadline exceeded"),
    }
}

fn acp_read_frames(
    mut stdout: std::process::ChildStdout,
    sender: std::sync::mpsc::SyncSender<String>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    failure: std::sync::Arc<std::sync::atomic::AtomicU8>,
    deadline: std::time::Instant,
) {
    use std::io::Read;
    use std::sync::atomic::Ordering;
    let send = |frame: Vec<u8>| -> Result<(), AcpReadFailure> {
        let mut frame = String::from_utf8(frame).map_err(|_| AcpReadFailure::InvalidUtf8)?;
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(AcpReadFailure::QueueTimeout);
            }
            match sender.try_send(frame) {
                Ok(()) | Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return Ok(()),
                Err(std::sync::mpsc::TrySendError::Full(pending)) => {
                    frame = pending;
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }
    };
    let result = (|| -> Result<(), AcpReadFailure> {
        let mut frame = Vec::new();
        let mut buffer = [0u8; 8192];
        while !cancelled.load(Ordering::Acquire) {
            match stdout.read(&mut buffer) {
                Ok(0) => {
                    if !frame.is_empty() {
                        send(frame)?;
                    }
                    return Ok(());
                }
                Ok(count) => {
                    for part in buffer[..count].split_inclusive(|byte| *byte == b'\n') {
                        if frame.len() + part.len() > MAX_ACP_FRAME_BYTES {
                            return Err(AcpReadFailure::Oversized);
                        }
                        frame.extend_from_slice(part);
                        if part.last() == Some(&b'\n') {
                            send(std::mem::take(&mut frame))?;
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(AcpReadFailure::Io),
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        failure.store(error as u8, Ordering::Release);
    }
}

fn acp_write_request(
    stdin: &mut std::process::ChildStdin,
    request: &[u8],
    deadline: std::time::Instant,
) -> std::io::Result<()> {
    use std::io::Write;
    let mut remaining = request;
    while !remaining.is_empty() {
        if std::time::Instant::now() >= deadline {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        match stdin.write(remaining) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(count) => remaining = &remaining[count..],
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn acp_observe_compaction(message: &Value, session_id: &str) -> Result<bool, &'static str> {
    if message.get("id").is_some()
        || message.get("method").and_then(Value::as_str) != Some("_cognition.ai/compaction")
        || message.pointer("/params/sessionId").and_then(Value::as_str) != Some(session_id)
    {
        return Ok(false);
    }
    match message.pointer("/params/status").and_then(Value::as_str) {
        Some("compacted" | "completed" | "done") => Ok(true),
        Some("failed" | "error" | "cancelled" | "canceled") => Err("ACP compaction failed"),
        _ => Ok(false),
    }
}

/// Run the ACP bridge against the same Devin data root used for discovery.
pub fn acp_compact_in_home(
    devin_bin: &Path,
    session_id: &str,
    cwd: &Path,
    timeout_secs: u64,
    devin_home: Option<&Path>,
) -> Result<(), AdapterError> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let io_err = |kind, message: &str| AdapterError::Io {
        path: devin_bin.to_path_buf(),
        source: std::io::Error::new(kind, message.to_string()),
    };
    if session_id.is_empty()
        || session_id.len() > 128
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(io_err(
            std::io::ErrorKind::InvalidInput,
            "invalid ACP session identifier",
        ));
    }
    let cwd = cwd.to_str().ok_or_else(|| {
        io_err(
            std::io::ErrorKind::InvalidInput,
            "ACP working directory is not UTF-8",
        )
    })?;
    if cwd.len() > MAX_ACP_REQUEST_BYTES {
        return Err(io_err(
            std::io::ErrorKind::InvalidInput,
            "ACP request exceeds byte limit",
        ));
    }
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(timeout_secs))
        .ok_or_else(|| {
            io_err(
                std::io::ErrorKind::InvalidInput,
                "ACP timeout exceeds supported range",
            )
        })?;
    // Bound serialization and admission before a provider process is launched.
    let requests = [
        (1, serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false}}}})),
        (2, serde_json::json!({"jsonrpc":"2.0","id":2,"method":"session/load","params":{"sessionId":session_id,"cwd":cwd,"mcpServers":[]}})),
        (3, serde_json::json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":session_id,"prompt":[{"type":"text","text":"/compact"}]}})),
    ].into_iter().map(|(id, request)| {
        let mut bytes = serde_json::to_vec(&request)
            .map_err(|_| io_err(std::io::ErrorKind::InvalidInput, "ACP request cannot be serialized"))?;
        bytes.push(b'\n');
        if bytes.len() > MAX_ACP_REQUEST_BYTES {
            return Err(io_err(std::io::ErrorKind::InvalidInput, "ACP request exceeds byte limit"));
        }
        Ok((id, bytes))
    }).collect::<Result<Vec<_>, _>>()?;
    let mut command = Command::new(devin_bin);
    command
        .arg("acp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(home) = devin_home {
        command.env("DEVIN_DATA_DIR", home);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command
        .spawn()
        .map_err(|error| io_err(error.kind(), "ACP bridge could not be started"))?;
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let failure = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0));
    let mut custody = AcpChild {
        child,
        cancelled: cancelled.clone(),
        reader: None,
    };
    let mut stdin = custody
        .child
        .stdin
        .take()
        .ok_or_else(|| io_err(std::io::ErrorKind::Other, "ACP stdin is unavailable"))?;
    let stdout = custody
        .child
        .stdout
        .take()
        .ok_or_else(|| io_err(std::io::ErrorKind::Other, "ACP stdout is unavailable"))?;
    acp_nonblocking(&stdin)
        .and_then(|()| acp_nonblocking(&stdout))
        .map_err(|error| io_err(error.kind(), "ACP nonblocking pipes are unavailable"))?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(MAX_ACP_QUEUED_FRAMES);
    let reader_failure = failure.clone();
    custody.reader = Some(
        std::thread::Builder::new()
            .name("gobstopper-acp-reader".into())
            .spawn(move || acp_read_frames(stdout, sender, cancelled, reader_failure, deadline))
            .map_err(|error| io_err(error.kind(), "ACP reader could not be started"))?,
    );

    let receive = || -> Result<Value, AdapterError> {
        loop {
            if let Some(reason) = acp_read_failure(&failure) {
                return Err(io_err(std::io::ErrorKind::InvalidData, reason));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(io_err(
                    std::io::ErrorKind::TimedOut,
                    "ACP response timed out; outcome unknown",
                ));
            }
            let line = receiver.recv_timeout(deadline - now).map_err(|error| {
                if let Some(reason) = acp_read_failure(&failure) {
                    io_err(std::io::ErrorKind::InvalidData, reason)
                } else {
                    match error {
                        std::sync::mpsc::RecvTimeoutError::Timeout => io_err(
                            std::io::ErrorKind::TimedOut,
                            "ACP response timed out; outcome unknown",
                        ),
                        std::sync::mpsc::RecvTimeoutError::Disconnected => io_err(
                            std::io::ErrorKind::UnexpectedEof,
                            "ACP bridge closed before matching completion",
                        ),
                    }
                }
            })?;
            if line.trim().is_empty() {
                continue;
            }
            let message: Value = serde_json::from_str(&line).map_err(|_| {
                io_err(
                    std::io::ErrorKind::InvalidData,
                    "ACP response is not valid JSON",
                )
            })?;
            if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                return Err(io_err(
                    std::io::ErrorKind::InvalidData,
                    "ACP response has an invalid protocol version",
                ));
            }
            return Ok(message);
        }
    };
    let mut completed = false;
    // Initialize and load must be acknowledged before /compact is dispatched.
    // Once dispatched, matching progress events count even before its ACK.
    for (id, request) in requests {
        if let Some(reason) = acp_read_failure(&failure) {
            return Err(io_err(std::io::ErrorKind::InvalidData, reason));
        }
        acp_write_request(&mut stdin, &request, deadline)
            .map_err(|error| io_err(error.kind(), "ACP request write failed; outcome unknown"))?;
        loop {
            let message = receive()?;
            if id == 3 {
                completed |= acp_observe_compaction(&message, session_id)
                    .map_err(|reason| io_err(std::io::ErrorKind::Other, reason))?;
            }
            if message.get("id") != Some(&serde_json::json!(id)) {
                continue;
            }
            if message.get("method").is_some() {
                return Err(io_err(
                    std::io::ErrorKind::InvalidData,
                    "ACP reply mixes request and response fields",
                ));
            }
            if message.get("error").is_some() {
                // Provider error objects may contain transcript content or credentials.
                return Err(io_err(
                    std::io::ErrorKind::Other,
                    "ACP request was rejected",
                ));
            }
            if message.get("result").is_none() {
                return Err(io_err(
                    std::io::ErrorKind::InvalidData,
                    "ACP reply has no result",
                ));
            }
            if id == 2 {
                let result = message
                    .get("result")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        io_err(
                            std::io::ErrorKind::InvalidData,
                            "ACP session load result is not an object",
                        )
                    })?;
                if let Some(meta) = result.get("_meta") {
                    let meta = meta.as_object().ok_or_else(|| {
                        io_err(
                            std::io::ErrorKind::InvalidData,
                            "ACP session metadata is not an object",
                        )
                    })?;
                    if let Some(locked) = meta.get("cognition.ai/isLocked") {
                        let locked = locked.as_bool().ok_or_else(|| {
                            io_err(
                                std::io::ErrorKind::InvalidData,
                                "ACP session ownership is not a boolean",
                            )
                        })?;
                        if locked {
                            return Err(io_err(
                                std::io::ErrorKind::Other,
                                "ACP session is held by another client",
                            ));
                        }
                    }
                }
            }
            break;
        }
    }
    loop {
        if let Some(reason) = acp_read_failure(&failure) {
            return Err(io_err(std::io::ErrorKind::InvalidData, reason));
        }
        if completed {
            return Ok(());
        }
        completed = acp_observe_compaction(&receive()?, session_id)
            .map_err(|reason| io_err(std::io::ErrorKind::Other, reason))?;
    }
}

/// Session id and working directory from an export file's first record.
/// Used when a detached export is passed to `plan`/`verify` by path.
pub fn scan_meta_export(path: &Path) -> (Option<String>, Option<PathBuf>) {
    let Some(record) = crate::payload::head_records(path, 1).into_iter().next() else {
        return (None, None);
    };
    if record.get("type").and_then(Value::as_str) != Some("session_meta") {
        return (None, None);
    }
    (
        record
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        record
            .get("working_directory")
            .and_then(Value::as_str)
            .map(PathBuf::from),
    )
}

/// Token accounting from a detached export file.
pub fn scan_usage_export(path: &Path) -> UsageSample {
    let Ok(bytes) = crate::transaction::read(path) else {
        let mut usage = UsageSample::default();
        usage.invalidate_context();
        return usage;
    };
    let handle = SessionHandle {
        provider: Provider::Devin,
        session_id: "detached".into(),
        path: path.into(),
        cwd: None,
        age_secs: u64::MAX,
    };
    load_bytes(handle, &bytes)
        .map(|t| t.usage)
        .unwrap_or_else(|_| {
            let mut usage = UsageSample::default();
            usage.invalidate_context();
            usage
        })
}

/// Parse a session into a normalized transcript. `handle.path` may be the
/// shared store (`sessions.db` — the session is exported first) or a
/// detached export file passed directly.
pub fn load(handle: SessionHandle) -> Result<Transcript, AdapterError> {
    let bytes = if is_store_path(&handle.path) {
        export_bytes(&handle.path, &handle.session_id)?
    } else {
        crate::transaction::read(&handle.path)?
    };
    load_bytes(handle, &bytes)
}

/// Direct path mutation is disabled, including detached-looking paths.
/// A caller-provided filename does not prove ownership; use [`transform`].
pub fn apply(_path: &Path, _edits: &[gobstopper_core::plan::Edit]) -> Result<u64, AdapterError> {
    Err(AdapterError::DirectMutationDisabled)
}

/// Transform a canonical detached export in memory. This never opens the
/// provider database or changes the caller's source bytes.
pub fn transform(
    original: &[u8],
    edits: &[gobstopper_core::plan::Edit],
) -> Result<Vec<u8>, AdapterError> {
    crate::payload::check_edit_bounds(edits)?;
    crate::transaction::prepare(Provider::Devin, original, |text| apply_inner(text, edits))
}

fn stub_for(template: &str, bytes: u64, kind: &str) -> String {
    template
        .replace("{bytes}", &bytes.to_string())
        .replace("{kind}", kind)
}

fn elide_record(line: &str, stub_template: &str, stub_override: Option<&str>) -> (String, u64) {
    let Ok(mut record) = crate::payload::decode_record(line) else {
        return (line.to_string(), 0);
    };
    if record.get("type").and_then(Value::as_str) != Some("message_node")
        || record.pointer("/chat_message/role").and_then(Value::as_str) != Some("tool")
    {
        return (line.to_string(), 0);
    }
    let Some(content) = record
        .get_mut("chat_message")
        .and_then(|m| m.get_mut("content"))
    else {
        return (line.to_string(), 0);
    };
    let old = crate::payload::eligible_bytes(content);
    if old == 0 {
        return (line.to_string(), 0);
    }
    let stub = stub_override
        .map(str::to_string)
        .unwrap_or_else(|| stub_for(stub_template, old, "tool_result"));
    let reclaimed = crate::payload::elide(content, stub);
    if reclaimed == 0 {
        return (line.to_string(), 0);
    }
    (
        serde_json::to_string(&record).unwrap_or_else(|_| line.to_string()),
        reclaimed,
    )
}

const DIGEST_MARKER: &str = "[gobstopper state card]";

fn digest_message(digest: &gobstopper_core::plan::DigestBlock) -> Value {
    let mut text = String::from(DIGEST_MARKER);
    text.push('\n');
    if let Some(goal) = &digest.goal {
        text.push_str(&format!("goal: {goal}\n"));
    }
    if let Some(summary) = &digest.summary {
        text.push_str(&format!("summary: {summary}\n"));
    }
    for c in &digest.concepts {
        text.push_str(&format!("concept: {c}\n"));
    }
    for f in &digest.files_touched {
        text.push_str(&format!("file: {f}\n"));
    }
    for d in &digest.decisions {
        text.push_str(&format!("decision: {d}\n"));
    }
    for e in &digest.errors {
        text.push_str(&format!("error: {e}\n"));
    }
    for t in &digest.open_tasks {
        text.push_str(&format!("todo: {t}\n"));
    }
    if let Some(work) = &digest.current_work {
        text.push_str(&format!("current: {work}\n"));
    }
    if let Some(context) = &digest.context {
        text.push_str(&format!("context: {context}\n"));
    }
    text.push_str(&format!(
        "(covers {} earlier records)\n",
        digest.covers_items
    ));
    serde_json::json!({
        "role": "user",
        "content": text,
        "metadata": {"is_user_input": true},
    })
}

fn apply_inner(
    original: &str,
    edits: &[gobstopper_core::plan::Edit],
) -> Result<String, AdapterError> {
    use gobstopper_core::plan::Edit;
    let mut raw = original.to_string();
    for edit in edits {
        match edit {
            Edit::Elide {
                line_indexes,
                stub_template,
                per_item_stubs,
            } => {
                let targets = crate::payload::elision_targets(Provider::Devin, &raw, line_indexes)?;
                let mut out = String::with_capacity(raw.len());
                for (idx, line) in raw.split_inclusive('\n').enumerate() {
                    if !targets.contains(&idx) {
                        out.push_str(line);
                        continue;
                    }
                    let (rewritten, _) = elide_record(
                        line.trim_end_matches('\n'),
                        stub_template,
                        per_item_stubs.get(&idx).map(String::as_str),
                    );
                    out.push_str(&rewritten);
                    if line.ends_with('\n') {
                        out.push('\n');
                    }
                }
                raw = out;
            }
            // A digest lands as a synthetic user-input node appended to the
            // live chain tail, so replayed exports keep it in context. The
            // head must be repointed at the new node — liveness walks up
            // from `main_chain_id`, so a child of the old head is dead.
            Edit::InjectDigest { digest } => {
                let mut head: Option<i64> = None;
                let mut last_node: Option<i64> = None;
                let mut meta_line: Option<usize> = None;
                for (index, line) in raw.lines().enumerate() {
                    let Ok(record) = crate::payload::decode_record(line) else {
                        continue;
                    };
                    match record.get("type").and_then(Value::as_str) {
                        Some("session_meta") => {
                            meta_line = Some(index);
                            head = record.get("main_chain_id").and_then(Value::as_i64).or(head);
                        }
                        Some("message_node") => {
                            if let Some(id) = record.get("node_id").and_then(Value::as_i64) {
                                last_node = Some(last_node.map_or(id, |m| m.max(id)));
                            }
                        }
                        _ => {}
                    }
                }
                let (Some(head), Some(meta_index)) = (head, meta_line) else {
                    return Err(AdapterError::InvalidEdit(
                        "devin export has no session head for digest attachment",
                    ));
                };
                let (parsed_head, rows) = parse_export(raw.as_bytes())?;
                if parsed_head != Some(head) || live_nodes(&rows, parsed_head).is_empty() {
                    return Err(AdapterError::InvalidEdit(
                        "Devin digest attachment has ambiguous or broken linkage",
                    ));
                }
                let new_id = last_node
                    .and_then(|m| m.checked_add(1))
                    .ok_or(AdapterError::InvalidEdit("Devin node identity exhausted"))?;
                let mut lines: Vec<String> = raw.split('\n').map(str::to_string).collect();
                let mut meta: Value = serde_json::from_str(&lines[meta_index]).map_err(|_| {
                    AdapterError::InvalidEdit("devin session_meta is not parseable")
                })?;
                meta["main_chain_id"] = Value::from(new_id);
                lines[meta_index] = serde_json::to_string(&meta)
                    .map_err(|_| AdapterError::InvalidEdit("session_meta not serializable"))?;
                raw = lines.join("\n");
                let record = serde_json::json!({
                    "type": "message_node",
                    "node_id": new_id,
                    "parent_node_id": head,
                    "chat_message": digest_message(digest),
                    "created_at": Value::Null,
                    "metadata": Value::Null,
                });
                crate::transaction::append_record(&mut raw, &record)?;
            }
            Edit::ProviderCompact { .. } | Edit::CacheEdit { .. } => {}
        }
    }
    Ok(raw)
}

#[derive(Debug)]
pub struct StoreReport {
    pub nodes_rewritten: u64,
    pub digest_node_id: Option<i64>,
    pub reclaimed_bytes: u64,
    /// sha256 of the post-write canonical export — the receipt's identity
    /// for the committed state.
    pub export_sha256: String,
    /// Provider-native resume command for the mutated session.
    pub resume_hint: String,
}

/// Direct provider-store writes are disabled until compatible lifetime custody
/// and target/recovery binding are qualified. Refusal precedes path resolution,
/// database opening, lock probing and every other effect.
pub fn apply_store(
    _root: &Path,
    _session_id: &str,
    _source_sha256: &str,
    _edits: &[gobstopper_core::plan::Edit],
) -> Result<StoreReport, AdapterError> {
    Err(AdapterError::DirectMutationDisabled)
}

/// Direct store restoration is disabled before inspecting the supplied store
/// or snapshot. Matching session IDs alone cannot authorize a database write.
pub fn restore_store(
    _root: &Path,
    _session_id: &str,
    _snapshot_bytes: &[u8],
) -> Result<StoreReport, AdapterError> {
    Err(AdapterError::DirectMutationDisabled)
}

/// Assistant `tool_calls[].id` → call name/label, used to annotate the
/// tool results that answer them and for orphan checks in `verify`.
struct NodeRow {
    line_index: usize,
    node_id: i64,
    parent_node_id: Option<i64>,
    message: Value,
    num_tokens_preceding: Option<u64>,
}

fn parse_export(bytes: &[u8]) -> Result<(Option<i64>, Vec<NodeRow>), AdapterError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| AdapterError::InvalidEdit("devin export is not valid UTF-8"))?;
    let mut main_chain_id = None;
    let mut rows = Vec::new();
    let mut saw_meta = false;
    let mut ambiguous = false;
    for (index, line) in text.lines().enumerate() {
        if index >= gobstopper_core::validation::MAX_ITEMS {
            return Err(AdapterError::InvalidEdit("transcript exceeds record limit"));
        }
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = crate::payload::decode_record(line) else {
            ambiguous = true;
            continue;
        };
        match record.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                ambiguous |= saw_meta
                    || !record
                        .get("main_chain_id")
                        .is_some_and(|v| v.is_null() || v.as_i64().is_some_and(|id| id >= 0));
                saw_meta = true;
                main_chain_id = record
                    .get("main_chain_id")
                    .and_then(Value::as_i64)
                    .or(main_chain_id);
            }
            Some("message_node") => {
                let Some(node_id) = record.get("node_id").and_then(Value::as_i64) else {
                    ambiguous = true;
                    continue;
                };
                ambiguous |= !record
                    .get("parent_node_id")
                    .is_some_and(|v| v.is_null() || v.as_i64().is_some_and(|id| id >= 0));
                let message = record.get("chat_message").cloned().unwrap_or(Value::Null);
                ambiguous |= !message.is_object();
                let num_tokens_preceding = record
                    .get("metadata")
                    .and_then(|m| m.get("num_tokens_preceding"))
                    .and_then(Value::as_u64);
                rows.push(NodeRow {
                    line_index: index,
                    node_id,
                    parent_node_id: record.get("parent_node_id").and_then(Value::as_i64),
                    message,
                    num_tokens_preceding,
                });
            }
            _ => continue,
        }
    }
    Ok((
        if ambiguous || !saw_meta {
            None
        } else {
            main_chain_id
        },
        rows,
    ))
}

/// Node ids reachable by walking `parent_node_id` up from the chain head.
fn live_nodes(rows: &[NodeRow], head: Option<i64>) -> std::collections::HashSet<i64> {
    let mut parents = std::collections::HashMap::new();
    for row in rows {
        if row.node_id < 0
            || parents
                .insert(row.node_id, (row.parent_node_id, row.line_index))
                .is_some()
        {
            return Default::default();
        }
    }
    let mut live = std::collections::HashSet::new();
    let mut cursor = head;
    while let Some(id) = cursor {
        let Some((parent, line)) = parents.get(&id) else {
            return Default::default();
        };
        if !live.insert(id) {
            return Default::default();
        }
        if parent.is_some_and(|id| {
            parents
                .get(&id)
                .is_none_or(|(_, parent_line)| parent_line >= line)
        }) {
            return Default::default();
        }
        cursor = *parent;
    }
    live
}

/// Fold one message's provider metrics into the usage sample. `on_live`
/// gates `context_tokens` (the latest live assistant reading wins) while
/// lifetime counters accumulate across every assistant message.
fn absorb_usage(message: &Value, usage: &mut UsageSample, on_live: bool) {
    use gobstopper_core::model::{ContextState, LifetimeScope};
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let parts = (|| {
        let metrics = message.pointer("/metadata/metrics")?.as_object()?;
        let component = |key: &str| match metrics.get(key) {
            None => Some(0),
            Some(value) => value.as_u64(),
        };
        let input = metrics
            .get("input_tokens")?
            .as_u64()?
            .saturating_add(component("cache_read_tokens")?)
            .saturating_add(component("cache_creation_tokens")?);
        Some((
            input,
            component("cache_read_tokens")?,
            input.saturating_add(component("output_tokens")?),
        ))
    })();
    let Some((input, cached, context)) = parts else {
        usage.lifetime_scope = LifetimeScope::Partial;
        if on_live {
            usage.invalidate_context();
        }
        return;
    };
    if on_live {
        usage.context_tokens = context;
        usage.context_state = ContextState::Reported;
    }
    usage.lifetime_input_tokens = usage.lifetime_input_tokens.saturating_add(input);
    usage.lifetime_cached_tokens = usage
        .lifetime_cached_tokens
        .saturating_add(cached)
        .min(usage.lifetime_input_tokens);
    if usage.lifetime_scope != LifetimeScope::Partial {
        usage.lifetime_scope = LifetimeScope::Full;
    }
}

fn item_tokens(message: &Value) -> u64 {
    if let Some(n) = message
        .get("metadata")
        .and_then(|m| m.get("num_tokens"))
        .and_then(Value::as_u64)
    {
        return n;
    }
    estimate_tokens(
        crate::payload::text_bytes(message.get("content").unwrap_or(&Value::Null)) as usize,
    )
}

fn collect_tool_calls(message: &Value, map: &mut std::collections::HashMap<String, String>) {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let Some(calls) = message.get("tool_calls").and_then(Value::as_array) else {
        return;
    };
    for call in calls {
        if let Some(id) = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| valid_tool_id(id))
        {
            map.entry(id.to_string()).or_insert_with(|| {
                call.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_string()
            });
        }
    }
}

pub(crate) fn valid_tool_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

/// Exact identity takes precedence. Otherwise one supported '#' prefix must
/// uniquely resolve; overlapping nonce namespaces are ambiguous, never guessed.
pub(crate) fn matching_tool_call<'a, V>(
    calls: &std::collections::HashMap<String, V>,
    result: &'a str,
) -> Option<&'a str> {
    if !valid_tool_id(result) {
        return None;
    }
    if calls.contains_key(result) {
        return Some(result);
    }
    let mut found = None;
    for (index, _) in result.match_indices('#') {
        let prefix = &result[..index + 1];
        if index + 1 < result.len() && calls.contains_key(prefix) {
            if found.is_some() {
                return None;
            }
            found = Some(prefix);
        }
    }
    found
}

fn user_prompt_summary(message: &Value) -> Option<String> {
    const MAX_SUMMARY: usize = 200;
    let meta = message.get("metadata")?;
    if meta.get("is_user_input").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let text = crate::payload::text(message.get("content")?);
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= MAX_SUMMARY {
        return Some(text);
    }
    Some(text.chars().take(MAX_SUMMARY).collect())
}

pub fn load_bytes(handle: SessionHandle, bytes: &[u8]) -> Result<Transcript, AdapterError> {
    if bytes.len() as u64 > crate::transaction::max_transcript_bytes() {
        return Err(AdapterError::InvalidEdit("transcript exceeds byte limit"));
    }
    let (main_chain_id, rows) = parse_export(bytes)?;
    let live = live_nodes(&rows, main_chain_id);
    let message_ids: std::collections::HashMap<i64, String> = rows
        .iter()
        .filter_map(|r| {
            r.message
                .get("message_id")
                .and_then(Value::as_str)
                .map(|id| (r.node_id, id.to_string()))
        })
        .collect();
    let mut calls = std::collections::HashMap::new();
    let mut usage = UsageSample::default();
    let mut preceding_on_live: Option<u64> = None;
    let mut items = Vec::with_capacity(rows.len() + 1);
    for row in &rows {
        let line_index = row.line_index;
        let on_live = live.contains(&row.node_id);
        if on_live {
            collect_tool_calls(&row.message, &mut calls);
        }
        let role = row
            .message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("meta");
        absorb_usage(&row.message, &mut usage, on_live);
        if on_live {
            if let Some(n) = row.num_tokens_preceding {
                preceding_on_live = Some(n);
            }
        }
        let est_tokens = if on_live {
            item_tokens(&row.message)
        } else {
            0
        };
        let (kind, elidable_bytes, tool_use_ids, label) = match role {
            "system" => (ItemKind::System, None, Vec::new(), "system".to_string()),
            "user" => (ItemKind::User, None, Vec::new(), "user".to_string()),
            "assistant" => {
                let label = if row
                    .message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .is_some_and(|c| !c.is_empty())
                {
                    "assistant+tool_calls".to_string()
                } else {
                    "assistant".to_string()
                };
                (ItemKind::Assistant, None, Vec::new(), label)
            }
            "tool" => {
                let id = row
                    .message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let name = id
                    .as_deref()
                    .and_then(|id| matching_tool_call(&calls, id))
                    .and_then(|id| calls.get(id))
                    .cloned()
                    .unwrap_or_else(|| "?".to_string());
                let content = row.message.get("content").cloned().unwrap_or(Value::Null);
                let supported = id.as_deref().is_some_and(valid_tool_id)
                    && row.message.as_object().is_some_and(|object| {
                        object.keys().all(|key| {
                            matches!(
                                key.as_str(),
                                "role" | "message_id" | "content" | "tool_call_id" | "metadata"
                            )
                        })
                    });
                let elidable = if supported {
                    crate::payload::eligible_bytes(&content)
                } else {
                    0
                };
                (
                    ItemKind::ToolResult,
                    (elidable > 0).then_some(elidable),
                    id.into_iter().collect(),
                    format!("tool_result:{name}"),
                )
            }
            _ => (ItemKind::Meta, None, Vec::new(), format!("meta:{role}")),
        };
        // Dead branches keep identity and structure but contribute no
        // context tokens and are never elision candidates.
        let elidable_bytes = if on_live { elidable_bytes } else { None };
        let payload_sha256 = row
            .message
            .get("content")
            .filter(|_| elidable_bytes.is_some())
            .and_then(|c| crate::payload::fingerprint([c]));
        items.push(TranscriptItem {
            line_index,
            kind,
            est_tokens,
            elidable_bytes,
            elidable_parts: u32::from(elidable_bytes.is_some()),
            label,
            summary: user_prompt_summary(&row.message),
            uuid: message_ids
                .get(&row.node_id)
                .cloned()
                .or_else(|| Some(row.node_id.to_string())),
            parent_uuid: row.parent_node_id.map(|p| {
                message_ids
                    .get(&p)
                    .cloned()
                    .unwrap_or_else(|| p.to_string())
            }),
            tool_use_ids,
            payload_sha256,
        });
    }
    if usage.context_state != gobstopper_core::model::ContextState::Reported {
        if let Some(n) = preceding_on_live {
            // This excludes the current message, so it is an estimator only.
            usage.context_tokens = n;
            usage.context_state = gobstopper_core::model::ContextState::Unknown;
        }
    }
    if live.is_empty() && !rows.is_empty() {
        usage.invalidate_context();
        usage.lifetime_scope = gobstopper_core::model::LifetimeScope::Partial;
    }
    if crate::verify::verify(Provider::Devin, bytes)
        .iter()
        .any(|f| f.code == "duplicate_tool_call_id")
    {
        usage.invalidate_context();
        for item in &mut items {
            item.est_tokens = 0;
            item.elidable_bytes = None;
            item.elidable_parts = 0;
            item.tool_use_ids.clear();
            item.payload_sha256 = None;
        }
    }
    Ok(Transcript {
        session: handle,
        items,
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use std::fs;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gobstopper-devin-{}-{}-{tag}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let root = tmpdir(tag);
            let conn = Connection::open(db_path(&root)).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    working_directory TEXT,
                    backend_type TEXT,
                    model TEXT,
                    agent_mode TEXT,
                    created_at INTEGER,
                    last_activity_at INTEGER,
                    title TEXT,
                    main_chain_id INTEGER,
                    shell_last_seen_index INTEGER,
                    cogs_json TEXT,
                    workspace_dirs TEXT,
                    hidden INTEGER,
                    metadata TEXT
                );
                CREATE TABLE message_nodes (
                    row_id INTEGER PRIMARY KEY,
                    session_id TEXT,
                    node_id INTEGER,
                    parent_node_id INTEGER,
                    chat_message TEXT,
                    created_at INTEGER,
                    metadata TEXT
                );",
            )
            .unwrap();
            Self { root }
        }

        fn add_session(&self, id: &str, title: &str, main_chain_id: i64, last_activity: i64) {
            let conn = Connection::open(db_path(&self.root)).unwrap();
            conn.execute(
                "INSERT INTO sessions (id, working_directory, title, main_chain_id, \
                 created_at, last_activity_at, hidden) VALUES (?1, ?2, ?3, ?4, ?5, ?5, 0)",
                params![id, "/work/repo", title, main_chain_id, last_activity],
            )
            .unwrap();
        }

        fn add_node(
            &self,
            session: &str,
            node_id: i64,
            parent: Option<i64>,
            message: Value,
            metadata: Option<&str>,
        ) {
            let conn = Connection::open(db_path(&self.root)).unwrap();
            conn.execute(
                "INSERT INTO message_nodes (session_id, node_id, parent_node_id, \
                 chat_message, created_at, metadata) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    session,
                    node_id,
                    parent,
                    serde_json::to_string(&message).unwrap(),
                    1_790_000_000i64,
                    metadata
                ],
            )
            .unwrap();
        }

        fn handle(&self, session: &str) -> SessionHandle {
            SessionHandle {
                provider: Provider::Devin,
                session_id: session.into(),
                path: db_path(&self.root),
                cwd: None,
                age_secs: 10,
            }
        }
    }

    fn assistant(content: &str, metrics: Option<Value>) -> Value {
        serde_json::json!({
            "message_id": format!("m-{}", content.len()),
            "role": "assistant",
            "content": content,
            "metadata": { "num_tokens": 100, "metrics": metrics.unwrap_or(Value::Null) },
        })
    }

    #[test]
    fn discover_and_load_basic_chain() {
        let fx = Fixture::new("basic");
        fx.add_session("sess-a", "demo", 4, 1_790_006_000);
        fx.add_node(
            "sess-a",
            0,
            None,
            serde_json::json!({"message_id": "s0", "role": "system", "content": "rules"}),
            None,
        );
        fx.add_node(
            "sess-a",
            1,
            Some(0),
            serde_json::json!({"message_id": "u1", "role": "user", "content": "hi",
                "metadata": {"is_user_input": true}}),
            None,
        );
        fx.add_node(
            "sess-a",
            2,
            Some(1),
            serde_json::json!({"message_id": "a1", "role": "assistant", "content": "",
                "tool_calls": [{"id": "call_1", "name": "exec", "arguments": {"command": "ls"}}],
                "metadata": {"num_tokens": 50, "metrics": {"input_tokens": 900, "output_tokens": 50, "cache_read_tokens": 5000}}}),
            None,
        );
        let big = "x".repeat(2000);
        fx.add_node(
            "sess-a",
            3,
            Some(2),
            serde_json::json!({"message_id": "t1", "role": "tool", "tool_call_id": "call_1",
                "content": big}),
            None,
        );
        fx.add_node(
            "sess-a",
            4,
            Some(3),
            assistant("done", Some(serde_json::json!({"input_tokens": 1200, "output_tokens": 80, "cache_read_tokens": 6000}))),
            Some("{\"num_tokens_preceding\": 7280}"),
        );

        let found = discover(&fx.root, 0, false);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].handle.session_id, "sess-a");
        assert_eq!(found[0].handle.provider, Provider::Devin);
        assert_eq!(found[0].usage.context_tokens, 1200 + 6000 + 80);

        let t = load(fx.handle("sess-a")).unwrap();
        assert_eq!(t.items.len(), 5);
        assert_eq!(t.items[0].kind, ItemKind::System);
        assert_eq!(t.items[3].kind, ItemKind::ToolResult);
        assert_eq!(t.items[3].tool_use_ids, vec!["call_1"]);
        assert!(t.items[3].elidable_bytes.unwrap() >= 2000);
        assert_eq!(t.items[3].label, "tool_result:exec");
        assert_eq!(t.usage.context_tokens, 1200 + 6000 + 80);
        assert_eq!(t.usage.lifetime_input_tokens, (900 + 5000) + (1200 + 6000));
        assert_eq!(t.items[1].summary.as_deref(), Some("hi"));
        // Linkage preserved: every non-root node names its parent's id.
        assert_eq!(t.items[2].uuid.as_deref(), Some("a1"));
        assert_eq!(t.items[2].parent_uuid.as_deref(), Some("u1"));
    }

    #[test]
    fn context_only_scan_matches_tail_context_without_lifetime() {
        let fx = Fixture::new("tailscan");
        fx.add_session("sess-t", "tail", 39, 1_790_006_000);
        // 40 filler user nodes push the first assistant message outside
        // the 32-row tail window; only the tail assistant's metrics may
        // contribute context.
        fx.add_node(
            "sess-t",
            0,
            None,
            assistant(
                "early",
                Some(serde_json::json!({"input_tokens": 111, "output_tokens": 1})),
            ),
            None,
        );
        for i in 1..39 {
            fx.add_node(
                "sess-t",
                i,
                Some(i - 1),
                serde_json::json!({"message_id": format!("u{i}"), "role": "user", "content": "q"}),
                None,
            );
        }
        fx.add_node(
            "sess-t",
            39,
            Some(38),
            assistant(
                "latest",
                Some(serde_json::json!({"input_tokens": 2000, "output_tokens": 100, "cache_read_tokens": 3000})),
            ),
            Some("{\"num_tokens_preceding\": 5100}"),
        );

        let full = discover(&fx.root, 0, false);
        let tail = discover(&fx.root, 0, true);
        assert_eq!(tail[0].usage.context_tokens, full[0].usage.context_tokens);
        assert_eq!(tail[0].usage.context_tokens, 2000 + 3000 + 100);
        assert_eq!(full[0].usage.lifetime_input_tokens, 111 + 2000 + 3000);
        assert_eq!(tail[0].usage.lifetime_input_tokens, 0);
    }

    #[test]
    fn discovery_accounting_uses_pinned_live_graph_and_rejects_embedded_duplicates() {
        let fx = Fixture::new("canonical-accounting");
        fx.add_session("s", "fixture", 1, 1_790_006_000);
        fx.add_node(
            "s",
            0,
            None,
            serde_json::json!({"role":"user","content":"goal"}),
            None,
        );
        fx.add_node(
            "s",
            1,
            Some(0),
            assistant(
                "live",
                Some(serde_json::json!({"input_tokens":100,"output_tokens":2})),
            ),
            None,
        );
        fx.add_node(
            "s",
            2,
            Some(0),
            assistant(
                "dead",
                Some(serde_json::json!({"input_tokens":9000,"output_tokens":3})),
            ),
            None,
        );
        let conn = open_readonly(&db_path(&fx.root)).unwrap();
        assert_eq!(scan_usage(&conn, "s").context_tokens, 102);
        assert_eq!(scan_usage(&conn, "s").lifetime_input_tokens, 9100);
        assert_eq!(scan_context(&conn, "s").context_tokens, 102);
        assert_eq!(scan_context(&conn, "s").lifetime_input_tokens, 0);
        let writer = Connection::open(db_path(&fx.root)).unwrap();
        writer
            .execute("UPDATE sessions SET main_chain_id = 99 WHERE id = 's'", [])
            .unwrap();
        assert_eq!(scan_context(&conn, "s").context_tokens, 0);
        assert_eq!(scan_usage(&conn, "s").context_tokens, 0);
        writer
            .execute("UPDATE sessions SET main_chain_id = 1 WHERE id = 's'", [])
            .unwrap();
        writer
            .execute(
                "UPDATE message_nodes SET chat_message = ?1 WHERE session_id = 's' AND node_id = 1",
                [r#"{"role":"assistant","role":"tool","content":"duplicate"}"#],
            )
            .unwrap();
        assert!(export_bytes(&db_path(&fx.root), "s").is_err());
        assert_eq!(scan_context(&conn, "s").context_tokens, 0);
        assert_eq!(scan_usage(&conn, "s").context_tokens, 0);
    }

    #[test]
    fn dead_branch_contributes_no_tokens_or_elision() {
        let fx = Fixture::new("branch");
        fx.add_session("sess-b", "branches", 2, 1_790_006_000);
        fx.add_node(
            "sess-b",
            0,
            None,
            serde_json::json!({"message_id": "u", "role": "user", "content": "q"}),
            None,
        );
        // Live branch head.
        fx.add_node("sess-b", 2, Some(0), assistant("live", None), None);
        // Dead sibling branch carrying a huge tool result.
        fx.add_node(
            "sess-b",
            1,
            Some(0),
            serde_json::json!({"message_id": "t", "role": "tool", "tool_call_id": "c",
                "content": "y".repeat(9000)}),
            None,
        );
        let t = load(fx.handle("sess-b")).unwrap();
        let dead = t
            .items
            .iter()
            .find(|i| i.uuid.as_deref() == Some("t"))
            .unwrap();
        assert_eq!(dead.est_tokens, 0);
        assert_eq!(dead.elidable_bytes, None);
        assert_eq!(
            t.items.iter().map(|i| i.est_tokens).sum::<u64>(),
            100 + estimate_tokens(1)
        );
    }

    #[test]
    fn find_matches_id_prefix_and_title() {
        let fx = Fixture::new("find");
        fx.add_session("victorious-bead", "GOBSTOPPER", 0, 1_790_006_000);
        fx.add_session("other-session", "unrelated", 0, 1_790_005_000);
        assert_eq!(find(&fx.root, "victorious").len(), 1);
        assert_eq!(find(&fx.root, "GOBSTOPPER").len(), 1);
        assert_eq!(find(&fx.root, "missing").len(), 0);
    }

    #[test]
    fn export_is_deterministic_and_roundtrips() {
        let fx = Fixture::new("export");
        fx.add_session("sess-e", "exp", 1, 1_790_006_000);
        fx.add_node(
            "sess-e",
            0,
            None,
            serde_json::json!({"role": "user", "content": "a"}),
            None,
        );
        fx.add_node(
            "sess-e",
            1,
            Some(0),
            serde_json::json!({"role": "assistant", "content": "b"}),
            Some("{\"num_tokens_preceding\": 42}"),
        );
        let a = export_bytes(&db_path(&fx.root), "sess-e").unwrap();
        let b = export_bytes(&db_path(&fx.root), "sess-e").unwrap();
        assert_eq!(a, b);
        let text = String::from_utf8(a).unwrap();
        let mut lines = text.lines();
        let meta: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(meta["type"], "session_meta");
        assert_eq!(meta["main_chain_id"], 1);
        let node: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(node["chat_message"]["role"], "user");
        let last: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(last["metadata"]["num_tokens_preceding"], 42);
    }

    #[test]
    fn session_active_tracks_held_flock() {
        use fs2::FileExt;
        let fx = Fixture::new("lock");
        assert!(!session_active(&fx.root, "sess-l"));
        let locks = fx.root.join("session_locks");
        fs::create_dir_all(&locks).unwrap();
        // A lock file that exists but is not held is stale: inactive.
        fs::write(locks.join("sess-l.lock"), b"12345").unwrap();
        assert!(!session_active(&fx.root, "sess-l"));
        // Held by us => the session is live.
        let held = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(locks.join("sess-l.lock"))
            .unwrap();
        held.lock_exclusive().unwrap();
        assert!(session_active(&fx.root, "sess-l"));
        held.unlock().unwrap();
        drop(held);
        assert!(!session_active(&fx.root, "sess-l"));
        for id in [
            "",
            "../escape",
            "nested/session",
            "line\nbreak",
            &"s".repeat(257),
        ] {
            assert!(session_active(&fx.root, id));
        }
        fs::create_dir(locks.join("directory.lock")).unwrap();
        assert!(session_active(&fx.root, "directory"));
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            use std::os::unix::fs::symlink;
            let fifo = locks.join("pipe.lock");
            let raw = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
            // SAFETY: a valid fresh fixture path and mode, no external target.
            assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
            assert!(session_active(&fx.root, "pipe"));
            symlink(locks.join("sess-l.lock"), locks.join("linked.lock")).unwrap();
            symlink(locks.join("absent.lock"), locks.join("dangling.lock")).unwrap();
            assert!(session_active(&fx.root, "linked"));
            assert!(session_active(&fx.root, "dangling"));

            // The SQLite read boundary must likewise refuse special/link leaves
            // before asking SQLite to open them. A valid database still exports.
            assert!(open_readonly(&fifo).is_err());
            let db_link = fx.root.join("linked.db");
            symlink(db_path(&fx.root), &db_link).unwrap();
            assert!(open_readonly(&db_link).is_err());
            assert!(open_readonly(&locks).is_err());
            assert!(open_readonly(&db_path(&fx.root)).is_ok());

            let moved = fx.root.join("old-locks");
            fs::rename(&locks, &moved).unwrap();
            symlink(&moved, &locks).unwrap();
            assert!(session_active(&fx.root, "sess-l"));
        }
    }

    #[test]
    fn session_observation_reports_usage_and_lock() {
        use fs2::FileExt;
        let fx = Fixture::new("observe");
        fx.add_session("sess-o", "obs", 1, 1_790_006_000);
        fx.add_node(
            "sess-o",
            0,
            None,
            serde_json::json!({"role": "user", "content": "q"}),
            None,
        );
        fx.add_node(
            "sess-o",
            1,
            Some(0),
            assistant("a", Some(serde_json::json!({"input_tokens": 10, "cache_read_tokens": 90, "output_tokens": 5}))),
            None,
        );
        let (usage, active) = session_observation(&fx.root, "sess-o").unwrap();
        assert_eq!(usage.context_tokens, 10 + 90 + 5);
        assert!(!active);
        assert!(session_observation(&fx.root, "missing").is_none());

        let locks = fx.root.join("session_locks");
        fs::create_dir_all(&locks).unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(locks.join("sess-o.lock"))
            .unwrap();
        held.lock_exclusive().unwrap();
        let (_, active) = session_observation(&fx.root, "sess-o").unwrap();
        assert!(active);
    }

    #[test]
    fn current_session_prefers_cwd_match_and_requires_lock() {
        use fs2::FileExt;
        let fx = Fixture::new("current");
        // Fixture sessions all live at /work/repo; sess-new is newer.
        fx.add_session("sess-old", "old", 0, 1_790_006_000);
        fx.add_session("sess-new", "new", 0, 1_790_006_900);
        // No locks held: cwd match alone is not enough.
        assert_eq!(current_session(&fx.root, Path::new("/work/repo/sub")), None);
        let locks = fx.root.join("session_locks");
        fs::create_dir_all(&locks).unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(locks.join("sess-old.lock"))
            .unwrap();
        held.lock_exclusive().unwrap();
        // Containment both ways resolves to the locked session.
        assert_eq!(
            current_session(&fx.root, Path::new("/work/repo/sub/dir")),
            Some("sess-old".to_string())
        );
        // Unrelated cwd + exactly one locked session: that session wins.
        assert_eq!(
            current_session(&fx.root, Path::new("/elsewhere")),
            Some("sess-old".to_string())
        );
        // Two locked sessions, ambiguous cwd: refuse to guess.
        let held2 = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(locks.join("sess-new.lock"))
            .unwrap();
        held2.lock_exclusive().unwrap();
        assert_eq!(current_session(&fx.root, Path::new("/elsewhere")), None);
        // Cwd still disambiguates (newer wins among matches).
        assert_eq!(
            current_session(&fx.root, Path::new("/work/repo")),
            Some("sess-new".to_string())
        );
    }

    #[test]
    fn verify_catches_broken_chain_and_orphans() {
        let fx = Fixture::new("verify");
        fx.add_session("sess-v", "v", 5, 1_790_006_000);
        fx.add_node(
            "sess-v",
            0,
            None,
            serde_json::json!({"role": "assistant", "tool_calls": [{"id": "call_a#", "name": "exec"}]}),
            None,
        );
        // Result answers with the full #nonce suffix (provider prefix pairing).
        fx.add_node(
            "sess-v",
            1,
            Some(0),
            serde_json::json!({"role": "tool", "tool_call_id": "call_a#f00d", "content": "ok"}),
            None,
        );
        // Dangling parent: node 77 does not exist.
        fx.add_node(
            "sess-v",
            5,
            Some(77),
            serde_json::json!({"role": "assistant", "tool_calls": [{"id": "call_b", "name": "exec"}]}),
            None,
        );
        let bytes = export_bytes(&db_path(&fx.root), "sess-v").unwrap();
        let findings = crate::verify::verify(Provider::Devin, &bytes);
        let codes: Vec<&str> = findings.iter().map(|f| f.code).collect();
        // call_a# pairs via prefix => no orphan for it. call_b is unanswered.
        assert!(codes.contains(&"broken_parent_chain"));
        assert!(codes.contains(&"orphaned_tool_call"));
        assert_eq!(
            findings
                .iter()
                .filter(|f| f.code == "orphaned_tool_call")
                .count(),
            1
        );
    }

    fn digest_block() -> gobstopper_core::plan::DigestBlock {
        gobstopper_core::plan::DigestBlock {
            goal: Some("ship it".into()),
            summary: Some("elided the noisy middle".into()),
            concepts: vec!["sqlite".into()],
            files_touched: vec!["devin.rs".into()],
            decisions: vec!["in-place tx".into()],
            errors: vec![],
            open_tasks: vec!["undo path".into()],
            current_work: None,
            context: None,
            covers_items: 3,
        }
    }

    fn node_payload(root: &Path, session: &str, node_id: i64) -> String {
        let conn = Connection::open(db_path(root)).unwrap();
        conn.query_row(
            "SELECT chat_message FROM message_nodes WHERE session_id = ?1 AND node_id = ?2",
            rusqlite::params![session, node_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn chain_head(root: &Path, session: &str) -> i64 {
        let conn = Connection::open(db_path(root)).unwrap();
        conn.query_row(
            "SELECT main_chain_id FROM sessions WHERE id = ?1",
            [session],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn transform_export_elides_and_injects_without_writing_store() {
        let fx = Fixture::new("apply-store");
        fx.add_session("sess-w", "write", 3, 1_790_006_000);
        fx.add_node(
            "sess-w",
            0,
            None,
            serde_json::json!({"role": "user", "content": "go"}),
            None,
        );
        fx.add_node(
            "sess-w",
            1,
            Some(0),
            serde_json::json!({"role": "assistant", "tool_calls": [{"id": "c1", "name": "exec"}]}),
            None,
        );
        fx.add_node(
            "sess-w",
            2,
            Some(1),
            serde_json::json!({"role": "tool", "tool_call_id": "c1", "content": "x".repeat(3000)}),
            None,
        );
        fx.add_node("sess-w", 3, Some(2), assistant("done", None), None);

        let before = export_bytes(&db_path(&fx.root), "sess-w").unwrap();
        let database_before = fs::read(db_path(&fx.root)).unwrap();
        let edits = vec![
            gobstopper_core::plan::Edit::Elide {
                // export line 3 = node 2 (line 0 is session_meta).
                line_indexes: vec![3],
                stub_template: "[elided {bytes} bytes]".into(),
                per_item_stubs: Default::default(),
            },
            gobstopper_core::plan::Edit::InjectDigest {
                digest: digest_block(),
            },
        ];
        let after = transform(&before, &edits).unwrap();
        assert!(after.len() < before.len());
        let records: Vec<Value> = std::str::from_utf8(&after)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let elided = records[3]["chat_message"]["content"].as_str().unwrap();
        assert!(elided.contains("[elided 3000 bytes]"));
        assert!(!elided.contains(&"x".repeat(3000)));
        assert_eq!(records[0]["main_chain_id"], 4);
        let digest = records[5]["chat_message"]["content"].as_str().unwrap();
        assert!(digest.contains("[gobstopper state card]"));
        assert!(digest.contains("goal: ship it"));
        assert!(crate::verify::verify(Provider::Devin, &after).is_empty());
        assert_eq!(
            load_bytes(fx.handle("sess-w"), &after).unwrap().items.len(),
            5
        );
        assert_eq!(fs::read(db_path(&fx.root)).unwrap(), database_before);
        assert_eq!(export_bytes(&db_path(&fx.root), "sess-w").unwrap(), before);
    }

    #[test]
    fn disabled_apply_preserves_provider_append() {
        let fx = Fixture::new("apply-drift");
        fx.add_session("sess-d", "drift", 1, 1_790_006_000);
        fx.add_node(
            "sess-d",
            0,
            None,
            serde_json::json!({"role": "user", "content": "go"}),
            None,
        );
        fx.add_node("sess-d", 1, Some(0), assistant("ok", None), None);
        let before = export_bytes(&db_path(&fx.root), "sess-d").unwrap();
        let sha = crate::copy::sha256(&before);
        // Provider appended a node between plan and apply.
        fx.add_node(
            "sess-d",
            2,
            Some(1),
            serde_json::json!({"role": "user", "content": "new"}),
            None,
        );
        let edits = vec![gobstopper_core::plan::Edit::InjectDigest {
            digest: digest_block(),
        }];
        let database_before = fs::read(db_path(&fx.root)).unwrap();
        let err = apply_store(&fx.root, "sess-d", &sha, &edits).unwrap_err();
        assert!(matches!(err, AdapterError::DirectMutationDisabled));
        assert_eq!(fs::read(db_path(&fx.root)).unwrap(), database_before);
        assert_eq!(chain_head(&fx.root, "sess-d"), 1);
        assert!(node_payload(&fx.root, "sess-d", 1).contains("ok"));
        assert!(node_payload(&fx.root, "sess-d", 2).contains("new"));
    }

    #[test]
    fn apply_store_refuses_locked_session() {
        use fs2::FileExt;
        let fx = Fixture::new("apply-lock");
        fx.add_session("sess-l2", "locked", 0, 1_790_006_000);
        fx.add_node(
            "sess-l2",
            0,
            None,
            serde_json::json!({"role": "user", "content": "x"}),
            None,
        );
        let locks = fx.root.join("session_locks");
        fs::create_dir_all(&locks).unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(locks.join("sess-l2.lock"))
            .unwrap();
        held.lock_exclusive().unwrap();
        let sha = crate::copy::sha256(&export_bytes(&db_path(&fx.root), "sess-l2").unwrap());
        let err = apply_store(&fx.root, "sess-l2", &sha, &[]).unwrap_err();
        assert!(matches!(err, AdapterError::DirectMutationDisabled));
        drop(held);
    }

    // Capture the complete fixture tree, including SQLite WAL/SHM and lock files.
    fn fixture_image(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        fn visit(
            root: &Path,
            path: &Path,
            image: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
        ) {
            if path.is_dir() {
                for entry in fs::read_dir(path).unwrap() {
                    visit(root, &entry.unwrap().path(), image);
                }
            } else {
                image.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
        let mut image = std::collections::BTreeMap::new();
        visit(root, root, &mut image);
        image
    }

    #[test]
    fn disabled_store_operations_refuse_foreign_identity_and_graph_without_effects() {
        let source = Fixture::new("disabled-source");
        let target = Fixture::new("disabled-target");
        for fixture in [&source, &target] {
            fixture.add_session("same-id", "session", 0, 1_790_006_000);
            fixture.add_node(
                "same-id",
                0,
                None,
                serde_json::json!({"role":"user", "content":"original"}),
                None,
            );
        }
        // Same session id and overlapping node ids do not establish store or graph identity.
        target.add_node(
            "same-id",
            1,
            Some(0),
            assistant("foreign graph", None),
            None,
        );
        target.add_session("unrelated", "other", 0, 1_790_006_000);
        target.add_node("unrelated", 0, None, assistant("preserve me", None), None);
        let snapshot = export_bytes(&db_path(&source.root), "same-id").unwrap();
        let target_snapshot = export_bytes(&db_path(&target.root), "same-id").unwrap();
        let unrelated = export_bytes(&db_path(&target.root), "unrelated").unwrap();
        let conn = Connection::open(db_path(&target.root)).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; UPDATE sessions SET title='wal fixture' WHERE id='same-id';",
        )
        .unwrap();
        assert!(target.root.join("sessions.db-wal").exists());
        assert!(target.root.join("sessions.db-shm").exists());
        let source_before = fixture_image(&source.root);
        let target_before = fixture_image(&target.root);
        let plan = gobstopper_core::CompactionPlan {
            strategy: "structured".into(),
            rationale: "refusal fixture".into(),
            context_tokens_before: 100,
            context_tokens_after: 10,
            edits: vec![gobstopper_core::Edit::InjectDigest {
                digest: digest_block(),
            }],
        };
        let hash = crate::copy::sha256(&snapshot);
        let vault = target.root.join("vault");
        for store in [&source, &target] {
            assert!(matches!(
                apply_store(&store.root, "same-id", &hash, &plan.edits),
                Err(AdapterError::DirectMutationDisabled)
            ));
            // Current, foreign-store, foreign-graph, foreign-session, and malformed snapshots
            // all reach the same refusal before a connection, lock probe, or parse.
            for bytes in [
                &snapshot[..],
                &target_snapshot[..],
                &unrelated[..],
                b"not JSON",
            ] {
                assert!(matches!(
                    restore_store(&store.root, "same-id", bytes),
                    Err(AdapterError::DirectMutationDisabled)
                ));
            }
            let error = crate::copy::compact_devin_store(
                &source.handle("same-id"),
                &hash,
                &plan,
                &vault,
                &store.root,
            )
            .unwrap_err();
            assert!(matches!(
                error.downcast_ref::<AdapterError>(),
                Some(AdapterError::DirectMutationDisabled)
            ));
        }
        assert_eq!(fixture_image(&source.root), source_before);
        assert_eq!(fixture_image(&target.root), target_before);
        assert!(!vault.exists());
        drop(conn);
        assert_eq!(
            export_bytes(&db_path(&source.root), "same-id").unwrap(),
            snapshot
        );
        assert_eq!(
            export_bytes(&db_path(&target.root), "same-id").unwrap(),
            target_snapshot
        );
        assert_eq!(
            export_bytes(&db_path(&target.root), "unrelated").unwrap(),
            unrelated
        );
    }

    #[test]
    fn disabled_store_operations_do_not_create_missing_paths() {
        let parent = tmpdir("disabled-missing");
        let missing = parent.join("missing");
        assert!(matches!(
            apply(&missing.join("export.jsonl"), &[]),
            Err(AdapterError::DirectMutationDisabled)
        ));
        assert!(matches!(
            apply_store(&missing, "unknown", "invalid hash", &[]),
            Err(AdapterError::DirectMutationDisabled)
        ));
        assert!(matches!(
            restore_store(&missing, "unknown", b"invalid export"),
            Err(AdapterError::DirectMutationDisabled)
        ));
        assert!(!missing.exists());
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);
    }

    // This child is a fixture provider with explicit test-only flock custody, not
    // evidence that an installed provider supports direct mutation admission.
    #[test]
    fn provider_session_lock_holder() {
        use fs2::FileExt;
        let Some(root) = std::env::var_os("GOBSTOPPER_TEST_CUSTODY_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let locks = root.join("session_locks");
        fs::create_dir(&locks).unwrap();
        let lock = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(locks.join("custody.lock"))
            .unwrap();
        lock.lock_exclusive().unwrap();
        let conn = Connection::open(db_path(&root)).unwrap();
        conn.execute("INSERT INTO message_nodes (session_id,node_id,parent_node_id,chat_message,created_at) VALUES ('custody',1,0,?1,1790000001)", [serde_json::json!({"role":"assistant","content":"provider append"}).to_string()]).unwrap();
        conn.execute("UPDATE sessions SET main_chain_id=1 WHERE id='custody'", [])
            .unwrap();
        drop(conn);
        fs::write(root.join("ready"), b"ready").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !root.join("release").exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "fixture parent did not release child"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        drop(lock);
    }

    #[test]
    fn provider_start_before_and_after_mutation_refusal_preserves_state() {
        struct OwnedChild(std::process::Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let fx = Fixture::new("custody-orderings");
        fx.add_session("custody", "custody", 0, 1_790_006_000);
        fx.add_node(
            "custody",
            0,
            None,
            serde_json::json!({"role":"user","content":"original"}),
            None,
        );
        let snapshot = export_bytes(&db_path(&fx.root), "custody").unwrap();
        let hash = crate::copy::sha256(&snapshot);
        let before = fixture_image(&fx.root);
        // Mutation runs before the provider starts. It must not admit an idle store.
        assert!(matches!(
            apply_store(&fx.root, "custody", &hash, &[]),
            Err(AdapterError::DirectMutationDisabled)
        ));
        assert!(matches!(
            restore_store(&fx.root, "custody", &snapshot),
            Err(AdapterError::DirectMutationDisabled)
        ));
        assert_eq!(fixture_image(&fx.root), before);
        let mut child = OwnedChild(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "devin::tests::provider_session_lock_holder",
                    "--nocapture",
                ])
                .env("GOBSTOPPER_TEST_CUSTODY_ROOT", &fx.root)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !fx.root.join("ready").exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "fixture provider exited before ready"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "fixture provider did not become ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Provider owns the fixture lock and has appended. Refusal is still unconditional.
        let live = fixture_image(&fx.root);
        assert!(matches!(
            apply_store(&fx.root, "custody", &hash, &[]),
            Err(AdapterError::DirectMutationDisabled)
        ));
        assert!(matches!(
            restore_store(&fx.root, "custody", &snapshot),
            Err(AdapterError::DirectMutationDisabled)
        ));
        assert_eq!(fixture_image(&fx.root), live);
        fs::write(fx.root.join("release"), b"release").unwrap();
        assert!(child.0.wait().unwrap().success());
        assert_eq!(chain_head(&fx.root, "custody"), 1);
        assert!(node_payload(&fx.root, "custody", 1).contains("provider append"));
    }

    #[test]
    fn vault_snapshot_of_store_holds_the_session_export() {
        let fx = Fixture::new("snap-store");
        fx.add_session("sess-s", "snap", 0, 1_790_006_000);
        fx.add_session("sess-other", "snap", 0, 1_790_006_000);
        fx.add_node(
            "sess-s",
            0,
            None,
            serde_json::json!({"role": "user", "content": "safe"}),
            None,
        );
        fx.add_node(
            "sess-other",
            0,
            None,
            serde_json::json!({"role": "user", "content": "someone else"}),
            None,
        );
        let vault_root = fx.root.join("vault");
        let entry = crate::vault::snapshot(
            &db_path(&fx.root),
            Provider::Devin,
            "sess-s",
            Some("test"),
            &vault_root,
        )
        .unwrap();
        let bytes = crate::vault::read_object(&entry.sha256, &vault_root).unwrap();
        // The object is the session's canonical detached export, not a raw
        // backup of the shared SQLite store.
        let text = String::from_utf8(bytes).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains("\"session_meta\""));
        assert!(lines[0].contains("sess-s"));
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("safe"));
        assert!(!text.contains("someone else"));
        // …and it parses as a transcript.
        let mut handle = fx.handle("sess-s");
        handle.path = fx.root.join("snapshot.jsonl");
        assert!(!load_bytes(handle, text.as_bytes())
            .unwrap()
            .items
            .is_empty());
    }

    #[test]
    fn export_transform_repoints_head_so_digest_stays_live() {
        let fx = Fixture::new("export-digest");
        fx.add_session("sess-e", "export", 1, 1_790_006_000);
        fx.add_node(
            "sess-e",
            0,
            None,
            serde_json::json!({"role": "user", "content": "hi"}),
            None,
        );
        fx.add_node(
            "sess-e",
            1,
            Some(0),
            serde_json::json!({"role": "assistant", "content": "ok"}),
            None,
        );
        let bytes = export_bytes(&db_path(&fx.root), "sess-e").unwrap();
        let path = fx.root.join("export.jsonl");
        fs::write(&path, &bytes).unwrap();
        let edits = vec![gobstopper_core::plan::Edit::InjectDigest {
            digest: digest_block(),
        }];
        let after = transform(&bytes, &edits).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let mut lines = after.split(|b| *b == b'\n');
        let meta: Value = serde_json::from_slice(lines.next().unwrap()).unwrap();
        assert_eq!(meta["main_chain_id"].as_i64().unwrap(), 2);
        let last: Value = serde_json::from_slice(lines.nth(2).unwrap()).unwrap();
        assert_eq!(last["node_id"].as_i64().unwrap(), 2);
        assert_eq!(last["parent_node_id"].as_i64().unwrap(), 1);
        // The repointed head must make the injected node live, not an orphan.
        let mut handle = fx.handle("sess-e");
        handle.path = path.clone();
        let transcript = load_bytes(handle, &after).unwrap();
        let item = transcript
            .items
            .iter()
            .find(|i| i.line_index == 3)
            .expect("digest node missing from transcript");
        assert!(item.est_tokens > 0, "injected digest node is dead");
    }

    #[test]
    fn restore_store_refuses_foreign_appends() {
        let fx = Fixture::new("restore-drift");
        fx.add_session("sess-rd", "drift", 0, 1_790_006_000);
        fx.add_node(
            "sess-rd",
            0,
            None,
            serde_json::json!({"role": "user", "content": "go"}),
            None,
        );
        let snapshot = export_bytes(&db_path(&fx.root), "sess-rd").unwrap();
        // A provider write landed after the snapshot.
        fx.add_node(
            "sess-rd",
            1,
            Some(0),
            serde_json::json!({"role": "user", "content": "real"}),
            None,
        );
        let before = fixture_image(&fx.root);
        let err = restore_store(&fx.root, "sess-rd", &snapshot).unwrap_err();
        assert!(matches!(err, AdapterError::DirectMutationDisabled));
        assert_eq!(fixture_image(&fx.root), before);
        assert!(node_payload(&fx.root, "sess-rd", 1).contains("real"));
    }

    #[test]
    fn missing_database_yields_empty() {
        let root = tmpdir("empty");
        assert!(discover(&root, 0, false).is_empty());
        assert!(find(&root, "x").is_empty());
        assert!(export_bytes(&db_path(&root), "x").is_err());
    }

    #[test]
    fn broken_chain_head_still_loads_remaining_rows() {
        let fx = Fixture::new("broken");
        fx.add_session("sess-x", "broken", 99, 1_790_006_000);
        fx.add_node(
            "sess-x",
            0,
            None,
            serde_json::json!({"role": "user", "content": "hi"}),
            None,
        );
        let t = load(fx.handle("sess-x")).unwrap();
        // Head 99 doesn't exist: no live chain, so nothing counts.
        assert_eq!(t.items.len(), 1);
        assert_eq!(t.items[0].est_tokens, 0);
    }

    #[test]
    fn chain_fingerprint_tracks_head_and_node_count() {
        let fx = Fixture::new("fp");
        fx.add_session("s1", "fp", 1, 1_790_006_000);
        fx.add_node("s1", 1, None, assistant("a", None), None);
        let db = db_path(&fx.root);

        let fp1 = chain_fingerprint(&db, "s1").unwrap();
        // Unchanged store: identical fingerprint.
        assert_eq!(fp1, chain_fingerprint(&db, "s1").unwrap());

        // Provider append: node count moves.
        fx.add_node("s1", 2, Some(1), assistant("b", None), None);
        let fp2 = chain_fingerprint(&db, "s1").unwrap();
        assert_ne!(fp1, fp2);

        // Head move alone (a compaction wrote no nodes): still moves.
        let conn = Connection::open(&db).unwrap();
        conn.execute("UPDATE sessions SET main_chain_id = 2 WHERE id = 's1'", [])
            .unwrap();
        drop(conn);
        let fp3 = chain_fingerprint(&db, "s1").unwrap();
        assert_ne!(fp2, fp3);

        assert!(chain_fingerprint(&db, "absent").is_none());
        assert!(chain_fingerprint(&fx.root.join("no.db"), "s1").is_none());
    }

    #[cfg(unix)]
    fn fake_devin(dir: &Path, script: &str) -> PathBuf {
        let bin = dir.join("devin");
        fs::write(&bin, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    #[cfg(unix)]
    #[test]
    fn acp_compact_accepts_completion_before_ack() {
        let dir = tmpdir("acp-pre-ack");
        let prompt = acp_line(
            serde_json::json!({"jsonrpc":"2.0","method":"_cognition.ai/compaction","params":{"sessionId":"other","status":"failed"}}),
        ) + &acp_line(
            serde_json::json!({"jsonrpc":"2.0","method":"_cognition.ai/compaction","params":{"sessionId":"selected","status":"compacted"}}),
        ) + &acp_reply(3, serde_json::json!({"stopReason":"end_turn"}));
        let bin = fake_acp(&dir, &acp_reply(2, serde_json::json!({})), &prompt);
        acp_compact(&bin, "selected", Path::new("/tmp"), 5).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    fn acp_line(value: Value) -> String {
        format!("printf '%s\\n' '{value}'\n")
    }

    #[cfg(unix)]
    fn acp_reply(id: u64, result: Value) -> String {
        acp_line(serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}))
    }

    #[cfg(unix)]
    fn fake_acp(dir: &Path, load: &str, prompt: &str) -> PathBuf {
        fake_devin(dir, &format!(
            "#!/bin/sh\nwhile IFS= read -r line; do\ncase \"$line\" in\n*'\"id\":1'*) {} ;;\n*'\"id\":2'*) {load} ;;\n*'\"id\":3'*) {prompt} ;;\nesac\ndone\n",
            acp_reply(1, serde_json::json!({})),
        ))
    }

    #[cfg(unix)]
    #[test]
    fn acp_compact_rejects_foreign_or_display_text_completion() {
        let dir = tmpdir("acp-false-completion");
        for notice in [
            serde_json::json!({"jsonrpc":"2.0","method":"_cognition.ai/compaction","params":{"sessionId":"other","status":"compacted"}}),
            serde_json::json!({"jsonrpc":"2.0","method":"_cognition.ai/compaction","params":{"status":"compacted"}}),
            serde_json::json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"selected","update":{"content":{"text":"Context compacted"}}}}),
        ] {
            let prompt = acp_reply(3, serde_json::json!({})) + &acp_line(notice) + "exit 0\n";
            let bin = fake_acp(&dir, &acp_reply(2, serde_json::json!({})), &prompt);
            assert!(acp_compact(&bin, "selected", Path::new("/tmp"), 5).is_err());
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn acp_compact_never_echoes_private_error_payloads() {
        let dir = tmpdir("acp-private-errors");
        for prompt in [
            acp_line(
                serde_json::json!({"jsonrpc":"2.0","id":3,"error":{"code":-1,"message":"PRIVATE_SENTINEL","data":{"secret":"PRIVATE_SENTINEL"}}}),
            ),
            acp_line(
                serde_json::json!({"jsonrpc":"2.0","method":"_cognition.ai/compaction","params":{"sessionId":"selected","status":"failed","message":"PRIVATE_SENTINEL"}}),
            ),
            "printf '%s\\n' 'PRIVATE_SENTINEL'\n".into(),
        ] {
            let bin = fake_acp(&dir, &acp_reply(2, serde_json::json!({})), &prompt);
            let error = acp_compact(&bin, "selected", Path::new("/tmp"), 5).unwrap_err();
            assert!(!error.to_string().contains("PRIVATE_SENTINEL"));
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn acp_compact_refuses_malformed_or_locked_load_before_prompt() {
        let dir = tmpdir("acp-load-admission");
        let dispatched = dir.join("prompt-dispatched");
        for result in [
            Value::Null,
            serde_json::json!({"_meta": "invalid"}),
            serde_json::json!({"_meta":{"cognition.ai/isLocked":"true"}}),
            serde_json::json!({"_meta":{"cognition.ai/isLocked":true}}),
        ] {
            let bin = fake_acp(
                &dir,
                &acp_reply(2, result),
                &format!("touch '{}'\n", dispatched.display()),
            );
            assert!(acp_compact(&bin, "selected", Path::new("/tmp"), 5).is_err());
            assert!(!dispatched.exists());
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn acp_compact_bounds_requests_before_spawn_and_bounds_response_frames() {
        let dir = tmpdir("acp-bounds");
        let spawned = dir.join("spawned");
        let bin = fake_devin(
            &dir,
            &format!("#!/bin/sh\ntouch '{}'\nexec sleep 30\n", spawned.display()),
        );
        for cwd in [
            "x".repeat(MAX_ACP_REQUEST_BYTES + 1),
            "\n".repeat(MAX_ACP_REQUEST_BYTES / 2),
        ] {
            let error = acp_compact(&bin, "selected", Path::new(&cwd), 1).unwrap_err();
            assert!(error.to_string().contains("byte limit"));
            assert!(!spawned.exists());
        }
        let bin = fake_devin(
            &dir,
            &format!(
                "#!/bin/sh\nhead -c {} /dev/zero | tr '\\000' x\n",
                MAX_ACP_FRAME_BYTES + 1
            ),
        );
        let error = acp_compact(&bin, "selected", Path::new("/tmp"), 5).unwrap_err();
        assert!(
            error.to_string().contains("frame exceeds byte limit"),
            "{error}"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn acp_compact_drains_history_bursts_through_bounded_queue() {
        let dir = tmpdir("acp-replay-burst");
        let update = acp_line(
            serde_json::json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"selected","update":{"text":"history"}}}),
        );
        let load = format!("i=0\nwhile [ \"$i\" -lt 256 ]; do\n{update}i=$((i+1))\ndone\n")
            + &acp_reply(2, serde_json::json!({}));
        let prompt = acp_reply(3, serde_json::json!({}))
            + &acp_line(
                serde_json::json!({"jsonrpc":"2.0","method":"_cognition.ai/compaction","params":{"sessionId":"selected","status":"compacted"}}),
            );
        let bin = fake_acp(&dir, &load, &prompt);
        acp_compact(&bin, "selected", Path::new("/tmp"), 5).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn acp_compact_terminates_descendants_on_completion_and_timeout() {
        let dir = tmpdir("acp-descendants");
        let pid_path = dir.join("descendant.pid");
        for completes in [true, false] {
            let mut prompt = format!(
                "sleep 30 &\nprintf '%s' \"$!\" > '{}'\n",
                pid_path.display()
            );
            if completes {
                prompt += &acp_reply(3, serde_json::json!({}));
                prompt += &acp_line(
                    serde_json::json!({"jsonrpc":"2.0","method":"_cognition.ai/compaction","params":{"sessionId":"selected","status":"compacted"}}),
                );
            }
            let bin = fake_acp(&dir, &acp_reply(2, serde_json::json!({})), &prompt);
            let start = std::time::Instant::now();
            let result = acp_compact(
                &bin,
                "selected",
                Path::new("/tmp"),
                if completes { 5 } else { 1 },
            );
            assert_eq!(result.is_ok(), completes);
            assert!(start.elapsed() < std::time::Duration::from_secs(5));
            let pid: i32 = fs::read_to_string(&pid_path).unwrap().parse().unwrap();
            let mut stopped = false;
            for _ in 0..100 {
                // SAFETY: signal 0 only probes the exact fixture-owned PID.
                if unsafe { libc::kill(pid, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    stopped = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(stopped, "owned descendant remains live");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn acp_request_write_observes_deadline_when_bridge_stops_reading() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        let child = Command::new("/bin/sh")
            .args(["-c", "exec sleep 30"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut custody = AcpChild {
            child,
            cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            reader: None,
        };
        let mut stdin = custody.child.stdin.take().unwrap();
        acp_nonblocking(&stdin).unwrap();
        let start = std::time::Instant::now();
        let error = acp_write_request(
            &mut stdin,
            &vec![b'x'; MAX_ACP_FRAME_BYTES],
            start + std::time::Duration::from_millis(100),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn acp_compact_loads_session_and_sends_compact() {
        let dir = tmpdir("acp-ok");
        let reqs = dir.join("requests.txt");
        let home_out = dir.join("home.txt");
        let bin = fake_devin(
            &dir,
            &format!(
                "#!/bin/sh\nprintf '%s' \"$DEVIN_DATA_DIR\" > '{}'\nwhile IFS= read -r line; do\n  printf '%s\\n' \"$line\" >> '{}'\n  case \"$line\" in\n    *'\"id\":1'*) printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{}}}}' ;;\n    *'\"id\":2'*) printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{}}}}' ;;\n    *'\"id\":3'*) printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{{\"stopReason\":\"end_turn\"}}}}'\n      printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"method\":\"_cognition.ai/compaction\",\"params\":{{\"status\":\"started\",\"sessionId\":\"sess-abc_123\"}}}}'\n      printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"method\":\"_cognition.ai/compaction\",\"params\":{{\"status\":\"compacted\",\"sessionId\":\"sess-abc_123\"}}}}' ;;\n  esac\ndone\n",
                home_out.display(), reqs.display()
            ),
        );
        acp_compact(&bin, "sess-abc_123", Path::new("/tmp"), 30).unwrap();
        let sent = fs::read_to_string(&reqs).unwrap();
        assert!(sent.contains("\"method\":\"session/load\""), "got: {sent}");
        assert!(
            sent.contains("\"sessionId\":\"sess-abc_123\""),
            "got: {sent}"
        );
        assert!(sent.contains("/compact"), "got: {sent}");
        let configured_home = dir.join("explicit devin home");
        acp_compact_in_home(
            &bin,
            "sess-abc_123",
            Path::new("/tmp"),
            30,
            Some(&configured_home),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(&home_out).unwrap(),
            configured_home.to_string_lossy()
        );

        // A JSON-RPC error on the prompt id surfaces as an error.
        let bin = fake_devin(
            &dir,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"id\":3'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"error\":{\"code\":-1,\"message\":\"nope\"}}' ;;\n    *) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}' | sed \"s/\\\"id\\\":1/\\\"id\\\":$(printf '%s' \"$line\" | grep -o '\\\"id\\\":[0-9]*' | grep -o '[0-9]*')/\" ;;\n  esac\ndone\n",
        );
        assert!(acp_compact(&bin, "sess-abc_123", Path::new("/tmp"), 30).is_err());

        // Unusual session ids are refused before spawn.
        assert!(acp_compact(&bin, "has space", Path::new("/tmp"), 30).is_err());
        assert!(acp_compact(&bin, "", Path::new("/tmp"), 30).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn acp_compact_times_out_and_handles_bridge_death() {
        let dir = tmpdir("acp-timeout");
        let bin = fake_devin(&dir, "#!/bin/sh\nsleep 60\n");
        let start = std::time::Instant::now();
        let err = acp_compact(&bin, "sess-abc", Path::new("/tmp"), 1).unwrap_err();
        assert!(start.elapsed() < std::time::Duration::from_secs(30));
        assert!(err.to_string().contains("timed out"), "got: {err}");

        // Bridge exits without answering id=3 — cannot prove the compact ran.
        let bin = fake_devin(&dir, "#!/bin/sh\nexit 0\n");
        assert!(acp_compact(&bin, "sess-abc", Path::new("/tmp"), 30).is_err());
        fs::remove_dir_all(&dir).ok();
    }
}
