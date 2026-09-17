//! Content-addressed snapshot vault: gobstopper's "undo a compaction".
//!
//! Before a rewrite touches a transcript, the caller snapshots it here;
//! [`restore`] puts the exact bytes back. Objects live at
//! `objects/<sha256>` deduplicated by content digest, and `index.jsonl`
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
            fs2::FileExt::try_lock_shared(&f)?;
            f
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("open {}", index.display())),
    };
    let mut entries = Vec::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<VaultEntry>(&line) {
            entries.push(entry);
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
    fs2::FileExt::try_lock_exclusive(&file)?;
    let mut line = serde_json::to_string(entry).context("serialize vault entry")?;
    line.push('\n');
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
/// The transcript is split into record-addressed storage: each line is
/// hashed and stored once under `records/<sha>`, and a small manifest
/// listing the record hashes is stored under `manifests/<sha>`. Identical
/// records are shared across sessions and versions (a Merkle-style DAG);
/// only the manifest and index are duplicated per snapshot. `path` is
/// stored verbatim — callers should pass an absolute path, and lookups
/// must match it verbatim.
pub fn snapshot(
    path: &Path,
    provider: Provider,
    session_id: &str,
    strategy: Option<&str>,
    root: &Path,
) -> anyhow::Result<VaultEntry> {
    let data = crate::transaction::read(path)?;
    let source_sha256 = sha256_hex(&data);
    let trailing_newline = data.last() == Some(&b'\n');

    // Split into JSONL records. Empty segments from leading/trailing
    // newlines are ignored; the trailing newline flag is tracked
    // separately so the exact original bytes can be reconstructed.
    let raw_lines: Vec<&[u8]> = data.split(|&b| b == b'\n').collect();
    let mut lines = Vec::with_capacity(raw_lines.len());
    for line in raw_lines {
        if !line.is_empty() {
            lines.push(line);
        }
    }

    crate::transaction::private_dir(root)?;
    let records = records_dir(root);
    let manifests = manifests_dir(root);
    crate::transaction::private_dir(&records)?;
    crate::transaction::private_dir(&manifests)?;

    let mut record_hashes = Vec::with_capacity(lines.len());
    for line in lines {
        let record_sha = sha256_hex(line);
        let record_path = records.join(&record_sha);
        if fs::symlink_metadata(&record_path).is_err() {
            match crate::transaction::publish_new(&record_path, line) {
                Ok(()) => {}
                Err(crate::AdapterError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }
        if crate::transaction::read(&record_path)? != line {
            bail!("record object {record_sha} failed integrity verification");
        }
        record_hashes.push(record_sha);
    }

    let manifest = serde_json::json!({
        "records": record_hashes,
        "trailing_newline": trailing_newline,
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

    let entry = VaultEntry {
        ts: now_secs(),
        sha256: manifest_sha,
        path: path.to_path_buf(),
        session_id: session_id.to_string(),
        provider,
        bytes: data.len() as u64,
        record_count: record_hashes.len() as u64,
        source_sha256,
        strategy: strategy.map(str::to_string),
    };
    append_index(root, &entry)?;
    Ok(entry)
}

/// Restore the snapshotted bytes for `sha256` over `target`, atomically:
/// write `<target>.gobstopper-restore-<pid>` then rename.
///
/// For record-addressed snapshots the manifest is verified and the
/// individual records are re-hashed before concatenation. Legacy full-byte
/// objects are also supported for snapshots created before the
/// record-addressed change.
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

    if target.exists() {
        let before = crate::transaction::read(target)?;
        crate::transaction::replace(target, &before, &data)?;
    } else {
        crate::transaction::publish_new(target, &data)?;
    }
    Ok(entry)
}

/// Read a manifest and concatenate its records back into the original
/// transcript bytes.
fn reconstruct_manifest(manifest_bytes: &[u8], root: &Path) -> anyhow::Result<Vec<u8>> {
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes)?;
    let records = manifest
        .get("records")
        .and_then(|v| v.as_array())
        .with_context(|| "missing records in manifest")?;
    let trailing = manifest
        .get("trailing_newline")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut out = Vec::with_capacity(records.len() * 256);
    for (i, r) in records.iter().enumerate() {
        let sha = r.as_str().with_context(|| "record hash is not a string")?;
        if !is_hex(sha) {
            bail!("invalid record hash in manifest: {sha:?}");
        }
        let record_path = records_dir(root).join(sha);
        let record = fs::read(&record_path)
            .with_context(|| format!("read vault record {}", record_path.display()))?;
        let actual = sha256_hex(&record);
        if actual != sha {
            bail!("record {sha} is corrupt: hashes to {actual}");
        }
        out.extend_from_slice(&record);
        if trailing || i + 1 < records.len() {
            out.push(b'\n');
        }
    }
    Ok(out)
}

/// The most recent snapshot taken of `path` (verbatim path match), if
/// any. Among entries sharing a timestamp the last-appended wins.
pub fn latest_for(path: &Path, root: &Path) -> anyhow::Result<Option<VaultEntry>> {
    Ok(read_index(root)?
        .into_iter()
        .filter(|e| e.path == path)
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
    Ok(list(root)?.into_iter().find(|entry| {
        entry.path == path
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
/// type for each side. Identical records are shared, so the diff is cheap:
/// only the manifest hash lists are compared.
pub fn diff(sha1: &str, sha2: &str, root: &Path) -> anyhow::Result<DiffSummary> {
    if !is_hex(sha1) || !is_hex(sha2) {
        bail!("invalid sha256 digest");
    }

    fn load_manifest(sha: &str, root: &Path) -> anyhow::Result<(Vec<String>, serde_json::Value)> {
        let p = manifests_dir(root).join(sha);
        let bytes = if p.exists() {
            crate::transaction::read(&p)?
        } else {
            // Legacy full-object snapshot: treat the whole transcript as one record.
            return Ok((vec![sha.to_string()], serde_json::Value::Null));
        };
        if sha256_hex(&bytes) != sha {
            bail!("manifest {sha} failed integrity verification");
        }
        let manifest: serde_json::Value = serde_json::from_slice(&bytes)?;
        let records = manifest
            .get("records")
            .and_then(|v| v.as_array())
            .context("missing records in manifest")?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .context("record hash is not a string")
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok((records, manifest))
    }

    let (a, _) = load_manifest(sha1, root)?;
    let (b, _) = load_manifest(sha2, root)?;

    let a_set: std::collections::HashSet<&str> = a.iter().map(String::as_str).collect();
    let b_set: std::collections::HashSet<&str> = b.iter().map(String::as_str).collect();

    let mut removed = Vec::new();
    let mut added = Vec::new();
    let mut type_summary_a = std::collections::BTreeMap::<String, usize>::new();
    let mut type_summary_b = std::collections::BTreeMap::<String, usize>::new();

    for h in &a {
        if !b_set.contains(h.as_str()) {
            removed.push(h.clone());
        }
        let record = read_record(h, root).unwrap_or_default();
        *type_summary_a.entry(record_type(&record)).or_default() += 1;
    }
    for h in &b {
        if !a_set.contains(h.as_str()) {
            added.push(h.clone());
        }
        let record = read_record(h, root).unwrap_or_default();
        *type_summary_b.entry(record_type(&record)).or_default() += 1;
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
    let bytes = fs::read(&p).with_context(|| format!("read record {}", p.display()))?;
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
/// given, is a case-insensitive substring matched against goal, decisions,
/// files, and open tasks. `sha` restricts the search to one snapshot.
/// Results are newest-first by snapshot timestamp, then by record order.
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
                &digest.decisions.join("\n"),
                &digest.files_touched.join("\n"),
                &digest.open_tasks.join("\n"),
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
/// bytes, if one exists. Claude carries it in a `user` message; Codex
/// carries it as the first `replacement_history` item of a `compacted`
/// record.
fn extract_digest_text(line: &[u8]) -> Option<String> {
    let record: serde_json::Value = serde_json::from_slice(line).ok()?;

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
        let original = b"{\"line\":1}\n{\"line\":2}\n";
        fs::write(&src, original).unwrap();

        let entry = snapshot(&src, Provider::ClaudeCode, "sess-1", Some("elide"), &root).unwrap();
        assert_eq!(entry.bytes, original.len() as u64);
        assert_eq!(entry.path, src);
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

        // Identical records are deduplicated across both snapshots.
        assert_eq!(fs::read_dir(records_dir(&root)).unwrap().count(), 1);
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
