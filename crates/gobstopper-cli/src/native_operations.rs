//! Durable native-dispatch custody for cooperating Gobstopper processes.
//!
//! The root and its ancestors must remain owner-controlled. Advisory locks do
//! not lock a provider or coordinate installations using different data roots.
//! Records are append-only; damaged or exhausted logs require explicit repair.
//! In particular, neither time nor configuration changes clear uncertain work.

use anyhow::{bail, Context, Result};
use gobstopper_adapters::vault;
use gobstopper_core::SessionHandle;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 512 * 1024 * 1024;
const MAX_TARGETS: usize = 4096;
const REGISTRY: &str = "targets.jsonl";
static NEXT: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, thiserror::Error)]
#[error("native provider executable is unavailable before dispatch")]
struct ExecutableUnavailable;

#[derive(Debug, thiserror::Error)]
#[error(
    "native provider activation is unqualified for this artifact; use owner-directed compaction"
)]
struct ActivationUnqualified;

pub(super) fn activation_unqualified(error: &anyhow::Error) -> bool {
    error.downcast_ref::<ActivationUnqualified>().is_some()
}

pub(super) fn executable_unavailable(error: &anyhow::Error) -> bool {
    error.downcast_ref::<ExecutableUnavailable>().is_some()
}

/// A retained startup executable, not a version string or a newly replaced path.
/// Keep the descriptor so unlink/replacement cannot recycle its identity while
/// the daemon runs. This is an owner-controlled-filesystem assumption, not an
/// attestation of a process launched from an already replaced executable.
struct ArtifactIdentity {
    path: PathBuf,
    file: File,
    metadata: fs::Metadata,
    sha256: String,
}

fn same_artifact_metadata(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        left.is_file()
            && right.is_file()
            && left.dev() == right.dev()
            && left.ino() == right.ino()
            && left.len() == right.len()
            && left.mtime() == right.mtime()
            && left.mtime_nsec() == right.mtime_nsec()
            && left.ctime() == right.ctime()
            && left.ctime_nsec() == right.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        let _ = (left, right);
        false
    }
}

impl ArtifactIdentity {
    fn open(path: PathBuf) -> Result<Self> {
        let file = open_regular(&path, false, false)?;
        let metadata = file.metadata()?;
        if metadata.len() > MAX_BINARY_BYTES {
            bail!("running artifact exceeds identity hash bound");
        }
        let mut hash = Sha256::new();
        let mut limited = (&file).take(MAX_BINARY_BYTES + 1);
        let mut buffer = [0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let n = limited.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > MAX_BINARY_BYTES {
                bail!("running artifact grew beyond identity hash bound");
            }
            hash.update(&buffer[..n]);
        }
        let artifact = Self {
            path,
            file,
            metadata,
            sha256: format!("{:x}", hash.finalize()),
        };
        artifact.validate()?;
        Ok(artifact)
    }

    fn validate(&self) -> Result<()> {
        if !same_artifact_metadata(&self.metadata, &self.file.metadata()?)
            || !same_artifact_metadata(&self.metadata, &fs::symlink_metadata(&self.path)?)
        {
            bail!("running artifact identity changed or is unsupported; restart required");
        }
        Ok(())
    }
}

/// Hash once, then refuse stale provenance after an in-place write or path
/// replacement. Call again at each pass boundary and before recording a receipt.
pub(super) fn artifact_sha256() -> Result<&'static str> {
    static ARTIFACT: OnceLock<Option<ArtifactIdentity>> = OnceLock::new();
    let artifact = ARTIFACT
        .get_or_init(|| ArtifactIdentity::open(std::env::current_exe().ok()?).ok())
        .as_ref()
        .context("running artifact identity unavailable; restart required")?;
    artifact
        .validate()
        .map_err(|_| anyhow::anyhow!("running artifact identity changed; restart required"))?;
    Ok(&artifact.sha256)
}

pub(super) fn artifact_activation_status() -> &'static str {
    if cfg!(debug_assertions) {
        "isolated_fixtures_only"
    } else {
        "unqualified"
    }
}

/// Early eligibility is independent of snapshot/export and never authorizes a
/// dispatch. Released artifacts reject without touching the provider binary.
/// Exact target/binary, recovery and custody checks remain in `prepare`.
pub(super) fn check_artifact_activation(
    handle: &SessionHandle,
    provider_home: &Path,
    binary: Option<&Path>,
) -> Result<()> {
    #[cfg(debug_assertions)]
    if std::env::var_os("GOBSTOPPER_NATIVE_FIXTURE_ROOT").is_some()
        && matches!(handle.provider.as_str(), "codex" | "claude_code")
    {
        return check_activation(handle, provider_home, binary.ok_or(ExecutableUnavailable)?);
    }
    let _ = (handle, provider_home, binary);
    Err(ActivationUnqualified.into())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Target {
    provider: String,
    session_id: String,
    source: PathBuf,
    provider_home: PathBuf,
}

impl Target {
    fn key(&self) -> Result<String> {
        Ok(sha(&serde_json::to_vec(self)?))
    }
    fn valid(&self) -> bool {
        matches!(self.provider.as_str(), "codex" | "claude_code")
            && !self.session_id.is_empty()
            && self.session_id.len() <= 256
            && !self.session_id.chars().any(char::is_control)
            && [&self.source, &self.provider_home]
                .into_iter()
                .all(|path| path.is_absolute() && path.as_os_str().len() <= 4096)
    }
    fn protocol(&self) -> &'static str {
        match self.provider.as_str() {
            "codex" => "private-app-server-matching-compaction-item-and-turn-v1",
            "claude_code" => "private-resume-exit-status-assumption-v1",
            _ => "unsupported",
        }
    }
}

/// This release has no qualified live native-dispatch cells. Artifact admission
/// does not activate experimental remote mutation, even if an old config opts in.
/// New cells require reviewed source, exact binary/platform/home scope and retained
/// continuation evidence; a user-authored receipt or an environment flag is not
/// qualification. See docs/assurance/qualification.json.
fn admit_activation(target: &Target, binary_sha256: &str) -> Result<()> {
    #[cfg(debug_assertions)]
    if isolated_protocol_fixture(target, binary_sha256) {
        return Ok(());
    }
    let _ = (target, binary_sha256);
    Err(ActivationUnqualified.into())
}

/// Development builds can exercise the exact production state machine using
/// ONLY the immutable repository fixtures under a fresh isolated test root.
/// Release builds contain neither this path nor its environment selector.
#[cfg(debug_assertions)]
fn isolated_protocol_fixture(target: &Target, binary_sha256: &str) -> bool {
    let Some(raw_root) = std::env::var_os("GOBSTOPPER_NATIVE_FIXTURE_ROOT").map(PathBuf::from)
    else {
        return false;
    };
    let Ok(root) = raw_root.canonicalize() else {
        return false;
    };
    let Ok(temporary) = std::env::temp_dir().canonicalize() else {
        return false;
    };
    if root.parent() != Some(temporary.as_path())
        || !root
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("gobstopper-watch-"))
        || std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) != Some(raw_root.join("data"))
        || std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) != Some(raw_root.join("config"))
        || !target.source.starts_with(&target.provider_home)
    {
        return false;
    }
    let (directory, bytes): (&str, &[u8]) = match target.provider.as_str() {
        "codex" => (
            "codex",
            include_bytes!("../tests/fixtures/codex-app-server.sh"),
        ),
        "claude_code" => (
            "claude",
            include_bytes!("../tests/fixtures/claude-compact.sh"),
        ),
        _ => return false,
    };
    target.provider_home == root.join(directory) && binary_sha256 == sha(bytes)
}

pub(super) fn check_activation(
    handle: &SessionHandle,
    provider_home: &Path,
    binary: &Path,
) -> Result<()> {
    let target = Target {
        provider: handle.provider.as_str().to_owned(),
        session_id: handle.session_id.clone(),
        source: handle.path.canonicalize()?,
        provider_home: provider_home.canonicalize()?,
    };
    let (_, digest) = executable(binary)?;
    admit_activation(&target, &digest)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    schema: u32,
    key: String,
    target: Target,
}

struct DirectoryCustody {
    _file: File,
}
impl DirectoryCustody {
    fn acquire(root: &Path, exclusive: bool) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
        }
        let file = options.open(root)?;
        if !file.metadata()?.is_dir() {
            bail!("native journal root must be a real directory");
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let mode = if exclusive {
                libc::LOCK_EX
            } else {
                libc::LOCK_SH
            };
            // SAFETY: retained directory descriptor; cooperating registry writers
            // hold exclusive custody only through durable target registration.
            if unsafe { libc::flock(file.as_raw_fd(), mode | libc::LOCK_NB) } != 0 {
                bail!("native target registry is busy; no dispatch performed");
            }
        }
        #[cfg(not(unix))]
        bail!("native directory custody is unqualified on this platform");
        Ok(Self { _file: file })
    }
}

fn registrations(root: &Path) -> Result<std::collections::BTreeMap<String, Target>> {
    let path = root.join(REGISTRY);
    let mut found = std::collections::BTreeMap::new();
    if path.try_exists()? {
        let file = open_regular(&path, false, false)?;
        let mut bytes = Vec::new();
        file.take(MAX_LOG_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_LOG_BYTES || (!bytes.is_empty() && !bytes.ends_with(b"\n")) {
            bail!("native target registry is torn or full; repair required");
        }
        for line in bytes.split_inclusive(|byte| *byte == b'\n') {
            let entry: Registration = serde_json::from_slice(line)?;
            if entry.schema != 1
                || !entry.target.valid()
                || entry.key != entry.target.key()?
                || found.len() >= MAX_TARGETS
                || found.insert(entry.key, entry.target).is_some()
            {
                bail!("native target registry is invalid; repair required");
            }
        }
    }
    let mut expected = std::collections::BTreeSet::new();
    if path.try_exists()? {
        expected.insert(REGISTRY.to_string());
    }
    for key in found.keys() {
        expected.insert(format!("{key}.lock"));
        expected.insert(format!("{key}.jsonl"));
    }
    let mut actual = std::collections::BTreeSet::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() || actual.len() > MAX_TARGETS * 2 {
            bail!("native registry contains unsupported artifacts; repair required");
        }
        actual.insert(
            entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("invalid native artifact name"))?,
        );
    }
    if actual != expected {
        bail!("native target artifacts are missing or renamed; repair required");
    }
    Ok(found)
}

fn register(
    root: &Path,
    target: &Target,
    found: &std::collections::BTreeMap<String, Target>,
) -> Result<()> {
    let key = target.key()?;
    if let Some(old) = found.get(&key) {
        if old != target {
            bail!("native target identity collision");
        }
        return Ok(());
    }
    if !target.valid() || found.len() >= MAX_TARGETS {
        bail!("native target registration unavailable");
    }
    for suffix in ["lock", "jsonl"] {
        open_regular(&root.join(format!("{key}.{suffix}")), true, true)?.sync_all()?;
    }
    sync_dir(root)?;
    let mut bytes = serde_json::to_vec(&Registration {
        schema: 1,
        key,
        target: target.clone(),
    })?;
    bytes.push(b'\n');
    let mut file = open_regular(&root.join(REGISTRY), true, true)?;
    if file
        .metadata()?
        .len()
        .checked_add(bytes.len() as u64)
        .is_none_or(|n| n > MAX_LOG_BYTES)
    {
        bail!("native target registry capacity exceeded; repair required");
    }
    file.write_all(&bytes)?;
    file.sync_all()?;
    sync_dir(root)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum State {
    Prepared,
    Dispatched,
    ObservedApplied,
    ObservedNoop,
    Rejected,
    Unknown,
    Reconciled,
}

impl State {
    fn unresolved(self) -> bool {
        matches!(self, Self::Dispatched | Self::Unknown)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    operation_sha256: String,
    target: Target,
    binary: PathBuf,
    binary_sha256: String,
    source_sha256: String,
    snapshot_sha256: String,
    policy_sha256: String,
    protocol_contract: String,
    state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal: Option<TerminalEvidence>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TerminalEvidence {
    pub(super) session_id: String,
    pub(super) turn_id: Option<String>,
    pub(super) item_id: Option<String>,
}

fn terminal_valid(record: &Record) -> bool {
    let completed = matches!(
        record.state,
        State::ObservedApplied | State::ObservedNoop | State::Reconciled
    );
    match &record.terminal {
        None => !completed,
        Some(terminal) => {
            (completed || record.state == State::Unknown)
                && terminal.session_id == record.target.session_id
                && [&terminal.turn_id, &terminal.item_id]
                    .into_iter()
                    .all(|value| {
                        value.as_ref().is_none_or(|value| {
                            !value.is_empty()
                                && value.len() <= 256
                                && !value.chars().any(char::is_control)
                        })
                    })
                && (record.target.provider != "codex"
                    || (terminal.turn_id.is_some() && terminal.item_id.is_some()))
        }
    }
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_sha(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn root() -> PathBuf {
    gobstopper_core::events::default_log_path()
        .parent()
        .expect("event log has a parent")
        .join("native-operations-v1")
}

fn private_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        if !parent.exists() {
            private_dir(parent)?;
        }
    }
    let mut options = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        options.mode(0o700);
    }
    match options.create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    if !fs::symlink_metadata(path)?.is_dir() {
        bail!("native operation root must be a real directory");
    }
    sync_dir(path)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        sync_dir(parent)?;
    }
    Ok(())
}

fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    bail!("native operation custody is not qualified on this platform");
    Ok(())
}

fn open_regular(path: &Path, append: bool, create: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).append(append).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        bail!("native operation state must be a regular file");
    }
    Ok(file)
}

fn lock(file: &File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: file retains this descriptor and the nonblocking lock for the
        // full operation. Never unlink or replace this custody inode.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            bail!("another native operation holds this target; no dispatch performed");
        }
        Ok(())
    }
    #[cfg(not(unix))]
    bail!("native operation custody is not qualified on this platform")
}

fn read_records(file: &File) -> Result<Vec<Record>> {
    if file.metadata()?.len() > MAX_LOG_BYTES {
        bail!("native operation journal exceeds its bound; repair required");
    }
    let mut bytes = Vec::new();
    file.take(MAX_LOG_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_LOG_BYTES || (!bytes.is_empty() && !bytes.ends_with(b"\n")) {
        bail!("native operation journal is torn or oversized; repair required");
    }
    let mut records: Vec<Record> = Vec::new();
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        let record: Record = serde_json::from_slice(line)
            .context("native operation journal is malformed; repair required")?;
        if record.schema != 1
            || !record.target.valid()
            || record.protocol_contract != record.target.protocol()
            || !record.binary.is_absolute()
            || record.binary.as_os_str().len() > 4096
            || !valid_sha(&record.operation_sha256)
            || !valid_sha(&record.binary_sha256)
            || !valid_sha(&record.source_sha256)
            || !valid_sha(&record.snapshot_sha256)
            || !valid_sha(&record.policy_sha256)
            || !terminal_valid(&record)
        {
            bail!("native operation journal has unsupported identity; repair required");
        }
        if let Some(last) = records.last() {
            if record.target != last.target {
                bail!("native operation target changed in journal; repair required");
            }
            if record.operation_sha256 == last.operation_sha256 {
                let mut expected = last.clone();
                expected.state = record.state;
                expected.terminal = record.terminal.clone();
                let transition = matches!(
                    (last.state, record.state),
                    (State::Prepared, State::Dispatched | State::Rejected)
                        | (
                            State::Dispatched,
                            State::ObservedApplied | State::ObservedNoop | State::Unknown
                        )
                ) || (last.state == State::Unknown
                    && record.state == State::Reconciled
                    && can_reconcile(last)
                    && last.terminal == record.terminal);
                if expected != record || !transition {
                    bail!("invalid native operation transition; repair required");
                }
            } else if last.state.unresolved()
                || record.state != State::Prepared
                || records
                    .iter()
                    .any(|old| old.operation_sha256 == record.operation_sha256)
            {
                bail!("native operation replay in journal; repair required");
            }
        } else if record.state != State::Prepared {
            bail!("native operation journal lacks its intent; repair required");
        }
        records.push(record);
    }
    Ok(records)
}

fn append(file: &mut File, record: &Record) -> Result<()> {
    let mut bytes = serde_json::to_vec(record)?;
    bytes.push(b'\n');
    if file
        .metadata()?
        .len()
        .checked_add(bytes.len() as u64)
        .is_none_or(|n| n > MAX_LOG_BYTES)
    {
        bail!("native operation journal is full; repair required");
    }
    // A partial write stays visible and fails closed on the next read. A failed
    // fsync never authorizes dispatch even when all bytes happened to land.
    #[cfg(test)]
    faults::hit(faults::Stage::BeforeWrite)?;
    #[cfg(test)]
    if faults::selected(faults::Stage::PartialWrite) {
        file.write_all(&bytes[..bytes.len() / 2])?;
        faults::hit(faults::Stage::PartialWrite)?;
    }
    file.write_all(&bytes)?;
    #[cfg(test)]
    faults::hit(faults::Stage::AfterWrite)?;
    #[cfg(test)]
    faults::hit(faults::Stage::BeforeSync)?;
    file.sync_all()?;
    #[cfg(test)]
    faults::hit(faults::Stage::AfterSync)?;
    Ok(())
}

#[cfg(test)]
mod faults {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) enum Stage {
        BeforeWrite,
        PartialWrite,
        AfterWrite,
        BeforeSync,
        AfterSync,
    }
    thread_local! {
        static SELECTED: std::cell::Cell<Option<Stage>> = const { std::cell::Cell::new(None) };
    }
    pub(super) fn selected(stage: Stage) -> bool {
        SELECTED.with(|selected| selected.get() == Some(stage))
    }
    pub(super) fn hit(stage: Stage) -> std::io::Result<()> {
        if selected(stage) {
            SELECTED.with(|selected| selected.set(None));
            return Err(std::io::Error::from_raw_os_error(libc::ENOSPC));
        }
        Ok(())
    }
    pub(super) fn arm(stage: Stage) {
        SELECTED.with(|selected| selected.set(Some(stage)));
    }
}

fn executable(path: &Path) -> Result<(PathBuf, String)> {
    let resolved = if path.components().count() == 1 && !path.is_absolute() {
        std::env::var_os("PATH")
            .and_then(|paths| {
                std::env::split_paths(&paths)
                    .map(|dir| dir.join(path))
                    .find(|p| p.is_file())
            })
            .ok_or(ExecutableUnavailable)?
    } else {
        path.to_path_buf()
    }
    .canonicalize()
    .map_err(|_| ExecutableUnavailable)?;
    let file = open_regular(&resolved, false, false).map_err(|_| ExecutableUnavailable)?;
    if file.metadata()?.len() > MAX_BINARY_BYTES {
        bail!("native provider executable exceeds identity hash bound");
    }
    let mut hash = Sha256::new();
    let mut limited = file.take(MAX_BINARY_BYTES + 1);
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = limited.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > MAX_BINARY_BYTES {
            bail!("native provider executable grew beyond identity bound");
        }
        hash.update(&buffer[..n]);
    }
    Ok((resolved, format!("{:x}", hash.finalize())))
}

pub(super) struct Operation {
    file: File,
    _custody: File,
    record: Record,
}

impl Operation {
    pub(super) fn pending(handle: &SessionHandle, provider_home: &Path) -> Result<bool> {
        let target = Target {
            provider: handle.provider.as_str().to_string(),
            session_id: handle.session_id.clone(),
            source: handle.path.canonicalize()?,
            provider_home: provider_home.canonicalize()?,
        };
        let root = root();
        match fs::symlink_metadata(&root) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
            Ok(_) => {}
        }
        let _directory = DirectoryCustody::acquire(&root, false)?;
        let found = registrations(&root)?;
        let key = target.key()?;
        if !found.contains_key(&key) {
            return Ok(false);
        }
        let path = root.join(format!("{key}.jsonl"));
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
            Ok(_) => {}
        }
        let custody = open_regular(&root.join(format!("{key}.lock")), false, false)?;
        lock(&custody)?;
        let records = read_records(&open_regular(&path, false, false)?)?;
        Ok(records
            .last()
            .is_some_and(|last| last.target != target || last.state.unresolved()))
    }

    pub(super) fn prepare(
        handle: &SessionHandle,
        provider_home: &Path,
        binary: &Path,
        before: &vault::VaultEntry,
        policy_sha256: &str,
        protocol_contract: &str,
    ) -> Result<Self> {
        let target = Target {
            provider: handle.provider.as_str().to_string(),
            session_id: handle.session_id.clone(),
            source: handle.path.canonicalize()?,
            provider_home: provider_home.canonicalize()?,
        };
        if before.provider != handle.provider.as_str()
            || before.session_id != handle.session_id
            || before.path.canonicalize()? != target.source
            || !valid_sha(policy_sha256)
            || protocol_contract != target.protocol()
        {
            bail!("native operation source and recovery identity disagree");
        }
        // Independently check metadata in retained bytes. A discovery fallback
        // filename or caller-labelled VaultEntry is not provider authority.
        {
            let reader = vault::Reader::open(&vault::default_root())?;
            let bytes = reader.read_object(&before.sha256)?;
            if gobstopper_adapters::fork::source_session_id(handle.provider, &bytes)?
                != handle.session_id
                || sha(&bytes) != before.source_sha256
            {
                bail!("native snapshot metadata does not identify the selected session");
            }
        }
        let (binary, binary_sha256) = executable(binary)?;
        admit_activation(&target, &binary_sha256)?;
        let root = root();
        private_dir(&root)?;
        let directory = DirectoryCustody::acquire(&root, true)?;
        let found = registrations(&root)?;
        register(&root, &target, &found)?;
        let key = target.key()?;
        let custody = open_regular(&root.join(format!("{key}.lock")), true, true)?;
        lock(&custody)?;
        let mut file = open_regular(&root.join(format!("{key}.jsonl")), true, true)?;
        let records = read_records(&file)?;
        if let Some(last) = records.last() {
            if last.target != target || last.state.unresolved() {
                bail!("native operation outcome remains unresolved; automatic replay is disabled");
            }
        }
        // Registration is complete. Retain only target custody during hashing,
        // vault work and dispatch, allowing independent targets to progress.
        drop(directory);
        let operation_sha256 = sha(&serde_json::to_vec(&(
            &target,
            &binary_sha256,
            &before.sha256,
            policy_sha256,
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos(),
        ))?);
        let record = Record {
            schema: 1,
            operation_sha256,
            target,
            binary,
            binary_sha256,
            source_sha256: before.source_sha256.clone(),
            snapshot_sha256: before.sha256.clone(),
            policy_sha256: policy_sha256.to_owned(),
            protocol_contract: protocol_contract.to_owned(),
            state: State::Prepared,
            terminal: None,
        };
        vault::retain_operation_snapshot(
            &record.operation_sha256,
            &before.sha256,
            &vault::default_root(),
        )?;
        append(&mut file, &record)?;
        sync_dir(&root)?;
        Ok(Self {
            file,
            _custody: custody,
            record,
        })
    }

    pub(super) fn binary(&self) -> &Path {
        &self.record.binary
    }

    pub(super) fn dispatch(&mut self) -> Result<()> {
        if self.record.state != State::Prepared {
            bail!("native operation is not prepared");
        }
        let (_, digest) = executable(&self.record.binary)?;
        if digest != self.record.binary_sha256 {
            bail!("native provider executable changed before dispatch");
        }
        self.record.state = State::Dispatched;
        append(&mut self.file, &self.record)
    }

    /// `Some` requires matching provider-terminal evidence, separately from
    /// whether the post-state measured a reduction. `None` is never replayable.
    pub(super) fn finish(
        &mut self,
        applied: Option<bool>,
        terminal: Option<TerminalEvidence>,
    ) -> Result<()> {
        if self.record.state != State::Dispatched {
            bail!("native operation was not dispatched");
        }
        self.record.state = match applied {
            Some(true) => State::ObservedApplied,
            Some(false) => State::ObservedNoop,
            None => State::Unknown,
        };
        self.record.terminal = terminal;
        if !terminal_valid(&self.record) {
            bail!("native terminal evidence does not match this operation");
        }
        append(&mut self.file, &self.record)
    }
}

/// Read-only metadata inspection; the complete source/session identity is only
/// returned by this explicitly requested CLI output. It never creates roots.
pub(super) fn inspect() -> Result<Vec<serde_json::Value>> {
    let root = root();
    match fs::symlink_metadata(&root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
        Ok(_) => {}
    }
    let _directory = DirectoryCustody::acquire(&root, false)?;
    let registered = registrations(&root)?;
    let mut output = Vec::new();
    for (key, target) in registered {
        let custody = open_regular(&root.join(format!("{key}.lock")), false, false)?;
        lock(&custody)?;
        let file = open_regular(&root.join(format!("{key}.jsonl")), false, false)?;
        let records = read_records(&file)?;
        if let Some(last) = records.last() {
            if last.target != target {
                bail!("native operation filename and target disagree; repair required");
            }
            output.push(serde_json::json!({
                "operation_sha256": last.operation_sha256, "provider": last.target.provider,
                "session_id": last.target.session_id, "source": last.target.source,
                "provider_home": last.target.provider_home, "state": last.state,
                "automatic_replay_blocked": last.state.unresolved(),
                "snapshot_sha256": last.snapshot_sha256, "source_sha256": last.source_sha256,
                "binary_sha256": last.binary_sha256, "policy_sha256": last.policy_sha256,
                "protocol_contract": last.protocol_contract, "records": records.len(),
                "terminal": last.terminal,
            }));
        }
    }
    output.sort_by(|a, b| {
        a["operation_sha256"]
            .as_str()
            .cmp(&b["operation_sha256"].as_str())
    });
    Ok(output)
}

fn can_reconcile(record: &Record) -> bool {
    record.state == State::Unknown
        && record.target.provider == "codex"
        && record
            .terminal
            .as_ref()
            .is_some_and(|terminal| terminal.turn_id.is_some() && terminal.item_id.is_some())
        && terminal_valid(record)
}

/// Only recorded matching terminal evidence can release an uncertain operation.
/// No supplied outcome, clock, changed source, or session-only callback suffices.
pub(super) fn reconcile(operation_sha256: &str) -> Result<()> {
    if !valid_sha(operation_sha256) {
        bail!("reconciliation requires the exact lowercase operation digest");
    }
    let mut count = 0;
    let _directory = DirectoryCustody::acquire(&root(), false)?;
    let registered = registrations(&root())?;
    for entry in fs::read_dir(root()).context("native operation journal unavailable")? {
        let path = entry?.path();
        if path.file_name().is_some_and(|name| name == REGISTRY) {
            continue;
        }
        if path
            .extension()
            .is_none_or(|extension| extension != "jsonl")
        {
            continue;
        }
        count += 1;
        if count > 4096 {
            bail!("native operation lookup exceeds target bound");
        }
        let custody = open_regular(&path.with_extension("lock"), false, false)?;
        lock(&custody)?;
        let mut file = open_regular(&path, true, false)?;
        let records = read_records(&file)?;
        if records.last().is_some_and(|last| {
            path.file_stem()
                .and_then(|v| v.to_str())
                .and_then(|key| registered.get(key))
                != Some(&last.target)
        }) {
            bail!("native operation filename and target disagree; repair required");
        }
        let Some(last) = records
            .last()
            .filter(|last| last.operation_sha256 == operation_sha256)
        else {
            continue;
        };
        if last.state == State::Reconciled {
            return Ok(());
        }
        if !can_reconcile(last) {
            bail!("matching provider terminal evidence is unavailable; operation remains blocked");
        }
        let mut reconciled = last.clone();
        reconciled.state = State::Reconciled;
        append(&mut file, &reconciled)?;
        return Ok(());
    }
    bail!("no current native operation matches this exact digest")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "gobstopper-native-journal-{}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn read(&self, records: &[Record]) -> Result<Vec<Record>> {
            let path = self.0.join("journal");
            let bytes: Vec<u8> = records
                .iter()
                .flat_map(|r| {
                    let mut bytes = serde_json::to_vec(r).unwrap();
                    bytes.push(b'\n');
                    bytes
                })
                .collect();
            fs::write(&path, bytes).unwrap();
            read_records(&open_regular(&path, false, false)?)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn record(state: State) -> Record {
        Record {
            schema: 1,
            operation_sha256: "1".repeat(64),
            target: Target {
                provider: "codex".into(),
                session_id: "synthetic".into(),
                source: "/synthetic/source".into(),
                provider_home: "/synthetic".into(),
            },
            binary: "/synthetic/bin".into(),
            binary_sha256: "2".repeat(64),
            source_sha256: "3".repeat(64),
            snapshot_sha256: "4".repeat(64),
            policy_sha256: "5".repeat(64),
            protocol_contract: "private-app-server-matching-compaction-item-and-turn-v1".into(),
            state,
            terminal: matches!(state, State::ObservedApplied | State::ObservedNoop).then(|| {
                TerminalEvidence {
                    session_id: "synthetic".into(),
                    turn_id: Some("turn".into()),
                    item_id: Some("item".into()),
                }
            }),
        }
    }

    /// An unrelated sibling test may fork while this test owns a flock. Even
    /// CLOEXEC descriptors remain open in that child until exec, so closing our
    /// copy alone need not release custody immediately. Test the drop invariant
    /// after exec in a process with no sibling tests, without weakening lock()
    /// or retrying a busy result. The reviewed runner bounds and reaps the child.
    fn isolated_lock_test_child(name: &str) -> bool {
        const SELECTOR: &str = "GOBSTOPPER_NATIVE_LOCK_TEST_CHILD";
        if std::env::var(SELECTOR).ok().as_deref() == Some(name) {
            return true;
        }
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", name, "--test-threads=1", "--color=never"])
            .env(SELECTOR, name);
        let output =
            gobstopper_adapters::plugins::run_bounded(command, Vec::new(), 10_000, 64 * 1024)
                .expect("isolated lock test must finish successfully within its process bounds");
        let output = String::from_utf8(output).unwrap();
        assert!(output.lines().any(|line| line == "running 1 test"));
        assert!(output
            .lines()
            .any(|line| line == format!("test {name} ... ok")));
        let outcomes: Vec<_> = output
            .lines()
            .filter(|line| line.starts_with("test result:"))
            .collect();
        assert_eq!(outcomes.len(), 1);
        assert!(
            outcomes[0].starts_with("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; ")
        );
        false
    }

    #[test]
    fn journal_accepts_terminal_history_but_rejects_uncertain_replay() {
        let f = Fixture::new();
        for end in [State::ObservedApplied, State::ObservedNoop, State::Unknown] {
            let mut history = vec![
                record(State::Prepared),
                record(State::Dispatched),
                record(end),
            ];
            assert_eq!(f.read(&history).unwrap(), history);
            let mut next = record(State::Prepared);
            next.operation_sha256 = "6".repeat(64);
            next.policy_sha256 = "7".repeat(64);
            history.push(next);
            assert_eq!(f.read(&history).is_err(), end == State::Unknown);
        }
        let mut next = record(State::Prepared);
        next.operation_sha256 = "8".repeat(64);
        assert!(f
            .read(&[record(State::Prepared), record(State::Dispatched), next])
            .is_err());
    }

    #[test]
    fn persistence_faults_at_every_record_state_leave_old_complete_or_repair_state() {
        use faults::Stage::*;
        let prior = |state| match state {
            State::Prepared => vec![],
            State::Dispatched | State::Rejected => vec![record(State::Prepared)],
            State::ObservedApplied | State::ObservedNoop | State::Unknown => {
                vec![record(State::Prepared), record(State::Dispatched)]
            }
            State::Reconciled => {
                let mut unknown = record(State::Unknown);
                unknown.terminal = record(State::ObservedApplied).terminal;
                vec![record(State::Prepared), record(State::Dispatched), unknown]
            }
        };
        for state in [
            State::Prepared,
            State::Dispatched,
            State::Rejected,
            State::ObservedApplied,
            State::ObservedNoop,
            State::Unknown,
            State::Reconciled,
        ] {
            for stage in [BeforeWrite, PartialWrite, AfterWrite, BeforeSync, AfterSync] {
                let fixture = Fixture::new();
                let history = prior(state);
                fixture.read(&history).unwrap();
                let path = fixture.0.join("journal");
                let mut next = record(state);
                if state == State::Reconciled {
                    next.terminal = history.last().unwrap().terminal.clone();
                }
                let mut file = open_regular(&path, true, false).unwrap();
                faults::arm(stage);
                assert!(append(&mut file, &next).is_err(), "{state:?} {stage:?}");
                drop(file);
                let recovered = read_records(&open_regular(&path, false, false).unwrap());
                match stage {
                    BeforeWrite => assert_eq!(recovered.unwrap(), history),
                    PartialWrite => assert!(recovered.is_err(), "torn state must not become empty"),
                    _ => {
                        let mut expected = history;
                        expected.push(next);
                        assert_eq!(recovered.unwrap(), expected);
                    }
                }
            }
        }
    }

    #[test]
    fn failed_dispatch_persistence_never_authorizes_a_provider_call() {
        if !isolated_lock_test_child(
            "native_operations::tests::failed_dispatch_persistence_never_authorizes_a_provider_call",
        ) {
            return;
        }
        use faults::Stage::*;
        for stage in [BeforeWrite, PartialWrite, AfterWrite, BeforeSync, AfterSync] {
            let fixture = Fixture::new();
            let binary = fixture.0.join("synthetic-binary");
            fs::write(&binary, b"never executed").unwrap();
            let (binary, binary_sha256) = executable(&binary).unwrap();
            let mut prepared = record(State::Prepared);
            prepared.binary = binary;
            prepared.binary_sha256 = binary_sha256;
            fixture.read(&[prepared.clone()]).unwrap();
            let custody = open_regular(&fixture.0.join("lock"), true, true).unwrap();
            #[cfg(unix)]
            {
                use std::os::fd::AsRawFd;
                let flags = unsafe { libc::fcntl(custody.as_raw_fd(), libc::F_GETFD) };
                assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
            }
            lock(&custody).unwrap();
            let mut operation = Operation {
                file: open_regular(&fixture.0.join("journal"), true, false).unwrap(),
                _custody: custody,
                record: prepared,
            };
            faults::arm(stage);
            let mut calls = 0;
            if operation.dispatch().is_ok() {
                calls += 1;
            }
            assert_eq!(calls, 0, "{stage:?}");
            drop(operation);
            // Dropping the failed owner releases its exact lock. Persisted
            // dispatched or torn records remain blocking on a fresh reader.
            let custody = open_regular(&fixture.0.join("lock"), false, false).unwrap();
            lock(&custody).unwrap();
            let recovered =
                read_records(&open_regular(&fixture.0.join("journal"), false, false).unwrap());
            match stage {
                BeforeWrite => {
                    assert_eq!(recovered.unwrap().last().unwrap().state, State::Prepared)
                }
                PartialWrite => assert!(recovered.is_err()),
                _ => assert!(recovered.unwrap().last().unwrap().state.unresolved()),
            }
        }
    }

    #[test]
    fn journal_rejects_fabricated_success_reconciliation_and_identity_changes() {
        let f = Fixture::new();
        assert!(f
            .read(&[record(State::Prepared), record(State::ObservedApplied)])
            .is_err());
        assert!(f
            .read(&[
                record(State::Prepared),
                record(State::Dispatched),
                record(State::Reconciled)
            ])
            .is_err());
        let mut changed = record(State::Dispatched);
        let mut terminal_unknown = record(State::ObservedApplied);
        terminal_unknown.state = State::Unknown;
        let mut reconciled = terminal_unknown.clone();
        reconciled.state = State::Reconciled;
        assert!(can_reconcile(&terminal_unknown));
        assert!(f
            .read(&[
                record(State::Prepared),
                record(State::Dispatched),
                terminal_unknown,
                reconciled
            ])
            .is_ok());
        changed.target.session_id = "foreign".into();
        assert!(f.read(&[record(State::Prepared), changed]).is_err());
        let mut changed = record(State::Dispatched);
        changed.policy_sha256 = "f".repeat(64);
        assert!(f.read(&[record(State::Prepared), changed]).is_err());
        let mut future = record(State::Prepared);
        future.schema = 2;
        assert!(f.read(&[future]).is_err());
    }

    #[test]
    fn torn_duplicate_and_oversized_journals_need_repair() {
        let f = Fixture::new();
        let path = f.0.join("journal");
        for bytes in [
            serde_json::to_vec(&record(State::Prepared)).unwrap(),
            b"{\"schema\":1,\"schema\":1}\n".to_vec(),
            b"\n".to_vec(),
            vec![b' '; MAX_LOG_BYTES as usize + 1],
        ] {
            fs::write(&path, &bytes).unwrap();
            assert!(read_records(&open_regular(&path, false, false).unwrap()).is_err());
            assert_eq!(
                fs::read(&path).unwrap(),
                bytes,
                "inspection must preserve damaged evidence"
            );
        }
    }

    #[test]
    fn registry_preserves_target_binding_after_rename_or_missing_file() {
        let f = Fixture::new();
        let target = record(State::Prepared).target;
        let empty = registrations(&f.0).unwrap();
        register(&f.0, &target, &empty).unwrap();
        assert_eq!(
            registrations(&f.0).unwrap().get(&target.key().unwrap()),
            Some(&target)
        );
        let journal = f.0.join(format!("{}.jsonl", target.key().unwrap()));
        let renamed = f.0.join(format!("{}.jsonl", "f".repeat(64)));
        fs::rename(&journal, &renamed).unwrap();
        assert!(registrations(&f.0).is_err());
        fs::rename(&renamed, &journal).unwrap();
        fs::remove_file(&journal).unwrap();
        assert!(registrations(&f.0).is_err());
    }

    #[test]
    fn journal_rejects_unknown_provider_contract_or_relative_identity() {
        let f = Fixture::new();
        for field in 0..5 {
            let mut bad = record(State::Prepared);
            match field {
                0 => bad.target.provider = "unknown-provider".into(),
                1 => bad.protocol_contract = "unsupported-contract".into(),
                2 => bad.target.session_id = "malformed\nidentity".into(),
                3 => bad.target.source = "relative/source".into(),
                _ => bad.binary = "relative/bin".into(),
            }
            assert!(f.read(&[bad]).is_err());
        }
    }

    #[test]
    fn independent_openers_share_custody_until_owner_releases() {
        if !isolated_lock_test_child(
            "native_operations::tests::independent_openers_share_custody_until_owner_releases",
        ) {
            return;
        }
        let f = Fixture::new();
        let path = f.0.join("lock");
        let first = open_regular(&path, true, true).unwrap();
        lock(&first).unwrap();
        let second = open_regular(&path, true, false).unwrap();
        assert!(lock(&second).is_err());
        drop(first);
        lock(&second).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_special_files_are_not_state() {
        let f = Fixture::new();
        let target = f.0.join("target");
        fs::write(&target, "preserved").unwrap();
        let link = f.0.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(open_regular(&link, true, true).is_err());
        assert!(open_regular(Path::new("/dev/null"), false, false).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "preserved");
    }

    #[cfg(unix)]
    #[test]
    fn artifact_identity_detects_replacement_and_same_size_writes() {
        for replacement in [false, true] {
            let f = Fixture::new();
            let path = f.0.join("artifact");
            fs::write(&path, b"first").unwrap();
            let artifact = ArtifactIdentity::open(path.clone()).unwrap();
            assert_eq!(artifact.sha256, sha(b"first"));
            artifact.validate().unwrap();
            if replacement {
                let candidate = f.0.join("candidate");
                // Even byte-identical replacement must invalidate the receipt.
                fs::write(&candidate, b"first").unwrap();
                fs::rename(candidate, path).unwrap();
            } else {
                fs::write(path, b"other").unwrap();
            }
            assert!(artifact.validate().is_err());
        }
    }
}
