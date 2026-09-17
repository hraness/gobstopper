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
}

/// Production vault root: `$XDG_DATA_HOME/gobstopper/vault`, else
/// `~/.local/share/gobstopper/vault`.
pub fn default_root() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
        })
        .unwrap_or_default();
    base.join("gobstopper").join("vault")
}

fn objects_dir(root: &Path) -> PathBuf {
    root.join("objects")
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
        },
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
    #[cfg(unix)] {
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
        if last[0] != b'\n' { file.write_all(b"\n")?; }
    }
    file.write_all(line.as_bytes())?;
    file.sync_all()?;
    crate::transaction::sync_dir(root)?;
    Ok(())
}

/// Snapshot `path` into the vault and record it in the index.
///
/// The bytes are hashed once; identical content reuses the existing
/// object (dedup), while a fresh index line is always appended so
/// [`latest_for`] sees the newest snapshot. `path` is stored verbatim —
/// callers should pass an absolute path, and lookups must match it
/// verbatim.
pub fn snapshot(
    path: &Path,
    provider: Provider,
    session_id: &str,
    strategy: Option<&str>,
    root: &Path,
) -> anyhow::Result<VaultEntry> {
    let data = crate::transaction::read(path)?;
    let sha256 = sha256_hex(&data);

    crate::transaction::private_dir(root)?;
    let objects = objects_dir(root);
    crate::transaction::private_dir(&objects)?;
    let object_path = objects.join(&sha256);
    if fs::symlink_metadata(&object_path).is_err() {
        // Temp + rename so a killed snapshot never leaves a
        // half-written object behind a valid digest name.
        match crate::transaction::publish_new(&object_path, &data) {
            Ok(()) => {},
            Err(crate::AdapterError::Io { source, .. }) if source.kind() == std::io::ErrorKind::AlreadyExists => {},
            Err(e) => return Err(e.into()),
        }
    }
    if crate::transaction::read(&object_path)? != data {
        bail!("existing vault object failed integrity verification");
    }

    let entry = VaultEntry {
        ts: now_secs(),
        sha256,
        path: path.to_path_buf(),
        session_id: session_id.to_string(),
        provider,
        bytes: data.len() as u64,
        strategy: strategy.map(str::to_string),
    };
    append_index(root, &entry)?;
    Ok(entry)
}

/// Restore the snapshotted bytes for `sha256` over `target`, atomically:
/// write `<target>.gobstopper-restore-<pid>` then rename.
///
/// The stored object is re-hashed before anything is written; a digest
/// mismatch means vault corruption, and the restore refuses rather than
/// resurrecting bad bytes into a live transcript.
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

    let object_path = objects_dir(root).join(&entry.sha256);
    let data = fs::read(&object_path)
        .with_context(|| format!("read vault object {}", object_path.display()))?;
    let actual = sha256_hex(&data);
    if actual != entry.sha256 {
        bail!(
            "vault object {} is corrupt: content hashes to {actual}",
            entry.sha256
        );
    }

    if target.exists() {
        let before = crate::transaction::read(target)?;
        crate::transaction::replace(target, &before, &data)?;
    } else {
        crate::transaction::publish_new(target, &data)?;
    }
    Ok(entry)
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
    if sha256.len() != 64 || !is_hex(sha256) { bail!("invalid snapshot digest"); }
    let bytes = crate::transaction::read(&objects_dir(root).join(sha256))?;
    if sha256_hex(&bytes) != sha256 { bail!("vault object failed integrity verification"); }
    Ok(bytes)
}

pub fn latest_pre_compaction(path: &Path, root: &Path) -> anyhow::Result<Option<VaultEntry>> {
    Ok(list(root)?.into_iter().find(|entry| entry.path == path
        && !matches!(entry.strategy.as_deref(), Some("post-compact" | "pre-undo"))))
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
        assert!(objects_dir(&root).join(&entry.sha256).is_file());

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

        // Tamper with the stored object.
        fs::write(objects_dir(&root).join(&entry.sha256), b"tampered").unwrap();

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

        assert_eq!(fs::read_dir(objects_dir(&root)).unwrap().count(), 1);
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
