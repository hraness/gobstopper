//! gobstopper-adapters: provider transcript dialects and session discovery.
//!
//! This crate is the only component allowed to parse provider session
//! files. Oompa's own rule — never parse provider transcripts — is
//! preserved by integrating through the CLI's JSON surface instead of
//! linking this crate's file logic into oompa itself.

pub mod claude;
pub mod codex;
pub mod codex_compact;
pub mod copy;
pub mod detect;
pub mod eval;
pub mod fork;
mod payload;
pub mod plugins;
pub mod recovery;
pub mod request;
pub mod study;
pub mod transaction;
pub mod vault;
pub mod verify;

pub use detect::{discover, Discovered, Roots};

#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("direct provider mutation is disabled; use a verified copy or provider-owned control")]
    DirectMutationDisabled,
    #[error("publication failed during {stage}: visibility={visibility:?}, durability={durability:?}: {source}")]
    Publication {
        path: PathBuf,
        expected_sha256: String,
        stage: &'static str,
        visibility: transaction::Visibility,
        durability: transaction::Durability,
        #[source]
        source: std::io::Error,
    },
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("transcript at {path} changed while it was being rewritten; aborted rather than clobber provider writes")]
    ChangedDuringWrite { path: PathBuf },
    #[error("unsupported provider for this operation")]
    UnsupportedProvider,
    #[error("invalid edit: {0}")]
    InvalidEdit(&'static str),
}

/// Replace `path` with `new_content` via a tmp file + atomic rename, but
/// only while the file still holds exactly `original`. Providers append
/// to live transcripts; a write landing between our read and the rename
/// would be silently lost, and the vault snapshot taken beforehand cannot
/// contain it. Re-reading before the rename shrinks the race window to
/// the rename syscall itself.
#[cfg(test)]
pub(crate) fn write_if_unchanged(
    path: &Path,
    original: &[u8],
    new_content: &str,
) -> Result<(), AdapterError> {
    transaction::replace(path, original, new_content.as_bytes())
}

/// A bounded tail plus whether it contains the complete file. `None` is an
/// unavailable/invalid read, distinct from a valid empty file.
pub(crate) fn tail_records(
    path: &std::path::Path,
    limit: u64,
) -> Option<(Vec<serde_json::Value>, bool)> {
    use std::io::{Read, Seek, SeekFrom};
    if limit == 0 || limit > transaction::max_transcript_bytes() {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    if !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {
        return None;
    }
    let mut file = options.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let offset = meta.len().saturating_sub(limit);
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut bytes = Vec::new();
    (&mut file).take(limit).read_to_end(&mut bytes).ok()?;
    let complete = offset == 0
        && bytes.len() as u64 == meta.len()
        && file.metadata().ok()?.len() == meta.len();
    let start = if offset == 0 {
        0
    } else {
        bytes
            .iter()
            .position(|b| *b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(bytes.len())
    };
    let mut records = Vec::new();
    for line in bytes[start..].split(|b| *b == b'\n') {
        let raw = std::str::from_utf8(line).ok()?;
        if raw.trim().is_empty() {
            continue;
        }
        let record = payload::decode_record(raw).ok()?;
        if !record.is_object() || records.len() >= gobstopper_core::validation::MAX_ITEMS {
            return None;
        }
        records.push(record);
    }
    Some((records, complete))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gobstopper-rw-{}-{}-{tag}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_if_unchanged_swaps_when_source_is_stable() {
        let dir = tmpdir("stable");
        let path = dir.join("session.jsonl");
        fs::write(&path, "original\n").unwrap();
        write_if_unchanged(&path, b"original\n", "rewritten\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "rewritten\n");
        assert!(!path.with_extension("jsonl.gobstopper-tmp").exists());
    }

    #[test]
    fn write_if_unchanged_aborts_when_provider_appended() {
        let dir = tmpdir("raced");
        let path = dir.join("session.jsonl");
        fs::write(&path, "original\n").unwrap();
        // The provider appended a line after our read: the rewrite must
        // not land, and the appended bytes must survive.
        fs::write(&path, "original\nprovider-append\n").unwrap();
        let result = write_if_unchanged(&path, b"original\n", "rewritten\n");
        assert!(matches!(
            result,
            Err(AdapterError::ChangedDuringWrite { .. })
        ));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "original\nprovider-append\n"
        );
        assert!(!path.with_extension("jsonl.gobstopper-tmp").exists());
    }
}

#[cfg(test)]
mod storage_tests;
