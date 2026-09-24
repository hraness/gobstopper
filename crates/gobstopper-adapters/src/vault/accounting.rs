//! Bounded observations of known vault directories and the index schema.
//!
//! Object, manifest, pin, and operation contents are never opened. Shared
//! custody excludes prune but permits publication, so even unchanged directory
//! metadata does not make the observation an atomic snapshot or a recovery audit.

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountingStatus {
    Complete,
    Incomplete,
    Busy,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanStatus {
    NotExamined,
    Missing,
    Complete,
    Incomplete,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountingIssue {
    MissingRoot,
    UnsupportedPlatform,
    InvalidRoot,
    CustodyBusy,
    IndexBusy,
    Symlink,
    UnexpectedEntry,
    IoFailure,
    EntryLimit,
    IndexByteLimit,
    IndexEntryLimit,
    IndexLineLimit,
    ElapsedLimit,
    InvalidIndexEntry,
    IncompleteIndexTail,
    MetadataChanged,
    CountOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryKind {
    Chunks,
    LegacyObjects,
    Records,
    Manifests,
    Pins,
    Operations,
}

impl DirectoryKind {
    const ALL: [Self; 6] = [
        Self::Chunks,
        Self::LegacyObjects,
        Self::Records,
        Self::Manifests,
        Self::Pins,
        Self::Operations,
    ];

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn name(self) -> &'static str {
        match self {
            Self::Chunks => "chunks",
            Self::LegacyObjects => "objects",
            Self::Records => "records",
            Self::Manifests => "manifests",
            Self::Pins => "pins",
            Self::Operations => "operations",
        }
    }
}

/// Categories of the recorded strategy label, not authenticated event origins.
/// Unrecognized labels never reach the output as arbitrary text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexOrigin {
    PreCompact,
    PostCompact,
    PreUndo,
    OtherStrategy,
    Unspecified,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct AccountingLimits {
    pub max_directory_entries: u64,
    pub max_index_bytes: u64,
    pub max_index_entries: u64,
    pub max_index_line_bytes: usize,
    /// Cooperative budget checked between filesystem operations, not a deadline
    /// on an individual kernel syscall or a guarantee for network filesystems.
    pub max_elapsed_ms: u64,
}

impl Default for AccountingLimits {
    fn default() -> Self {
        Self {
            max_directory_entries: 200_000,
            max_index_bytes: 8 * 1024 * 1024,
            max_index_entries: 50_000,
            max_index_line_bytes: super::MAX_INDEX_LINE_BYTES,
            max_elapsed_ms: 3_000,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DirectoryAccounting {
    pub status: ScanStatus,
    /// Sums include all observed regular files, including unexpected names.
    /// Null means the directory was unavailable or not examined.
    pub files: Option<u64>,
    pub logical_bytes: Option<u64>,
    pub entries_examined: u64,
    pub skipped_symlinks: u64,
    pub skipped_special_or_nested_entries: u64,
    pub unexpected_names: u64,
}

impl Default for DirectoryAccounting {
    fn default() -> Self {
        Self {
            status: ScanStatus::NotExamined,
            files: None,
            logical_bytes: None,
            entries_examined: 0,
            skipped_symlinks: 0,
            skipped_special_or_nested_entries: 0,
            unexpected_names: 0,
        }
    }
}

#[derive(Debug, Default, Serialize)]
pub struct IndexCounts {
    pub valid_entries: u64,
    pub invalid_entries: u64,
    /// Repeated (provider, source path, session, root digest) references.
    pub duplicate_entries: u64,
    pub distinct_source_references: u64,
    /// Index root references only; no manifest edges or pin contents are read.
    pub distinct_object_references: u64,
    /// Sum of declared source bytes across valid entries, including duplicates.
    pub referenced_source_logical_bytes: u64,
    pub origins: BTreeMap<IndexOrigin, u64>,
}

#[derive(Debug, Serialize)]
pub struct IndexAccounting {
    pub status: ScanStatus,
    pub observed_file_bytes: Option<u64>,
    pub bytes_read: u64,
    pub lines_examined: u64,
    pub counts: Option<IndexCounts>,
}

impl Default for IndexAccounting {
    fn default() -> Self {
        Self {
            status: ScanStatus::NotExamined,
            observed_file_bytes: None,
            bytes_read: 0,
            lines_examined: 0,
            counts: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct VaultAccounting {
    pub schema: &'static str,
    /// Complete means the bounded metadata/schema walk finished, not that any
    /// payload, recovery reference, physical size, or reclaimability was checked.
    pub status: AccountingStatus,
    pub scope: &'static str,
    pub consistency: &'static str,
    pub started_at_unix_ms: u64,
    pub elapsed_ms: u64,
    pub limits: AccountingLimits,
    pub directory_entries_examined: u64,
    pub directories: BTreeMap<DirectoryKind, DirectoryAccounting>,
    pub index: IndexAccounting,
    /// This compares directory/index identity, size, mtime, and ctime at the
    /// observation endpoints. It makes no claim about intervening changes.
    pub observed_directory_and_index_metadata_stable: Option<bool>,
    pub object_contents_read: bool,
    pub recovery_references_validated: bool,
    pub physical_bytes: Option<u64>,
    pub reclaimable_bytes: Option<u64>,
    pub issues: Vec<AccountingIssue>,
}

impl VaultAccounting {
    fn new(limits: AccountingLimits) -> Self {
        Self {
            schema: "gobstopper-vault-accounting-v1",
            status: AccountingStatus::Incomplete,
            scope: "known_vault_directories_and_index_schema",
            consistency: "non_atomic_metadata_window",
            started_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis().min(u64::MAX as u128) as u64)
                .unwrap_or(0),
            elapsed_ms: 0,
            limits,
            directory_entries_examined: 0,
            directories: DirectoryKind::ALL
                .into_iter()
                .map(|kind| (kind, DirectoryAccounting::default()))
                .collect(),
            index: IndexAccounting::default(),
            observed_directory_and_index_metadata_stable: None,
            object_contents_read: false,
            recovery_references_validated: false,
            physical_bytes: None,
            reclaimable_bytes: None,
            issues: Vec::new(),
        }
    }

    fn issue(&mut self, issue: AccountingIssue) {
        if !self.issues.contains(&issue) {
            self.issues.push(issue);
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn within_time(&mut self, start: Instant) -> bool {
        if start.elapsed().as_millis() >= self.limits.max_elapsed_ms as u128 {
            self.issue(AccountingIssue::ElapsedLimit);
            false
        } else {
            true
        }
    }
}

/// Inspect existing vault metadata without creating files or waiting for locks.
/// The root and each traversed directory component must not be a symlink.
/// Missing roots and unsupported custody platforms return unavailable evidence.
pub fn inspect(root: &Path) -> VaultAccounting {
    inspect_with_limits(root, AccountingLimits::default())
}

fn inspect_with_limits(root: &Path, limits: AccountingLimits) -> VaultAccounting {
    let start = Instant::now();
    let mut report = VaultAccounting::new(limits);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    unix::scan(root, start, &mut report);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = root;
        report.status = AccountingStatus::Unavailable;
        report.issue(AccountingIssue::UnsupportedPlatform);
    }
    report.elapsed_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
    report.issues.sort();
    report
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
// libc's mode_t and stat field widths differ on the supported platforms.
#[allow(clippy::unnecessary_cast)]
mod unix {
    use super::*;
    use std::collections::BTreeSet;
    use std::ffi::{CStr, CString};
    use std::fs::File;
    use std::io::{self, Read};
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;
    use std::ptr::NonNull;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Node {
        device: u64,
        inode: u64,
        size: u64,
        mode: u32,
        mtime: (i64, i64),
        ctime: (i64, i64),
    }

    impl Node {
        fn from_stat(raw: libc::stat) -> io::Result<Self> {
            // libc field widths differ between supported macOS/Linux targets.
            #[allow(clippy::unnecessary_cast)]
            Ok(Self {
                device: raw.st_dev as u64,
                inode: raw.st_ino as u64,
                size: u64::try_from(raw.st_size)
                    .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?,
                mode: raw.st_mode as u32,
                mtime: (raw.st_mtime as i64, raw.st_mtime_nsec as i64),
                ctime: (raw.st_ctime as i64, raw.st_ctime_nsec as i64),
            })
        }

        fn is_type(self, kind: u32) -> bool {
            self.mode & libc::S_IFMT as u32 == kind
        }
    }

    fn node_at(parent: &File, name: &CStr) -> io::Result<Option<Node>> {
        let mut raw = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: parent is live, name is NUL-terminated, and fstatat initializes
        // raw only on success. AT_SYMLINK_NOFOLLOW never follows an entry link.
        let result = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                raw.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result == 0 {
            // SAFETY: successful fstatat initialized the complete stat value.
            Node::from_stat(unsafe { raw.assume_init() }).map(Some)
        } else {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(error)
            }
        }
    }

    fn node_file(file: &File) -> io::Result<Node> {
        let mut raw = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: file remains live and fstat initializes raw on success.
        if unsafe { libc::fstat(file.as_raw_fd(), raw.as_mut_ptr()) } == 0 {
            // SAFETY: fstat succeeded.
            Node::from_stat(unsafe { raw.assume_init() })
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn open_at(parent: &File, name: &CStr, directory: bool) -> io::Result<File> {
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if directory { libc::O_DIRECTORY } else { 0 };
        // SAFETY: parent and name remain valid during openat. No create flag or
        // permission-changing operation is used, and no contents are read here.
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: openat returned one new owned descriptor.
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }

    fn open_root(path: &Path) -> io::Result<File> {
        if path.as_os_str().as_bytes().len() > 8192 || path.components().count() > 128 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut parent = File::open(if path.is_absolute() { "/" } else { "." })?;
        for component in path.components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(name) => {
                    let name = CString::new(name.as_bytes())
                        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
                    parent = open_at(&parent, &name, true)?;
                }
                _ => return Err(io::ErrorKind::InvalidInput.into()),
            }
        }
        Ok(parent)
    }

    struct DirectoryStream(NonNull<libc::DIR>);

    impl DirectoryStream {
        fn new(file: File) -> io::Result<Self> {
            let fd = file.into_raw_fd();
            // SAFETY: fd is owned and references a directory. On success DIR
            // owns it; on failure we close the still-owned descriptor below.
            match NonNull::new(unsafe { libc::fdopendir(fd) }) {
                Some(pointer) => Ok(Self(pointer)),
                None => {
                    let error = io::Error::last_os_error();
                    // SAFETY: failed fdopendir did not consume fd.
                    unsafe { libc::close(fd) };
                    Err(error)
                }
            }
        }

        fn next(&mut self) -> io::Result<Option<CString>> {
            #[cfg(target_os = "macos")]
            let errno = unsafe { libc::__error() };
            #[cfg(target_os = "linux")]
            let errno = unsafe { libc::__errno_location() };
            // SAFETY: errno is this thread's live errno location. readdir uses
            // this exclusively owned DIR; copy d_name before the next call.
            unsafe {
                *errno = 0;
                let entry = libc::readdir(self.0.as_ptr());
                if entry.is_null() {
                    if *errno == 0 {
                        Ok(None)
                    } else {
                        Err(io::Error::last_os_error())
                    }
                } else {
                    Ok(Some(CStr::from_ptr((*entry).d_name.as_ptr()).to_owned()))
                }
            }
        }
    }

    impl Drop for DirectoryStream {
        fn drop(&mut self) {
            // SAFETY: this is the sole owner of the live DIR and its descriptor.
            unsafe { libc::closedir(self.0.as_ptr()) };
        }
    }

    fn busy(error: &io::Error) -> bool {
        error.kind() == io::ErrorKind::WouldBlock || error.raw_os_error() == Some(libc::EWOULDBLOCK)
    }

    fn directory_name(kind: DirectoryKind) -> CString {
        CString::new(kind.name()).expect("fixed directory name has no NUL")
    }

    fn valid_name(kind: DirectoryKind, name: &CStr) -> bool {
        let Ok(name) = name.to_str() else {
            return false;
        };
        let digest = match kind {
            DirectoryKind::Pins => name
                .strip_prefix("native-")
                .and_then(|name| name.strip_suffix(".json")),
            DirectoryKind::Operations => name
                .strip_suffix(".json")
                .or_else(|| name.strip_suffix(".lock")),
            _ => Some(name),
        };
        digest.is_some_and(|digest| digest.len() == 64 && super::super::is_hex(digest))
    }

    fn scan_directory(
        root: &File,
        kind: DirectoryKind,
        before: Option<Node>,
        start: Instant,
        report: &mut VaultAccounting,
    ) {
        let mut counts = DirectoryAccounting::default();
        let Some(before) = before else {
            counts.status = ScanStatus::Missing;
            counts.files = Some(0);
            counts.logical_bytes = Some(0);
            report.directories.insert(kind, counts);
            return;
        };
        if !before.is_type(libc::S_IFDIR as u32) {
            counts.status = ScanStatus::Unavailable;
            report.issue(if before.is_type(libc::S_IFLNK as u32) {
                AccountingIssue::Symlink
            } else {
                AccountingIssue::UnexpectedEntry
            });
            report.directories.insert(kind, counts);
            return;
        }
        if !report.within_time(start) {
            return;
        }
        let result = (|| -> io::Result<()> {
            let directory = open_at(root, &directory_name(kind), true)?;
            if node_file(&directory)? != before {
                report.issue(AccountingIssue::MetadataChanged);
                return Ok(());
            }
            let mut stream = DirectoryStream::new(directory.try_clone()?)?;
            counts.files = Some(0);
            counts.logical_bytes = Some(0);
            counts.status = ScanStatus::Complete;
            loop {
                if !report.within_time(start) {
                    counts.status = ScanStatus::Incomplete;
                    break;
                }
                // One extra readdir identifies exhaustion without another stat.
                let Some(name) = stream.next()? else { break };
                if name.as_bytes() == b"." || name.as_bytes() == b".." {
                    continue;
                }
                if report.directory_entries_examined >= report.limits.max_directory_entries {
                    report.issue(AccountingIssue::EntryLimit);
                    counts.status = ScanStatus::Incomplete;
                    break;
                }
                report.directory_entries_examined += 1;
                counts.entries_examined += 1;
                let Some(node) = node_at(&directory, &name)? else {
                    report.issue(AccountingIssue::MetadataChanged);
                    counts.status = ScanStatus::Incomplete;
                    continue;
                };
                if node.is_type(libc::S_IFLNK as u32) {
                    counts.skipped_symlinks += 1;
                    counts.status = ScanStatus::Incomplete;
                    report.issue(AccountingIssue::Symlink);
                } else if !node.is_type(libc::S_IFREG as u32) {
                    counts.skipped_special_or_nested_entries += 1;
                    counts.status = ScanStatus::Incomplete;
                    report.issue(AccountingIssue::UnexpectedEntry);
                } else {
                    counts.files = counts.files.and_then(|n| n.checked_add(1));
                    counts.logical_bytes =
                        counts.logical_bytes.and_then(|n| n.checked_add(node.size));
                    if counts.files.is_none() || counts.logical_bytes.is_none() {
                        report.issue(AccountingIssue::CountOverflow);
                        counts.status = ScanStatus::Incomplete;
                        break;
                    }
                    if !valid_name(kind, &name) {
                        counts.unexpected_names += 1;
                        counts.status = ScanStatus::Incomplete;
                        report.issue(AccountingIssue::UnexpectedEntry);
                    }
                }
            }
            Ok(())
        })();
        if result.is_err() {
            counts.status = if counts.files.is_some() {
                ScanStatus::Incomplete
            } else {
                ScanStatus::Unavailable
            };
            report.issue(AccountingIssue::IoFailure);
        }
        report.directories.insert(kind, counts);
    }

    fn scan_index(file: &mut File, before: Node, start: Instant, report: &mut VaultAccounting) {
        report.index.observed_file_bytes = Some(before.size);
        report.index.status = ScanStatus::Complete;
        let mut bytes = Vec::new();
        let target = before.size.min(report.limits.max_index_bytes);
        let mut buffer = [0; 8192];
        while (bytes.len() as u64) < target {
            if !report.within_time(start) {
                report.index.status = ScanStatus::Incomplete;
                break;
            }
            let take = (target - bytes.len() as u64).min(buffer.len() as u64) as usize;
            match file.read(&mut buffer[..take]) {
                Ok(0) => {
                    report.issue(AccountingIssue::MetadataChanged);
                    report.index.status = ScanStatus::Incomplete;
                    break;
                }
                Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                Err(_) => {
                    report.issue(AccountingIssue::IoFailure);
                    report.index.status = ScanStatus::Incomplete;
                    break;
                }
            }
        }
        report.index.bytes_read = bytes.len() as u64;
        if before.size > report.limits.max_index_bytes {
            report.issue(AccountingIssue::IndexByteLimit);
            report.index.status = ScanStatus::Incomplete;
        }
        let complete_bytes = bytes.len() as u64 == before.size;
        let mut counts = IndexCounts::default();
        let mut bindings = BTreeSet::new();
        let mut roots = BTreeSet::new();
        for line in bytes.split_inclusive(|byte| *byte == b'\n') {
            if !report.within_time(start) {
                report.index.status = ScanStatus::Incomplete;
                break;
            }
            if report.index.lines_examined >= report.limits.max_index_entries {
                report.issue(AccountingIssue::IndexEntryLimit);
                report.index.status = ScanStatus::Incomplete;
                break;
            }
            let Some(line) = line.strip_suffix(b"\n") else {
                if complete_bytes {
                    counts.invalid_entries += 1;
                    report.index.lines_examined += 1;
                    report.issue(AccountingIssue::IncompleteIndexTail);
                }
                report.index.status = ScanStatus::Incomplete;
                break;
            };
            report.index.lines_examined += 1;
            if line.len() > report.limits.max_index_line_bytes {
                counts.invalid_entries += 1;
                report.issue(AccountingIssue::IndexLineLimit);
                report.index.status = ScanStatus::Incomplete;
                continue;
            }
            let entry = serde_json::from_slice::<super::super::VaultEntry>(line)
                .ok()
                .filter(super::super::valid_entry);
            let Some(entry) = entry else {
                counts.invalid_entries += 1;
                report.issue(AccountingIssue::InvalidIndexEntry);
                report.index.status = ScanStatus::Incomplete;
                continue;
            };
            let Some(total) = counts
                .referenced_source_logical_bytes
                .checked_add(entry.bytes)
            else {
                report.issue(AccountingIssue::CountOverflow);
                report.index.status = ScanStatus::Incomplete;
                break;
            };
            counts.referenced_source_logical_bytes = total;
            counts.valid_entries += 1;
            let origin = match entry.strategy.as_deref() {
                Some("pre-compact") => IndexOrigin::PreCompact,
                Some("post-compact") => IndexOrigin::PostCompact,
                Some("pre-undo") => IndexOrigin::PreUndo,
                Some(_) => IndexOrigin::OtherStrategy,
                None => IndexOrigin::Unspecified,
            };
            *counts.origins.entry(origin).or_default() += 1;
            roots.insert(entry.sha256.clone());
            if !bindings.insert((
                entry.provider.as_str(),
                entry.path,
                entry.session_id,
                entry.sha256,
            )) {
                counts.duplicate_entries += 1;
            }
        }
        counts.distinct_source_references = bindings.len() as u64;
        counts.distinct_object_references = roots.len() as u64;
        report.index.counts = Some(counts);
    }

    pub(super) fn scan(root_path: &Path, start: Instant, report: &mut VaultAccounting) {
        scan_with_hook(root_path, start, report, |_| {});
    }

    pub(super) fn scan_with_hook(
        root_path: &Path,
        mut start: Instant,
        report: &mut VaultAccounting,
        after_scan: impl FnOnce(&mut Instant),
    ) {
        if !report.within_time(start) {
            return;
        }
        let root = match open_root(root_path) {
            Ok(file) => file,
            Err(error) => {
                report.status = AccountingStatus::Unavailable;
                report.issue(if error.kind() == io::ErrorKind::NotFound {
                    AccountingIssue::MissingRoot
                } else if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR))
                    || error.kind() == io::ErrorKind::InvalidInput
                {
                    AccountingIssue::InvalidRoot
                } else {
                    AccountingIssue::IoFailure
                });
                return;
            }
        };
        if let Err(error) = fs2::FileExt::try_lock_shared(&root) {
            report.status = if busy(&error) {
                AccountingStatus::Busy
            } else {
                AccountingStatus::Unavailable
            };
            report.issue(if busy(&error) {
                AccountingIssue::CustodyBusy
            } else {
                AccountingIssue::IoFailure
            });
            return;
        }
        let Ok(root_before) = node_file(&root) else {
            report.status = AccountingStatus::Unavailable;
            report.issue(AccountingIssue::IoFailure);
            return;
        };
        let index_name = CString::new("index.jsonl").unwrap();
        let index_before = match node_at(&root, &index_name) {
            Ok(node) => node,
            Err(_) => {
                report.status = AccountingStatus::Unavailable;
                report.index.status = ScanStatus::Unavailable;
                report.issue(AccountingIssue::IoFailure);
                return;
            }
        };
        let mut index_file = None;
        if let Some(before) = index_before {
            if !before.is_type(libc::S_IFREG as u32) {
                report.status = AccountingStatus::Unavailable;
                report.index.status = ScanStatus::Unavailable;
                report.issue(if before.is_type(libc::S_IFLNK as u32) {
                    AccountingIssue::Symlink
                } else {
                    AccountingIssue::UnexpectedEntry
                });
                return;
            }
            let file = match open_at(&root, &index_name, false) {
                Ok(file) if node_file(&file).ok() == Some(before) => file,
                _ => {
                    report.issue(AccountingIssue::MetadataChanged);
                    report.index.status = ScanStatus::Unavailable;
                    return;
                }
            };
            if let Err(error) = fs2::FileExt::try_lock_shared(&file) {
                report.status = if busy(&error) {
                    AccountingStatus::Busy
                } else {
                    AccountingStatus::Unavailable
                };
                report.issue(if busy(&error) {
                    AccountingIssue::IndexBusy
                } else {
                    AccountingIssue::IoFailure
                });
                return;
            }
            index_file = Some(file);
        } else {
            report.index.status = ScanStatus::Missing;
            report.index.observed_file_bytes = Some(0);
            report.index.counts = Some(IndexCounts::default());
        }
        let mut layout = Vec::new();
        for kind in DirectoryKind::ALL {
            match node_at(&root, &directory_name(kind)) {
                Ok(node) => layout.push((kind, node)),
                Err(_) => {
                    report.directories.get_mut(&kind).unwrap().status = ScanStatus::Unavailable;
                    report.issue(AccountingIssue::IoFailure);
                }
            }
        }
        if let (Some(file), Some(before)) = (&mut index_file, index_before) {
            scan_index(file, before, start, report);
        }
        for (kind, before) in &layout {
            scan_directory(&root, *kind, *before, start, report);
        }
        // Private race/deadline test seam; production does not alter the clock.
        after_scan(&mut start);
        // Check known names again through retained directory descriptors. Root
        // path replacement is separately detected by opening its components.
        let stable = node_file(&root).ok() == Some(root_before)
            && open_root(root_path).and_then(|file| node_file(&file)).ok() == Some(root_before)
            && node_at(&root, &index_name).ok() == Some(index_before)
            && index_file.as_ref().map(node_file).transpose().ok() == Some(index_before)
            && layout
                .iter()
                .all(|(kind, before)| node_at(&root, &directory_name(*kind)).ok() == Some(*before));
        report.observed_directory_and_index_metadata_stable = Some(stable && layout.len() == 6);
        if !stable {
            report.issue(AccountingIssue::MetadataChanged);
        }
        // Initial path lookup and the fixed metadata rechecks can also spend
        // the cooperative budget. No syscall is claimed to have a hard timeout.
        report.within_time(start);
        report.status = if report.issues.is_empty() {
            AccountingStatus::Complete
        } else {
            AccountingStatus::Incomplete
        };
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use crate::vault::VaultEntry;
    use gobstopper_core::Provider;
    use std::ffi::CString;
    use std::fs::{self, File};
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            // Darwin's /var alias is a symlink; fixtures deliberately use the
            // existing physical parent because accounting refuses path links.
            let base = std::env::temp_dir().canonicalize().unwrap();
            let root = base.join(format!(
                "gobstopper-accounting-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }

        fn vault(&self) -> PathBuf {
            let root = self.0.join("vault");
            fs::create_dir(&root).unwrap();
            root
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn entry(session: &str, digest: u8, strategy: Option<&str>) -> VaultEntry {
        VaultEntry {
            ts: 1,
            sha256: format!("{digest:064x}"),
            path: PathBuf::from("/private/source-sentinel-do-not-print.jsonl"),
            session_id: session.into(),
            provider: Provider::Codex,
            bytes: 100,
            strategy: strategy.map(str::to_owned),
            record_count: 1,
            source_sha256: String::new(),
        }
    }

    fn index(root: &Path, entries: &[VaultEntry]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for entry in entries {
            serde_json::to_writer(&mut bytes, entry).unwrap();
            bytes.push(b'\n');
        }
        fs::write(root.join("index.jsonl"), &bytes).unwrap();
        bytes
    }

    fn assert_locks_released(root: &Path) {
        let root_lock = File::open(root).unwrap();
        fs2::FileExt::try_lock_exclusive(&root_lock).unwrap();
        if let Ok(index_lock) = File::open(root.join("index.jsonl")) {
            fs2::FileExt::try_lock_exclusive(&index_lock).unwrap();
        }
    }

    #[test]
    fn missing_and_empty_roots_are_distinct_without_creation() {
        let fixture = TestRoot::new();
        let missing = fixture.0.join("missing");
        let report = inspect(&missing);
        assert_eq!(report.status, AccountingStatus::Unavailable);
        assert_eq!(report.issues, [AccountingIssue::MissingRoot]);
        assert!(!missing.exists());
        assert_eq!(report.index.status, ScanStatus::NotExamined);
        assert!(report
            .directories
            .values()
            .all(|counts| counts.files.is_none()));

        let root = fixture.vault();
        let report = inspect(&root);
        assert_eq!(report.status, AccountingStatus::Complete);
        assert_eq!(report.index.status, ScanStatus::Missing);
        assert_eq!(report.index.counts.unwrap().valid_entries, 0);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        assert!(report
            .directories
            .values()
            .all(|counts| counts.status == ScanStatus::Missing));
        assert_eq!(
            report.observed_directory_and_index_metadata_stable,
            Some(true)
        );
        assert_locks_released(&root);
    }

    #[test]
    fn metadata_counts_all_six_categories_without_reading_payloads() {
        let fixture = TestRoot::new();
        let root = fixture.vault();
        for kind in DirectoryKind::ALL {
            let directory = root.join(kind.name());
            fs::create_dir(&directory).unwrap();
            let name = match kind {
                DirectoryKind::Pins => format!("native-{:064x}.json", 1),
                DirectoryKind::Operations => format!("{:064x}.json", 1),
                _ => format!("{:064x}", 1),
            };
            let path = directory.join(name);
            // Invalid and unreadable payloads still have meaningful metadata.
            // No content validation or recovery guarantee is claimed.
            fs::write(&path, b"not-json: private-payload-sentinel").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o0)).unwrap();
        }
        let report = inspect(&root);
        assert_eq!(report.status, AccountingStatus::Complete);
        assert_eq!(report.directory_entries_examined, 6);
        for counts in report.directories.values() {
            assert_eq!(counts.files, Some(1));
            assert_eq!(counts.logical_bytes, Some(34));
            assert_eq!(counts.status, ScanStatus::Complete);
        }
        assert!(!report.object_contents_read);
        assert!(!report.recovery_references_validated);
        assert!(report.physical_bytes.is_none());
        assert!(report.reclaimable_bytes.is_none());
        assert_eq!(report.consistency, "non_atomic_metadata_window");
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("private-payload-sentinel"));
        assert!(!serialized.contains(root.to_str().unwrap()));
        assert_locks_released(&root);
    }

    #[test]
    fn index_references_duplicates_and_origins_do_not_expose_private_labels() {
        let fixture = TestRoot::new();
        let root = fixture.vault();
        let mut another_provider = entry("session-private", 1, Some("post-compact"));
        another_provider.provider = Provider::ClaudeCode;
        let bytes = index(
            &root,
            &[
                entry("session-private", 1, Some("pre-compact")),
                entry("session-private", 1, Some("pre-compact")),
                entry("session-private", 2, Some("private-plugin-label")),
                entry("second-private", 1, None),
                another_provider,
                entry("third-private", 1, Some("pre-undo")),
            ],
        );
        let report = inspect(&root);
        assert_eq!(report.status, AccountingStatus::Complete);
        let counts = report.index.counts.as_ref().unwrap();
        assert_eq!(counts.valid_entries, 6);
        assert_eq!(counts.invalid_entries, 0);
        assert_eq!(counts.duplicate_entries, 1);
        assert_eq!(counts.distinct_source_references, 5);
        assert_eq!(counts.distinct_object_references, 2);
        assert_eq!(counts.referenced_source_logical_bytes, 600);
        assert_eq!(counts.origins[&IndexOrigin::PreCompact], 2);
        assert_eq!(counts.origins[&IndexOrigin::OtherStrategy], 1);
        assert_eq!(counts.origins[&IndexOrigin::Unspecified], 1);
        let serialized = serde_json::to_string(&report).unwrap();
        for private in [
            "session-private",
            "second-private",
            "third-private",
            "source-sentinel",
            "private-plugin-label",
        ] {
            assert!(!serialized.contains(private));
        }
        assert_eq!(fs::read(root.join("index.jsonl")).unwrap(), bytes);
        assert_locks_released(&root);
    }

    #[test]
    fn malformed_entries_and_incomplete_tail_preserve_partial_denominators() {
        let fixture = TestRoot::new();
        let root = fixture.vault();
        let mut bytes = index(&root, &[entry("session", 1, None)]);
        bytes.extend_from_slice(b"{}\n[]\n\n{not-json}\n");
        bytes.extend_from_slice(&serde_json::to_vec(&entry("tail", 2, None)).unwrap());
        fs::write(root.join("index.jsonl"), &bytes).unwrap();
        let report = inspect(&root);
        assert_eq!(report.status, AccountingStatus::Incomplete);
        assert_eq!(report.index.status, ScanStatus::Incomplete);
        let counts = report.index.counts.unwrap();
        assert_eq!(counts.valid_entries, 1);
        assert_eq!(counts.invalid_entries, 5);
        assert_eq!(report.index.lines_examined, 6);
        assert!(report
            .issues
            .contains(&AccountingIssue::IncompleteIndexTail));
        assert!(report.issues.contains(&AccountingIssue::InvalidIndexEntry));
        assert_eq!(fs::read(root.join("index.jsonl")).unwrap(), bytes);
        assert_locks_released(&root);
    }

    #[test]
    fn index_byte_line_and_entry_limits_are_explicit_and_release_locks() {
        let fixture = TestRoot::new();
        let root = fixture.vault();
        let first = serde_json::to_vec(&entry("first", 1, None)).unwrap().len() as u64 + 1;
        index(&root, &[entry("first", 1, None), entry("second", 2, None)]);
        let limits = AccountingLimits {
            max_index_bytes: first + 3,
            ..AccountingLimits::default()
        };
        let report = inspect_with_limits(&root, limits);
        assert_eq!(report.index.bytes_read, first + 3);
        assert_eq!(report.index.counts.as_ref().unwrap().valid_entries, 1);
        assert_eq!(report.index.counts.as_ref().unwrap().invalid_entries, 0);
        assert!(report.issues.contains(&AccountingIssue::IndexByteLimit));
        assert_locks_released(&root);

        let report = inspect_with_limits(
            &root,
            AccountingLimits {
                max_index_entries: 1,
                ..AccountingLimits::default()
            },
        );
        assert_eq!(report.index.lines_examined, 1);
        assert!(report.issues.contains(&AccountingIssue::IndexEntryLimit));
        assert_locks_released(&root);

        let report = inspect_with_limits(
            &root,
            AccountingLimits {
                max_index_line_bytes: 8,
                ..AccountingLimits::default()
            },
        );
        assert_eq!(report.index.counts.unwrap().invalid_entries, 2);
        assert!(report.issues.contains(&AccountingIssue::IndexLineLimit));
        assert_locks_released(&root);
    }

    fn contended_inspect(root: PathBuf, lock: File) -> VaultAccounting {
        fs2::FileExt::try_lock_exclusive(&lock).unwrap();
        let (send, receive) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let report = inspect(&root);
            send.send(()).unwrap();
            report
        });
        let completed_without_release = receive.recv_timeout(Duration::from_secs(2)).is_ok();
        drop(lock);
        let report = worker.join().unwrap();
        assert!(
            completed_without_release,
            "accounting waited for another lock owner"
        );
        report
    }

    #[test]
    fn root_and_index_contention_return_busy_without_mutation_or_waiting() {
        let fixture = TestRoot::new();
        let root = fixture.vault();
        let original = index(&root, &[entry("session", 1, None)]);
        let report = contended_inspect(root.clone(), File::open(&root).unwrap());
        assert_eq!(report.status, AccountingStatus::Busy);
        assert_eq!(report.issues, [AccountingIssue::CustodyBusy]);
        assert!(report.index.counts.is_none());
        let report = contended_inspect(root.clone(), File::open(root.join("index.jsonl")).unwrap());
        assert_eq!(report.status, AccountingStatus::Busy);
        assert_eq!(report.issues, [AccountingIssue::IndexBusy]);
        assert_eq!(report.index.bytes_read, 0);
        assert_eq!(fs::read(root.join("index.jsonl")).unwrap(), original);
        assert_locks_released(&root);
    }

    #[test]
    fn symlink_roots_ancestors_directories_and_index_are_never_followed() {
        let fixture = TestRoot::new();
        let root = fixture.vault();
        index(&root, &[entry("private-session", 1, None)]);
        let alias = fixture.0.join("root-link");
        symlink(&root, &alias).unwrap();
        assert_eq!(inspect(&alias).status, AccountingStatus::Unavailable);
        let parent_alias = fixture.0.join("parent-link");
        symlink(&fixture.0, &parent_alias).unwrap();
        assert_eq!(
            inspect(&parent_alias.join("vault")).status,
            AccountingStatus::Unavailable
        );
        let other = fixture.0.join("outside");
        fs::create_dir(&other).unwrap();
        fs::write(other.join(format!("{:064x}", 1)), b"outside-content").unwrap();
        symlink(&other, root.join("chunks")).unwrap();
        let report = inspect(&root);
        assert_eq!(report.status, AccountingStatus::Incomplete);
        assert!(report.issues.contains(&AccountingIssue::Symlink));
        assert!(report.directories[&DirectoryKind::Chunks].files.is_none());
        fs::remove_file(root.join("index.jsonl")).unwrap();
        symlink(other.join(format!("{:064x}", 1)), root.join("index.jsonl")).unwrap();
        let report = inspect(&root);
        assert_eq!(report.status, AccountingStatus::Unavailable);
        assert_eq!(report.index.bytes_read, 0);
        assert!(report.issues.contains(&AccountingIssue::Symlink));
        assert_eq!(
            fs::read(other.join(format!("{:064x}", 1))).unwrap(),
            b"outside-content"
        );
    }

    #[test]
    fn nested_special_and_unexpected_entries_are_counted_without_recursion() {
        let fixture = TestRoot::new();
        let root = fixture.vault();
        let chunks = root.join("chunks");
        fs::create_dir(&chunks).unwrap();
        fs::write(chunks.join("unexpected"), b"abc").unwrap();
        let nested = chunks.join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("hidden"), b"must-not-count").unwrap();
        symlink(&nested, chunks.join(format!("{:064x}", 2))).unwrap();
        let pipe = CString::new(chunks.join("pipe").as_os_str().as_bytes()).unwrap();
        // SAFETY: the fixture owns this new path and mode creates only a FIFO.
        assert_eq!(unsafe { libc::mkfifo(pipe.as_ptr(), 0o600) }, 0);
        let report = inspect(&root);
        let counts = &report.directories[&DirectoryKind::Chunks];
        assert_eq!(report.status, AccountingStatus::Incomplete);
        assert_eq!(counts.files, Some(1));
        assert_eq!(counts.logical_bytes, Some(3));
        assert_eq!(counts.unexpected_names, 1);
        assert_eq!(counts.skipped_symlinks, 1);
        assert_eq!(counts.skipped_special_or_nested_entries, 2);
        assert_eq!(counts.entries_examined, 4);
        assert_locks_released(&root);
    }

    #[test]
    fn directory_and_time_limits_leave_explicit_partial_results_and_no_locks() {
        let fixture = TestRoot::new();
        let root = fixture.vault();
        let chunks = root.join("chunks");
        fs::create_dir(&chunks).unwrap();
        for number in 0..4 {
            fs::write(chunks.join(format!("{number:064x}")), b"abc").unwrap();
        }
        index(&root, &[]);
        let report = inspect_with_limits(
            &root,
            AccountingLimits {
                max_directory_entries: 2,
                ..AccountingLimits::default()
            },
        );
        assert_eq!(report.status, AccountingStatus::Incomplete);
        assert_eq!(report.directory_entries_examined, 2);
        assert_eq!(
            report.directories[&DirectoryKind::Chunks].logical_bytes,
            Some(6)
        );
        assert!(report.issues.contains(&AccountingIssue::EntryLimit));
        assert_locks_released(&root);
        let report = inspect_with_limits(
            &root,
            AccountingLimits {
                max_elapsed_ms: 0,
                ..AccountingLimits::default()
            },
        );
        assert_eq!(report.status, AccountingStatus::Incomplete);
        assert_eq!(report.directory_entries_examined, 0);
        assert_eq!(report.issues, [AccountingIssue::ElapsedLimit]);
        assert!(report.index.counts.is_none());
        assert_locks_released(&root);

        let mut report = VaultAccounting::new(AccountingLimits::default());
        unix::scan_with_hook(&root, Instant::now(), &mut report, |start| {
            // Deterministically model time spent after traversal, without a
            // scheduler-dependent sleep or changing any filesystem contents.
            *start -= Duration::from_millis(AccountingLimits::default().max_elapsed_ms + 1);
        });
        assert_eq!(report.status, AccountingStatus::Incomplete);
        assert!(report.issues.contains(&AccountingIssue::ElapsedLimit));
        assert_eq!(report.directories[&DirectoryKind::Chunks].files, Some(4));
        assert_locks_released(&root);
    }

    #[test]
    fn publication_during_shared_observation_is_incomplete_not_an_atomic_snapshot() {
        let fixture = TestRoot::new();
        let root = fixture.vault();
        fs::create_dir(root.join("chunks")).unwrap();
        index(&root, &[]);
        let mut report = VaultAccounting::new(AccountingLimits::default());
        unix::scan_with_hook(&root, Instant::now(), &mut report, |_| {
            let writer = File::open(&root).unwrap();
            // A publisher can share directory custody while this reader runs.
            fs2::FileExt::try_lock_shared(&writer).unwrap();
            fs::write(root.join("chunks").join(format!("{:064x}", 1)), b"new").unwrap();
        });
        assert_eq!(report.status, AccountingStatus::Incomplete);
        assert_eq!(
            report.observed_directory_and_index_metadata_stable,
            Some(false)
        );
        assert!(report.issues.contains(&AccountingIssue::MetadataChanged));
        assert_eq!(report.consistency, "non_atomic_metadata_window");
        assert_locks_released(&root);
    }

    #[test]
    fn missing_index_created_during_scan_is_not_reported_as_complete_absence() {
        let fixture = TestRoot::new();
        let root = fixture.vault();
        let mut report = VaultAccounting::new(AccountingLimits::default());
        unix::scan_with_hook(&root, Instant::now(), &mut report, |_| {
            index(&root, &[entry("new", 1, None)]);
        });
        assert_eq!(report.index.status, ScanStatus::Missing);
        assert_eq!(report.status, AccountingStatus::Incomplete);
        assert_eq!(
            report.observed_directory_and_index_metadata_stable,
            Some(false)
        );
        assert!(report.issues.contains(&AccountingIssue::MetadataChanged));
        assert_locks_released(&root);
    }

    #[test]
    fn changed_index_metadata_is_detected_even_when_root_directory_is_unchanged() {
        let fixture = TestRoot::new();
        let root = fixture.vault();
        index(&root, &[]);
        let mut report = VaultAccounting::new(AccountingLimits::default());
        unix::scan_with_hook(&root, Instant::now(), &mut report, |_| {
            let mut writer = fs::OpenOptions::new()
                .append(true)
                .open(root.join("index.jsonl"))
                .unwrap();
            // Simulate a noncooperating append to check observation honesty.
            writer.write_all(b"{}\n").unwrap();
        });
        assert_eq!(report.status, AccountingStatus::Incomplete);
        assert_eq!(
            report.observed_directory_and_index_metadata_stable,
            Some(false)
        );
        assert_locks_released(&root);
    }
}
