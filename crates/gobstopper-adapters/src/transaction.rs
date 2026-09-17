use crate::{verify, AdapterError};
use gobstopper_core::Provider;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const MAX_TRANSCRIPT_BYTES: u64 = 128 * 1024 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(0);

pub(crate) fn io(path: &Path, source: std::io::Error) -> AdapterError {
    AdapterError::Io { path: path.into(), source }
}

pub fn read(path: &Path) -> Result<Vec<u8>, AdapterError> {
    let meta = fs::symlink_metadata(path).map_err(|e| io(path, e))?;
    if !meta.is_file() || meta.len() > MAX_TRANSCRIPT_BYTES {
        return Err(AdapterError::InvalidEdit("source must be a bounded regular file"));
    }
    let file = File::open(path).map_err(|e| io(path, e))?;
    let mut bytes = Vec::new();
    file.take(MAX_TRANSCRIPT_BYTES + 1).read_to_end(&mut bytes).map_err(|e| io(path, e))?;
    if bytes.len() as u64 > MAX_TRANSCRIPT_BYTES {
        return Err(AdapterError::InvalidEdit("transcript exceeds byte limit"));
    }
    Ok(bytes)
}

pub(crate) fn sync_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    Ok(())
}

pub(crate) struct Temporary {
    pub path: PathBuf,
}

impl Temporary {
    pub fn new(parent: &Path, bytes: &[u8]) -> Result<Self, AdapterError> {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        let path = parent.join(format!(".gobstopper-{}-{stamp}-{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|e| io(&path, e))?;
        let temp = Self { path };
        file.write_all(bytes).map_err(|e| io(&temp.path, e))?;
        Ok(temp)
    }
}

impl Drop for Temporary {
    fn drop(&mut self) { let _ = fs::remove_file(&self.path); }
}

pub(crate) fn private_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        if !parent.exists() { private_dir(parent)?; }
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)] {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => {},
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {},
        Err(e) => return Err(e),
    }
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(std::io::Error::other("private directory must not be a symlink"));
    }
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    sync_dir(path)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) { sync_dir(parent)?; }
    Ok(())
}

pub fn publish_new(path: &Path, bytes: &[u8]) -> Result<(), AdapterError> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let temp = Temporary::new(parent, bytes)?;
    File::open(&temp.path).and_then(|f| f.sync_all()).map_err(|e| io(path, e))?;
    fs::hard_link(&temp.path, path).map_err(|e| io(path, e))?;
    sync_dir(parent).map_err(|e| io(path, e))
}

pub(crate) fn replace(path: &Path, original: &[u8], candidate: &[u8]) -> Result<(), AdapterError> {
    if read(path)? != original {
        return Err(AdapterError::ChangedDuringWrite { path: path.into() });
    }
    if candidate == original { return Ok(()); }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let temp = Temporary::new(parent, candidate)?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path).map_err(|e| io(path, e))?.permissions().mode();
        fs::set_permissions(&temp.path, fs::Permissions::from_mode(mode)).map_err(|e| io(path, e))?;
    }
    if read(path)? != original {
        return Err(AdapterError::ChangedDuringWrite { path: path.into() });
    }
    File::open(&temp.path).and_then(|f| f.sync_all()).map_err(|e| io(path, e))?;
    fs::rename(&temp.path, path).map_err(|e| io(path, e))?;
    sync_dir(parent).map_err(|e| io(path, e))
}

pub(crate) fn apply<F>(provider: Provider, path: &Path, mutate: F) -> Result<u64, AdapterError>
where F: FnOnce(&str) -> Result<String, AdapterError> {
    let original = read(path)?;
    let text = std::str::from_utf8(&original).map_err(|_| AdapterError::InvalidEdit("transcript is not UTF-8"))?;
    let candidate = mutate(text)?.into_bytes();
    if candidate.len() as u64 > MAX_TRANSCRIPT_BYTES {
        return Err(AdapterError::InvalidEdit("candidate exceeds transcript byte limit"));
    }
    if candidate != original {
        let before = verify::verify(provider, &original);
        let after = verify::verify(provider, &candidate);
        if after.iter().any(|finding| !before.contains(finding)) {
            return Err(AdapterError::InvalidEdit("candidate introduces verification findings"));
        }
    }
    replace(path, &original, &candidate)?;
    Ok((original.len() as u64).saturating_sub(candidate.len() as u64))
}

pub(crate) fn append_record(candidate: &mut String, record: &serde_json::Value) -> Result<(), AdapterError> {
    if !candidate.is_empty() && !candidate.ends_with('\n') { candidate.push('\n'); }
    candidate.push_str(&serde_json::to_string(record).map_err(|_| AdapterError::InvalidEdit("record is not serializable"))?);
    candidate.push('\n');
    Ok(())
}
