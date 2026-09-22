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
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const CHUNK_BYTES: usize = 1024 * 1024;
const MAX_INDEX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_INDEX_LINE_BYTES: usize = 64 * 1024;

/// One snapshot record in the vault index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultEntry {
    /// Unix seconds when the snapshot was taken.
    pub ts: u64,
    /// Hex sha256 of the snapshotted bytes; also the object file name.
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

/// Read the append-only ledger in append (oldest-first) order. A missing
/// index is an empty vault; blank and unparseable lines are skipped so a
/// torn write can never wedge restore.
fn read_index(root: &Path) -> anyhow::Result<Vec<VaultEntry>> {
    let index = index_path(root);
    let file = match fs::File::open(&index) {
        Ok(f) => {
            if f.metadata()?.len() > MAX_INDEX_BYTES {
                bail!("vault index exceeds byte limit");
            }
            fs2::FileExt::lock_shared(&f)?;
            f
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("open {}", index.display())),
    };
    let mut entries = Vec::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() || line.len() > MAX_INDEX_LINE_BYTES {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<VaultEntry>(&line) {
            if entry.sha256.len() == 64
                && is_hex(&entry.sha256)
                && (entry.source_sha256.is_empty()
                    || entry.source_sha256.len() == 64 && is_hex(&entry.source_sha256))
                && entry.session_id.len() <= 256
                && entry
                    .strategy
                    .as_ref()
                    .is_none_or(|value| value.len() <= 128)
                && entry.bytes <= crate::transaction::max_transcript_bytes()
                && entry.record_count <= gobstopper_core::validation::MAX_ITEMS as u64
            {
                entries.push(entry);
            }
        }
    }
    Ok(entries)
}

fn append_index(root: &Path, entry: &VaultEntry) -> anyhow::Result<()> {
    crate::transaction::private_dir(root)?;
    let index = index_path(root);
    if fs::symlink_metadata(&index).is_ok_and(|m| !m.is_file()) {
        bail!("vault index must be a regular file");
    }
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true).read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&index).context("open vault index")?;
    fs2::FileExt::lock_exclusive(&file)?;
    let mut line = serde_json::to_string(entry).context("serialize vault entry")?;
    line.push('\n');
    if line.len() > MAX_INDEX_LINE_BYTES
        || file.metadata()?.len().saturating_add(line.len() as u64 + 1) > MAX_INDEX_BYTES
    {
        bail!("vault index capacity exceeded");
    }
    use std::io::{Read, Seek, SeekFrom};
    if file.metadata()?.len() > 0 {
        file.seek(SeekFrom::End(-1))?;
        let mut last = [0];
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            file.write_all(b"\n")?;
        }
    }
    file.write_all(line.as_bytes())?;
    file.sync_all()?;
    crate::transaction::sync_dir(root)?;
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

    crate::transaction::private_dir(root)?;
    let chunks = chunks_dir(root);
    let manifests = manifests_dir(root);
    crate::transaction::private_dir(&chunks)?;
    crate::transaction::private_dir(&manifests)?;

    let mut chunk_hashes = Vec::with_capacity(data.len().div_ceil(CHUNK_BYTES));
    for chunk in data.chunks(CHUNK_BYTES) {
        let chunk_sha = sha256_hex(chunk);
        let chunk_path = chunks.join(&chunk_sha);
        if fs::symlink_metadata(&chunk_path).is_err() {
            match crate::transaction::publish_new(&chunk_path, chunk) {
                Ok(()) => {}
                Err(crate::AdapterError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }
        if crate::transaction::read(&chunk_path)? != chunk {
            bail!("vault chunk {chunk_sha} failed integrity verification");
        }
        chunk_hashes.push(chunk_sha);
    }

    let manifest = serde_json::json!({
        "schema_version": 3,
        "source_sha256": source_sha256.clone(),
        "bytes": data.len(),
        "chunks": chunk_hashes,
    });
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    let manifest_sha = sha256_hex(&manifest_bytes);
    let manifest_path = manifests.join(&manifest_sha);
    if fs::symlink_metadata(&manifest_path).is_err() {
        match crate::transaction::publish_new(&manifest_path, &manifest_bytes) {
            Ok(()) => {}
            Err(crate::AdapterError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    if crate::transaction::read(&manifest_path)? != manifest_bytes {
        bail!("manifest object {manifest_sha} failed integrity verification");
    }
    if reconstruct_manifest(&manifest_bytes, root)? != data {
        bail!("vault manifest does not reconstruct the source exactly");
    }

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

/// Restore the snapshotted bytes for `sha256` over `target`, atomically:
/// write `<target>.gobstopper-restore-<pid>` then rename.
///
/// Content-addressed manifests and their objects are re-hashed before
/// concatenation. Legacy record-addressed and full-byte objects remain
/// supported.
pub fn restore(sha256: &str, target: &Path, root: &Path) -> anyhow::Result<VaultEntry> {
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

    if target.exists() {
        let before = crate::transaction::read(target)?;
        crate::transaction::replace(target, &before, &data)?;
    } else {
        crate::transaction::publish_new(target, &data)?;
    }
    Ok(entry)
}

/// Read a manifest and concatenate its chunks or records back into the
/// original transcript bytes.
fn reconstruct_manifest(manifest_bytes: &[u8], root: &Path) -> anyhow::Result<Vec<u8>> {
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes)?;
    let version = manifest
        .get("schema_version")
        .and_then(serde_json::Value::as_u64);
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
            let chunk = crate::transaction::read(&chunks_dir(root).join(sha))?;
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
    if expected_source.is_some_and(|sha| sha256_hex(&out) != sha) {
        bail!("vault manifest source digest mismatch");
    }
    Ok(out)
}

/// The most recent snapshot taken of `path`, matching its canonical form
/// when available. Among entries sharing a timestamp the last-appended wins.
pub fn latest_for(path: &Path, root: &Path) -> anyhow::Result<Option<VaultEntry>> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    Ok(read_index(root)?
        .into_iter()
        .filter(|e| e.path == path || e.path == canonical)
        .max_by_key(|e| e.ts))
}

/// Every parseable index entry, newest first. Corrupt lines are skipped.
pub fn list(root: &Path) -> anyhow::Result<Vec<VaultEntry>> {
    let mut entries = read_index(root)?;
    // Reverse first so equal timestamps still order newest-append first
    // under the stable sort.
    entries.reverse();
    entries.sort_by_key(|e| std::cmp::Reverse(e.ts));
    Ok(entries)
}

pub fn read_object(sha256: &str, root: &Path) -> anyhow::Result<Vec<u8>> {
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

fn record_type(record: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(record)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str().map(str::to_string)))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Structural diff between two vault snapshots.
///
/// Returns added and removed record hash lists plus the record counts by
/// type for each side. New chunk manifests are reconstructed within the
/// transcript byte bound; legacy record manifests use their hash lists.
pub fn diff(sha1: &str, sha2: &str, root: &Path) -> anyhow::Result<DiffSummary> {
    if !is_hex(sha1) || !is_hex(sha2) {
        bail!("invalid sha256 digest");
    }

    fn load_records(sha: &str, root: &Path) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
        let manifest_path = manifests_dir(root).join(sha);
        if manifest_path.exists() {
            let manifest_bytes = crate::transaction::read(&manifest_path)?;
            if sha256_hex(&manifest_bytes) != sha {
                bail!("manifest {sha} failed integrity verification");
            }
            let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)?;
            if let Some(records) = manifest
                .get("records")
                .and_then(serde_json::Value::as_array)
            {
                if records.len() > gobstopper_core::validation::MAX_ITEMS {
                    bail!("vault manifest exceeds record limit");
                }
                return records
                    .iter()
                    .map(|value| {
                        let hash = value
                            .as_str()
                            .context("record hash is not a string")?
                            .to_string();
                        let bytes = read_record(&hash, root)?;
                        Ok((hash, bytes))
                    })
                    .collect();
            }
        }
        let data = read_object(sha, root)?;
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
    if !is_hex(sha) {
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
    let mut entries = list(root)?;
    if let Some(prefix) = sha {
        entries.retain(|e| e.sha256.starts_with(prefix));
    } else if session != "*" && !session.is_empty() {
        entries.retain(|e| {
            e.session_id.starts_with(session) || e.path.to_string_lossy().contains(session)
        });
    }
    entries.sort_by_key(|b| std::cmp::Reverse(b.ts));

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
        let data = read_object(&entry.sha256, root)?;
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
    let record: serde_json::Value = serde_json::from_slice(line).ok()?;

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
        let restored = restore(&entry.sha256, &src, &root).unwrap();
        assert_eq!(restored.sha256, entry.sha256);
        assert_eq!(fs::read(&src).unwrap(), original);

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
    fn list_is_newest_first_and_skips_bad_lines() {
        let dir = TestDir::new();
        let root = dir.0.join("vault");
        let src = dir.0.join("s.jsonl");
        fs::write(&src, b"one").unwrap();
        snapshot(&src, Provider::Codex, "first", None, &root).unwrap();
        fs::write(&src, b"two!").unwrap();
        snapshot(&src, Provider::Codex, "second", None, &root).unwrap();

        // A torn write mid-index must not wedge listing.
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(index_path(&root))
            .unwrap();
        f.write_all(b"{not json}\n").unwrap();
        drop(f);

        fs::write(&src, b"three").unwrap();
        snapshot(&src, Provider::Codex, "third", None, &root).unwrap();

        let entries = list(&root).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].session_id, "third");
        assert_eq!(entries[1].session_id, "second");
        assert_eq!(entries[2].session_id, "first");
    }

    #[test]
    fn default_root_ends_with_vault() {
        assert!(default_root().ends_with("gobstopper/vault"));
    }
}
