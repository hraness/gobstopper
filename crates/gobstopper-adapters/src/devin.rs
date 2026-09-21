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
//! Reads are always available; writes (`apply_store`, `restore_store`)
//! run only for idle sessions — the provider's `session_locks/<id>.lock`
//! flock must be free, then a single `BEGIN IMMEDIATE` transaction does
//! the work: re-export + sha identity check (no drift since planning),
//! conditional payload updates keyed on the stored `chat_message` text,
//! an in-tx `verify` pass, then commit. Live sessions stay delegated to
//! native `/compact`.

use gobstopper_core::estimate::estimate_tokens;
use gobstopper_core::model::{ItemKind, SessionHandle, TranscriptItem, UsageSample};
use gobstopper_core::{Provider, Transcript};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::detect::Discovered;
use crate::AdapterError;

/// Devin-owned marker meaning "session is live": while a CLI process has
/// the session open it holds an `flock` on `session_locks/<id>.lock`.
/// Lock files persist after the process exits, so existence alone is not
/// proof — only a still-held flock counts. Stale files lock cleanly.
pub fn session_active(root: &Path, session_id: &str) -> bool {
    let path = root
        .join("session_locks")
        .join(format!("{session_id}.lock"));
    let Ok(file) = std::fs::File::open(&path) else {
        return false;
    };
    fs2::FileExt::try_lock_exclusive(&file).is_err()
}

pub fn db_path(root: &Path) -> PathBuf {
    root.join("sessions.db")
}

/// Whether `path` is the shared provider store rather than a detached
/// export file. Export files are safe rewrite targets (they are copies);
/// the store is never written by this adapter today.
pub fn is_store_path(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == "sessions.db")
}

fn open_readonly(db: &Path) -> Result<Connection, AdapterError> {
    Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| AdapterError::Io {
        path: db.to_path_buf(),
        source: std::io::Error::other(e),
    })
}

fn open_readwrite(db: &Path) -> Result<Connection, AdapterError> {
    let conn = Connection::open(db).map_err(|e| AdapterError::Io {
        path: db.to_path_buf(),
        source: std::io::Error::other(e),
    })?;
    // A writer that already holds the WAL write lock makes BEGIN
    // IMMEDIATE wait briefly, then fail — never block a provider.
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(rusqlite_io(db))?;
    Ok(conn)
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
/// from `last_activity_at` rather than file mtime.
pub fn discover(root: &Path, max_age_secs: u64) -> Vec<Discovered> {
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
        let usage = scan_usage(&conn, &session_id);
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
    let mut stmt = match conn.prepare(
        "SELECT m.chat_message, m.metadata FROM message_nodes m \
         WHERE m.session_id = ?1 ORDER BY m.node_id",
    ) {
        Ok(stmt) => stmt,
        Err(_) => return UsageSample::default(),
    };
    let rows = stmt
        .query_map([session_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })
        .map(|it| it.flatten().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut usage = UsageSample::default();
    let mut preceding: Option<u64> = None;
    for (raw, meta) in &rows {
        let Ok(msg) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        if let Some(n) = meta
            .as_deref()
            .and_then(|m| serde_json::from_str::<Value>(m).ok())
            .and_then(|m| m.get("num_tokens_preceding").and_then(Value::as_u64))
        {
            preceding = Some(n);
        }
        // A scan lacks the chain walk (cheap by design); the latest
        // assistant metrics approximate live context. `load` performs the
        // exact live-chain accounting.
        absorb_usage(&msg, &mut usage, true);
    }
    if usage.context_tokens == 0 {
        usage.context_tokens = preceding.unwrap_or(0);
    }
    usage
}

/// Deterministic byte identity for one session: a `session_meta` record
/// then one `message_node` record per row, ordered by `node_id`. This is
/// the substrate for hashing, vault snapshots, and `verify` — the SQLite
/// file itself is never treated as the transcript.
pub fn export_bytes(db: &Path, session_id: &str) -> Result<Vec<u8>, AdapterError> {
    let conn = open_readonly(db)?;
    export_bytes_conn(&conn, db, session_id)
}

/// Export using an existing connection — used inside write transactions
/// so the identity check and the mutation see the same snapshot.
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
    for row in rows {
        let row = row.map_err(rusqlite_io(db))?;
        // chat_message and metadata are JSON text columns; embed them as
        // values rather than escaped strings so consumers parse once.
        let mut record = row;
        for key in ["chat_message", "metadata"] {
            if let Some(raw) = record.get(key).and_then(Value::as_str) {
                if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
                    record[key] = parsed;
                }
            }
        }
        out.extend_from_slice(&serde_json::to_vec(&record).unwrap_or_default());
        out.push(b'\n');
    }
    Ok(out)
}

/// Session id and working directory from an export file's first record.
/// Used when a detached export is passed to `plan`/`verify` by path.
pub fn scan_meta_export(path: &Path) -> (Option<String>, Option<PathBuf>) {
    let Ok(bytes) = crate::transaction::read(path) else {
        return (None, None);
    };
    let Some(line) = bytes.split(|b| *b == b'\n').next() else {
        return (None, None);
    };
    let Ok(record) = serde_json::from_slice::<Value>(line) else {
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
        return UsageSample::default();
    };
    let Ok((_, rows)) = parse_export(&bytes) else {
        return UsageSample::default();
    };
    let mut usage = UsageSample::default();
    for row in &rows {
        absorb_usage(&row.message, &mut usage, true);
    }
    usage
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

/// Rewrite `elidable` `chat_message` payloads inside a detached export
/// file. The provider store is never a valid target: `sessions.db` writes
/// require the guarded, still-unwired transactional path, so applying to
/// it is refused here as defense in depth.
pub fn apply(path: &Path, edits: &[gobstopper_core::plan::Edit]) -> Result<u64, AdapterError> {
    if is_store_path(path) {
        return Err(AdapterError::UnsupportedProvider);
    }
    crate::transaction::apply(Provider::Devin, path, |candidate| {
        apply_inner(candidate, edits)
    })
}

fn stub_for(template: &str, bytes: u64, kind: &str) -> String {
    template
        .replace("{bytes}", &bytes.to_string())
        .replace("{kind}", kind)
}

fn elide_record(line: &str, stub_template: &str, stub_override: Option<&str>) -> (String, u64) {
    let Ok(mut record) = serde_json::from_str::<Value>(line) else {
        return (line.to_string(), 0);
    };
    if record.get("type").and_then(Value::as_str) != Some("message_node") {
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

/// Whether a stored `chat_message` is one of our injected digest nodes —
/// used by `restore_store` to distinguish them from provider writes.
fn is_digest_message(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("user")
        && message
            .get("content")
            .and_then(Value::as_str)
            .is_some_and(|c| c.starts_with(DIGEST_MARKER))
        && message
            .get("metadata")
            .and_then(|m| m.get("is_user_input"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
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
                let targets: std::collections::HashSet<usize> =
                    line_indexes.iter().copied().collect();
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
            // live chain tail, so replayed exports keep it in context.
            Edit::InjectDigest { digest } => {
                let last_node: Option<i64> = raw
                    .lines()
                    .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                    .filter(|r| r.get("type").and_then(Value::as_str) == Some("message_node"))
                    .filter_map(|r| r.get("node_id").and_then(Value::as_i64))
                    .max();
                let Some(parent) = last_node else {
                    return Err(AdapterError::InvalidEdit(
                        "devin export has no message nodes for digest attachment",
                    ));
                };
                let record = serde_json::json!({
                    "type": "message_node",
                    "node_id": parent + 1,
                    "parent_node_id": parent,
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

/// One `(node_id, raw chat_message)` pair in `ORDER BY node_id` — the
/// same order export lines use, so export `line_index` i maps to
/// `nodes[i - 1]` (line 0 is `session_meta`).
fn store_nodes(
    conn: &Connection,
    db: &Path,
    session_id: &str,
) -> Result<Vec<(i64, String)>, AdapterError> {
    let mut stmt = conn
        .prepare(
            "SELECT node_id, chat_message FROM message_nodes \
             WHERE session_id = ?1 ORDER BY node_id",
        )
        .map_err(rusqlite_io(db))?;
    let rows = stmt
        .query_map([session_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(rusqlite_io(db))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(rusqlite_io(db))?);
    }
    Ok(out)
}

/// Rewrite elided `chat_message` payloads and inject digest nodes inside
/// the session store, in one immediate transaction.
///
/// Safety order: refuse while the provider holds the session lock, then
/// re-export inside the transaction and require the canonical sha to
/// equal `source_sha256` (the bytes the plan was computed against), so
/// any concurrent provider write between plan and apply aborts cleanly.
/// Each elide is a conditional `UPDATE ... WHERE chat_message = <raw>` —
/// zero rows means intra-transaction inconsistency and rolls back.
/// A digest becomes a new `message_nodes` row whose `parent_node_id` is
/// the old head, plus a conditional `main_chain_id` move; verification
/// re-exports the candidate state and rejects any new finding before
/// commit.
pub fn apply_store(
    root: &Path,
    session_id: &str,
    source_sha256: &str,
    edits: &[gobstopper_core::plan::Edit],
) -> Result<StoreReport, AdapterError> {
    use gobstopper_core::plan::Edit;
    if edits
        .iter()
        .any(|e| matches!(e, Edit::ProviderCompact { .. } | Edit::CacheEdit { .. }))
    {
        return Err(AdapterError::UnsupportedProvider);
    }
    let db = db_path(root);
    if session_active(root, session_id) {
        return Err(AdapterError::InvalidEdit(
            "session is live (provider holds its lock); compact with /compact in-session",
        ));
    }
    let mut conn = open_readwrite(&db)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(rusqlite_io(&db))?;
    let report = (|| -> Result<StoreReport, AdapterError> {
        let before = export_bytes_conn(&tx, &db, session_id)?;
        if crate::copy::sha256(&before) != source_sha256 {
            return Err(AdapterError::ChangedDuringWrite { path: db.clone() });
        }
        let nodes = store_nodes(&tx, &db, session_id)?;
        let mut reclaimed = 0u64;
        let mut rewritten = 0u64;
        let mut digest_node_id = None;
        for edit in edits {
            match edit {
                Edit::Elide {
                    line_indexes,
                    stub_template,
                    per_item_stubs,
                } => {
                    for &line_index in line_indexes {
                        let Some((node_id, raw)) =
                            line_index.checked_sub(1).and_then(|i| nodes.get(i))
                        else {
                            return Err(AdapterError::InvalidEdit(
                                "elide target line is out of range for this session",
                            ));
                        };
                        let Ok(mut message) = serde_json::from_str::<Value>(raw) else {
                            continue;
                        };
                        let Some(content) = message.get_mut("content") else {
                            continue;
                        };
                        let eligible = crate::payload::eligible_bytes(content);
                        if eligible == 0 {
                            continue;
                        }
                        let stub = per_item_stubs
                            .get(&line_index)
                            .cloned()
                            .unwrap_or_else(|| stub_for(stub_template, eligible, "tool_result"));
                        let shrunk = crate::payload::elide(content, stub);
                        if shrunk == 0 {
                            continue;
                        }
                        let candidate = serde_json::to_string(&message).map_err(|_| {
                            AdapterError::InvalidEdit("rewritten message not serializable")
                        })?;
                        let changed = tx
                            .execute(
                                "UPDATE message_nodes SET chat_message = ?1 \
                                 WHERE session_id = ?2 AND node_id = ?3 AND chat_message = ?4",
                                rusqlite::params![candidate, session_id, node_id, raw],
                            )
                            .map_err(rusqlite_io(&db))?;
                        if changed != 1 {
                            return Err(AdapterError::ChangedDuringWrite { path: db.clone() });
                        }
                        reclaimed += shrunk;
                        rewritten += 1;
                    }
                }
                Edit::InjectDigest { digest } => {
                    let head: Option<i64> = tx
                        .query_row(
                            "SELECT main_chain_id FROM sessions WHERE id = ?1",
                            [session_id],
                            |r| r.get(0),
                        )
                        .map_err(rusqlite_io(&db))?;
                    let Some(head) = head else {
                        return Err(AdapterError::InvalidEdit("session has no main chain head"));
                    };
                    let new_id: i64 = tx
                        .query_row(
                            "SELECT COALESCE(MAX(node_id), -1) + 1 FROM message_nodes \
                             WHERE session_id = ?1",
                            [session_id],
                            |r| r.get(0),
                        )
                        .map_err(rusqlite_io(&db))?;
                    let message = serde_json::to_string(&digest_message(digest)).map_err(|_| {
                        AdapterError::InvalidEdit("digest message not serializable")
                    })?;
                    tx.execute(
                        "INSERT INTO message_nodes (session_id, node_id, parent_node_id, \
                         chat_message, created_at, metadata) VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
                        rusqlite::params![session_id, new_id, head, message, now_secs() as i64],
                    )
                    .map_err(rusqlite_io(&db))?;
                    let moved = tx
                        .execute(
                            "UPDATE sessions SET main_chain_id = ?1 \
                             WHERE id = ?2 AND main_chain_id = ?3",
                            rusqlite::params![new_id, session_id, head],
                        )
                        .map_err(rusqlite_io(&db))?;
                    if moved != 1 {
                        return Err(AdapterError::ChangedDuringWrite { path: db.clone() });
                    }
                    digest_node_id = Some(new_id);
                }
                Edit::ProviderCompact { .. } | Edit::CacheEdit { .. } => {}
            }
        }
        let after = export_bytes_conn(&tx, &db, session_id)?;
        let before_findings = crate::verify::verify(Provider::Devin, &before);
        let after_findings = crate::verify::verify(Provider::Devin, &after);
        if after_findings.iter().any(|f| !before_findings.contains(f)) {
            return Err(AdapterError::InvalidEdit(
                "rewrite introduces structural findings; rolled back",
            ));
        }
        Ok(StoreReport {
            nodes_rewritten: rewritten,
            digest_node_id,
            reclaimed_bytes: reclaimed,
            export_sha256: crate::copy::sha256(&after),
            resume_hint: format!("devin --resume {session_id}"),
        })
    })()?;
    tx.commit().map_err(rusqlite_io(&db))?;
    Ok(report)
}

/// Undo a store apply: restore the snapshot's `chat_message` payloads and
/// `main_chain_id`, and delete injected digest nodes. Refuses when the
/// session has any node the snapshot does not know and that is not one of
/// ours — restoring would orphan a genuine provider write.
pub fn restore_store(
    root: &Path,
    session_id: &str,
    snapshot_bytes: &[u8],
) -> Result<StoreReport, AdapterError> {
    let db = db_path(root);
    if session_active(root, session_id) {
        return Err(AdapterError::InvalidEdit(
            "session is live (provider holds its lock); restore after it closes",
        ));
    }
    let (snapshot_head, snapshot_rows) = parse_export(snapshot_bytes)?;
    let mut snapshot_msgs = std::collections::HashMap::new();
    for row in &snapshot_rows {
        snapshot_msgs.insert(row.node_id, row.message.clone());
    }
    let mut conn = open_readwrite(&db)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(rusqlite_io(&db))?;
    let report = (|| -> Result<StoreReport, AdapterError> {
        let before = export_bytes_conn(&tx, &db, session_id)?;
        let nodes = store_nodes(&tx, &db, session_id)?;
        let mut injected = Vec::new();
        for (node_id, raw) in &nodes {
            match snapshot_msgs.get(node_id) {
                Some(_) => {}
                None => {
                    let parsed: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
                    if is_digest_message(&parsed) {
                        injected.push(*node_id);
                    } else {
                        // A foreign node written since the snapshot —
                        // restoring would orphan provider state.
                        return Err(AdapterError::ChangedDuringWrite { path: db.clone() });
                    }
                }
            }
        }
        let mut rewritten = 0u64;
        for (node_id, message) in &snapshot_msgs {
            let text = serde_json::to_string(message)
                .map_err(|_| AdapterError::InvalidEdit("snapshot message not serializable"))?;
            let changed = tx
                .execute(
                    "UPDATE message_nodes SET chat_message = ?1 \
                     WHERE session_id = ?2 AND node_id = ?3",
                    rusqlite::params![text, session_id, node_id],
                )
                .map_err(rusqlite_io(&db))?;
            if changed == 0 {
                return Err(AdapterError::ChangedDuringWrite { path: db.clone() });
            }
            rewritten += 1;
        }
        for node_id in &injected {
            tx.execute(
                "DELETE FROM message_nodes WHERE session_id = ?1 AND node_id = ?2",
                rusqlite::params![session_id, node_id],
            )
            .map_err(rusqlite_io(&db))?;
        }
        let current_head: Option<i64> = tx
            .query_row(
                "SELECT main_chain_id FROM sessions WHERE id = ?1",
                [session_id],
                |r| r.get(0),
            )
            .map_err(rusqlite_io(&db))?;
        if current_head != snapshot_head {
            let moved = tx
                .execute(
                    "UPDATE sessions SET main_chain_id = ?1 \
                     WHERE id = ?2 AND main_chain_id = ?3",
                    rusqlite::params![snapshot_head, session_id, current_head],
                )
                .map_err(rusqlite_io(&db))?;
            if moved != 1 {
                return Err(AdapterError::ChangedDuringWrite { path: db.clone() });
            }
        }
        let after = export_bytes_conn(&tx, &db, session_id)?;
        let before_findings = crate::verify::verify(Provider::Devin, &before);
        let after_findings = crate::verify::verify(Provider::Devin, &after);
        if after_findings.iter().any(|f| !before_findings.contains(f)) {
            return Err(AdapterError::InvalidEdit(
                "restore introduces findings; rolled back",
            ));
        }
        Ok(StoreReport {
            nodes_rewritten: rewritten,
            digest_node_id: None,
            reclaimed_bytes: 0,
            export_sha256: crate::copy::sha256(&after),
            resume_hint: format!("devin --resume {session_id}"),
        })
    })()?;
    tx.commit().map_err(rusqlite_io(&db))?;
    Ok(report)
}

/// Assistant `tool_calls[].id` → call name/label, used to annotate the
/// tool results that answer them and for orphan checks in `verify`.
struct NodeRow {
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
    for (index, line) in text.lines().enumerate() {
        if index >= gobstopper_core::validation::MAX_ITEMS {
            return Err(AdapterError::InvalidEdit("transcript exceeds record limit"));
        }
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match record.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                main_chain_id = record
                    .get("main_chain_id")
                    .and_then(Value::as_i64)
                    .or(main_chain_id);
            }
            Some("message_node") => {
                let Some(node_id) = record.get("node_id").and_then(Value::as_i64) else {
                    continue;
                };
                let message = record.get("chat_message").cloned().unwrap_or(Value::Null);
                let num_tokens_preceding = record
                    .get("metadata")
                    .and_then(|m| m.get("num_tokens_preceding"))
                    .and_then(Value::as_u64);
                rows.push(NodeRow {
                    node_id,
                    parent_node_id: record.get("parent_node_id").and_then(Value::as_i64),
                    message,
                    num_tokens_preceding,
                });
            }
            _ => continue,
        }
    }
    Ok((main_chain_id, rows))
}

/// Node ids reachable by walking `parent_node_id` up from the chain head.
fn live_nodes(rows: &[NodeRow], head: Option<i64>) -> std::collections::HashSet<i64> {
    let parents: std::collections::HashMap<i64, Option<i64>> =
        rows.iter().map(|r| (r.node_id, r.parent_node_id)).collect();
    let mut live = std::collections::HashSet::new();
    let mut cursor = head;
    while let Some(id) = cursor {
        if !live.insert(id) {
            break; // cycle guard
        }
        cursor = parents.get(&id).copied().flatten();
    }
    live
}

/// Fold one message's provider metrics into the usage sample. `on_live`
/// gates `context_tokens` (the latest live assistant reading wins) while
/// lifetime counters accumulate across every assistant message.
fn absorb_usage(message: &Value, usage: &mut UsageSample, on_live: bool) {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let metrics = message.pointer("/metadata/metrics");
    let get = |key: &str| {
        metrics
            .and_then(|m| m.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let input = get("input_tokens")
        .saturating_add(get("cache_read_tokens"))
        .saturating_add(get("cache_creation_tokens"));
    if on_live {
        let context = input.saturating_add(get("output_tokens"));
        if context > 0 {
            usage.context_tokens = context;
        }
    }
    usage.lifetime_input_tokens = usage.lifetime_input_tokens.saturating_add(input);
    usage.lifetime_cached_tokens = usage
        .lifetime_cached_tokens
        .saturating_add(get("cache_read_tokens"));
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

fn tool_call_map(rows: &[NodeRow]) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for row in rows {
        if row.message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(calls) = row.message.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        for call in calls {
            if let Some(id) = call.get("id").and_then(Value::as_str) {
                map.entry(id.to_string()).or_insert_with(|| {
                    call.get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string()
                });
            }
        }
    }
    map
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
    if bytes.len() as u64 > crate::transaction::MAX_TRANSCRIPT_BYTES {
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
    let calls = tool_call_map(&rows);
    let mut usage = UsageSample::default();
    let mut preceding_on_live: Option<u64> = None;
    let mut items = Vec::with_capacity(rows.len() + 1);
    for (index, row) in rows.iter().enumerate() {
        // +1: export line 0 is the session_meta record.
        let line_index = index + 1;
        let on_live = live.contains(&row.node_id);
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
                // `tool_call_id` may carry a `#<nonce>` suffix beyond the
                // call's `id`; match exactly or as a prefix.
                let name = id
                    .as_deref()
                    .and_then(|i| {
                        calls.get(i).cloned().or_else(|| {
                            calls
                                .iter()
                                .find(|(cid, _)| i.starts_with(cid.as_str()))
                                .map(|(_, n)| n.clone())
                        })
                    })
                    .unwrap_or_else(|| "?".to_string());
                let content = row.message.get("content").cloned().unwrap_or(Value::Null);
                let elidable = crate::payload::eligible_bytes(&content);
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
            elidable_parts: 1,
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
    if usage.context_tokens == 0 {
        usage.context_tokens = preceding_on_live.unwrap_or(0);
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

        let found = discover(&fx.root, 0);
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
    fn apply_store_elides_and_injects_in_place() {
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
        let sha = crate::copy::sha256(&before);
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
        let report = apply_store(&fx.root, "sess-w", &sha, &edits).unwrap();
        assert_eq!(report.nodes_rewritten, 1);
        assert_eq!(report.digest_node_id, Some(4));
        assert!(report.reclaimed_bytes > 0);

        let elided = node_payload(&fx.root, "sess-w", 2);
        assert!(elided.contains("[elided 3000 bytes]"));
        assert!(!elided.contains(&"x".repeat(3000)));
        assert_eq!(chain_head(&fx.root, "sess-w"), 4);
        let digest = node_payload(&fx.root, "sess-w", 4);
        assert!(digest.contains("[gobstopper state card]"));
        assert!(digest.contains("goal: ship it"));

        // The committed export still verifies clean and round-trips.
        let after = export_bytes(&db_path(&fx.root), "sess-w").unwrap();
        assert!(crate::verify::verify(Provider::Devin, &after).is_empty());
        let t = load(fx.handle("sess-w")).unwrap();
        assert_eq!(t.items.len(), 5);
    }

    #[test]
    fn apply_store_aborts_on_drift_without_writes() {
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
        let err = apply_store(&fx.root, "sess-d", &sha, &edits).unwrap_err();
        assert!(matches!(err, AdapterError::ChangedDuringWrite { .. }));
        assert_eq!(chain_head(&fx.root, "sess-d"), 1, "rolled back head move");
        assert!(node_payload(&fx.root, "sess-d", 1).contains("ok"));
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
        assert!(matches!(err, AdapterError::InvalidEdit(_)));
        drop(held);
    }

    #[test]
    fn restore_store_undoes_apply() {
        let fx = Fixture::new("restore");
        fx.add_session("sess-r", "undo", 2, 1_790_006_000);
        fx.add_node(
            "sess-r",
            0,
            None,
            serde_json::json!({"role": "user", "content": "go"}),
            None,
        );
        fx.add_node(
            "sess-r",
            1,
            Some(0),
            serde_json::json!({"role": "assistant", "tool_calls": [{"id": "c1", "name": "exec"}]}),
            None,
        );
        fx.add_node(
            "sess-r",
            2,
            Some(1),
            serde_json::json!({"role": "tool", "tool_call_id": "c1", "content": "y".repeat(4000)}),
            None,
        );
        let snapshot = export_bytes(&db_path(&fx.root), "sess-r").unwrap();
        let sha = crate::copy::sha256(&snapshot);
        let edits = vec![
            gobstopper_core::plan::Edit::Elide {
                line_indexes: vec![3],
                stub_template: "[gone {bytes}]".into(),
                per_item_stubs: Default::default(),
            },
            gobstopper_core::plan::Edit::InjectDigest {
                digest: digest_block(),
            },
        ];
        apply_store(&fx.root, "sess-r", &sha, &edits).unwrap();
        assert_eq!(chain_head(&fx.root, "sess-r"), 3);

        let report = restore_store(&fx.root, "sess-r", &snapshot).unwrap();
        assert_eq!(report.nodes_rewritten, 3);
        assert_eq!(chain_head(&fx.root, "sess-r"), 2);
        let restored = node_payload(&fx.root, "sess-r", 2);
        assert!(restored.contains(&"y".repeat(4000)));
        // Injected digest node is gone, not just orphaned.
        let conn = Connection::open(db_path(&fx.root)).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM message_nodes WHERE session_id = 'sess-r'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 3);
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
        let err = restore_store(&fx.root, "sess-rd", &snapshot).unwrap_err();
        assert!(matches!(err, AdapterError::ChangedDuringWrite { .. }));
    }

    #[test]
    fn missing_database_yields_empty() {
        let root = tmpdir("empty");
        assert!(discover(&root, 0).is_empty());
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
}
