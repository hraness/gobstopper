use super::{metrics, schema::*};
use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const APPLICATION_ID: i32 = 0x47534442;
const SCHEMA_VERSION: u32 = 2;
pub const MAX_BATCH_EVENTS: usize = 10_000;
pub const MAX_QUERY_EVENTS: usize = 100_000;
pub const MAX_IMPORT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DATABASE_BYTES: u64 = 1024 * 1024 * 1024;
pub const DEFAULT_READ_TIMEOUT_MS: u64 = 120_000;
const MAX_READ_TIMEOUT_MS: u64 = 600_000;
const MAX_ARCHIVE_SEGMENTS: u64 = 100_000;
const EVENT_COLUMNS: &str = "sequence,event_id,source_id,observed_at_ms,kind,session_id,attempt_id,lifecycle_key,envelope,digest";
const SELECTION: &str = "(?1 IS NULL OR observed_at_ms>=?1) AND (?2 IS NULL OR observed_at_ms<?2) AND (?3 IS NULL OR session_id=?3)";

struct ReadDeadline {
    start: Instant,
    duration: Duration,
    timer: Option<(std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>)>,
}

impl ReadDeadline {
    fn new(timeout_ms: u64, interrupt: Option<rusqlite::InterruptHandle>) -> Result<Self> {
        if timeout_ms == 0 || timeout_ms > MAX_READ_TIMEOUT_MS {
            bail!("data_invalid_read_timeout");
        }
        let duration = Duration::from_millis(timeout_ms);
        let start = Instant::now();
        let timer = if let Some(interrupt) = interrupt {
            let (cancel, wait) = std::sync::mpsc::channel();
            let worker = std::thread::Builder::new()
                .name("session-data-read-deadline".into())
                .spawn(move || {
                    if matches!(
                        wait.recv_timeout(duration.saturating_sub(start.elapsed())),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                    ) {
                        loop {
                            interrupt.interrupt();
                            if !matches!(
                                wait.recv_timeout(Duration::from_millis(10)),
                                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                            ) {
                                break;
                            }
                        }
                    }
                })
                .context("data_read_timer_unavailable")?;
            Some((cancel, worker))
        } else {
            None
        };
        Ok(Self {
            start,
            duration,
            timer,
        })
    }

    fn check(&self) -> Result<()> {
        if self.start.elapsed() >= self.duration {
            bail!("data_read_timeout");
        }
        Ok(())
    }

    fn stop(&mut self) {
        if let Some((cancel, worker)) = self.timer.take() {
            let _ = cancel.send(());
            let _ = worker.join();
        }
    }
}

impl Drop for ReadDeadline {
    fn drop(&mut self) {
        self.stop();
    }
}

fn retry_open_lock_contention<T>(
    busy_timeout: std::time::Duration,
    mut open: impl FnMut() -> Result<T>,
) -> Result<T> {
    // Concurrent WAL startup can return BUSY without waiting on the busy
    // handler, or exhaust SQLite's internal lock-protocol retries. Discard
    // that connection and re-read schema state on a fresh one.
    // Initialization/migration is transactional and
    // rechecks the version under its writer lock, so replay is safe here only;
    // normal reads and writes must not acquire this automatic replay behavior.
    // This bounds attempts, not wall time inside SQLite's own lock handling.
    for attempt in 0..3 {
        match open() {
            Err(error)
                if attempt < 2
                    && !busy_timeout.is_zero()
                    && error.downcast_ref::<rusqlite::Error>().is_some_and(|e| {
                        matches!(
                            e.sqlite_error_code(),
                            Some(
                                rusqlite::ErrorCode::FileLockingProtocolFailed
                                    | rusqlite::ErrorCode::DatabaseBusy
                            )
                        )
                    }) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10 << attempt));
            }
            result => return result,
        }
    }
    unreachable!("the final open attempt always returns")
}

pub fn default_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("GOBSTOPPER_DATA_DIR") {
        if path.is_empty() {
            bail!("data_directory_empty");
        }
        return Ok(PathBuf::from(path));
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            #[cfg(windows)]
            {
                std::env::var_os("LOCALAPPDATA")
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from)
                    .or_else(|| {
                        std::env::var_os("USERPROFILE")
                            .filter(|p| !p.is_empty())
                            .map(|p| PathBuf::from(p).join("AppData/Local"))
                    })
            }
            #[cfg(not(windows))]
            {
                std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share"))
            }
        })
        .ok_or_else(|| anyhow::anyhow!("data_home_unavailable"))?;
    Ok(base.join("gobstopper/private"))
}

pub fn default_path() -> Result<PathBuf> {
    Ok(default_dir()?.join("sessions.sqlite3"))
}

pub fn prepare_private_dir(dir: &Path) -> Result<()> {
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).context("data_parent_unavailable")?;
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => bail!("data_directory_unavailable"),
    }
    check_private(dir, true)
}

fn check_private(path: &Path, directory: bool) -> Result<()> {
    let meta = check_private_state(path, directory)?;
    if !directory && meta.len() > MAX_DATABASE_BYTES {
        bail!("data_database_limit");
    }
    Ok(())
}

fn check_private_state(path: &Path, directory: bool) -> Result<fs::Metadata> {
    let meta = fs::symlink_metadata(path).context("data_state_unavailable")?;
    if meta.file_type().is_symlink()
        || (directory && !meta.is_dir())
        || (!directory && !meta.is_file())
    {
        bail!("data_private_state_required");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o077 != 0 || (!directory && meta.nlink() != 1) {
            bail!("data_private_state_required");
        }
    }
    Ok(meta)
}

fn new_private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path).context("data_destination_must_be_new")
}

fn private_input(path: &Path) -> Result<BufReader<File>> {
    check_private_state(path, false)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).context("data_input_unavailable")?;
    if !file.metadata()?.is_file() {
        bail!("data_input_regular_file_required");
    }
    Ok(BufReader::new(file))
}

fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn create_private_file(path: &Path) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    match options.open(path) {
        Ok(file) => file.sync_all().context("data_initialize_failed"),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            check_private(path, false)
        }
        Err(_) => bail!("data_initialize_failed"),
    }
}

#[derive(Debug, Default, Serialize)]
pub struct AppendReceipt {
    pub inserted: u64,
    pub duplicates: u64,
    pub revision: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub since_ms: Option<u64>,
    pub until_ms: Option<u64>,
    pub session_id: Option<OpaqueId>,
}

impl Query {
    fn validate(&self) -> Result<()> {
        if self.since_ms.is_some_and(|v| v > 8_640_000_000_000_000)
            || self.until_ms.is_some_and(|v| v > 8_640_000_000_000_000)
            || matches!((self.since_ms,self.until_ms),(Some(a),Some(b)) if a >= b)
            || self.session_id.as_ref().is_some_and(|id| !id.validate())
        {
            bail!("data_invalid_query");
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct PageOptions {
    pub after_sequence: u64,
    pub through_sequence: Option<u64>,
    pub limit: usize,
    pub timeout_ms: u64,
}

impl Default for PageOptions {
    fn default() -> Self {
        Self {
            after_sequence: 0,
            through_sequence: None,
            limit: 1000,
            timeout_ms: DEFAULT_READ_TIMEOUT_MS,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct EventRow {
    pub sequence: u64,
    pub envelope: Envelope,
}

#[derive(Debug, Serialize)]
pub struct EventPage {
    pub schema_version: u32,
    pub revision: u64,
    pub snapshot_sequence: u64,
    pub events: Vec<EventRow>,
    pub next_after_sequence: Option<u64>,
    pub complete: bool,
}

#[derive(Debug, Serialize)]
pub struct Status {
    pub schema_version: u32,
    pub revision: u64,
    pub events: u64,
    pub incomplete_attempts: u64,
    pub database_bytes: u64,
    pub journal_bytes: u64,
    pub write_limit_bytes: u64,
    pub remaining_capacity_bytes: u64,
    pub capacity_warning: Option<&'static str>,
    pub at_write_capacity: bool,
    pub read_only: bool,
    pub read_timeout_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct Integrity {
    pub ok: bool,
    pub checked_events: u64,
    pub revision: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportHeader {
    format: String,
    schema_version: u32,
    event_count: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportEnd {
    sha256: String,
    event_count: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveReceipt {
    pub schema_version: u32,
    pub format: String,
    pub complete: bool,
    pub revision: u64,
    pub snapshot_sequence: u64,
    pub event_count: u64,
    pub segments: u64,
    pub manifest_sha256: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveHeader {
    format: String,
    schema_version: u32,
    revision: u64,
    snapshot_sequence: u64,
    selection: Query,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveSegment {
    file: String,
    event_count: u64,
    sha256: String,
}

struct ExportInfo {
    event_count: u64,
    sha256: String,
}

pub struct Store {
    connection: Connection,
    path: PathBuf,
    namespace: [u8; 32],
    schema_version: u32,
    read_only: bool,
}

#[derive(Clone)]
pub struct IdentityNamespace([u8; 32]);
impl IdentityNamespace {
    pub fn random() -> Result<Self> {
        let mut key = [0; 32];
        getrandom::fill(&mut key).map_err(|_| anyhow::anyhow!("data_random_unavailable"))?;
        Ok(Self(key))
    }
    pub fn opaque(&self, domain: &str, native: &str) -> OpaqueId {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).expect("fixed key size");
        mac.update(b"gobstopper-observation-id-v1\0");
        mac.update(&(domain.len() as u64).to_le_bytes());
        mac.update(domain.as_bytes());
        mac.update(&(native.len() as u64).to_le_bytes());
        mac.update(native.as_bytes());
        OpaqueId(hex(&mac.finalize().into_bytes()))
    }
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_busy_timeout(path, std::time::Duration::from_secs(2))
    }

    pub fn open_for_read(path: &Path) -> Result<Self> {
        match fs::symlink_metadata(path) {
            Ok(_) => Self::open_readonly(path),
            Err(_) => bail!("data_state_unavailable"),
        }
    }

    pub fn open_readonly(path: &Path) -> Result<Self> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| anyhow::anyhow!("data_parent_required"))?;
        check_private_state(parent, true)?;
        let name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("data_filename_required"))?;
        let path = fs::canonicalize(parent)?.join(name);
        check_private_state(&path, false)?;
        for suffix in ["-wal", "-shm", "-journal"] {
            let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
            if fs::symlink_metadata(&sidecar).is_ok() {
                check_private_state(&sidecar, false)?;
            }
        }
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .context("data_open_failed")?;
        connection.busy_timeout(Duration::from_secs(2))?;
        connection.set_limit(
            rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
            MAX_EVENT_BYTES as i32 * 2,
        )?;
        connection.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_ATTACHED, 0)?;
        connection.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON; PRAGMA temp_store=FILE; PRAGMA cache_size=-2048;")?;
        let tx = connection.unchecked_transaction()?;
        let version: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let application: i32 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            bail!("data_schema_newer_than_binary");
        }
        if version == 0 || application != APPLICATION_ID {
            bail!("data_wrong_application");
        }
        let namespace: Vec<u8> = tx
            .query_row(
                "SELECT namespace FROM metadata WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .context("data_invalid_metadata")?;
        let namespace = namespace
            .try_into()
            .map_err(|_| anyhow::anyhow!("data_invalid_namespace"))?;
        tx.commit()?;
        Ok(Self {
            connection,
            path,
            namespace,
            schema_version: version,
            read_only: true,
        })
    }

    fn snapshot<T>(
        &self,
        timeout_ms: u64,
        read: impl FnOnce(&Connection, &ReadDeadline) -> Result<T>,
    ) -> Result<T> {
        let mut deadline =
            ReadDeadline::new(timeout_ms, Some(self.connection.get_interrupt_handle()))?;
        let result = (|| {
            deadline.check()?;
            let tx = self.connection.unchecked_transaction()?;
            let value = read(&tx, &deadline);
            deadline.stop();
            deadline.check()?;
            let value = value?;
            tx.commit()?;
            Ok(value)
        })();
        deadline.stop();
        deadline.check()?;
        result
    }

    pub fn open_with_busy_timeout(path: &Path, busy_timeout: std::time::Duration) -> Result<Self> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| anyhow::anyhow!("data_parent_required"))?;
        prepare_private_dir(parent)?;
        let name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("data_filename_required"))?;
        let path = fs::canonicalize(parent)?.join(name);
        create_private_file(&path)?;
        retry_open_lock_contention(busy_timeout, || Self::open_once(&path, busy_timeout))
    }

    fn open_once(path: &Path, busy_timeout: std::time::Duration) -> Result<Self> {
        check_private(path, false)?;
        for suffix in ["-wal", "-shm", "-journal"] {
            let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
            if fs::symlink_metadata(&sidecar).is_ok() {
                check_private(&sidecar, false)?;
            }
        }
        let mut connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .context("data_open_failed")?;
        connection.busy_timeout(busy_timeout)?;
        connection.set_limit(
            rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
            MAX_EVENT_BYTES as i32 * 2,
        )?;
        connection.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_ATTACHED, 0)?;
        connection.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA foreign_keys=ON; PRAGMA temp_store=FILE; PRAGMA cache_size=-2048; PRAGMA synchronous=FULL; PRAGMA fullfsync=ON;").context("data_configure_failed")?;
        // Read a coherent schema identity without taking the writer lock. Most
        // opens are metrics queries against an already initialized database.
        let snapshot = connection
            .transaction()
            .context("data_schema_read_failed")?;
        let version: u32 = snapshot
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .context("data_schema_read_failed")?;
        let application: i32 = snapshot
            .pragma_query_value(None, "application_id", |r| r.get(0))
            .context("data_schema_read_failed")?;
        if version > SCHEMA_VERSION {
            bail!("data_schema_newer_than_binary");
        }
        if version != 0 && application != APPLICATION_ID {
            bail!("data_wrong_application");
        }
        snapshot.commit().context("data_schema_read_failed")?;
        let page_size: u64 = connection.pragma_query_value(None, "page_size", |r| r.get(0))?;
        let page_count: u64 = connection.pragma_query_value(None, "page_count", |r| r.get(0))?;
        let page_limit = MAX_DATABASE_BYTES
            .checked_div(page_size)
            .ok_or_else(|| anyhow::anyhow!("data_invalid_page_size"))?
            .min(262_144);
        if page_count > page_limit {
            bail!("data_database_limit");
        }
        connection.pragma_update(None, "max_page_count", page_limit)?;
        if version < SCHEMA_VERSION {
            // Recheck under the initialization lock: another opener may have
            // initialized or migrated the database after our read snapshot.
            let tx = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .context("data_initialize_failed")?;
            let version: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
            let application: i32 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
            if version > SCHEMA_VERSION {
                bail!("data_schema_newer_than_binary");
            }
            if version == 0 {
                let tables: u64 =
                    tx.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
                if application != 0 || tables != 0 {
                    bail!("data_unknown_database");
                }
                let mut namespace = [0; 32];
                getrandom::fill(&mut namespace)
                    .map_err(|_| anyhow::anyhow!("data_random_unavailable"))?;
                tx.execute_batch("CREATE TABLE metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1), namespace BLOB NOT NULL CHECK(length(namespace)=32), revision INTEGER NOT NULL CHECK(revision>=0)) STRICT;
                        CREATE TABLE events(sequence INTEGER PRIMARY KEY, event_id TEXT NOT NULL UNIQUE, source_id TEXT NOT NULL, observed_at_ms INTEGER NOT NULL CHECK(observed_at_ms>=0), kind TEXT NOT NULL, session_id TEXT, attempt_id TEXT, lifecycle_key TEXT UNIQUE, envelope TEXT NOT NULL, digest BLOB NOT NULL CHECK(length(digest)=32)) STRICT;")?;
                tx.execute(
                    "INSERT INTO metadata VALUES(1,?1,0)",
                    [namespace.as_slice()],
                )?;
                tx.pragma_update(None, "application_id", APPLICATION_ID)?;
                tx.pragma_update(None, "user_version", 1)?;
            } else if application != APPLICATION_ID {
                bail!("data_wrong_application");
            }
            tx.commit()?;
            migrate(&mut connection).context("data_migration_failed")?;
        }
        connection
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=256;")
            .context("data_journal_failed")?;
        let namespace: Vec<u8> = connection
            .query_row(
                "SELECT namespace FROM metadata WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .context("data_invalid_metadata")?;
        let namespace: [u8; 32] = namespace
            .try_into()
            .map_err(|_| anyhow::anyhow!("data_invalid_namespace"))?;
        Ok(Self {
            connection,
            path: path.to_path_buf(),
            namespace,
            schema_version: SCHEMA_VERSION,
            read_only: false,
        })
    }

    /// Locally keyed and domain-separated native identity. The secret namespace
    /// is backed up with the database but never included in portable exports.
    pub fn opaque(&self, domain: &str, native: &str) -> OpaqueId {
        self.identity_namespace().opaque(domain, native)
    }

    pub fn identity_namespace(&self) -> IdentityNamespace {
        IdentityNamespace(self.namespace)
    }

    pub fn append(&mut self, events: &[Envelope]) -> Result<AppendReceipt> {
        if self.read_only {
            bail!("data_read_only");
        }
        check_private(&self.path, false)?;
        let wal = PathBuf::from(format!("{}-wal", self.path.display()));
        if fs::symlink_metadata(&wal).is_ok() {
            check_private(&wal, false)?;
        }
        if events.len() > MAX_BATCH_EVENTS {
            bail!("data_batch_limit");
        }
        let prepared: Vec<_> = events
            .iter()
            .map(|event| {
                event.validate()?;
                let body = serde_json::to_string(event)?;
                let digest: [u8; 32] = Sha256::digest(body.as_bytes()).into();
                Ok((event, body, digest))
            })
            .collect::<Result<_>>()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut receipt = AppendReceipt::default();
        for (event, body, digest) in &prepared {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT envelope FROM events WHERE event_id=?1",
                    [&event.event_id.0],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(previous) = existing {
                if previous != *body {
                    bail!("data_event_identity_conflict");
                }
                receipt.duplicates += 1;
                continue;
            }
            if let Some(key) = event.lifecycle_key() {
                if tx
                    .query_row("SELECT 1 FROM events WHERE lifecycle_key=?1", [key], |r| {
                        r.get::<_, u8>(0)
                    })
                    .optional()?
                    .is_some()
                {
                    bail!("data_lifecycle_conflict");
                }
            }
            if let Some(attempt) = &event.identity.attempt_id {
                let prior: Option<String> = tx
                    .query_row(
                        "SELECT envelope FROM events WHERE source_id=?1 AND attempt_id=?2 LIMIT 1",
                        params![event.source.id.0, attempt.0],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(prior) = prior {
                    let prior: Envelope = serde_json::from_str(&prior)
                        .map_err(|_| anyhow::anyhow!("data_invalid_event"))?;
                    let mut prior_identity = prior.identity;
                    prior_identity.tool_id = None;
                    let mut identity = event.identity.clone();
                    identity.tool_id = None;
                    if prior_identity != identity || prior.source != event.source {
                        bail!("data_attempt_identity_conflict");
                    }
                }
            }
            tx.execute("INSERT INTO events(event_id,source_id,observed_at_ms,kind,session_id,attempt_id,lifecycle_key,envelope,digest) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![event.event_id.0,event.source.id.0,event.observed_at_ms,event.kind(),event.identity.session_id.as_ref().map(|id|&id.0),event.identity.attempt_id.as_ref().map(|id|&id.0),event.lifecycle_key(),body,digest.as_slice()])?;
            receipt.inserted += 1;
        }
        if receipt.inserted > 0 {
            tx.execute(
                "UPDATE metadata SET revision=revision+1 WHERE singleton=1",
                [],
            )?;
        }
        receipt.revision =
            tx.query_row("SELECT revision FROM metadata WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
        tx.commit()?;
        Ok(receipt)
    }

    pub fn append_batch(&mut self, events: &[Envelope]) -> Result<AppendReceipt> {
        self.append(events)
    }

    pub fn events(&self, query: &Query) -> Result<Vec<Envelope>> {
        query.validate()?;
        self.snapshot(DEFAULT_READ_TIMEOUT_MS, |connection, deadline| {
            let mut stmt = connection.prepare(&format!(
                "SELECT {EVENT_COLUMNS} FROM events WHERE {SELECTION} ORDER BY sequence LIMIT ?4"
            ))?;
            let mut rows = stmt.query(params![
                query.since_ms,
                query.until_ms,
                query.session_id.as_ref().map(|id| &id.0),
                MAX_QUERY_EVENTS + 1
            ])?;
            let mut events = Vec::new();
            let mut bytes = 0u64;
            while let Some(row) = rows.next()? {
                deadline.check()?;
                bytes += row.get_ref(8)?.as_str()?.len() as u64;
                if events.len() == MAX_QUERY_EVENTS || bytes > MAX_IMPORT_BYTES {
                    bail!("data_query_limit_narrow_window");
                }
                let (_, event) = validated_row(row)?;
                events.push(event);
            }
            Ok(events)
        })
    }

    pub fn metrics(&self, query: &Query) -> Result<metrics::Metrics> {
        self.metrics_with_timeout(query, DEFAULT_READ_TIMEOUT_MS)
    }

    pub fn metrics_with_timeout(&self, query: &Query, timeout_ms: u64) -> Result<metrics::Metrics> {
        query.validate()?;
        if self.schema_version < 2 {
            bail!("data_schema_migration_required");
        }
        self.snapshot(timeout_ms, |connection, deadline| {
            let mut accumulator = metrics::BoundedAccumulator::default();
            let mut stmt = connection.prepare(&format!("SELECT {EVENT_COLUMNS} FROM events WHERE {SELECTION} AND (kind IS NULL OR kind NOT IN ('request_started','request_finished')) ORDER BY sequence"))?;
            let mut rows = stmt.query(params![query.since_ms, query.until_ms, query.session_id.as_ref().map(|id| &id.0)])?;
            while let Some(row) = rows.next()? {
                deadline.check()?;
                accumulator.event(&validated_row(row)?.1)?;
            }
            drop(rows);
            drop(stmt);
            let mut stmt = connection.prepare(&format!("SELECT {EVENT_COLUMNS} FROM events INDEXED BY events_attempt WHERE {SELECTION} AND kind IN ('request_started','request_finished') ORDER BY source_id,attempt_id,kind,sequence"))?;
            let mut rows = stmt.query(params![query.since_ms, query.until_ms, query.session_id.as_ref().map(|id| &id.0)])?;
            let mut pending: Option<(u64, metrics::Request)> = None;
            while let Some(row) = rows.next()? {
                deadline.check()?;
                let (sequence, item) = validated_row(row)?;
                accumulator.event(&item)?;
                if pending.as_ref().is_some_and(|(_, request)| (&request.source.id, &request.identity.attempt_id) > (&item.source.id, &item.identity.attempt_id)) {
                    bail!("data_projection_inconsistent");
                }
                let same = pending.as_ref().is_some_and(|(_, request)| request.source.id == item.source.id && request.identity.attempt_id == item.identity.attempt_id);
                if !same {
                    if let Some((_, request)) = pending.take() {
                        accumulator.request(&request)?;
                    }
                    pending = Some((sequence, metrics::Request::new(&item)));
                }
                let (first_sequence, request) = pending.as_mut().expect("a request group exists");
                let mut identity = item.identity.clone();
                identity.tool_id = None;
                let mut previous = request.identity.clone();
                previous.tool_id = None;
                if request.source != item.source || previous != identity {
                    bail!("data_attempt_identity_conflict");
                }
                if sequence < *first_sequence {
                    request.identity = item.identity.clone();
                    *first_sequence = sequence;
                }
                if (matches!(item.event, Event::RequestStarted { .. }) && request.started_at_ms.is_some())
                    || (matches!(item.event, Event::RequestFinished { .. }) && request.finished_at_ms.is_some())
                {
                    bail!("data_lifecycle_conflict");
                }
                request.observe(&item);
            }
            if let Some((_, request)) = pending {
                accumulator.request(&request)?;
            }
            Ok(accumulator.finish())
        })
    }

    pub fn event_page(&self, query: &Query, page: &PageOptions) -> Result<EventPage> {
        query.validate()?;
        if page.limit == 0
            || page.limit > MAX_BATCH_EVENTS
            || page.after_sequence > i64::MAX as u64
            || page.through_sequence.is_some_and(|n| n > i64::MAX as u64)
        {
            bail!("data_invalid_page");
        }
        self.snapshot(page.timeout_ms, |connection, deadline| {
            let revision = revision(connection)?;
            let latest: u64 = connection.query_row("SELECT coalesce(max(sequence),0) FROM events", [], |r| r.get(0))?;
            let through = page.through_sequence.unwrap_or(latest);
            if through > latest || page.after_sequence > through {
                bail!("data_invalid_page");
            }
            let mut stmt = connection.prepare(&format!("SELECT {EVENT_COLUMNS} FROM events WHERE {SELECTION} AND sequence>?4 AND sequence<=?5 ORDER BY sequence LIMIT ?6"))?;
            let mut rows = stmt.query(params![query.since_ms, query.until_ms, query.session_id.as_ref().map(|id| &id.0), page.after_sequence, through, page.limit + 1])?;
            let mut events = Vec::new();
            let mut bytes = 0u64;
            let mut complete = true;
            while let Some(row) = rows.next()? {
                deadline.check()?;
                let (sequence, envelope) = validated_row(row)?;
                let size = serde_json::to_vec(&envelope)?.len() as u64 + 64;
                if events.len() == page.limit || bytes + size > MAX_IMPORT_BYTES {
                    complete = false;
                    break;
                }
                bytes += size;
                events.push(EventRow { sequence, envelope });
            }
            let next_after_sequence = if complete { None } else { events.last().map(|event| event.sequence) };
            Ok(EventPage { schema_version: 1, revision, snapshot_sequence: through, events, next_after_sequence, complete })
        })
    }

    pub fn status(&self) -> Result<Status> {
        self.snapshot(DEFAULT_READ_TIMEOUT_MS, |connection, _| {
            let revision = revision(connection)?;
            let events = connection.query_row("SELECT count(*) FROM events", [], |r| r.get(0))?;
            let incomplete_attempts = connection.query_row("SELECT count(*) FROM events s WHERE s.kind='request_started' AND NOT EXISTS(SELECT 1 FROM events f WHERE f.source_id=s.source_id AND f.attempt_id=s.attempt_id AND f.kind='request_finished')", [], |r| r.get(0))?;
            let database_bytes = fs::metadata(&self.path)?.len();
            let journal_bytes = fs::metadata(format!("{}-wal", self.path.display())).map(|m| m.len()).unwrap_or(0);
            let pages: u64 = connection.pragma_query_value(None, "page_count", |r| r.get(0))?;
            let page_size: u64 = connection.pragma_query_value(None, "page_size", |r| r.get(0))?;
            let used = database_bytes.max(pages.saturating_mul(page_size)).max(journal_bytes);
            let capacity_warning = if used > MAX_DATABASE_BYTES {
                Some("write_limit_exceeded")
            } else if used == MAX_DATABASE_BYTES {
                Some("write_limit_reached")
            } else if used >= (MAX_DATABASE_BYTES * 9).div_ceil(10) {
                Some("near_write_limit")
            } else {
                None
            };
            Ok(Status {
                schema_version: self.schema_version,
                revision,
                events,
                incomplete_attempts,
                database_bytes,
                journal_bytes,
                write_limit_bytes: MAX_DATABASE_BYTES,
                remaining_capacity_bytes: MAX_DATABASE_BYTES.saturating_sub(used),
                capacity_warning,
                at_write_capacity: used >= MAX_DATABASE_BYTES,
                read_only: self.read_only,
                read_timeout_ms: DEFAULT_READ_TIMEOUT_MS,
            })
        })
    }

    pub fn check(&self) -> Result<Integrity> {
        self.snapshot(DEFAULT_READ_TIMEOUT_MS, |connection, deadline| {
            let result: String =
                connection.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
            if result != "ok" {
                bail!("data_integrity_failed");
            }
            let mut stmt = connection.prepare(&format!(
                "SELECT {EVENT_COLUMNS} FROM events ORDER BY sequence"
            ))?;
            let mut rows = stmt.query([])?;
            let mut count = 0u64;
            while let Some(row) = rows.next()? {
                deadline.check()?;
                validated_row(row)?;
                count = count
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("data_counter_overflow"))?;
            }
            Ok(Integrity {
                ok: true,
                checked_events: count,
                revision: revision(connection)?,
            })
        })
    }

    pub fn export(&self, query: &Query, mut output: impl Write) -> Result<u64> {
        query.validate()?;
        self.snapshot(DEFAULT_READ_TIMEOUT_MS, |connection, deadline| {
            let mut stmt = connection.prepare(&format!(
                "SELECT {EVENT_COLUMNS} FROM events WHERE {SELECTION} ORDER BY sequence LIMIT ?4"
            ))?;
            let mut rows = stmt.query(params![
                query.since_ms,
                query.until_ms,
                query.session_id.as_ref().map(|id| &id.0),
                MAX_BATCH_EVENTS + 1
            ])?;
            let mut bodies = Vec::new();
            let mut bytes = 0u64;
            while let Some(row) = rows.next()? {
                deadline.check()?;
                let (_, event) = validated_row(row)?;
                let body = serde_json::to_vec(&event)?;
                bytes += body.len() as u64 + 1;
                if bodies.len() == MAX_BATCH_EVENTS || bytes > MAX_IMPORT_BYTES {
                    bail!("data_export_limit_narrow_window");
                }
                bodies.push(body);
            }
            Ok(write_export(&bodies, &mut output)?.event_count)
        })
    }

    pub fn import(&mut self, mut input: impl BufRead) -> Result<AppendReceipt> {
        let mut events = Vec::new();
        read_export(&mut input, |event| {
            events.push(event);
            Ok(())
        })?;
        self.append(&events)
    }

    pub fn archive(
        &self,
        query: &Query,
        destination: &Path,
        segment_events: usize,
        timeout_ms: u64,
    ) -> Result<ArchiveReceipt> {
        query.validate()?;
        if segment_events == 0 || segment_events > MAX_BATCH_EVENTS {
            bail!("data_invalid_archive_segment_size");
        }
        let receipt = self.snapshot(timeout_ms, |connection, deadline| {
            let revision = revision(connection)?;
            let snapshot_sequence =
                connection.query_row("SELECT coalesce(max(sequence),0) FROM events", [], |r| {
                    r.get(0)
                })?;
            let expected: u64 = connection.query_row(
                &format!("SELECT count(*) FROM events WHERE {SELECTION}"),
                params![
                    query.since_ms,
                    query.until_ms,
                    query.session_id.as_ref().map(|id| &id.0)
                ],
                |r| r.get(0),
            )?;
            let parent = destination
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            if !fs::metadata(parent)
                .context("data_archive_parent_required")?
                .is_dir()
            {
                bail!("data_archive_parent_required");
            }
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(destination)
                .context("data_archive_destination_must_be_new")?;
            check_private_state(destination, true)?;
            let mut manifest = new_private_file(&destination.join("manifest.jsonl"))?;
            let mut manifest_digest = Sha256::new();
            let mut manifest_budget = MAX_IMPORT_BYTES;
            manifest_line(
                &mut manifest,
                &ArchiveHeader {
                    format: "gobstopper-observation-archive".into(),
                    schema_version: 1,
                    revision,
                    snapshot_sequence,
                    selection: query.clone(),
                },
                &mut manifest_digest,
                &mut manifest_budget,
            )?;
            let mut stmt = connection.prepare(&format!(
                "SELECT {EVENT_COLUMNS} FROM events WHERE {SELECTION} ORDER BY sequence"
            ))?;
            let mut rows = stmt.query(params![
                query.since_ms,
                query.until_ms,
                query.session_id.as_ref().map(|id| &id.0)
            ])?;
            let mut bodies = Vec::new();
            let mut bytes = 512u64;
            let mut event_count = 0u64;
            let mut segments = 0u64;
            while let Some(row) = rows.next()? {
                deadline.check()?;
                let (_, event) = validated_row(row)?;
                let body = serde_json::to_vec(&event)?;
                if bodies.len() == segment_events
                    || bytes + body.len() as u64 + 1 > MAX_IMPORT_BYTES
                {
                    segments += 1;
                    let segment = archive_segment(destination, segments, &bodies)?;
                    manifest_line(
                        &mut manifest,
                        &segment,
                        &mut manifest_digest,
                        &mut manifest_budget,
                    )?;
                    bodies.clear();
                    bytes = 512;
                }
                bytes += body.len() as u64 + 1;
                bodies.push(body);
                event_count = event_count
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("data_counter_overflow"))?;
            }
            if !bodies.is_empty() {
                segments += 1;
                let segment = archive_segment(destination, segments, &bodies)?;
                manifest_line(
                    &mut manifest,
                    &segment,
                    &mut manifest_digest,
                    &mut manifest_budget,
                )?;
            }
            drop(bodies);
            if event_count != expected {
                bail!("data_archive_incomplete");
            }
            manifest.sync_all()?;
            drop(manifest);
            let receipt = ArchiveReceipt {
                schema_version: 1,
                format: "gobstopper-observation-archive".into(),
                complete: true,
                revision,
                snapshot_sequence,
                event_count,
                segments,
                manifest_sha256: hex(&manifest_digest.finalize()),
            };
            check_archive_parts(destination, &receipt, deadline)?;
            Ok(receipt)
        })?;
        let mut marker = new_private_file(&destination.join("complete.json"))?;
        serde_json::to_writer(&mut marker, &receipt)?;
        marker.write_all(b"\n")?;
        marker.sync_all()?;
        sync_dir(destination)?;
        if let Some(parent) = destination.parent().filter(|p| !p.as_os_str().is_empty()) {
            sync_dir(parent)?;
        }
        Ok(receipt)
    }

    pub fn backup(&self, destination: &Path) -> Result<Integrity> {
        let deadline = ReadDeadline::new(DEFAULT_READ_TIMEOUT_MS, None)?;
        let result = (|| {
            self.check()?;
            if fs::symlink_metadata(destination).is_ok() {
                bail!("data_backup_destination_exists");
            }
            let parent = destination
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .ok_or_else(|| anyhow::anyhow!("data_parent_required"))?;
            prepare_private_dir(parent)?;
            let file =
                new_private_file(destination).context("data_backup_destination_must_be_new")?;
            file.sync_all()?;
            // SQLite's backup API includes committed WAL pages and provides one
            // coherent snapshot even while another connection continues collecting.
            let mut target = Connection::open_with_flags(
                destination,
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )?;
            let backup = rusqlite::backup::Backup::new(&self.connection, &mut target)?;
            loop {
                deadline.check()?;
                match backup.step(256)? {
                    rusqlite::backup::StepResult::Done => break,
                    rusqlite::backup::StepResult::More => {}
                    rusqlite::backup::StepResult::Busy | rusqlite::backup::StepResult::Locked => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    _ => bail!("data_backup_failed"),
                }
            }
            drop(backup);
            drop(target);
            file.sync_all()?;
            let copy = Self::open_readonly(destination)?;
            if copy.namespace != self.namespace {
                bail!("data_backup_identity_mismatch");
            }
            let integrity = copy.check()?;
            sync_dir(parent)?;
            Ok(integrity)
        })();
        deadline.check()?;
        result
    }
}

fn manifest_line(
    output: &mut impl Write,
    value: &impl Serialize,
    digest: &mut Sha256,
    budget: &mut u64,
) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    let size = body.len() as u64 + 1;
    if size > *budget || body.len() > MAX_EVENT_BYTES {
        bail!("data_archive_manifest_limit");
    }
    *budget -= size;
    write_hashed_line(output, &body, digest)
}

fn archive_segment(directory: &Path, index: u64, bodies: &[Vec<u8>]) -> Result<ArchiveSegment> {
    if index > MAX_ARCHIVE_SEGMENTS {
        bail!("data_archive_segment_limit");
    }
    let name = format!("segment-{index:06}.jsonl");
    let mut file = new_private_file(&directory.join(&name))?;
    let info = write_export(bodies, &mut file)?;
    file.sync_all()?;
    Ok(ArchiveSegment {
        file: name,
        event_count: info.event_count,
        sha256: info.sha256,
    })
}

fn check_archive_parts(
    directory: &Path,
    receipt: &ArchiveReceipt,
    deadline: &ReadDeadline,
) -> Result<()> {
    check_private_state(directory, true)?;
    let mut manifest = private_input(&directory.join("manifest.jsonl"))?;
    let mut budget = MAX_IMPORT_BYTES;
    let mut digest = Sha256::new();
    let line = read_hashed_line(&mut manifest, &mut budget, &mut digest)?
        .ok_or_else(|| anyhow::anyhow!("data_archive_incomplete"))?;
    let header: ArchiveHeader =
        serde_json::from_slice(&line).map_err(|_| anyhow::anyhow!("data_archive_invalid"))?;
    header.selection.validate()?;
    if header.format != "gobstopper-observation-archive"
        || header.schema_version != 1
        || header.revision != receipt.revision
        || header.snapshot_sequence != receipt.snapshot_sequence
    {
        bail!("data_archive_invalid");
    }
    let mut segments = 0u64;
    let mut events = 0u64;
    while let Some(line) = read_hashed_line(&mut manifest, &mut budget, &mut digest)? {
        deadline.check()?;
        segments += 1;
        let segment: ArchiveSegment =
            serde_json::from_slice(&line).map_err(|_| anyhow::anyhow!("data_archive_invalid"))?;
        if segments > MAX_ARCHIVE_SEGMENTS || segment.file != format!("segment-{segments:06}.jsonl")
        {
            bail!("data_archive_invalid");
        }
        let mut input = private_input(&directory.join(&segment.file))?;
        let info = read_export(&mut input, |event| {
            deadline.check()?;
            if header
                .selection
                .since_ms
                .is_some_and(|n| event.observed_at_ms < n)
                || header
                    .selection
                    .until_ms
                    .is_some_and(|n| event.observed_at_ms >= n)
                || header
                    .selection
                    .session_id
                    .as_ref()
                    .is_some_and(|id| event.identity.session_id.as_ref() != Some(id))
            {
                bail!("data_archive_selection_mismatch");
            }
            Ok(())
        })?;
        if info.event_count != segment.event_count || info.sha256 != segment.sha256 {
            bail!("data_archive_checksum_failed");
        }
        events = events
            .checked_add(info.event_count)
            .ok_or_else(|| anyhow::anyhow!("data_counter_overflow"))?;
    }
    if segments != receipt.segments
        || events != receipt.event_count
        || hex(&digest.finalize()) != receipt.manifest_sha256
    {
        bail!("data_archive_checksum_failed");
    }
    deadline.check()
}

pub fn check_archive(directory: &Path, timeout_ms: u64) -> Result<ArchiveReceipt> {
    let deadline = ReadDeadline::new(timeout_ms, None)?;
    check_private_state(directory, true)?;
    let mut input =
        private_input(&directory.join("complete.json")).context("data_archive_incomplete")?;
    let mut budget = MAX_EVENT_BYTES as u64 + 1;
    let body = read_line(&mut input, &mut budget)?
        .ok_or_else(|| anyhow::anyhow!("data_archive_incomplete"))?;
    let receipt: ArchiveReceipt =
        serde_json::from_slice(&body).map_err(|_| anyhow::anyhow!("data_archive_invalid"))?;
    if !receipt.complete
        || receipt.format != "gobstopper-observation-archive"
        || receipt.schema_version != 1
        || receipt.segments > MAX_ARCHIVE_SEGMENTS
        || receipt.snapshot_sequence > i64::MAX as u64
        || read_line(&mut input, &mut budget)?.is_some()
    {
        bail!("data_archive_incomplete");
    }
    check_archive_parts(directory, &receipt, &deadline)?;
    Ok(receipt)
}

fn write_hashed_line(output: &mut impl Write, body: &[u8], digest: &mut Sha256) -> Result<()> {
    output.write_all(body)?;
    output.write_all(b"\n")?;
    digest.update(body);
    digest.update(b"\n");
    Ok(())
}

fn read_hashed_line(
    input: &mut impl BufRead,
    budget: &mut u64,
    digest: &mut Sha256,
) -> Result<Option<Vec<u8>>> {
    let line = read_line(input, budget)?;
    if let Some(body) = &line {
        digest.update(body);
        digest.update(b"\n");
    }
    Ok(line)
}

fn write_export(bodies: &[Vec<u8>], output: &mut impl Write) -> Result<ExportInfo> {
    let count = bodies.len() as u64;
    let header = serde_json::to_vec(&ExportHeader {
        format: "gobstopper-observations".into(),
        schema_version: 1,
        event_count: count,
    })?;
    let mut event_digest = Sha256::new();
    let mut bytes = header.len() as u64 + 1;
    for body in bodies {
        event_digest.update(body);
        event_digest.update(b"\n");
        bytes += body.len() as u64 + 1;
    }
    let footer = serde_json::to_vec(&ExportEnd {
        sha256: hex(&event_digest.finalize()),
        event_count: count,
    })?;
    if bodies.len() > MAX_BATCH_EVENTS || bytes + footer.len() as u64 + 1 > MAX_IMPORT_BYTES {
        bail!("data_export_limit_narrow_window");
    }
    let mut digest = Sha256::new();
    write_hashed_line(output, &header, &mut digest)?;
    for body in bodies {
        write_hashed_line(output, body, &mut digest)?;
    }
    write_hashed_line(output, &footer, &mut digest)?;
    Ok(ExportInfo {
        event_count: count,
        sha256: hex(&digest.finalize()),
    })
}

fn read_export(
    input: &mut impl BufRead,
    mut visit: impl FnMut(Envelope) -> Result<()>,
) -> Result<ExportInfo> {
    let mut budget = MAX_IMPORT_BYTES;
    let mut digest = Sha256::new();
    let header = read_hashed_line(input, &mut budget, &mut digest)?
        .ok_or_else(|| anyhow::anyhow!("data_export_incomplete"))?;
    let header: ExportHeader =
        serde_json::from_slice(&header).map_err(|_| anyhow::anyhow!("data_export_invalid"))?;
    if header.format != "gobstopper-observations"
        || header.schema_version != 1
        || header.event_count > MAX_BATCH_EVENTS as u64
    {
        bail!("data_export_unsupported");
    }
    let mut event_digest = Sha256::new();
    for _ in 0..header.event_count {
        let body = read_hashed_line(input, &mut budget, &mut digest)?
            .ok_or_else(|| anyhow::anyhow!("data_export_incomplete"))?;
        event_digest.update(&body);
        event_digest.update(b"\n");
        let event: Envelope =
            serde_json::from_slice(&body).map_err(|_| anyhow::anyhow!("data_export_invalid"))?;
        event.validate()?;
        visit(event)?;
    }
    let footer = read_hashed_line(input, &mut budget, &mut digest)?
        .ok_or_else(|| anyhow::anyhow!("data_export_incomplete"))?;
    let footer: ExportEnd =
        serde_json::from_slice(&footer).map_err(|_| anyhow::anyhow!("data_export_invalid"))?;
    if footer.event_count != header.event_count
        || footer.sha256 != hex(&event_digest.finalize())
        || read_line(input, &mut budget)?.is_some()
    {
        bail!("data_export_checksum_failed");
    }
    Ok(ExportInfo {
        event_count: header.event_count,
        sha256: hex(&digest.finalize()),
    })
}

fn revision(connection: &Connection) -> Result<u64> {
    connection
        .query_row("SELECT revision FROM metadata WHERE singleton=1", [], |r| {
            r.get(0)
        })
        .context("data_invalid_metadata")
}

fn validated_row(row: &rusqlite::Row<'_>) -> Result<(u64, Envelope)> {
    let sequence: u64 = row.get(0)?;
    let body: String = row.get(8)?;
    let digest: Vec<u8> = row.get(9)?;
    if sequence == 0
        || body.len() > MAX_EVENT_BYTES
        || Sha256::digest(body.as_bytes()).as_slice() != digest
    {
        bail!("data_integrity_failed");
    }
    let event: Envelope =
        serde_json::from_str(&body).map_err(|_| anyhow::anyhow!("data_invalid_event"))?;
    event.validate()?;
    if row.get::<_, String>(1)? != event.event_id.0
        || row.get::<_, String>(2)? != event.source.id.0
        || row.get::<_, u64>(3)? != event.observed_at_ms
        || row.get::<_, String>(4)? != event.kind()
        || row.get::<_, Option<String>>(5)?
            != event.identity.session_id.as_ref().map(|id| id.0.clone())
        || row.get::<_, Option<String>>(6)?
            != event.identity.attempt_id.as_ref().map(|id| id.0.clone())
        || row.get::<_, Option<String>>(7)? != event.lifecycle_key()
    {
        bail!("data_projection_inconsistent");
    }
    Ok((sequence, event))
}

fn migrate(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let application: i32 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
    let version: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if application != APPLICATION_ID || !(1..=SCHEMA_VERSION).contains(&version) {
        bail!("data_schema_unsupported");
    }
    if version == 1 {
        tx.execute_batch("CREATE INDEX events_time ON events(observed_at_ms); CREATE INDEX events_session ON events(session_id,observed_at_ms); CREATE INDEX events_attempt ON events(source_id,attempt_id,kind);")?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn read_line(input: &mut impl BufRead, budget: &mut u64) -> Result<Option<Vec<u8>>> {
    let mut result = Vec::new();
    loop {
        let available = input.fill_buf().context("data_read_failed")?;
        if available.is_empty() {
            if result.is_empty() {
                return Ok(None);
            }
            bail!("data_unfinished_line");
        }
        let take = available
            .iter()
            .position(|b| *b == b'\n')
            .map(|n| n + 1)
            .unwrap_or(available.len());
        if take as u64 > *budget || result.len() + take > MAX_EVENT_BYTES + 1 {
            bail!("data_import_limit");
        }
        let complete = available[take - 1] == b'\n';
        result.extend_from_slice(&available[..take]);
        input.consume(take);
        *budget -= take as u64;
        if complete {
            result.pop();
            return Ok(Some(result));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_deadline_interrupts_sql_work_and_never_interrupts_the_next_read() {
        let connection = Connection::open_in_memory().unwrap();
        let deadline = ReadDeadline::new(5, Some(connection.get_interrupt_handle())).unwrap();
        let result = connection.query_row("WITH RECURSIVE n(value) AS (SELECT 0 UNION ALL SELECT value+1 FROM n WHERE value<100000000) SELECT sum(value) FROM n", [], |row| row.get::<_, i64>(0));
        assert_eq!(
            result.unwrap_err().sqlite_error_code(),
            Some(rusqlite::ErrorCode::OperationInterrupted)
        );
        assert_eq!(
            deadline.check().unwrap_err().to_string(),
            "data_read_timeout"
        );
        drop(deadline);
        let deadline = ReadDeadline::new(5, Some(connection.get_interrupt_handle())).unwrap();
        drop(deadline);
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(
            connection
                .query_row("SELECT 1", [], |row| row.get::<_, u64>(0))
                .unwrap(),
            1
        );
    }

    fn sqlite_error(code: i32) -> anyhow::Error {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None).into()
    }

    #[test]
    fn open_lock_retry_uses_fresh_connection_and_preserves_migrated_data() {
        let directory = std::env::temp_dir().join(format!(
            "gobstopper-open-retry-test-{}",
            OpaqueId::random().unwrap().0
        ));
        let path = directory.join("sessions.sqlite3");
        let mut store = Store::open(&path).unwrap();
        let path = fs::canonicalize(path).unwrap();
        let event = Envelope::new(
            Source {
                kind: SourceKind::LegacyStats,
                id: OpaqueId::random().unwrap(),
                profile: "gobstopper-stats-v0".into(),
            },
            Identity::default(),
            Event::LegacyContext {
                estimated_before_tokens: 1,
                estimated_after_tokens: 1,
                compacted: false,
            },
        )
        .unwrap();
        store.append_batch(std::slice::from_ref(&event)).unwrap();
        let namespace = store.opaque("session", "same-native-id");
        store.connection.execute_batch("DROP INDEX events_time; DROP INDEX events_session; DROP INDEX events_attempt; PRAGMA user_version=1;").unwrap();
        drop(store);

        let busy_timeout = std::time::Duration::from_millis(100);
        let mut attempts = 0;
        let recovered = retry_open_lock_contention(busy_timeout, || {
            attempts += 1;
            let store = Store::open_once(&path, busy_timeout)?;
            assert_eq!(store.status()?.schema_version, SCHEMA_VERSION);
            assert_eq!(store.status()?.revision, 1);
            assert_eq!(store.events(&Query::default())?, vec![event.clone()]);
            assert_eq!(store.opaque("session", "same-native-id"), namespace);
            assert_eq!(
                store.connection.query_row(
                    "SELECT count(*) FROM sqlite_temp_master WHERE name='failed_open'",
                    [],
                    |row| row.get::<_, u64>(0)
                )?,
                0,
                "a failed connection's temporary state must not survive"
            );
            if attempts < 3 {
                // Inject the observed SQLite failure after migration has
                // committed and another transaction has begun. Dropping the
                // failed opener must roll back only uncommitted work; the next
                // connection must discover the already migrated schema.
                store.connection.execute_batch("CREATE TEMP TABLE failed_open(value); BEGIN IMMEDIATE; UPDATE metadata SET revision=999;")?;
                let code = if attempts == 1 {
                    rusqlite::ffi::SQLITE_PROTOCOL
                } else {
                    rusqlite::ffi::SQLITE_BUSY
                };
                return Err(sqlite_error(code).context("data_invalid_metadata"));
            }
            Ok(store)
        })
        .unwrap();
        assert_eq!(attempts, 3);
        assert!(recovered.check().unwrap().ok);
        assert_eq!(
            recovered
                .connection
                .pragma_query_value::<u64, _>(None, "busy_timeout", |row| row.get(0))
                .unwrap(),
            100,
            "the recovered connection keeps the caller's timeout"
        );
        drop(recovered);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn open_lock_retry_is_bounded_and_returns_the_last_failure() {
        let mut attempts = 0;
        let result: Result<()> =
            retry_open_lock_contention(std::time::Duration::from_millis(100), || {
                attempts += 1;
                Err(sqlite_error(rusqlite::ffi::SQLITE_PROTOCOL)
                    .context(format!("attempt_{attempts}")))
            });
        let error = result.unwrap_err();
        assert_eq!(attempts, 3);
        assert_eq!(error.to_string(), "attempt_3");
        assert_eq!(
            error
                .downcast_ref::<rusqlite::Error>()
                .unwrap()
                .sqlite_error_code(),
            Some(rusqlite::ErrorCode::FileLockingProtocolFailed)
        );
    }

    #[test]
    fn open_lock_retry_preserves_immediate_and_other_failure_semantics() {
        for (timeout, code) in [
            (std::time::Duration::ZERO, rusqlite::ffi::SQLITE_PROTOCOL),
            (std::time::Duration::ZERO, rusqlite::ffi::SQLITE_BUSY),
            (
                std::time::Duration::from_secs(2),
                rusqlite::ffi::SQLITE_LOCKED,
            ),
            (
                std::time::Duration::from_secs(2),
                rusqlite::ffi::SQLITE_CORRUPT,
            ),
            (
                std::time::Duration::from_secs(2),
                rusqlite::ffi::SQLITE_READONLY,
            ),
            (
                std::time::Duration::from_secs(2),
                rusqlite::ffi::SQLITE_IOERR,
            ),
        ] {
            let mut attempts = 0;
            let result: Result<()> = retry_open_lock_contention(timeout, || {
                attempts += 1;
                Err(sqlite_error(code).context("original_error"))
            });
            assert_eq!(attempts, 1);
            assert_eq!(result.unwrap_err().to_string(), "original_error");
        }
        let mut attempts = 0;
        let result: Result<()> =
            retry_open_lock_contention(std::time::Duration::from_secs(2), || {
                attempts += 1;
                bail!("data_wrong_application");
            });
        assert_eq!(attempts, 1);
        assert_eq!(result.unwrap_err().to_string(), "data_wrong_application");
    }

    #[test]
    fn database_full_rolls_back_batch_and_recovers_without_reset() {
        let directory = std::env::temp_dir().join(format!(
            "gobstopper-full-test-{}",
            OpaqueId::random().unwrap().0
        ));
        prepare_private_dir(&directory).unwrap();
        let mut store = Store::open(&directory.join("sessions.sqlite3")).unwrap();
        let event = || {
            Envelope::new(
                Source {
                    kind: SourceKind::LegacyStats,
                    id: OpaqueId::random().unwrap(),
                    profile: "gobstopper-stats-v0".into(),
                },
                Identity::default(),
                Event::LegacyContext {
                    estimated_before_tokens: 1,
                    estimated_after_tokens: 1,
                    compacted: false,
                },
            )
            .unwrap()
        };
        store.append_batch(&[event()]).unwrap();
        let pages: u32 = store
            .connection
            .pragma_query_value(None, "page_count", |row| row.get(0))
            .unwrap();
        store
            .connection
            .pragma_update(None, "max_page_count", pages)
            .unwrap();
        let batch: Vec<_> = (0..200).map(|_| event()).collect();
        assert!(store.append_batch(&batch).is_err());
        assert_eq!(store.status().unwrap().events, 1);
        assert_eq!(store.check().unwrap().revision, 1);
        store
            .connection
            .pragma_update(None, "max_page_count", 262144)
            .unwrap();
        assert_eq!(store.append_batch(&batch).unwrap().inserted, 200);
        assert_eq!(store.check().unwrap().checked_events, 201);
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
