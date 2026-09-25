use crate::{verify, AdapterError};
use gobstopper_core::Provider;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const DEFAULT_MAX_TRANSCRIPT_BYTES: u64 = 512 * 1024 * 1024;
/// Supported overrides also fit allocation and chunk-count arithmetic.
pub const MAX_TRANSCRIPT_BYTES: u64 = 8 * 1024 * 1024 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    NotPublished,
    Published,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    Unconfirmed,
    Confirmed,
}

pub(crate) fn parse_limit(value: Option<&str>) -> u64 {
    value
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v >= 1024 && *v <= MAX_TRANSCRIPT_BYTES.min(isize::MAX as u64 - 1))
        .unwrap_or(DEFAULT_MAX_TRANSCRIPT_BYTES)
}

/// Upper bound on transcript bytes loaded or rewritten. Guards memory
/// use on pathological inputs; operators can raise it with
/// `GOBSTOPPER_MAX_TRANSCRIPT_BYTES` (a byte count, minimum 1 KiB).
pub fn max_transcript_bytes() -> u64 {
    static CACHED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        parse_limit(
            std::env::var("GOBSTOPPER_MAX_TRANSCRIPT_BYTES")
                .ok()
                .as_deref(),
        )
    })
}

pub(crate) fn io(path: &Path, source: std::io::Error) -> AdapterError {
    AdapterError::Io {
        path: path.into(),
        source,
    }
}

pub fn read(path: &Path) -> Result<Vec<u8>, AdapterError> {
    read_with_limit(path, max_transcript_bytes())
}

/// Leaf symlinks and special files are refused. Parent directories must remain
/// stable and owner-controlled; this is not a hostile-filesystem sandbox.
pub(crate) fn read_with_limit(path: &Path, limit: u64) -> Result<Vec<u8>, AdapterError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| io(path, e))?;
    let meta = file.metadata().map_err(|e| io(path, e))?;
    if !meta.is_file()
        || meta.len() > limit
        || limit > MAX_TRANSCRIPT_BYTES.min(isize::MAX as u64 - 1)
    {
        return Err(AdapterError::InvalidEdit(
            "source must be a bounded regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io(path, e))?;
    if bytes.len() as u64 > limit {
        return Err(AdapterError::InvalidEdit("transcript exceeds byte limit"));
    }
    Ok(bytes)
}

pub(crate) fn sync_dir(path: &Path) -> std::io::Result<()> {
    checkpoint("directory_sync", path, false)?;
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    return Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "directory sync is unsupported on this platform",
    ));
    checkpoint("directory_sync", path, true)?;
    Ok(())
}

pub(crate) fn checkpoint(stage: &'static str, path: &Path, after: bool) -> std::io::Result<()> {
    #[cfg(test)]
    return faults::hit(stage, path, after);
    #[cfg(not(test))]
    {
        let _ = (stage, path, after);
        Ok(())
    }
}

pub(crate) fn write_all(file: &mut File, bytes: &[u8], path: &Path) -> std::io::Result<()> {
    checkpoint("write", path, false)?;
    // The test seam also exercises a torn write, without a production switch.
    #[cfg(test)]
    {
        let split = bytes.len() / 2;
        file.write_all(&bytes[..split])?;
        checkpoint("partial_write", path, true)?;
        file.write_all(&bytes[split..])?;
    }
    #[cfg(not(test))]
    file.write_all(bytes)?;
    checkpoint("write", path, true)
}

pub(crate) fn sync_file(file: &File, path: &Path) -> std::io::Result<()> {
    checkpoint("file_sync", path, false)?;
    file.sync_all()?;
    checkpoint("file_sync", path, true)
}

pub(crate) fn remove_file(path: &Path) -> std::io::Result<()> {
    checkpoint("unlink", path, false)?;
    fs::remove_file(path)?;
    checkpoint("unlink", path, true)
}

pub(crate) struct Temporary {
    pub path: PathBuf,
}

impl Temporary {
    pub fn new(parent: &Path, bytes: &[u8]) -> Result<Self, AdapterError> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = parent.join(format!(
            ".gobstopper-{}-{stamp}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|e| io(&path, e))?;
        let temp = Self { path };
        write_all(&mut file, bytes, &temp.path).map_err(|e| io(&temp.path, e))?;
        Ok(temp)
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Create private directories within stable, owner-controlled parents. An
/// existing directory belongs to the caller: never change its permissions.
pub(crate) fn private_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        if !parent.exists() {
            private_dir(parent)?;
        }
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    let created = match builder.create(path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(e),
    };
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(std::io::Error::other(
            "private directory must not be a symlink",
        ));
    }
    #[cfg(unix)]
    if created {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        let _ = created;
    }
    sync_dir(path)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        sync_dir(parent)?;
    }
    Ok(())
}

pub fn publish_new(path: &Path, bytes: &[u8]) -> Result<(), AdapterError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temp = Temporary::new(parent, bytes)?;
    File::open(&temp.path)
        .and_then(|f| sync_file(&f, &temp.path))
        .map_err(|e| io(path, e))?;
    publish_namespace(path, bytes, "link", || fs::hard_link(&temp.path, path))?;
    sync_publication_dir(parent, path, bytes)
}

pub(crate) fn publication_error(
    path: &Path,
    bytes: &[u8],
    stage: &'static str,
    visibility: Visibility,
    durability: Durability,
    source: std::io::Error,
) -> AdapterError {
    AdapterError::Publication {
        path: path.into(),
        expected_sha256: format!("{:x}", Sha256::digest(bytes)),
        stage,
        visibility,
        durability,
        source,
    }
}

fn publish_namespace<F>(
    path: &Path,
    bytes: &[u8],
    stage: &'static str,
    action: F,
) -> Result<(), AdapterError>
where
    F: FnOnce() -> std::io::Result<()>,
{
    checkpoint(stage, path, false).map_err(|e| {
        publication_error(
            path,
            bytes,
            stage,
            Visibility::NotPublished,
            Durability::Unconfirmed,
            e,
        )
    })?;
    if let Err(e) = action() {
        // AlreadyExists positively refuses create-new and remains compatible
        // with immutable-object deduplication. Other syscall errors are unknown.
        if stage == "link" && e.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(io(path, e));
        }
        return Err(publication_error(
            path,
            bytes,
            stage,
            Visibility::Unknown,
            Durability::Unconfirmed,
            e,
        ));
    }
    checkpoint(stage, path, true).map_err(|e| {
        publication_error(
            path,
            bytes,
            stage,
            Visibility::Published,
            Durability::Unconfirmed,
            e,
        )
    })
}

fn sync_publication_dir(parent: &Path, path: &Path, bytes: &[u8]) -> Result<(), AdapterError> {
    checkpoint("publication_sync", path, false).map_err(|e| {
        publication_error(
            path,
            bytes,
            "directory_sync",
            Visibility::Published,
            Durability::Unconfirmed,
            e,
        )
    })?;
    sync_dir(parent).map_err(|e| {
        publication_error(
            path,
            bytes,
            "directory_sync",
            Visibility::Published,
            Durability::Unconfirmed,
            e,
        )
    })?;
    checkpoint("publication_sync", path, true).map_err(|e| {
        publication_error(
            path,
            bytes,
            "directory_sync",
            Visibility::Published,
            Durability::Confirmed,
            e,
        )
    })
}

/// Reconfirm a visible candidate before completing its operation receipt.
pub(crate) fn confirm_publication(path: &Path, expected_sha256: &str) -> Result<(), AdapterError> {
    let bytes = read(path)?;
    if format!("{:x}", Sha256::digest(&bytes)) != expected_sha256 {
        return Err(AdapterError::ChangedDuringWrite { path: path.into() });
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| io(path, e))?;
    sync_file(&file, path).map_err(|e| {
        publication_error(
            path,
            &bytes,
            "file_sync",
            Visibility::Published,
            Durability::Unconfirmed,
            e,
        )
    })?;
    sync_publication_dir(
        path.parent().unwrap_or_else(|| Path::new(".")),
        path,
        &bytes,
    )?;
    if read(path)? != bytes {
        return Err(AdapterError::ChangedDuringWrite { path: path.into() });
    }
    Ok(())
}

/// Atomically swap `path`'s contents for `candidate`, refusing to write if
/// the file no longer matches `original` (e.g. the provider appended a turn
/// while the edit was being prepared).
pub(crate) fn replace(path: &Path, original: &[u8], candidate: &[u8]) -> Result<(), AdapterError> {
    if read(path)? != original {
        return Err(AdapterError::ChangedDuringWrite { path: path.into() });
    }
    if candidate == original {
        return Ok(());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temp = Temporary::new(parent, candidate)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path)
            .map_err(|e| io(path, e))?
            .permissions()
            .mode();
        fs::set_permissions(&temp.path, fs::Permissions::from_mode(mode))
            .map_err(|e| io(path, e))?;
    }
    if read(path)? != original {
        return Err(AdapterError::ChangedDuringWrite { path: path.into() });
    }
    File::open(&temp.path)
        .and_then(|f| sync_file(&f, &temp.path))
        .map_err(|e| io(path, e))?;
    publish_namespace(path, candidate, "rename", || fs::rename(&temp.path, path))?;
    sync_publication_dir(parent, path, candidate)
}

/// Prepare a bounded, structurally checked candidate without filesystem I/O.
pub fn prepare<F>(provider: Provider, original: &[u8], mutate: F) -> Result<Vec<u8>, AdapterError>
where
    F: FnOnce(&str) -> Result<String, AdapterError>,
{
    if original.len() as u64 > max_transcript_bytes() {
        return Err(AdapterError::InvalidEdit("transcript exceeds byte limit"));
    }
    let text = std::str::from_utf8(original)
        .map_err(|_| AdapterError::InvalidEdit("transcript is not UTF-8"))?;
    let candidate = mutate(text)?.into_bytes();
    if candidate.len() as u64 > max_transcript_bytes() {
        return Err(AdapterError::InvalidEdit(
            "candidate exceeds transcript byte limit",
        ));
    }
    if candidate != original {
        let before = verify::verify(provider, original);
        let after = verify::verify(provider, &candidate);
        if after.iter().any(|finding| !before.contains(finding)) {
            return Err(AdapterError::InvalidEdit(
                "candidate introduces verification findings",
            ));
        }
    }
    Ok(candidate)
}

pub(crate) fn append_record(
    candidate: &mut String,
    record: &serde_json::Value,
) -> Result<(), AdapterError> {
    if !candidate.is_empty() && !candidate.ends_with('\n') {
        candidate.push('\n');
    }
    candidate.push_str(
        &serde_json::to_string(record)
            .map_err(|_| AdapterError::InvalidEdit("record is not serializable"))?,
    );
    candidate.push('\n');
    Ok(())
}

#[cfg(test)]
pub(crate) mod faults {
    use super::*;
    use std::cell::RefCell;
    #[derive(Clone, Copy)]
    pub(crate) enum Action {
        Error,
        Kill,
    }
    struct Spec {
        stage: &'static str,
        path: Option<PathBuf>,
        after: bool,
        left: usize,
        action: Action,
    }
    thread_local! { static SPEC: RefCell<Option<Spec>> = const { RefCell::new(None) }; }
    pub(crate) struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            SPEC.with(|s| *s.borrow_mut() = None);
        }
    }
    pub(crate) fn install(
        stage: &'static str,
        path: Option<&Path>,
        after: bool,
        occurrence: usize,
        action: Action,
    ) -> Guard {
        assert!(occurrence > 0);
        SPEC.with(|s| {
            assert!(s.borrow().is_none());
            *s.borrow_mut() = Some(Spec {
                stage,
                path: path.map(Path::to_path_buf),
                after,
                left: occurrence,
                action,
            });
        });
        Guard
    }
    pub(super) fn hit(stage: &'static str, path: &Path, after: bool) -> std::io::Result<()> {
        let action = SPEC.with(|s| {
            let mut s = s.borrow_mut();
            let spec = s.as_mut()?;
            if spec.stage != stage
                || spec.after != after
                || spec.path.as_deref().is_some_and(|p| p != path)
            {
                return None;
            }
            spec.left -= 1;
            if spec.left != 0 {
                return None;
            }
            let action = spec.action;
            *s = None;
            Some(action)
        });
        match action {
            None => Ok(()),
            Some(Action::Error) => Err(std::io::Error::other("injected storage failure")),
            Some(Action::Kill) => {
                // Test-only: terminate this fixture process, never another PID.
                #[cfg(unix)]
                unsafe {
                    libc::raise(libc::SIGKILL);
                }
                std::process::exit(137)
            }
        }
    }
}
