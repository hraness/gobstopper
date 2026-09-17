//! gobstopper-adapters: provider transcript dialects and session discovery.
//!
//! This crate is the only component allowed to parse provider session
//! files. Oompa's own rule — never parse provider transcripts — is
//! preserved by integrating through the CLI's JSON surface instead of
//! linking this crate's file logic into oompa itself.

pub mod claude;
pub mod codex;
pub mod codex_compact;
pub mod detect;
pub mod eval;
pub mod fork;
pub mod verify;
pub mod vault;

pub use detect::{discover, Discovered, Roots};

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

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
}

/// Replace `path` with `new_content` via a tmp file + atomic rename, but
/// only while the file still holds exactly `original`. Providers append
/// to live transcripts; a write landing between our read and the rename
/// would be silently lost, and the vault snapshot taken beforehand cannot
/// contain it. Re-reading before the rename shrinks the race window to
/// the rename syscall itself.
pub(crate) fn write_if_unchanged(
    path: &Path,
    original: &[u8],
    new_content: &str,
) -> Result<(), AdapterError> {
    let io = |e: std::io::Error| AdapterError::Io {
        path: path.to_path_buf(),
        source: e,
    };
    let tmp = path.with_extension("jsonl.gobstopper-tmp");
    fs::File::create(&tmp)
        .and_then(|mut f| f.write_all(new_content.as_bytes()))
        .map_err(|e| AdapterError::Io {
            path: tmp.clone(),
            source: e,
        })?;
    let now = fs::read(path).map_err(io)?;
    if now != original {
        let _ = fs::remove_file(&tmp);
        return Err(AdapterError::ChangedDuringWrite {
            path: path.to_path_buf(),
        });
    }
    fs::rename(&tmp, path).map_err(io)
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
