//! Content-addressed snapshot vault: gobstopper's "undo a compaction".
//!
//! Before a rewrite touches a transcript, the caller snapshots it here;
//! [`restore`] puts the exact bytes back. Chunks live at
//! `chunks/<sha256>` deduplicated by content digest, and `index.jsonl`
//! is an append-only ledger of what was snapshotted, when, and by which
//! strategy. Only digests and paths are recorded — never transcript
//! payloads.

use anyhow::{bail, Context};
use gobstopper_core::Provider;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const CHUNK_BYTES: usize = 1024 * 1024;
const MAX_INDEX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_INDEX_LINE_BYTES: usize = 64 * 1024;

pub mod accounting;

/// Stable lifetime lock on the vault root directory inode: snapshots/readers
/// share custody, while pruning excludes publication and reconstruction until
/// deletion finishes. Unlike index.jsonl, this directory is never replaced.
/// Opening an existing directory also keeps reads and dry runs free of writes.
pub(crate) struct Custody {
    _file: fs::File,
}

impl Custody {
    fn acquire(root: &Path, exclusive: bool) -> anyhow::Result<Self> {
        if !fs::symlink_metadata(root)?.is_dir() {
            bail!("vault root must be a real directory, not a symlink");
        }
        #[cfg(not(unix))]
        {
            let _ = exclusive;
            bail!("safe vault custody requires directory locking on this platform");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(root)
                .context("open vault custody directory")?;
            if exclusive {
                fs2::FileExt::lock_exclusive(&file)?;
            } else {
                fs2::FileExt::lock_shared(&file)?;
            }
            Ok(Self { _file: file })
        }
    }

    pub(crate) fn shared(root: &Path) -> anyhow::Result<Self> {
        Self::acquire(root, false)
    }

    fn exclusive(root: &Path) -> anyhow::Result<Self> {
        Self::acquire(root, true)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativePin {
    schema_version: u32,
    manifest_sha256: String,
}

/// Retain a verified recovery object for a provider operation indefinitely.
/// Callers must release shared Readers before acquiring this exclusive custody.
pub fn retain_operation_snapshot(
    operation_sha256: &str,
    manifest_sha256: &str,
    root: &Path,
) -> anyhow::Result<()> {
    if operation_sha256.len() != 64
        || !is_hex(operation_sha256)
        || manifest_sha256.len() != 64
        || !is_hex(manifest_sha256)
    {
        bail!("invalid recovery pin digest");
    }
    crate::transaction::private_dir(root)?;
    let _custody = Custody::exclusive(root)?;
    read_object_locked(manifest_sha256, root)?;
    let pins = root.join("pins");
    crate::transaction::private_dir(&pins)?;
    let path = pins.join(format!("native-{operation_sha256}.json"));
    let bytes = serde_json::to_vec(&NativePin {
        schema_version: 1,
        manifest_sha256: manifest_sha256.into(),
    })?;
    if path.try_exists()? {
        if crate::transaction::read_with_limit(&path, 1024)? != bytes {
            bail!("conflicting recovery pin; repair required");
        }
        crate::transaction::confirm_publication(&path, &sha256_hex(&bytes))?;
    } else {
        crate::transaction::publish_new(&path, &bytes)?;
    }
    Ok(())
}

/// One snapshot record in the vault index.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultEntry {
    /// Unix seconds when the snapshot was taken.
    pub ts: u64,
    /// Manifest digest (or full-byte object digest for legacy snapshots).
    pub sha256: String,
    /// Original absolute path of the snapshotted file.
    pub path: PathBuf,
    /// Provider session/thread id.
    pub session_id: String,
    /// Provider that owns the transcript.
    pub provider: Provider,
    /// Byte length of the snapshotted content.
    pub bytes: u64,
    /// Strategy that produced the post-snapshot edit, if known.
    pub strategy: Option<String>,
    /// Number of transcript records in the snapshot (new in record-addressed vault).
    #[serde(default)]
    pub record_count: u64,
    /// SHA-256 of the full snapshotted transcript bytes (for source binding).
    #[serde(default)]
    pub source_sha256: String,
}

/// Production vault root: `$XDG_DATA_HOME/gobstopper/vault`, else
/// `~/.local/share/gobstopper/vault`.
pub fn default_root() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .unwrap_or_default();
    base.join("gobstopper").join("vault")
}

fn exists_no_follow(path: &Path) -> std::io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn check_object_directory(path: &Path) -> anyhow::Result<()> {
    if exists_no_follow(path)? && !fs::symlink_metadata(path)?.is_dir() {
        bail!("vault object directory is invalid; repair required");
    }
    Ok(())
}

fn objects_dir(root: &Path) -> PathBuf {
    root.join("objects")
}

fn records_dir(root: &Path) -> PathBuf {
    root.join("records")
}

fn chunks_dir(root: &Path) -> PathBuf {
    root.join("chunks")
}

fn manifests_dir(root: &Path) -> PathBuf {
    root.join("manifests")
}

fn index_path(root: &Path) -> PathBuf {
    root.join("index.jsonl")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn sha256_hex(data: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(data);
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Object names come from index lines; refuse anything that is not a
/// plain hex digest so a corrupt index can never traverse out of
/// `objects/`.
fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn valid_entry(entry: &VaultEntry) -> bool {
    entry.sha256.len() == 64
        && is_hex(&entry.sha256)
        && (entry.source_sha256.is_empty()
            || entry.source_sha256.len() == 64 && is_hex(&entry.source_sha256))
        && !entry.session_id.is_empty()
        && entry.session_id.len() <= 256
        && entry.strategy.as_ref().is_none_or(|s| s.len() <= 128)
        && entry.bytes <= crate::transaction::max_transcript_bytes()
        && entry.record_count <= gobstopper_core::validation::MAX_ITEMS as u64
}

fn parse_index(bytes: &[u8]) -> anyhow::Result<Vec<VaultEntry>> {
    if bytes.len() as u64 > MAX_INDEX_BYTES {
        bail!("vault index exceeds byte limit");
    }
    if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
        bail!("vault index has an incomplete tail; repair required");
    }
    let mut entries = Vec::new();
    for line in bytes
        .strip_suffix(b"\n")
        .unwrap_or(bytes)
        .split(|b| *b == b'\n')
        .filter(|_| !bytes.is_empty())
    {
        let entry = if !line.is_empty() && line.len() <= MAX_INDEX_LINE_BYTES {
            serde_json::from_slice::<VaultEntry>(line)
                .ok()
                .filter(valid_entry)
        } else {
            None
        };
        match entry {
            Some(entry) => entries.push(entry),
            None => {
                bail!("vault index contains an unknown or invalid root; repair required")
            }
        }
    }
    Ok(entries)
}

fn read_index_file(file: &mut fs::File) -> anyhow::Result<Vec<u8>> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_INDEX_BYTES {
        bail!("invalid or oversized vault index");
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(MAX_INDEX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_INDEX_BYTES {
        bail!("vault index exceeds byte limit");
    }
    Ok(bytes)
}

fn index_bytes_locked(root: &Path) -> anyhow::Result<Vec<u8>> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = match options.open(index_path(root)) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    if !file.metadata()?.is_file() {
        bail!("vault index must be a regular file");
    }
    fs2::FileExt::lock_shared(&file)?;
    read_index_file(&mut file)
}

/// Inspection and mutation both reject incomplete indexes. A partial view must
/// never masquerade as a complete empty history or authorize a recovery choice.
pub(crate) fn entries_locked(root: &Path) -> anyhow::Result<Vec<VaultEntry>> {
    parse_index(&index_bytes_locked(root)?)
}

fn read_index(root: &Path) -> anyhow::Result<Vec<VaultEntry>> {
    if !exists_no_follow(root)? {
        return Ok(Vec::new());
    }
    let _custody = Custody::shared(root)?;
    parse_index(&index_bytes_locked(root)?)
}

fn append_index(root: &Path, entry: &VaultEntry) -> anyhow::Result<()> {
    if !valid_entry(entry) {
        bail!("invalid snapshot index entry");
    }
    let index = index_path(root);
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true).read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(&index).context("open vault index")?;
    if !file.metadata()?.is_file() {
        bail!("vault index must be a regular file");
    }
    fs2::FileExt::lock_exclusive(&file)?;
    let original = read_index_file(&mut file)?;
    parse_index(&original)?;
    let mut line = serde_json::to_vec(entry)?;
    line.push(b'\n');
    if line.len() > MAX_INDEX_LINE_BYTES
        || original.len() as u64 + line.len() as u64 > MAX_INDEX_BYTES
    {
        bail!("vault index capacity exceeded");
    }
    let mut expected = original;
    expected.extend_from_slice(&line);
    crate::transaction::write_all(&mut file, &line, &index).map_err(|e| {
        crate::transaction::publication_error(
            &index,
            &expected,
            "index_append",
            crate::transaction::Visibility::Unknown,
            crate::transaction::Durability::Unconfirmed,
            e,
        )
    })?;
    crate::transaction::sync_file(&file, &index)
        .and_then(|_| crate::transaction::sync_dir(root))
        .map_err(|e| {
            crate::transaction::publication_error(
                &index,
                &expected,
                "index_sync",
                crate::transaction::Visibility::Published,
                crate::transaction::Durability::Unconfirmed,
                e,
            )
        })?;
    crate::transaction::checkpoint("index_appended", &index, true)?;
    Ok(())
}

/// Snapshot `path` into the vault and record it in the index.
///
/// The transcript is split into fixed-size content-addressed chunks stored
/// under `chunks/<sha>`, and a small manifest lists those hashes. Appended
/// versions reuse every complete prefix chunk while bounding each snapshot
/// to at most 128 object reads. Legacy record-addressed manifests remain
/// readable. `path` is stored verbatim; callers should pass an absolute
/// path, and lookups must match it verbatim.
pub fn snapshot(
    path: &Path,
    provider: Provider,
    session_id: &str,
    strategy: Option<&str>,
    root: &Path,
) -> anyhow::Result<VaultEntry> {
    let path = path.canonicalize()?;
    // Devin's unit of record is the session, not the shared WAL store: a
    // raw store copy tears mid-checkpoint and doesn't scale (the store is
    // shared and unbounded), while `restore_store` expects the canonical
    // export. Snapshot the session's export, exported under one read
    // transaction so the image is consistent.
    if provider == Provider::Devin && crate::devin::is_store_path(&path) {
        let data = crate::devin::export_bytes(&path, session_id)?;
        return snapshot_data(&data, &path, provider, session_id, strategy, root);
    }
    let data = crate::transaction::read(&path)?;
    snapshot_data(&data, &path, provider, session_id, strategy, root)
}

/// Snapshot caller-supplied bytes under `path`'s identity — for providers
/// whose unit of record is not the file on disk (Devin's session export
/// rather than the shared `sessions.db`).
pub fn snapshot_data(
    data: &[u8],
    path: &Path,
    provider: Provider,
    session_id: &str,
    strategy: Option<&str>,
    root: &Path,
) -> anyhow::Result<VaultEntry> {
    if data.len() as u64 > crate::transaction::max_transcript_bytes() {
        bail!("snapshot exceeds transcript byte limit");
    }
    crate::transaction::private_dir(root)?;
    let _custody = Custody::shared(root)?;
    let data = data.to_vec();
    let path = path.to_path_buf();
    let source_sha256 = sha256_hex(&data);
    let trailing_newline = data.last() == Some(&b'\n');
    let record_count = if data.is_empty() {
        0
    } else {
        data.split(|&byte| byte == b'\n').count() - usize::from(trailing_newline)
    };
    if record_count > gobstopper_core::validation::MAX_ITEMS {
        bail!("transcript exceeds vault record limit");
    }

    parse_index(&index_bytes_locked(root)?)?;
    let manifest_sha = store_object_locked(&data, root)?;

    let entry = VaultEntry {
        ts: now_secs(),
        sha256: manifest_sha,
        path,
        session_id: session_id.to_string(),
        provider,
        bytes: data.len() as u64,
        record_count: record_count as u64,
        source_sha256,
        strategy: strategy.map(str::to_string),
    };
    append_index(root, &entry)?;
    Ok(entry)
}

/// Store a verified immutable object without inventing an index entry. Callers
/// must publish a durable recovery pin before releasing custody if they rely on it.
pub(crate) fn store_object_locked(data: &[u8], root: &Path) -> anyhow::Result<String> {
    if data.len() as u64 > crate::transaction::max_transcript_bytes() {
        bail!("object exceeds byte limit");
    }
    crate::transaction::private_dir(root)?;
    let chunks = chunks_dir(root);
    let manifests = manifests_dir(root);
    crate::transaction::private_dir(&chunks)?;
    crate::transaction::private_dir(&manifests)?;

    let mut chunk_hashes = Vec::with_capacity(data.len().div_ceil(CHUNK_BYTES));
    for chunk in data.chunks(CHUNK_BYTES) {
        let chunk_sha = sha256_hex(chunk);
        let chunk_path = chunks.join(&chunk_sha);
        let published = if fs::symlink_metadata(&chunk_path).is_err() {
            match crate::transaction::publish_new(&chunk_path, chunk) {
                Ok(()) => true,
                Err(crate::AdapterError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::AlreadyExists =>
                {
                    false
                }
                Err(e) => return Err(e.into()),
            }
        } else {
            false
        };
        if crate::transaction::read(&chunk_path)? != chunk {
            bail!("vault chunk {chunk_sha} failed integrity verification");
        }
        if !published {
            // Shared writers may observe a peer's hard link before its directory
            // sync succeeds. Byte equality alone cannot authorize a durable
            // reference: reconfirm the existing inode and its namespace here.
            crate::transaction::confirm_publication(&chunk_path, &chunk_sha)?;
        }
        chunk_hashes.push(chunk_sha);
    }

    let manifest = serde_json::json!({
        "schema_version": 3,
        "source_sha256": sha256_hex(data),
        "bytes": data.len(),
        "chunks": chunk_hashes,
    });
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    let manifest_sha = sha256_hex(&manifest_bytes);
    let manifest_path = manifests.join(&manifest_sha);
    let published = if fs::symlink_metadata(&manifest_path).is_err() {
        match crate::transaction::publish_new(&manifest_path, &manifest_bytes) {
            Ok(()) => true,
            Err(crate::AdapterError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                false
            }
            Err(e) => return Err(e.into()),
        }
    } else {
        false
    };
    if crate::transaction::read(&manifest_path)? != manifest_bytes {
        bail!("manifest object {manifest_sha} failed integrity verification");
    }
    if !published {
        crate::transaction::confirm_publication(&manifest_path, &manifest_sha)?;
    }
    if reconstruct_manifest(&manifest_bytes, root)? != data {
        bail!("vault manifest does not reconstruct the source exactly");
    }

    Ok(manifest_sha)
}

/// Restore the snapshotted bytes for `sha256` over `target`, atomically:
/// write `<target>.gobstopper-restore-<pid>` then rename.
///
/// Content-addressed manifests and their objects are re-hashed before
/// concatenation. Legacy record-addressed and full-byte objects remain
/// supported.
pub fn restore(sha256: &str, target: &Path, root: &Path) -> anyhow::Result<VaultEntry> {
    if fs::symlink_metadata(target).is_ok() {
        return Err(crate::AdapterError::DirectMutationDisabled.into());
    }
    if !is_hex(sha256) {
        bail!("invalid sha256 digest: {sha256:?}");
    }
    let entry = read_index(root)?
        .into_iter()
        .rev()
        .find(|e| e.sha256 == sha256)
        .with_context(|| format!("no vault entry for sha256 {sha256}"))?;
    if !is_hex(&entry.sha256) {
        bail!("vault index entry has invalid sha256: {:?}", entry.sha256);
    }

    let data = read_object(&entry.sha256, root)?;
    if data.len() as u64 != entry.bytes
        || (!entry.source_sha256.is_empty() && sha256_hex(&data) != entry.source_sha256)
    {
        bail!("vault snapshot does not match its index binding");
    }

    if fs::symlink_metadata(target).is_ok() {
        return Err(crate::AdapterError::DirectMutationDisabled.into());
    }
    crate::transaction::publish_new(target, &data)?;
    Ok(entry)
}

/// Read a manifest and concatenate its chunks or records back into the
/// original transcript bytes.
fn reconstruct_manifest(manifest_bytes: &[u8], root: &Path) -> anyhow::Result<Vec<u8>> {
    let manifest = crate::payload::decode_record(std::str::from_utf8(manifest_bytes)?)?;
    let version = manifest
        .get("schema_version")
        .and_then(serde_json::Value::as_u64);
    if manifest.get("schema_version").is_some() && version.is_none() {
        bail!("invalid vault manifest version");
    }
    if version == Some(3) {
        let chunks = manifest
            .get("chunks")
            .and_then(serde_json::Value::as_array)
            .context("missing chunks in vault manifest")?;
        let max_chunks =
            (crate::transaction::max_transcript_bytes() as usize).div_ceil(CHUNK_BYTES);
        if chunks.len() > max_chunks {
            bail!("vault manifest exceeds chunk limit");
        }
        let expected_bytes = manifest
            .get("bytes")
            .and_then(serde_json::Value::as_u64)
            .context("missing byte count in vault manifest")?;
        if expected_bytes > crate::transaction::max_transcript_bytes() {
            bail!("vault manifest exceeds byte limit");
        }
        let mut out = Vec::with_capacity(expected_bytes as usize);
        for value in chunks {
            let sha = value.as_str().context("chunk hash is not a string")?;
            if sha.len() != 64 || !is_hex(sha) {
                bail!("invalid chunk hash in manifest");
            }
            let chunk = crate::transaction::read_with_limit(
                &chunks_dir(root).join(sha),
                CHUNK_BYTES as u64,
            )?;
            if chunk.len() > CHUNK_BYTES || sha256_hex(&chunk) != sha {
                bail!("vault chunk failed integrity verification");
            }
            if out
                .len()
                .checked_add(chunk.len())
                .is_none_or(|size| size as u64 > expected_bytes)
            {
                bail!("vault chunks exceed manifest byte count");
            }
            out.extend_from_slice(&chunk);
        }
        if out.len() as u64 != expected_bytes {
            bail!("vault chunks do not reach manifest byte count");
        }
        let expected_source = manifest
            .get("source_sha256")
            .and_then(serde_json::Value::as_str)
            .context("missing source digest in vault manifest")?;
        if expected_source.len() != 64
            || !is_hex(expected_source)
            || sha256_hex(&out) != expected_source
        {
            bail!("vault manifest source digest mismatch");
        }
        return Ok(out);
    }
    if version.is_some_and(|version| version != 2) {
        bail!("unsupported vault manifest version");
    }
    let records = manifest
        .get("records")
        .and_then(|v| v.as_array())
        .with_context(|| "missing records in manifest")?;
    if records.len() > gobstopper_core::validation::MAX_ITEMS {
        bail!("vault manifest exceeds record limit");
    }
    if manifest
        .get("trailing_newline")
        .is_some_and(|v| !v.is_boolean())
    {
        bail!("invalid legacy newline flag");
    }
    if manifest
        .get("source_sha256")
        .is_some_and(|v| !v.is_string())
    {
        bail!("invalid legacy source digest");
    }
    if manifest.get("bytes").is_some_and(|v| v.as_u64().is_none()) {
        bail!("invalid legacy byte count");
    }
    let trailing = manifest
        .get("trailing_newline")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let expected_source = manifest
        .get("source_sha256")
        .and_then(serde_json::Value::as_str);
    if expected_source.is_some_and(|sha| sha.len() != 64 || !is_hex(sha)) {
        bail!("invalid source digest in vault manifest");
    }

    let mut out = Vec::new();
    for (i, r) in records.iter().enumerate() {
        let sha = r.as_str().with_context(|| "record hash is not a string")?;
        if sha.len() != 64 || !is_hex(sha) {
            bail!("invalid record hash in manifest: {sha:?}");
        }
        let record_path = records_dir(root).join(sha);
        let record = crate::transaction::read(&record_path)
            .with_context(|| format!("read vault record {}", record_path.display()))?;
        let actual = sha256_hex(&record);
        if actual != sha {
            bail!("record {sha} is corrupt: hashes to {actual}");
        }
        let newline = usize::from(trailing || i + 1 < records.len());
        let next = out
            .len()
            .checked_add(record.len())
            .and_then(|size| size.checked_add(newline))
            .filter(|size| *size as u64 <= crate::transaction::max_transcript_bytes())
            .context("reconstructed transcript exceeds byte limit")?;
        out.reserve(next - out.len());
        out.extend_from_slice(&record);
        if newline == 1 {
            out.push(b'\n');
        }
    }
    if manifest
        .get("bytes")
        .and_then(|v| v.as_u64())
        .is_some_and(|n| n != out.len() as u64)
    {
        bail!("legacy byte count mismatch");
    }
    if expected_source.is_some_and(|sha| sha256_hex(&out) != sha) {
        bail!("vault manifest source digest mismatch");
    }
    Ok(out)
}

/// The most recent snapshot taken of `path`, matching its canonical form
/// when available. Append order, not wall time, defines recency.
pub fn latest_for(path: &Path, root: &Path) -> anyhow::Result<Option<VaultEntry>> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    Ok(read_index(root)?
        .into_iter()
        .rev()
        .find(|e| e.path == path || e.path == canonical))
}

/// Every index entry, newest first. Damaged or ambiguous input is rejected.
pub fn list(root: &Path) -> anyhow::Result<Vec<VaultEntry>> {
    let mut entries = read_index(root)?;
    // Append order is authoritative even when the wall clock moves backward.
    entries.reverse();
    Ok(entries)
}

/// Shared custody across a caller's selection and all dependent object reads.
pub struct Reader {
    root: PathBuf,
    _custody: Option<Custody>,
}
impl Reader {
    pub fn open(root: &Path) -> anyhow::Result<Self> {
        let custody = match fs::symlink_metadata(root) {
            Ok(_) => Some(Custody::shared(root)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            root: root.into(),
            _custody: custody,
        })
    }
    pub fn entries(&self) -> anyhow::Result<Vec<VaultEntry>> {
        if self._custody.is_none() {
            return Ok(Vec::new());
        }
        let mut entries = parse_index(&index_bytes_locked(&self.root)?)?;
        entries.reverse();
        Ok(entries)
    }
    pub fn diff(&self, sha1: &str, sha2: &str) -> anyhow::Result<DiffSummary> {
        if self._custody.is_none() {
            bail!("vault does not exist");
        }
        diff_locked(sha1, sha2, &self.root)
    }
    pub fn read_object(&self, sha: &str) -> anyhow::Result<Vec<u8>> {
        if self._custody.is_none() {
            bail!("vault does not exist");
        }
        read_object_locked(sha, &self.root)
    }
    pub fn recall_entries(
        &self,
        entries: &[VaultEntry],
        query: Option<&str>,
    ) -> anyhow::Result<Vec<RecallDigest>> {
        if self._custody.is_none() {
            if entries.is_empty() {
                return Ok(Vec::new());
            }
            bail!("vault does not exist");
        }
        recall_entries_locked(entries.to_vec(), query, &self.root)
    }
}

pub fn read_object(sha256: &str, root: &Path) -> anyhow::Result<Vec<u8>> {
    Reader::open(root)?.read_object(sha256)
}

pub(crate) fn read_object_locked(sha256: &str, root: &Path) -> anyhow::Result<Vec<u8>> {
    for name in ["manifests", "chunks", "records", "objects"] {
        check_object_directory(&root.join(name))?;
    }
    if sha256.len() != 64 || !is_hex(sha256) {
        bail!("invalid snapshot digest");
    }

    // Try a record-addressed manifest first.
    let manifest_path = manifests_dir(root).join(sha256);
    if manifest_path.exists() {
        let bytes = crate::transaction::read(&manifest_path)?;
        if sha256_hex(&bytes) != sha256 {
            bail!("manifest failed integrity verification");
        }
        return reconstruct_manifest(&bytes, root);
    }

    // Fall back to a legacy full-byte object.
    let bytes = crate::transaction::read(&objects_dir(root).join(sha256))?;
    if sha256_hex(&bytes) != sha256 {
        bail!("vault object failed integrity verification");
    }
    Ok(bytes)
}

pub fn latest_pre_compaction(path: &Path, root: &Path) -> anyhow::Result<Option<VaultEntry>> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    Ok(list(root)?.into_iter().find(|entry| {
        (entry.path == path || entry.path == canonical)
            && !matches!(entry.strategy.as_deref(), Some("post-compact" | "pre-undo"))
    }))
}

/// All snapshots for a session id (newest first). Accepts a prefix.
pub fn for_session(session_id: &str, root: &Path) -> anyhow::Result<Vec<VaultEntry>> {
    Ok(list(root)?
        .into_iter()
        .filter(|e| e.session_id.starts_with(session_id))
        .collect())
}

/// Outcome of a [`prune`] pass (planned when `dry_run`, applied otherwise).
#[derive(Debug, serde::Serialize)]
pub struct PruneReport {
    /// Distinct (provider, session) streams seen in the index.
    pub streams: usize,
    /// Index entries retained.
    pub kept_entries: usize,
    /// Index entries dropped.
    pub dropped_entries: usize,
    /// Manifest objects removed (deduplicated — several index entries can
    /// share one manifest).
    pub manifests_removed: usize,
    /// Content chunks removed that no surviving manifest references.
    pub chunks_removed: usize,
    /// Sum of chunk sizes removed from disk.
    pub bytes_reclaimed: u64,
    /// When true nothing was deleted — the report is the plan.
    pub dry_run: bool,
}

/// Retain only the newest `keep` snapshots per (provider, session)
/// stream, then drop manifest objects no index entry references and
/// content chunks no surviving manifest reaches.
///
/// Stable exclusive custody excludes snapshots and active readers for
/// the full scan and deletion. Receipt recovery references are also
/// retained. `index.jsonl` is rewritten before unreachable objects are
/// unlinked, so a crash mid-delete leaves only orphans. `dry_run`
/// computes the same plan without changing stored data.
#[derive(Debug, thiserror::Error)]
#[error("prune cleanup incomplete during {stage}; index retirement may be visible, durability unconfirmed: {source}")]
pub struct PruneIncomplete {
    pub report: PruneReport,
    pub stage: &'static str,
    #[source]
    pub source: std::io::Error,
}

fn object_names(directory: &Path) -> anyhow::Result<std::collections::BTreeSet<String>> {
    let mut names = std::collections::BTreeSet::new();
    if !exists_no_follow(directory)? {
        return Ok(names);
    }
    if !fs::symlink_metadata(directory)?.is_dir() {
        bail!("vault object directory is invalid; repair required");
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("unknown vault object name; repair required"))?;
        if !entry.file_type()?.is_file() || name.len() != 64 || !is_hex(&name) {
            bail!("unknown vault object or abandoned temporary; repair required");
        }
        names.insert(name);
    }
    Ok(names)
}

pub fn prune(root: &Path, keep: usize, dry_run: bool) -> anyhow::Result<PruneReport> {
    use std::collections::{BTreeMap, BTreeSet, HashSet};
    if !exists_no_follow(root)? {
        return Ok(PruneReport {
            streams: 0,
            kept_entries: 0,
            dropped_entries: 0,
            manifests_removed: 0,
            chunks_removed: 0,
            bytes_reclaimed: 0,
            dry_run,
        });
    }
    let _custody = Custody::exclusive(root)?;
    let index = index_path(root);
    let original_index = index_bytes_locked(root)?;
    let entries = parse_index(&original_index)?;
    let mut groups = BTreeMap::<(String, PathBuf, String), Vec<usize>>::new();
    for (i, entry) in entries.iter().enumerate() {
        groups
            .entry((
                entry.provider.as_str().into(),
                entry.path.clone(),
                entry.session_id.clone(),
            ))
            .or_default()
            .push(i);
    }
    let mut retained = HashSet::new();
    for group in groups.values() {
        retained.extend(group.iter().rev().take(keep).copied());
    }
    let mut roots: BTreeSet<_> = entries
        .iter()
        .enumerate()
        .filter(|(i, _)| retained.contains(i))
        .map(|(_, entry)| entry.sha256.clone())
        .collect();
    let operations = root.join("operations");
    if exists_no_follow(&operations)? {
        if !fs::symlink_metadata(&operations)?.is_dir() {
            bail!("invalid operations directory; repair required");
        }
        for entry in fs::read_dir(&operations)? {
            let entry = entry?;
            let path = entry.path();
            let identity = path
                .file_stem()
                .and_then(|s| s.to_str())
                .context("invalid operation name; repair required")?;
            if !entry.file_type()?.is_file() || identity.len() != 64 || !is_hex(identity) {
                bail!("unknown operation artifact; repair required");
            }
            match path.extension().and_then(|s| s.to_str()) {
                Some("lock") => {}
                Some("json") => {
                    let raw = crate::transaction::read_with_limit(&path, 64 * 1024)?;
                    roots.extend(crate::copy::recovery_roots_locked(
                        &raw, identity, &entries, root,
                    )?);
                }
                _ => bail!("unknown operation artifact; repair required"),
            }
        }
    }
    let pins = root.join("pins");
    if exists_no_follow(&pins)? {
        if !fs::symlink_metadata(&pins)?.is_dir() {
            bail!("invalid pin directory; repair required");
        }
        for entry in fs::read_dir(&pins)? {
            let entry = entry?;
            let name = entry.file_name();
            let identity = name
                .to_str()
                .and_then(|s| s.strip_prefix("native-"))
                .and_then(|s| s.strip_suffix(".json"))
                .context("unknown recovery pin; repair required")?;
            if !entry.file_type()?.is_file() || identity.len() != 64 || !is_hex(identity) {
                bail!("invalid recovery pin; repair required");
            }
            let pin: NativePin =
                serde_json::from_slice(&crate::transaction::read_with_limit(&entry.path(), 1024)?)?;
            if pin.schema_version != 1
                || pin.manifest_sha256.len() != 64
                || !is_hex(&pin.manifest_sha256)
            {
                bail!("unsupported recovery pin; repair required");
            }
            roots.insert(pin.manifest_sha256);
        }
    }
    for sha in &roots {
        read_object_locked(sha, root)
            .context("retained snapshot is unreadable; repair required")?;
    }
    let manifests = manifests_dir(root);
    let manifests_on_disk = object_names(&manifests)?;
    let chunks = chunks_dir(root);
    let chunks_on_disk = object_names(&chunks)?;
    // Unknown legacy roots are also damage, even though this phase never
    // collects legacy record/full-byte objects.
    object_names(&objects_dir(root))?;
    object_names(&records_dir(root))?;
    let remove_manifests: BTreeSet<_> = entries
        .iter()
        .enumerate()
        .filter(|(i, e)| {
            !retained.contains(i)
                && !roots.contains(&e.sha256)
                && manifests_on_disk.contains(&e.sha256)
        })
        .map(|(_, e)| e.sha256.clone())
        .collect();
    let mut keep_chunks = BTreeSet::new();
    // Validate all manifests before mutation, including retired ones. Orphans
    // are conservatively retained; we never guess which crash created them.
    for sha in &manifests_on_disk {
        let bytes = crate::transaction::read_with_limit(&manifests.join(sha), 16 * 1024 * 1024)?;
        if sha256_hex(&bytes) != *sha {
            bail!("manifest integrity failure; repair required");
        }
        reconstruct_manifest(&bytes, root)?;
        if !remove_manifests.contains(sha) {
            let doc = crate::payload::decode_record(std::str::from_utf8(&bytes)?)?;
            if let Some(values) = doc.get("chunks").and_then(|v| v.as_array()) {
                for value in values {
                    keep_chunks.insert(value.as_str().context("invalid chunk digest")?.to_string());
                }
            }
        }
    }
    let remove_chunks: Vec<_> = chunks_on_disk
        .difference(&keep_chunks)
        .map(|sha| chunks.join(sha))
        .collect();
    let mut planned_bytes = 0u64;
    for path in &remove_chunks {
        planned_bytes = planned_bytes
            .checked_add(fs::metadata(path)?.len())
            .context("prune byte count overflow")?;
    }
    let mut report = PruneReport {
        streams: groups.len(),
        kept_entries: retained.len(),
        dropped_entries: entries.len() - retained.len(),
        manifests_removed: 0,
        chunks_removed: 0,
        bytes_reclaimed: 0,
        dry_run,
    };
    if dry_run {
        report.manifests_removed = remove_manifests.len();
        report.chunks_removed = remove_chunks.len();
        report.bytes_reclaimed = planned_bytes;
        return Ok(report);
    }
    let mut new_index = Vec::new();
    // Preserve exact metadata bytes and order for each retained entry.
    for (i, line) in original_index
        .strip_suffix(b"\n")
        .unwrap_or(&original_index)
        .split(|b| *b == b'\n')
        .filter(|_| !original_index.is_empty())
        .enumerate()
    {
        if retained.contains(&i) {
            new_index.extend_from_slice(line);
            new_index.push(b'\n');
        }
    }
    if index.try_exists()? {
        crate::transaction::replace(&index, &original_index, &new_index)?;
    }
    crate::transaction::checkpoint("prune_index_replaced", &index, true)?;
    for sha in &remove_manifests {
        let path = manifests.join(sha);
        let result = crate::transaction::remove_file(&path);
        if result.is_ok()
            || fs::symlink_metadata(&path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            report.manifests_removed += 1;
        }
        if let Err(source) = result {
            return Err(PruneIncomplete {
                report,
                stage: "manifest removal",
                source,
            }
            .into());
        }
    }
    if manifests.try_exists()? {
        if let Err(source) = crate::transaction::sync_dir(&manifests) {
            return Err(PruneIncomplete {
                report,
                stage: "manifest sync",
                source,
            }
            .into());
        }
    }
    // Manifest removal and its directory sync must finish before any chunk
    // removal. A failed unlink cannot leave a dangling manifest.
    for path in remove_chunks {
        let bytes = fs::metadata(&path)?.len();
        let result = crate::transaction::remove_file(&path);
        if result.is_ok()
            || fs::symlink_metadata(&path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            report.chunks_removed += 1;
            report.bytes_reclaimed += bytes;
        }
        if let Err(source) = result {
            return Err(PruneIncomplete {
                report,
                stage: "chunk removal",
                source,
            }
            .into());
        }
    }
    if chunks.try_exists()? {
        if let Err(source) = crate::transaction::sync_dir(&chunks) {
            return Err(PruneIncomplete {
                report,
                stage: "chunk sync",
                source,
            }
            .into());
        }
    }
    Ok(report)
}

fn record_type(record: &[u8]) -> String {
    std::str::from_utf8(record)
        .ok()
        .and_then(|raw| crate::payload::decode_record(raw).ok())
        .and_then(|v| v.get("type").and_then(|t| t.as_str().map(str::to_string)))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Structural diff between two vault snapshots.
///
/// Returns added and removed record hash lists plus the record counts by
/// type for each side. New chunk manifests are reconstructed within the
/// transcript byte bound; legacy record manifests use their hash lists.
pub fn diff(sha1: &str, sha2: &str, root: &Path) -> anyhow::Result<DiffSummary> {
    Reader::open(root)?.diff(sha1, sha2)
}

fn diff_locked(sha1: &str, sha2: &str, root: &Path) -> anyhow::Result<DiffSummary> {
    if !is_hex(sha1) || !is_hex(sha2) {
        bail!("invalid sha256 digest");
    }

    fn load_records(sha: &str, root: &Path) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
        let data = read_object_locked(sha, root)?;
        let trailing = data.last() == Some(&b'\n');
        let mut records: Vec<&[u8]> = if data.is_empty() {
            Vec::new()
        } else {
            data.split(|&byte| byte == b'\n').collect()
        };
        if trailing {
            records.pop();
        }
        if records.len() > gobstopper_core::validation::MAX_ITEMS {
            bail!("snapshot exceeds record limit");
        }
        Ok(records
            .into_iter()
            .map(|record| (sha256_hex(record), record.to_vec()))
            .collect())
    }

    let a = load_records(sha1, root)?;
    let b = load_records(sha2, root)?;

    let a_set: std::collections::HashSet<&str> = a.iter().map(|(hash, _)| hash.as_str()).collect();
    let b_set: std::collections::HashSet<&str> = b.iter().map(|(hash, _)| hash.as_str()).collect();

    let mut removed = Vec::new();
    let mut added = Vec::new();
    let mut type_summary_a = std::collections::BTreeMap::<String, usize>::new();
    let mut type_summary_b = std::collections::BTreeMap::<String, usize>::new();

    for (hash, record) in &a {
        if !b_set.contains(hash.as_str()) {
            removed.push(hash.clone());
        }
        *type_summary_a.entry(record_type(record)).or_default() += 1;
    }
    for (hash, record) in &b {
        if !a_set.contains(hash.as_str()) {
            added.push(hash.clone());
        }
        *type_summary_b.entry(record_type(record)).or_default() += 1;
    }

    Ok(DiffSummary {
        sha1: sha1.to_string(),
        sha2: sha2.to_string(),
        record_count_a: a.len(),
        record_count_b: b.len(),
        added,
        removed,
        type_summary_a,
        type_summary_b,
    })
}

/// Read a single record object by hash.
pub fn read_record(sha: &str, root: &Path) -> anyhow::Result<Vec<u8>> {
    let _custody = Custody::shared(root)?;
    check_object_directory(&records_dir(root))?;
    if sha.len() != 64 || !is_hex(sha) {
        bail!("invalid record digest");
    }
    let p = records_dir(root).join(sha);
    let bytes =
        crate::transaction::read(&p).with_context(|| format!("read record {}", p.display()))?;
    if sha256_hex(&bytes) != sha {
        bail!("record {sha} is corrupt");
    }
    Ok(bytes)
}

#[derive(Debug, Clone)]
pub struct DiffSummary {
    pub sha1: String,
    pub sha2: String,
    pub record_count_a: usize,
    pub record_count_b: usize,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub type_summary_a: std::collections::BTreeMap<String, usize>,
    pub type_summary_b: std::collections::BTreeMap<String, usize>,
}

/// One gobstopper state-card digest recovered from a vault snapshot.
/// This is the agent-addressable memory surface: high-level state, no
/// verbatim tool output.
#[derive(Debug, Clone)]
pub struct RecallDigest {
    pub snapshot_sha: String,
    pub ts: u64,
    pub provider: Provider,
    pub session_id: String,
    pub record_index: usize,
    /// Query-relevance score: higher means more keyword matches.
    pub score: usize,
    pub digest: gobstopper_core::plan::DigestBlock,
}

/// Search the vault for state-card digests belonging to `session`.
///
/// `session` is a session-id prefix or a transcript path. `query`, if
/// given, is a case-insensitive substring matched against every state-card
/// text field. `sha` restricts the search to matching snapshot prefixes.
/// Results rank match counts, then newest snapshot timestamps.
pub fn recall(
    session: &str,
    query: Option<&str>,
    sha: Option<&str>,
    root: &Path,
) -> anyhow::Result<Vec<RecallDigest>> {
    if !exists_no_follow(root)? {
        return Ok(Vec::new());
    }
    let reader = Reader::open(root)?;
    let mut entries = reader.entries()?;
    if let Some(prefix) = sha {
        entries.retain(|e| e.sha256.starts_with(prefix));
    }
    if session != "*" && !session.is_empty() {
        entries.retain(|e| {
            e.session_id.starts_with(session) || e.path.to_string_lossy().contains(session)
        });
    }
    reader.recall_entries(&entries, query)
}

fn recall_entries_locked(
    entries: Vec<VaultEntry>,
    query: Option<&str>,
    root: &Path,
) -> anyhow::Result<Vec<RecallDigest>> {
    // The same manifest may be indexed more than once under different
    // strategy labels (e.g. "compacted" and "gobstopper-compacted").
    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::with_capacity(entries.len());
    for e in entries {
        if seen.insert(e.sha256.clone()) {
            deduped.push(e);
        }
    }
    let entries = deduped;

    let needle = query.map(|q| q.to_lowercase());
    let mut out = Vec::new();
    for entry in entries {
        let data = read_object_locked(&entry.sha256, root)?;
        for (idx, line) in data.split(|&b| b == b'\n').enumerate() {
            if line.is_empty() {
                continue;
            }
            let Some(text) = extract_digest_text(line) else {
                continue;
            };
            let Some(digest) = gobstopper_core::plan::DigestBlock::parse(&text) else {
                continue;
            };
            let hay = [
                digest.goal.as_deref().unwrap_or(""),
                digest.summary.as_deref().unwrap_or(""),
                &digest.concepts.join("\n"),
                &digest.decisions.join("\n"),
                &digest.files_touched.join("\n"),
                &digest.errors.join("\n"),
                &digest.open_tasks.join("\n"),
                digest.current_work.as_deref().unwrap_or(""),
                digest.context.as_deref().unwrap_or(""),
            ]
            .join("\n")
            .to_lowercase();
            let score = needle.as_ref().map(|n| hay.matches(n).count()).unwrap_or(0);
            if needle.is_some() && score == 0 {
                continue;
            }
            out.push(RecallDigest {
                snapshot_sha: entry.sha256.clone(),
                ts: entry.ts,
                provider: entry.provider,
                session_id: entry.session_id.clone(),
                record_index: idx,
                score,
                digest,
            });
        }
    }
    out.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.ts.cmp(&a.ts)));
    Ok(out)
}

/// Extract the gobstopper state-card text from a single transcript record
/// bytes, if one exists. Claude carries it in a `user` message; Codex uses
/// a portable user response message or the first replacement-history item
/// of an experimental `compacted` record. Card text is untrusted history.
fn extract_digest_text(line: &[u8]) -> Option<String> {
    let record = crate::payload::decode_record(std::str::from_utf8(line).ok()?).ok()?;

    // The default portable Codex writer appends a user response message.
    // Do not accept lookalike markers in assistant messages or tool outputs.
    if record.get("type").and_then(|v| v.as_str()) == Some("response_item") {
        let payload = record.get("payload")?;
        if payload.get("type").and_then(|v| v.as_str()) == Some("message")
            && payload.get("role").and_then(|v| v.as_str()) == Some("user")
        {
            if let Some(text) =
                payload
                    .get("content")
                    .and_then(|v| v.as_array())
                    .and_then(|items| {
                        items.iter().find_map(|item| {
                            (item.get("type").and_then(|v| v.as_str()) == Some("input_text"))
                                .then(|| item.get("text").and_then(|v| v.as_str()))
                                .flatten()
                                .filter(|text| {
                                    text.starts_with(gobstopper_core::plan::DigestBlock::MARKER)
                                })
                        })
                    })
            {
                return Some(text.to_string());
            }
        }
    }

    // Claude: a user record with a plain string message.content.
    if record.get("type").and_then(|v| v.as_str()) == Some("user") {
        if let Some(text) = record
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
        {
            if text.starts_with(gobstopper_core::plan::DigestBlock::MARKER) {
                return Some(text.to_string());
            }
        }
    }

    // Codex: a compacted record whose first replacement_history item is
    // a user-shaped message with input_text content.
    if record.get("type").and_then(|v| v.as_str()) == Some("compacted") {
        if let Some(text) = record
            .get("payload")
            .and_then(|p| p.get("replacement_history"))
            .and_then(|h| h.as_array())
            .and_then(|a| a.first())
            .and_then(|first| first.get("content"))
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|first| first.get("text"))
            .and_then(|t| t.as_str())
        {
            if text.starts_with(gobstopper_core::plan::DigestBlock::MARKER) {
                return Some(text.to_string());
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "gob-vault-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn snapshot_then_restore_round_trips_bytes() {
        let dir = TestDir::new();
        let root = dir.0.join("vault");
        let src = dir.0.join("session.jsonl");
        let original = b"\n{\"line\":1}\n\n{\"line\":2}\n \n";
        fs::write(&src, original).unwrap();

        let entry = snapshot(&src, Provider::ClaudeCode, "sess-1", Some("elide"), &root).unwrap();
        assert_eq!(entry.bytes, original.len() as u64);
        assert_eq!(entry.path, src.canonicalize().unwrap());
        assert_eq!(entry.session_id, "sess-1");
        assert_eq!(entry.strategy.as_deref(), Some("elide"));
        assert!(manifests_dir(&root).join(&entry.sha256).is_file());

        // Simulate a compaction edit clobbering the transcript.
        fs::write(&src, b"{\"compacted\":true}\n").unwrap();
        assert!(restore(&entry.sha256, &src, &root).is_err());
        let target = dir.0.join("restored.jsonl");
        let restored = restore(&entry.sha256, &target, &root).unwrap();
        assert_eq!(restored.sha256, entry.sha256);
        assert_eq!(fs::read(&target).unwrap(), original);
        assert_eq!(fs::read(&src).unwrap(), b"{\"compacted\":true}\n");

        // No temp file left behind.
        let tmp = PathBuf::from(format!(
            "{}.gobstopper-restore-{}",
            src.display(),
            std::process::id()
        ));
        assert!(!tmp.exists());
    }

    #[test]
    fn restore_refuses_corrupt_object() {
        let dir = TestDir::new();
        let root = dir.0.join("vault");
        let src = dir.0.join("t.jsonl");
        fs::write(&src, b"original bytes").unwrap();
        let entry = snapshot(&src, Provider::Codex, "s", None, &root).unwrap();

        // Tamper with the stored manifest.
        fs::write(manifests_dir(&root).join(&entry.sha256), b"tampered").unwrap();

        assert!(restore(&entry.sha256, &src, &root).is_err());
        // The target must be untouched.
        assert_eq!(fs::read(&src).unwrap(), b"original bytes");
    }

    #[test]
    fn restore_unknown_sha_errors() {
        let dir = TestDir::new();
        let root = dir.0.join("vault");
        let target = dir.0.join("t.jsonl");
        assert!(restore("deadbeef", &target, &root).is_err());
        // Non-hex input is rejected before any object-path join.
        assert!(restore("../escape", &target, &root).is_err());
    }

    #[test]
    fn latest_for_returns_newest_snapshot() {
        let dir = TestDir::new();
        let root = dir.0.join("vault");
        let a = dir.0.join("a.jsonl");
        let b = dir.0.join("b.jsonl");
        fs::write(&a, b"v1").unwrap();
        fs::write(&b, b"other").unwrap();

        snapshot(&a, Provider::Codex, "first", None, &root).unwrap();
        snapshot(&b, Provider::Codex, "b", None, &root).unwrap();
        fs::write(&a, b"v2").unwrap();
        snapshot(&a, Provider::Codex, "second", Some("elide"), &root).unwrap();

        // The two `a` snapshots may share a timestamp; last appended wins.
        let latest = latest_for(&a, &root).unwrap().unwrap();
        assert_eq!(latest.session_id, "second");
        assert_eq!(latest.strategy.as_deref(), Some("elide"));

        // Path equality is exact; other files resolve independently.
        assert_eq!(latest_for(&b, &root).unwrap().unwrap().session_id, "b");
        assert!(latest_for(&dir.0.join("never.jsonl"), &root)
            .unwrap()
            .is_none());
    }

    #[test]
    fn identical_content_dedups_object_but_appends_index() {
        let dir = TestDir::new();
        let root = dir.0.join("vault");
        let src = dir.0.join("s.jsonl");
        fs::write(&src, b"same bytes").unwrap();

        let e1 = snapshot(&src, Provider::ClaudeCode, "s1", None, &root).unwrap();
        let e2 = snapshot(&src, Provider::ClaudeCode, "s2", None, &root).unwrap();
        assert_eq!(e1.sha256, e2.sha256);

        // Identical chunks are deduplicated across both snapshots.
        assert_eq!(fs::read_dir(chunks_dir(&root)).unwrap().count(), 1);
        assert_eq!(fs::read_dir(manifests_dir(&root)).unwrap().count(), 1);
        assert_eq!(list(&root).unwrap().len(), 2);
    }

    #[test]
    fn list_is_newest_first_and_rejects_incomplete_history() {
        let dir = TestDir::new();
        let root = dir.0.join("vault");
        let src = dir.0.join("s.jsonl");
        fs::write(&src, b"one").unwrap();
        snapshot(&src, Provider::Codex, "first", None, &root).unwrap();
        fs::write(&src, b"two!").unwrap();
        snapshot(&src, Provider::Codex, "second", None, &root).unwrap();

        let entries = list(&root).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].session_id, "second");
        assert_eq!(entries[1].session_id, "first");

        // A damaged history must never look like a complete partial history.
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(index_path(&root))
            .unwrap();
        f.write_all(b"{not json}\n").unwrap();
        drop(f);

        fs::write(&src, b"three").unwrap();
        let damaged = fs::read(index_path(&root)).unwrap();
        assert!(snapshot(&src, Provider::Codex, "third", None, &root).is_err());
        assert!(prune(&root, 0, false).is_err());
        assert_eq!(fs::read(index_path(&root)).unwrap(), damaged);

        assert!(list(&root).is_err());
        assert!(Reader::open(&root).unwrap().entries().is_err());
        assert_eq!(fs::read(index_path(&root)).unwrap(), damaged);
    }

    #[test]
    fn stable_custody_excludes_prune_until_snapshot_and_reader_finish() {
        let dir = TestDir::new();
        let root = dir.0.join("vault");
        fs::create_dir(&root).unwrap();
        let snapshot_custody = Custody::shared(&root).unwrap();
        let reader_custody = Custody::shared(&root).unwrap();
        let prune_lock = fs::File::open(&root).unwrap();
        assert!(fs2::FileExt::try_lock_exclusive(&prune_lock).is_err());
        drop(snapshot_custody);
        assert!(fs2::FileExt::try_lock_exclusive(&prune_lock).is_err());
        drop(reader_custody);
        fs2::FileExt::try_lock_exclusive(&prune_lock).unwrap();
        let late_reader = fs::File::open(&root).unwrap();
        assert!(fs2::FileExt::try_lock_shared(&late_reader).is_err());
        drop(prune_lock);
        fs2::FileExt::try_lock_shared(&late_reader).unwrap();
    }

    #[test]
    fn prune_keeps_last_appended_snapshot_when_timestamps_tie() {
        let dir = TestDir::new();
        let root = dir.0.join("vault");
        let src = dir.0.join("s.jsonl");
        let first = snapshot_data(b"first", &src, Provider::Codex, "s", None, &root).unwrap();
        let second = snapshot_data(b"second", &src, Provider::Codex, "s", None, &root).unwrap();
        let mut entries = [first, second];
        // Append the larger hash last so lexicographic hash ordering
        // would deterministically retain the wrong version.
        entries.sort_by(|a, b| a.sha256.cmp(&b.sha256));
        for entry in &mut entries {
            entry.ts = 42;
        }
        let index = entries
            .iter()
            .map(|entry| format!("{}\n", serde_json::to_string(entry).unwrap()))
            .collect::<String>();
        fs::write(index_path(&root), index).unwrap();
        prune(&root, 1, false).unwrap();
        let kept = list(&root).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].sha256, entries[1].sha256);
        assert!(read_object(&kept[0].sha256, &root).is_ok());
    }

    #[test]
    fn prune_refuses_corrupt_retained_manifest_without_deleting_anything() {
        let dir = TestDir::new();
        let root = dir.0.join("vault");
        let src = dir.0.join("s.jsonl");
        let entry = snapshot_data(b"retained", &src, Provider::Codex, "s", None, &root).unwrap();
        let before_index = fs::read(index_path(&root)).unwrap();
        let chunks_before: Vec<_> = fs::read_dir(chunks_dir(&root))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        fs::write(manifests_dir(&root).join(entry.sha256), b"{broken").unwrap();
        assert!(prune(&root, 1, false).is_err());
        assert_eq!(fs::read(index_path(&root)).unwrap(), before_index);
        assert!(chunks_before.iter().all(|path| path.exists()));
    }

    #[test]
    fn legacy_reads_and_dry_run_create_no_control_files() {
        let dir = TestDir::new();
        let root = dir.0.join("vault");
        let objects = objects_dir(&root);
        fs::create_dir_all(&objects).unwrap();
        let bytes = b"legacy snapshot";
        let sha = sha256_hex(bytes);
        fs::write(objects.join(&sha), bytes).unwrap();
        let children = || {
            let mut names: Vec<_> = fs::read_dir(&root)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            names
        };
        let before = children();
        assert_eq!(read_object(&sha, &root).unwrap(), bytes);
        prune(&root, 1, true).unwrap();
        assert_eq!(children(), before);
        assert!(!index_path(&root).exists());
        assert_eq!(fs::read(objects.join(sha)).unwrap(), bytes);
    }

    #[test]
    fn prune_empty_vault_is_a_noop() {
        let dir = TestDir::new();
        let root = dir.0.join("vault");
        let report = prune(&root, 1, false).unwrap();
        assert_eq!(report.dropped_entries, 0);
        assert_eq!(report.chunks_removed, 0);
        assert!(!index_path(&root).exists());
        assert!(!root.exists());
    }

    #[test]
    fn prune_keeps_newest_per_stream_and_drops_unreachable_objects() {
        let dir = TestDir::new();
        let root = dir.0.join("vault");
        let src = dir.0.join("s.jsonl");
        // Three versions of one stream plus a second stream's snapshot.
        for body in [b"v1".as_slice(), b"v2", b"v3"] {
            fs::write(&src, body).unwrap();
            snapshot(&src, Provider::Codex, "stream-a", None, &root).unwrap();
            // Distinct second-resolution timestamps keep ordering honest.
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
        let other = dir.0.join("other.jsonl");
        fs::write(&other, b"other stream").unwrap();
        snapshot(&other, Provider::Codex, "stream-b", None, &root).unwrap();
        assert_eq!(list(&root).unwrap().len(), 4);

        // Dry-run plans without touching anything.
        let plan = prune(&root, 1, true).unwrap();
        assert_eq!(plan.dropped_entries, 2);
        assert_eq!(plan.kept_entries, 2);
        assert!(plan.chunks_removed > 0);
        assert_eq!(list(&root).unwrap().len(), 4);

        let report = prune(&root, 1, false).unwrap();
        assert_eq!(report.dropped_entries, 2);
        let entries = list(&root).unwrap();
        assert_eq!(entries.len(), 2);
        let mut ids: Vec<&str> = entries.iter().map(|e| e.session_id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["stream-a", "stream-b"]);
        // The stream-a survivor is the newest version, byte-exact.
        let survivor = entries.iter().find(|e| e.session_id == "stream-a").unwrap();
        let out = dir.0.join("restored.jsonl");
        restore(&survivor.sha256, &out, &root).unwrap();
        assert_eq!(fs::read(&out).unwrap(), b"v3");
        // A second pass is a no-op.
        assert_eq!(prune(&root, 1, false).unwrap().dropped_entries, 0);
    }

    #[test]
    fn prune_preserves_a_manifest_shared_by_kept_and_dropped_entries() {
        let dir = TestDir::new();
        let root = dir.0.join("vault");
        // Same bytes snapshotted under two streams: one manifest, two
        // index entries. Dropping one stream's entry must keep the
        // manifest and chunks alive for the survivor.
        let a = dir.0.join("a.jsonl");
        let b = dir.0.join("b.jsonl");
        fs::write(&a, b"shared").unwrap();
        fs::write(&b, b"shared").unwrap();
        snapshot(&a, Provider::Codex, "a", None, &root).unwrap();
        snapshot(&b, Provider::Codex, "b", None, &root).unwrap();
        fs::write(&a, b"shared-new").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        snapshot(&a, Provider::Codex, "a", None, &root).unwrap();

        let report = prune(&root, 1, false).unwrap();
        // Only stream-a's older index entry is dropped — the shared
        // manifest stays referenced by stream-b's kept entry.
        assert_eq!(report.dropped_entries, 1);
        assert_eq!(report.manifests_removed, 0);
        assert_eq!(report.chunks_removed, 0);
        assert_eq!(fs::read_dir(manifests_dir(&root)).unwrap().count(), 2);
        assert_eq!(fs::read_dir(chunks_dir(&root)).unwrap().count(), 2);
        let entries = list(&root).unwrap();
        assert_eq!(entries.len(), 2);
        let out = dir.0.join("r.jsonl");
        restore(&entries[0].sha256, &out, &root).unwrap();
    }

    #[test]
    fn default_root_ends_with_vault() {
        assert!(default_root().ends_with("gobstopper/vault"));
    }
}
