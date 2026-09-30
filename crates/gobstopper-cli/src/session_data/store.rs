use super::schema::*;
use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

const APPLICATION_ID: i32 = 0x47534442;
const SCHEMA_VERSION: u32 = 2;
pub const MAX_BATCH_EVENTS: usize = 10_000;
pub const MAX_QUERY_EVENTS: usize = 100_000;
pub const MAX_IMPORT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DATABASE_BYTES: u64 = 1024 * 1024 * 1024;

pub fn default_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("GOBSTOPPER_DATA_DIR") {
        if path.is_empty() {
            bail!("data_directory_empty");
        }
        return Ok(PathBuf::from(path));
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")))
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
    if !directory && meta.len() > MAX_DATABASE_BYTES {
        bail!("data_database_limit");
    }
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

#[derive(Debug, Serialize)]
pub struct Status {
    pub schema_version: u32,
    pub revision: u64,
    pub events: u64,
    pub incomplete_attempts: u64,
    pub database_bytes: u64,
    pub journal_bytes: u64,
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

pub struct Store {
    connection: Connection,
    path: PathBuf,
    namespace: [u8; 32],
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
        for suffix in ["-wal", "-shm", "-journal"] {
            let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
            if fs::symlink_metadata(&sidecar).is_ok() {
                check_private(&sidecar, false)?;
            }
        }
        let mut connection = Connection::open_with_flags(
            &path,
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
        connection.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA foreign_keys=ON; PRAGMA temp_store=MEMORY; PRAGMA synchronous=FULL; PRAGMA fullfsync=ON;")?;
        // Inspect schema identity under the initialization lock. A concurrent
        // first opener must see the committed schema and preserve its namespace.
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
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
        migrate(&mut connection)?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=256; PRAGMA max_page_count=262144;",
        )?;
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
            path,
            namespace,
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
                    let prior: Envelope =
                        serde_json::from_str(&prior).context("data_invalid_event")?;
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
        let mut stmt=self.connection.prepare("SELECT envelope,digest FROM events WHERE (?1 IS NULL OR observed_at_ms>=?1) AND (?2 IS NULL OR observed_at_ms<?2) AND (?3 IS NULL OR session_id=?3) ORDER BY sequence LIMIT ?4")?;
        let mut rows = stmt.query(params![
            query.since_ms,
            query.until_ms,
            query.session_id.as_ref().map(|id| &id.0),
            MAX_QUERY_EVENTS + 1
        ])?;
        let mut events = Vec::new();
        while let Some(row) = rows.next()? {
            if events.len() == MAX_QUERY_EVENTS {
                bail!("data_query_limit_narrow_window");
            }
            let body: String = row.get(0)?;
            let digest: Vec<u8> = row.get(1)?;
            if Sha256::digest(body.as_bytes()).as_slice() != digest {
                bail!("data_integrity_failed");
            }
            let event: Envelope = serde_json::from_str(&body).context("data_invalid_event")?;
            event.validate()?;
            events.push(event);
        }
        Ok(events)
    }

    pub fn status(&self) -> Result<Status> {
        let revision = self.connection.query_row(
            "SELECT revision FROM metadata WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let events = self
            .connection
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))?;
        let incomplete_attempts=self.connection.query_row("SELECT count(*) FROM events s WHERE s.kind='request_started' AND NOT EXISTS(SELECT 1 FROM events f WHERE f.source_id=s.source_id AND f.attempt_id=s.attempt_id AND f.kind='request_finished')",[],|r|r.get(0))?;
        Ok(Status {
            schema_version: SCHEMA_VERSION,
            revision,
            events,
            incomplete_attempts,
            database_bytes: fs::metadata(&self.path)?.len(),
            journal_bytes: fs::metadata(format!("{}-wal", self.path.display()))
                .map(|m| m.len())
                .unwrap_or(0),
        })
    }

    pub fn check(&self) -> Result<Integrity> {
        let result: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if result != "ok" {
            bail!("data_integrity_failed");
        }
        let mut stmt=self.connection.prepare("SELECT event_id,source_id,observed_at_ms,kind,session_id,attempt_id,lifecycle_key,envelope,digest FROM events ORDER BY sequence")?;
        let mut rows = stmt.query([])?;
        let mut count = 0u64;
        while let Some(row) = rows.next()? {
            let body: String = row.get(7)?;
            let digest: Vec<u8> = row.get(8)?;
            if body.len() > MAX_EVENT_BYTES || Sha256::digest(body.as_bytes()).as_slice() != digest
            {
                bail!("data_integrity_failed");
            }
            let event: Envelope = serde_json::from_str(&body).context("data_invalid_event")?;
            event.validate()?;
            if row.get::<_, String>(0)? != event.event_id.0
                || row.get::<_, String>(1)? != event.source.id.0
                || row.get::<_, u64>(2)? != event.observed_at_ms
                || row.get::<_, String>(3)? != event.kind()
                || row.get::<_, Option<String>>(4)?
                    != event.identity.session_id.as_ref().map(|id| id.0.clone())
                || row.get::<_, Option<String>>(5)?
                    != event.identity.attempt_id.as_ref().map(|id| id.0.clone())
                || row.get::<_, Option<String>>(6)? != event.lifecycle_key()
            {
                bail!("data_projection_inconsistent");
            }
            count = count
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("data_counter_overflow"))?;
        }
        Ok(Integrity {
            ok: true,
            checked_events: count,
            revision: self.status()?.revision,
        })
    }

    pub fn export(&self, query: &Query, mut output: impl Write) -> Result<u64> {
        let events = self.events(query)?;
        if events.len() > MAX_BATCH_EVENTS {
            bail!("data_export_limit_narrow_window");
        }
        let header = ExportHeader {
            format: "gobstopper-observations".into(),
            schema_version: 1,
            event_count: events.len() as u64,
        };
        let header = serde_json::to_string(&header)?;
        writeln!(output, "{header}")?;
        let mut digest = Sha256::new();
        let mut bytes = header.len() as u64 + 1;
        for event in &events {
            let body = serde_json::to_vec(event)?;
            bytes += body.len() as u64 + 1;
            if bytes > MAX_IMPORT_BYTES {
                bail!("data_export_limit_narrow_window");
            }
            digest.update(&body);
            digest.update(b"\n");
            output.write_all(&body)?;
            output.write_all(b"\n")?;
        }
        let footer = serde_json::to_string(&ExportEnd {
            sha256: hex(&digest.finalize()),
            event_count: events.len() as u64,
        })?;
        if bytes + footer.len() as u64 + 1 > MAX_IMPORT_BYTES {
            bail!("data_export_limit_narrow_window");
        }
        writeln!(output, "{footer}")?;
        Ok(events.len() as u64)
    }

    pub fn import(&mut self, mut input: impl BufRead) -> Result<AppendReceipt> {
        let mut budget = MAX_IMPORT_BYTES;
        let header = read_line(&mut input, &mut budget)?
            .ok_or_else(|| anyhow::anyhow!("data_export_incomplete"))?;
        let header: ExportHeader =
            serde_json::from_slice(&header).context("data_export_invalid")?;
        if header.format != "gobstopper-observations"
            || header.schema_version != 1
            || header.event_count > MAX_BATCH_EVENTS as u64
        {
            bail!("data_export_unsupported");
        }
        let mut events = Vec::new();
        let mut digest = Sha256::new();
        for _ in 0..header.event_count {
            let body = read_line(&mut input, &mut budget)?
                .ok_or_else(|| anyhow::anyhow!("data_export_incomplete"))?;
            digest.update(&body);
            digest.update(b"\n");
            let event: Envelope = serde_json::from_slice(&body).context("data_export_invalid")?;
            event.validate()?;
            events.push(event);
        }
        let footer = read_line(&mut input, &mut budget)?
            .ok_or_else(|| anyhow::anyhow!("data_export_incomplete"))?;
        let footer: ExportEnd = serde_json::from_slice(&footer).context("data_export_invalid")?;
        if footer.event_count != header.event_count
            || footer.sha256 != hex(&digest.finalize())
            || read_line(&mut input, &mut budget)?.is_some()
        {
            bail!("data_export_checksum_failed");
        }
        self.append(&events)
    }

    pub fn backup(&self, destination: &Path) -> Result<Integrity> {
        self.check()?;
        if fs::symlink_metadata(destination).is_ok() {
            bail!("data_backup_destination_exists");
        }
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| anyhow::anyhow!("data_parent_required"))?;
        prepare_private_dir(parent)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        options
            .open(destination)
            .context("data_backup_destination_must_be_new")?
            .sync_all()?;
        // SQLite's backup API includes committed WAL pages and provides one
        // coherent snapshot even while another connection continues collecting.
        self.connection
            .backup(rusqlite::MAIN_DB, destination, None)?;
        let copy = Self::open(destination)?;
        copy.check()
    }
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
