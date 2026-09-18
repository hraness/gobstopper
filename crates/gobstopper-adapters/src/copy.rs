use crate::{claude, codex, codex_compact, fork, transaction, vault, verify};
use anyhow::{bail, Context};
use gobstopper_core::plan::DigestBlock;
use gobstopper_core::{CompactionPlan, Edit, Provider, SessionHandle, Transcript};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// Response items carried verbatim into a custom `compacted` record's
/// `replacement_history` after the digest — the recent window the resumed
/// session still sees word-for-word.
pub const COMPACTED_KEEP_TAIL: usize = 24;

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn load_bound(handle: SessionHandle) -> anyhow::Result<(Transcript, String)> {
    let bytes = transaction::read(&handle.path)?;
    let transcript = match handle.provider {
        Provider::Codex => codex::load_bytes(handle, &bytes)?,
        Provider::ClaudeCode => claude::load_bytes(handle, &bytes)?,
    };
    Ok((transcript, sha256(&bytes)))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyReceipt {
    pub schema_version: u32,
    pub source_sha256: String,
    pub output_sha256: String,
    pub session_id: String,
    pub path: PathBuf,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub reclaimed_bytes: u64,
    pub snapshot_sha256: String,
    pub completed: bool,
}

pub fn compact(
    handle: &SessionHandle,
    source_sha256: &str,
    plan: &CompactionPlan,
    vault_root: &Path,
) -> anyhow::Result<CopyReceipt> {
    if plan
        .edits
        .iter()
        .any(|e| matches!(e, Edit::ProviderCompact { .. } | Edit::CacheEdit { .. }))
    {
        bail!("copy plans cannot contain provider controls");
    }
    let original = transaction::read(&handle.path)?;
    if sha256(&original) != source_sha256 {
        bail!("source changed since planning");
    }
    let source_path = handle.path.canonicalize()?;
    let identity = sha256(&serde_json::to_vec(&(
        handle.provider,
        &source_path,
        source_sha256,
        &plan.edits,
    ))?);
    let id = format!(
        "{}-{}-4{}-a{}-{}",
        &identity[..8],
        &identity[8..12],
        &identity[13..16],
        &identity[17..20],
        &identity[20..32]
    );
    let operations = vault_root.join("operations");
    transaction::private_dir(vault_root)?;
    transaction::private_dir(&operations)?;
    let lock_path = operations.join(format!("{identity}.lock"));
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    if fs::symlink_metadata(&lock_path).is_ok_and(|m| !m.is_file()) {
        bail!("invalid operation lock");
    }
    let lock = options.open(&lock_path)?;
    fs2::FileExt::try_lock_exclusive(&lock).context("copy operation is already running")?;
    let receipt_path = operations.join(format!("{identity}.json"));
    if receipt_path.exists() {
        let raw = transaction::read(&receipt_path)?;
        let mut receipt: CopyReceipt = serde_json::from_slice(&raw)?;
        if receipt.schema_version != 1
            || receipt.source_sha256 != source_sha256
            || receipt.session_id != id
            || receipt.path != fork::target_path(handle.provider, &handle.path, &id)
            || receipt.snapshot_sha256 != source_sha256
            || receipt.reclaimed_bytes != receipt.bytes_before.saturating_sub(receipt.bytes_after)
        {
            bail!("copy receipt identity mismatch");
        }
        if transaction::read(&receipt.path)
            .map(|b| sha256(&b) == receipt.output_sha256)
            .unwrap_or(false)
        {
            if !receipt.completed {
                receipt.completed = true;
                transaction::replace(&receipt_path, &raw, &serde_json::to_vec(&receipt)?)?;
            }
            return Ok(receipt);
        }
        bail!("prior copy operation requires recovery; refusing speculative replay");
    }
    let snapshot = vault::snapshot(
        &handle.path,
        handle.provider,
        &handle.session_id,
        Some(&plan.strategy),
        vault_root,
    )?;
    if snapshot.source_sha256 != source_sha256 {
        bail!("source changed before snapshot");
    }
    let parent = handle.path.parent().unwrap_or_else(|| Path::new("."));
    let temp = transaction::Temporary::new(parent, &original)?;
    match handle.provider {
        Provider::Codex => codex::apply(&temp.path, &plan.edits)?,
        Provider::ClaudeCode => claude::apply(&temp.path, &plan.edits)?,
    };
    let candidate = transaction::read(&temp.path)?;
    if candidate.len() >= original.len() {
        bail!("compaction must reduce actual transcript bytes");
    }
    if !verify::verify(handle.provider, &candidate).is_empty() {
        bail!("candidate is not structurally clean");
    }
    let output = fork::rewrite_identity(handle.provider, std::str::from_utf8(&candidate)?, &id);
    let path = fork::target_path(handle.provider, &handle.path, &id);
    let bytes_after = output.len() as u64;
    if bytes_after >= original.len() as u64 {
        bail!("fork identity overhead exceeds savings");
    }
    let mut receipt = CopyReceipt {
        schema_version: 1,
        source_sha256: source_sha256.into(),
        output_sha256: sha256(output.as_bytes()),
        session_id: id,
        path,
        bytes_before: original.len() as u64,
        bytes_after,
        reclaimed_bytes: original.len() as u64 - bytes_after,
        snapshot_sha256: snapshot.source_sha256,
        completed: false,
    };
    let intent = serde_json::to_vec(&receipt)?;
    transaction::publish_new(&receipt_path, &intent)?;
    if transaction::read(&handle.path)? != original {
        bail!("source changed before publication");
    }
    transaction::publish_new(&receipt.path, output.as_bytes())?;
    receipt.completed = true;
    transaction::replace(&receipt_path, &intent, &serde_json::to_vec(&receipt)?)?;
    Ok(receipt)
}

/// Codex-only experimental path: a plan carrying an `InjectDigest` edit is
/// lowered to a real `compacted` record whose `replacement_history` is the
/// digest plus the newest `keep_tail` verbatim response items. On resume the
/// provider swaps that history in — the file-level savings are therefore the
/// superseded window, not appended bytes, so this fork may be *larger* than
/// the source. The receipt still records exact file bytes for audit; context
/// savings live in the plan's token fields.
///
/// Any `Elide` edits in the same plan still apply to the pre-compaction
/// lines first — they shrink the file and cannot hurt the provider's view,
/// which is the `replacement_history` plus post-record lines.
pub fn compact_via_compacted(
    handle: &SessionHandle,
    source_sha256: &str,
    plan: &CompactionPlan,
    digest: &DigestBlock,
    keep_tail: usize,
    vault_root: &Path,
) -> anyhow::Result<CopyReceipt> {
    if handle.provider != Provider::Codex {
        bail!("custom compacted records are only defined for Codex rollouts");
    }
    if plan
        .edits
        .iter()
        .any(|e| matches!(e, Edit::ProviderCompact { .. } | Edit::CacheEdit { .. }))
    {
        bail!("compacted-record plans cannot contain provider controls");
    }
    let file_edits: Vec<Edit> = plan
        .edits
        .iter()
        .filter(|e| !matches!(e, Edit::InjectDigest { .. }))
        .cloned()
        .collect();
    let original = transaction::read(&handle.path)?;
    if sha256(&original) != source_sha256 {
        bail!("source changed since planning");
    }
    let source_path = handle.path.canonicalize()?;
    let identity = sha256(&serde_json::to_vec(&(
        handle.provider,
        &source_path,
        source_sha256,
        &plan.edits,
        "compacted",
    ))?);
    let id = format!(
        "{}-{}-4{}-a{}-{}",
        &identity[..8],
        &identity[8..12],
        &identity[13..16],
        &identity[17..20],
        &identity[20..32]
    );
    let operations = vault_root.join("operations");
    transaction::private_dir(vault_root)?;
    transaction::private_dir(&operations)?;
    let lock_path = operations.join(format!("{identity}.lock"));
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    if fs::symlink_metadata(&lock_path).is_ok_and(|m| !m.is_file()) {
        bail!("invalid operation lock");
    }
    let lock = options.open(&lock_path)?;
    fs2::FileExt::try_lock_exclusive(&lock).context("copy operation is already running")?;
    let receipt_path = operations.join(format!("{identity}.json"));
    let target = fork::target_path(handle.provider, &handle.path, &id);
    if receipt_path.exists() {
        let raw = transaction::read(&receipt_path)?;
        let mut receipt: CopyReceipt = serde_json::from_slice(&raw)?;
        if receipt.schema_version != 1
            || receipt.source_sha256 != source_sha256
            || receipt.session_id != id
            || receipt.path != target
            || receipt.snapshot_sha256 != source_sha256
        {
            bail!("copy receipt identity mismatch");
        }
        if transaction::read(&receipt.path)
            .map(|b| sha256(&b) == receipt.output_sha256)
            .unwrap_or(false)
        {
            if !receipt.completed {
                receipt.completed = true;
                transaction::replace(&receipt_path, &raw, &serde_json::to_vec(&receipt)?)?;
            }
            return Ok(receipt);
        }
        bail!("prior copy operation requires recovery; refusing speculative replay");
    }
    let snapshot = vault::snapshot(
        &handle.path,
        handle.provider,
        &handle.session_id,
        Some(&plan.strategy),
        vault_root,
    )?;
    if snapshot.source_sha256 != source_sha256 {
        bail!("source changed before snapshot");
    }
    let parent = handle.path.parent().unwrap_or_else(|| Path::new("."));
    let temp = transaction::Temporary::new(parent, &original)?;
    if !file_edits.is_empty() {
        codex::apply(&temp.path, &file_edits)?;
    }
    // Build and append the compacted record against the post-edit temp so
    // `ordinal` is the appended line index and the tail carried into
    // `replacement_history` reflects the same bytes the fork ships.
    let lines = codex_compact::read_rollout_lines(&temp.path)?;
    let tail = codex_compact::tail_response_items(&lines, keep_tail);
    let history = codex_compact::digest_to_replacement_history(digest, &tail);
    let record_line = codex_compact::build_compacted_record(&lines, history)?;
    codex_compact::append_compacted(&temp.path, &record_line)?;
    let candidate = transaction::read(&temp.path)?;
    if !verify::verify(handle.provider, &candidate).is_empty() {
        bail!("candidate is not structurally clean");
    }
    let output = fork::rewrite_identity(handle.provider, std::str::from_utf8(&candidate)?, &id);
    let bytes_after = output.len() as u64;
    let mut receipt = CopyReceipt {
        schema_version: 1,
        source_sha256: source_sha256.into(),
        output_sha256: sha256(output.as_bytes()),
        session_id: id,
        path: target,
        bytes_before: original.len() as u64,
        bytes_after,
        reclaimed_bytes: original.len().saturating_sub(output.len() as usize) as u64,
        snapshot_sha256: snapshot.source_sha256,
        completed: false,
    };
    let intent = serde_json::to_vec(&receipt)?;
    transaction::publish_new(&receipt_path, &intent)?;
    if transaction::read(&handle.path)? != original {
        bail!("source changed before publication");
    }
    transaction::publish_new(&receipt.path, output.as_bytes())?;
    receipt.completed = true;
    transaction::replace(&receipt_path, &intent, &serde_json::to_vec(&receipt)?)?;
    Ok(receipt)
}
