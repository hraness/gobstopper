//! gobstopper-adapters: provider transcript dialects and session discovery.
//!
//! This crate is the only component allowed to parse provider session
//! files. Oompa's own rule — never parse provider transcripts — is
//! preserved by integrating through the CLI's JSON surface instead of
//! linking this crate's file logic into oompa itself.

pub mod claude;
pub mod codex;
pub mod codex_compact;
pub mod codex_history;
pub mod copy;
pub mod detect;
pub mod eval;
pub mod fork;
mod payload;
pub mod plugins;
pub mod recovery;
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

pub(crate) fn tail_records(path: &std::path::Path, limit: u64) -> Vec<serde_json::Value> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(meta) = file.metadata() else {
        return Vec::new();
    };
    let start = meta.len().saturating_sub(limit);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.take(limit).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let start = if start == 0 {
        0
    } else {
        bytes
            .iter()
            .position(|b| *b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(bytes.len())
    };
    bytes[start..]
        .split(|b| *b == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect()
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
