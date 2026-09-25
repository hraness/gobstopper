use crate::{claude, codex, codex_compact, fork, transaction, vault};
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

/// Version 2 binds the exact prepared output, not a recipe to rerun.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationIdentity {
    pub revision: u32,
    pub kind: String,
    pub provider: Provider,
    pub source_path: PathBuf,
    pub source_session_id: String,
    pub source_sha256: String,
    pub inputs_sha256: String,
    pub output_session_id: String,
}
impl OperationIdentity {
    fn id(&self) -> anyhow::Result<String> {
        Ok(sha256(&serde_json::to_vec(self)?))
    }
    fn target(&self) -> PathBuf {
        fork::target_path_bound(
            self.provider,
            &self.source_path,
            &self.source_session_id,
            &self.output_session_id,
        )
    }
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
    /// Legacy meaning: raw source-byte digest, never a manifest digest.
    pub snapshot_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_manifest_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_manifest_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<OperationIdentity>,
    pub completed: bool,
}

fn digest_valid(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn validate_receipt(receipt: &CopyReceipt, operation_id: &str) -> anyhow::Result<()> {
    if !digest_valid(operation_id)
        || !digest_valid(&receipt.source_sha256)
        || !digest_valid(&receipt.output_sha256)
        || receipt.snapshot_sha256 != receipt.source_sha256
        || receipt.bytes_before > transaction::max_transcript_bytes()
        || receipt.bytes_after > transaction::max_transcript_bytes()
        || receipt.reclaimed_bytes != receipt.bytes_before.saturating_sub(receipt.bytes_after)
        || !receipt.path.is_absolute()
    {
        bail!("invalid copy receipt; repair required");
    }
    fork::validate_session_id(&receipt.session_id)?;
    match receipt.schema_version {
        1 if receipt.operation.is_none() && receipt.output_manifest_sha256.is_none() => {}
        2 => {
            let op = receipt
                .operation
                .as_ref()
                .context("missing operation identity; repair required")?;
            if op.revision != 2
                || !matches!(
                    op.kind.as_str(),
                    "compact" | "compacted" | "fork" | "restore"
                )
                || !op.source_path.is_absolute()
                || op.source_session_id.is_empty()
                || op.source_session_id.len() > 256
                || !digest_valid(&op.inputs_sha256)
                || op.source_sha256 != receipt.source_sha256
                || op.output_session_id != receipt.session_id
                || op.target() != receipt.path
                || op.id()? != operation_id
                || receipt.snapshot_manifest_sha256.is_none()
                || receipt.output_manifest_sha256.is_none()
            {
                bail!("copy receipt identity mismatch; repair required");
            }
        }
        _ => bail!("unsupported operation receipt version; repair required"),
    }
    Ok(())
}

fn source_roots(
    source: &str,
    manifest: Option<&str>,
    entries: &[vault::VaultEntry],
    root: &Path,
) -> anyhow::Result<Vec<String>> {
    if !digest_valid(source) {
        bail!("invalid receipt source digest; repair required");
    }
    let mut roots = if let Some(manifest) = manifest {
        if !digest_valid(manifest) {
            bail!("invalid receipt manifest digest; repair required");
        }
        vec![manifest.to_string()]
    } else {
        let mut roots: Vec<_> = entries
            .iter()
            .filter(|e| e.source_sha256 == source || e.sha256 == source)
            .map(|e| e.sha256.clone())
            .collect();
        if root.join("objects").join(source).try_exists()? {
            roots.push(source.to_string());
        }
        roots.sort();
        roots.dedup();
        roots
    };
    if roots.is_empty() && manifest.is_none() {
        // A prior prune may have retired the legacy index row. Resolve its raw
        // source digest against verified immutable manifests, never filename guesses.
        let directory = root.join("manifests");
        if directory.try_exists()? {
            for entry in fs::read_dir(directory)? {
                let entry = entry?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("unknown legacy root; repair required"))?;
                if !entry.file_type()?.is_file() || !digest_valid(&name) {
                    bail!("unknown legacy root; repair required");
                }
                let raw = transaction::read_with_limit(&entry.path(), 16 * 1024 * 1024)?;
                if sha256(&raw) != name {
                    bail!("legacy root integrity failure; repair required");
                }
                let doc = crate::payload::decode_record(std::str::from_utf8(&raw)?)?;
                if (doc.get("source_sha256").is_none()
                    || doc.get("source_sha256").and_then(|v| v.as_str()) == Some(source))
                    && sha256(&vault::read_object_locked(&name, root)?) == source
                {
                    roots.push(name);
                }
            }
        }
    }
    if roots.is_empty() {
        bail!("legacy receipt recovery root is unresolved; repair required");
    }
    for digest in &roots {
        if sha256(&vault::read_object_locked(digest, root)?) != source {
            bail!("copy receipt recovery snapshot does not match source");
        }
    }
    roots.sort();
    roots.dedup();
    Ok(roots)
}

/// Strict operation-root decoder shared by recovery and collection. Every pin
/// is verified before deletion admission. Pins have no automatic expiry.
/// Legacy Devin store receipt kept only so vault recovery can still parse
/// receipts written before Devin support was removed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevinStoreReceipt {
    pub schema_version: u32,
    pub source_sha256: String,
    pub export_sha256: String,
    pub session_id: String,
    pub nodes_rewritten: u64,
    pub digest_node_id: Option<i64>,
    pub reclaimed_bytes: u64,
    pub snapshot_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_manifest_sha256: Option<String>,
    #[serde(default)]
    pub resume_hint: String,
}

pub(crate) fn recovery_roots_locked(
    raw: &[u8],
    operation_id: &str,
    entries: &[vault::VaultEntry],
    root: &Path,
) -> anyhow::Result<Vec<String>> {
    let value = crate::payload::decode_record(std::str::from_utf8(raw)?)
        .context("invalid operation receipt; repair required")?;
    if value.get("export_sha256").is_some() {
        let receipt: DevinStoreReceipt = serde_json::from_slice(raw)?;
        if receipt.schema_version != 1
            || !digest_valid(operation_id)
            || !digest_valid(&receipt.export_sha256)
            || receipt.snapshot_sha256 != receipt.source_sha256
            || receipt.session_id.is_empty()
            || receipt.session_id.len() > 256
        {
            bail!("invalid legacy store receipt; repair required");
        }
        return source_roots(
            &receipt.source_sha256,
            receipt.snapshot_manifest_sha256.as_deref(),
            entries,
            root,
        );
    }
    let receipt: CopyReceipt = serde_json::from_slice(raw)?;
    validate_receipt(&receipt, operation_id)?;
    let mut roots = source_roots(
        &receipt.source_sha256,
        receipt.snapshot_manifest_sha256.as_deref(),
        entries,
        root,
    )?;
    if vault::read_object_locked(&roots[0], root)?.len() as u64 != receipt.bytes_before {
        bail!("recovery snapshot byte count mismatch; repair required");
    }
    if let Some(output) = &receipt.output_manifest_sha256 {
        let bytes = vault::read_object_locked(output, root)?;
        if sha256(&bytes) != receipt.output_sha256 || bytes.len() as u64 != receipt.bytes_after {
            bail!("prepared output pin mismatch; repair required");
        }
        roots.push(output.clone());
    }
    Ok(roots)
}

fn lock_operation(root: &Path, identity: &str) -> anyhow::Result<fs::File> {
    transaction::private_dir(&root.join("operations"))?;
    let path = root.join("operations").join(format!("{identity}.lock"));
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        bail!("invalid operation lock");
    }
    fs2::FileExt::try_lock_exclusive(&file).context("copy operation is already running")?;
    Ok(file)
}

fn reconcile_locked(identity: &str, root: &Path) -> anyhow::Result<CopyReceipt> {
    let path = root.join("operations").join(format!("{identity}.json"));
    let raw = transaction::read_with_limit(&path, 64 * 1024)?;
    let mut receipt: CopyReceipt = serde_json::from_slice(&raw)?;
    validate_receipt(&receipt, identity)?;
    let entries = vault::entries_locked(root)?;
    recovery_roots_locked(&raw, identity, &entries, root)?;
    match transaction::read(&receipt.path) {
        Ok(bytes)
            if sha256(&bytes) == receipt.output_sha256
                && bytes.len() as u64 == receipt.bytes_after => {}
        Err(crate::AdapterError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            if receipt.completed {
                bail!("completed copy output is missing; repair required");
            }
            let output = receipt
                .output_manifest_sha256
                .as_deref()
                .context("legacy pending operation lacks exact prepared bytes; repair required")?;
            let bytes = vault::read_object_locked(output, root)?;
            transaction::publish_new(&receipt.path, &bytes)?;
        }
        _ => bail!("conflicting or unreadable copy output; repair required"),
    }
    if !receipt.completed {
        // A prior call may have linked the target but failed directory sync.
        // Reconfirm the bytes and sync before claiming durable completion.
        transaction::confirm_publication(&receipt.path, &receipt.output_sha256)?;
        transaction::checkpoint("output_published", &receipt.path, true)?;
        receipt.completed = true;
        transaction::replace(&path, &raw, &serde_json::to_vec(&receipt)?)?;
        transaction::checkpoint("receipt_completed", &path, true)?;
    }
    Ok(receipt)
}

/// Resume one recorded operation without rereading or modifying its source.
/// Missing or conflicting state requires repair; no transform is rerun.
pub fn recover_operation(identity: &str, root: &Path) -> anyhow::Result<CopyReceipt> {
    if !digest_valid(identity) {
        bail!("invalid operation identity");
    }
    let _custody = vault::Custody::shared(root)?;
    let _operation = lock_operation(root, identity)?;
    reconcile_locked(identity, root)
}

fn session_id(identity: &str) -> String {
    format!(
        "{}-{}-4{}-a{}-{}",
        &identity[..8],
        &identity[8..12],
        &identity[13..16],
        &identity[17..20],
        &identity[20..32]
    )
}

pub(crate) fn publish_prepared(
    op: OperationIdentity,
    original: &[u8],
    output: &[u8],
    strategy: Option<&str>,
    root: &Path,
) -> anyhow::Result<CopyReceipt> {
    transaction::private_dir(root)?;
    let _custody = vault::Custody::shared(root)?;
    let identity = op.id()?;
    let _operation = lock_operation(root, &identity)?;
    let path = root.join("operations").join(format!("{identity}.json"));
    if path.try_exists()? {
        let recorded: CopyReceipt =
            serde_json::from_slice(&transaction::read_with_limit(&path, 64 * 1024)?)?;
        if recorded.schema_version != 2 {
            bail!("unexpected legacy receipt identity; repair required");
        }
        return reconcile_locked(&identity, root);
    }
    if fs::symlink_metadata(op.target()).is_ok() {
        bail!("copy target already exists; no-clobber publication refused");
    }
    if sha256(original) != op.source_sha256 {
        bail!("prepared source binding mismatch");
    }
    if output.len() as u64 > transaction::max_transcript_bytes() {
        bail!("prepared output exceeds byte limit");
    }
    let snapshot = vault::snapshot_data(
        original,
        &op.source_path,
        op.provider,
        &op.source_session_id,
        strategy,
        root,
    )?;
    let output_manifest = vault::store_object_locked(output, root)?;
    let receipt = CopyReceipt {
        schema_version: 2,
        source_sha256: op.source_sha256.clone(),
        output_sha256: sha256(output),
        session_id: op.output_session_id.clone(),
        path: op.target(),
        bytes_before: original.len() as u64,
        bytes_after: output.len() as u64,
        reclaimed_bytes: original.len().saturating_sub(output.len()) as u64,
        snapshot_sha256: snapshot.source_sha256,
        snapshot_manifest_sha256: Some(snapshot.sha256),
        output_manifest_sha256: Some(output_manifest),
        operation: Some(op),
        completed: false,
    };
    validate_receipt(&receipt, &identity)?;
    transaction::publish_new(&path, &serde_json::to_vec(&receipt)?)?;
    transaction::checkpoint("intent_prepared", &path, true)?;
    reconcile_locked(&identity, root)
}

#[allow(clippy::too_many_arguments)]
fn compact_common<F>(
    handle: &SessionHandle,
    source: &str,
    plan: &CompactionPlan,
    kind: &str,
    effective: &[u8],
    legacy_identity: &str,
    root: &Path,
    transform: F,
) -> anyhow::Result<CopyReceipt>
where
    F: FnOnce(&[u8]) -> anyhow::Result<Vec<u8>>,
{
    if !digest_valid(source) {
        bail!("invalid source digest");
    }
    if plan
        .edits
        .iter()
        .any(|e| matches!(e, Edit::ProviderCompact { .. } | Edit::CacheEdit { .. }))
    {
        bail!("copy plans cannot contain provider controls");
    }
    let source_path = handle.path.canonicalize()?;
    transaction::private_dir(root)?;
    let _custody = vault::Custody::shared(root)?;
    // Legacy filenames are checked explicitly; unsupported pending receipts are
    // never silently replaced with a newly computed candidate.
    if root
        .join("operations")
        .join(format!("{legacy_identity}.json"))
        .try_exists()?
    {
        let _operation = lock_operation(root, legacy_identity)?;
        let raw = transaction::read_with_limit(
            &root
                .join("operations")
                .join(format!("{legacy_identity}.json")),
            64 * 1024,
        )?;
        let receipt: CopyReceipt = serde_json::from_slice(&raw)?;
        validate_receipt(&receipt, legacy_identity)?;
        if receipt.source_sha256 != source
            || receipt.session_id != session_id(legacy_identity)
            || receipt.path
                != fork::target_path_bound(
                    handle.provider,
                    &source_path,
                    &handle.session_id,
                    &receipt.session_id,
                )
        {
            bail!("legacy copy receipt identity mismatch");
        }
        return reconcile_locked(legacy_identity, root);
    }
    let inputs_sha256 = sha256(effective);
    let seed = sha256(&serde_json::to_vec(&(
        2,
        kind,
        handle.provider,
        &source_path,
        &handle.session_id,
        source,
        &inputs_sha256,
    ))?);
    let op = OperationIdentity {
        revision: 2,
        kind: kind.into(),
        provider: handle.provider,
        source_path,
        source_session_id: handle.session_id.clone(),
        source_sha256: source.into(),
        inputs_sha256,
        output_session_id: session_id(&seed),
    };
    let identity = op.id()?;
    if root
        .join("operations")
        .join(format!("{identity}.json"))
        .try_exists()?
    {
        let _operation = lock_operation(root, &identity)?;
        let recorded: CopyReceipt = serde_json::from_slice(&transaction::read_with_limit(
            &root.join("operations").join(format!("{identity}.json")),
            64 * 1024,
        )?)?;
        if recorded.schema_version != 2 {
            bail!("unexpected legacy receipt identity; repair required");
        }
        return reconcile_locked(&identity, root);
    }
    let original = transaction::read(&handle.path)?;
    if sha256(&original) != source {
        bail!("source changed since planning");
    }
    if fork::source_session_id(handle.provider, &original)? != handle.session_id {
        bail!("source metadata does not match the selected session");
    }
    let candidate = transform(&original)?;
    let output = transaction::prepare(handle.provider, &original, |_| {
        Ok(fork::rewrite_identity(
            handle.provider,
            std::str::from_utf8(&candidate)
                .map_err(|_| crate::AdapterError::InvalidEdit("candidate is not UTF-8"))?,
            &op.output_session_id,
        ))
    })?;
    if kind == "compact" && output.len() >= original.len() {
        bail!("compaction must reduce actual transcript bytes");
    }
    if transaction::read(&handle.path)? != original {
        bail!("source changed before publication");
    }
    publish_prepared(op, &original, &output, Some(&plan.strategy), root)
}

pub fn compact(
    handle: &SessionHandle,
    source_sha256: &str,
    plan: &CompactionPlan,
    vault_root: &Path,
) -> anyhow::Result<CopyReceipt> {
    let source_path = handle.path.canonicalize()?;
    let legacy = sha256(&serde_json::to_vec(&(
        handle.provider,
        &source_path,
        source_sha256,
        &plan.edits,
    ))?);
    compact_common(
        handle,
        source_sha256,
        plan,
        "compact",
        &serde_json::to_vec(&plan.edits)?,
        &legacy,
        vault_root,
        |bytes| {
            Ok(match handle.provider {
                Provider::Codex => codex::transform(bytes, &plan.edits)?,
                Provider::ClaudeCode => claude::transform(bytes, &plan.edits)?,
            })
        },
    )
}

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
    let source_path = handle.path.canonicalize()?;
    let legacy = sha256(&serde_json::to_vec(&(
        handle.provider,
        &source_path,
        source_sha256,
        &plan.edits,
        digest,
        keep_tail,
        "compacted",
    ))?);
    compact_common(
        handle,
        source_sha256,
        plan,
        "compacted",
        &serde_json::to_vec(&(&plan.edits, digest, keep_tail))?,
        &legacy,
        vault_root,
        |bytes| {
            let edits: Vec<_> = plan
                .edits
                .iter()
                .filter(|e| !matches!(e, Edit::InjectDigest { .. }))
                .cloned()
                .collect();
            let candidate = codex::transform(bytes, &edits)?;
            Ok(codex_compact::transform_with_digest(&candidate, digest, keep_tail)?.0)
        },
    )
}
