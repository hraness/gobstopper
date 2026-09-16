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

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("unsupported provider for this operation")]
    UnsupportedProvider,
}
