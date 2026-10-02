//! Descriptor-relative installation writes. Never follow a destination symlink,
//! reuse a shared file, or let a child process outlive the transaction's lock.
use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::ffi::{CString, OsStr};
use std::fs::{File, Metadata};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SERIAL: AtomicU64 = AtomicU64::new(0);

fn name(value: &OsStr) -> Result<CString> {
    ensure!(
        matches!(
            Path::new(value).components().next(),
            Some(Component::Normal(_))
        ) && Path::new(value).components().count() == 1,
        "Expected one ordinary filename"
    );
    Ok(CString::new(value.as_bytes())?)
}

fn check(meta: &Metadata, directory: bool, private: bool) -> Result<()> {
    let uid = unsafe { libc::geteuid() };
    ensure!(
        if directory {
            meta.is_dir()
        } else {
            meta.is_file()
        },
        "Installation path has the wrong file type"
    );
    let root_sticky = directory && meta.uid() == 0 && meta.mode() & 0o1000 != 0;
    ensure!(
        (meta.uid() == uid || (directory && !private && meta.uid() == 0))
            && (root_sticky || meta.mode() & 0o022 == 0)
            && (!private || meta.mode() & 0o077 == 0)
            && (directory || meta.nlink() == 1),
        "Installation paths must be owned by this user and not shared or writable by others"
    );
    Ok(())
}

fn same(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

pub(super) struct Directory {
    pub path: PathBuf,
    file: File,
    private: bool,
}

impl Directory {
    pub fn open(path: &Path, create: bool, private: bool) -> Result<Self> {
        ensure!(path.is_absolute(), "Installation path must be absolute");
        ensure!(
            !path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir)),
            "Installation path must not contain dot components"
        );
        let mut file = File::open("/")?;
        check(&file.metadata()?, true, false)?;
        let components: Vec<_> = path
            .components()
            .filter_map(|c| match c {
                Component::Normal(value) => Some(value),
                _ => None,
            })
            .collect();
        ensure!(
            !private || !components.is_empty(),
            "Filesystem root cannot hold private update state"
        );
        for (index, component) in components.iter().enumerate() {
            let component = name(component)?;
            let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
            let mut fd = unsafe { libc::openat(file.as_raw_fd(), component.as_ptr(), flags) };
            if fd < 0
                && create
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
            {
                let created = unsafe { libc::mkdirat(file.as_raw_fd(), component.as_ptr(), 0o700) };
                if created < 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
                {
                    return Err(std::io::Error::last_os_error())
                        .context("Create install directory");
                }
                fd = unsafe { libc::openat(file.as_raw_fd(), component.as_ptr(), flags) };
            }
            if fd < 0 {
                return Err(std::io::Error::last_os_error())
                    .context("Open install directory without symlinks");
            }
            file = unsafe { File::from_raw_fd(fd) };
            check(
                &file.metadata()?,
                true,
                private && index + 1 == components.len(),
            )?;
        }
        Ok(Self {
            path: path.into(),
            file,
            private,
        })
    }

    pub fn validate(&self) -> Result<()> {
        let now = Self::open(&self.path, false, self.private)?;
        ensure!(
            same(&self.file.metadata()?, &now.file.metadata()?),
            "Install directory changed during verification"
        );
        Ok(())
    }

    pub fn mkdir(&self, filename: &str) -> Result<bool> {
        self.validate()?;
        let filename = name(OsStr::new(filename))?;
        if unsafe { libc::mkdirat(self.file.as_raw_fd(), filename.as_ptr(), 0o700) } == 0 {
            self.file.sync_all()?;
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Ok(false);
        }
        Err(error).context("Create private install directory")
    }

    fn open_file(
        &self,
        filename: &OsStr,
        flags: i32,
        mode: u32,
        private: bool,
    ) -> Result<Option<File>> {
        self.validate()?;
        let filename = name(filename)?;
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                filename.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                // openat is variadic: C integer promotion applies to mode_t
                // (u16 on macOS), unlike mkdirat/fchmod's typed parameters.
                mode as libc::c_uint,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound && flags & libc::O_CREAT == 0 {
                return Ok(None);
            }
            return Err(error).context("Open install file without symlinks");
        }
        let file = unsafe { File::from_raw_fd(fd) };
        check(&file.metadata()?, false, private)?;
        Ok(Some(file))
    }

    pub fn read(&self, filename: &str, limit: usize) -> Result<Option<Vec<u8>>> {
        let Some(file) = self.open_file(OsStr::new(filename), libc::O_RDONLY, 0, false)? else {
            return Ok(None);
        };
        ensure!(
            file.metadata()?.len() <= limit as u64,
            "Install file exceeds its size limit"
        );
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= limit,
            "Install file grew beyond its size limit"
        );
        Ok(Some(bytes))
    }

    pub fn write_new(&self, filename: &str, bytes: &[u8], executable: bool) -> Result<()> {
        let mut file = self
            .open_file(
                OsStr::new(filename),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                0o600,
                true,
            )?
            .context("Create staged file")?;
        file.write_all(bytes)?;
        if executable && unsafe { libc::fchmod(file.as_raw_fd(), 0o755) } != 0 {
            return Err(std::io::Error::last_os_error()).context("Make staged binary executable");
        }
        file.sync_all()?;
        self.file.sync_all()?;
        Ok(())
    }

    pub fn copy_mode(&self, target: &str, source: &Directory, filename: &str) -> Result<()> {
        let original = source
            .open_file(OsStr::new(filename), libc::O_RDONLY, 0, false)?
            .context("Original executable disappeared")?;
        let staged = self
            .open_file(OsStr::new(target), libc::O_RDONLY, 0, false)?
            .context("Backup executable disappeared")?;
        staged.set_permissions(std::fs::Permissions::from_mode(
            original.metadata()?.mode() & 0o777,
        ))?;
        staged.sync_all()?;
        Ok(())
    }

    pub fn rename(&self, source: &str, destination: &Self, target: &str) -> Result<()> {
        self.validate()?;
        destination.validate()?;
        let source = name(OsStr::new(source))?;
        let target = name(OsStr::new(target))?;
        if unsafe {
            libc::renameat(
                self.file.as_raw_fd(),
                source.as_ptr(),
                destination.file.as_raw_fd(),
                target.as_ptr(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error())
                .context("Atomically replace installation file");
        }
        destination.file.sync_all()?;
        self.file.sync_all()?;
        Ok(())
    }

    pub fn remove(&self, filename: &str, directory: bool) -> Result<()> {
        self.validate()?;
        let filename = name(OsStr::new(filename))?;
        if unsafe {
            libc::unlinkat(
                self.file.as_raw_fd(),
                filename.as_ptr(),
                if directory { libc::AT_REMOVEDIR } else { 0 },
            )
        } != 0
        {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error).context("Remove own installation staging file");
            }
        }
        self.file.sync_all()?;
        Ok(())
    }

    pub fn lock(&self) -> Result<Lock<'_>> {
        let file = self
            .open_file(
                OsStr::new("activity.lock"),
                libc::O_RDWR | libc::O_CREAT,
                0o600,
                true,
            )?
            .context("Create activity lock")?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            bail!("Gobstopper is running or another installer is active; retry when it finishes");
        }
        let lock = Lock {
            directory: self,
            file,
            owner: std::process::id(),
        };
        lock.validate()?;
        Ok(lock)
    }
}

pub(super) struct Lock<'a> {
    directory: &'a Directory,
    file: File,
    owner: u32,
}

impl Lock<'_> {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.owner == std::process::id(),
            "Install lock belongs to another process"
        );
        let now = self
            .directory
            .open_file(OsStr::new("activity.lock"), libc::O_RDONLY, 0, true)?
            .context("Activity lock disappeared")?;
        ensure!(
            same(&now.metadata()?, &self.file.metadata()?),
            "Activity lock changed during installation"
        );
        Ok(())
    }
}

impl Drop for Lock<'_> {
    fn drop(&mut self) {
        if self.owner == std::process::id() {
            // Release this scope even if an unrelated fork briefly inherited
            // the descriptor before exec. A child cannot unlock its parent.
            unsafe {
                libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

pub(super) struct Stage<'a> {
    pub directory: Directory,
    parent: &'a Directory,
    name: String,
    pub preserve: bool,
}

impl<'a> Stage<'a> {
    pub fn planned(parent: &Directory) -> PathBuf {
        parent.path.join(format!(
            ".gobstopper-update-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ))
    }

    pub fn create(parent: &'a Directory, path: &Path) -> Result<Self> {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .context("Invalid upgrade stage name")?;
        ensure!(
            name.starts_with(".gobstopper-update-") && path.parent() == Some(parent.path.as_path()),
            "Upgrade stage is outside the installation directory"
        );
        ensure!(parent.mkdir(name)?, "Upgrade stage already exists");
        Ok(Self {
            directory: Directory::open(path, false, true)?,
            parent,
            name: name.to_owned(),
            preserve: false,
        })
    }

    pub fn remove_checked(&self) -> Result<()> {
        self.directory.validate()?;
        for filename in [
            "archive",
            "checksum",
            "gobstopper",
            "previous",
            "old-receipt",
            "new-receipt",
        ] {
            self.directory.remove(filename, false)?;
        }
        self.parent.remove(&self.name, true)
    }

    pub fn existing(parent: &'a Directory, path: &Path) -> Result<Self> {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .context("Invalid upgrade stage name")?;
        ensure!(
            name.starts_with(".gobstopper-update-") && path.parent() == Some(parent.path.as_path()),
            "Upgrade stage is outside the installation directory"
        );
        Ok(Self {
            directory: Directory::open(path, false, true)?,
            parent,
            name: name.to_owned(),
            preserve: true,
        })
    }

    pub fn new(parent: &'a Directory) -> Result<Self> {
        for _ in 0..100 {
            let name = format!(
                ".gobstopper-update-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            );
            if parent.mkdir(&name)? {
                return Ok(Self {
                    directory: Directory::open(&parent.path.join(&name), false, true)?,
                    parent,
                    name,
                    preserve: false,
                });
            }
        }
        bail!("Could not create a fresh private staging directory")
    }
}

impl Drop for Stage<'_> {
    fn drop(&mut self) {
        if self.preserve || self.directory.validate().is_err() {
            return;
        }
        for filename in [
            "archive",
            "checksum",
            "gobstopper",
            "previous",
            "old-receipt",
            "new-receipt",
        ] {
            let _ = self.directory.remove(filename, false);
        }
        let _ = self.parent.remove(&self.name, true);
    }
}

pub(super) fn read_path(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let directory = Directory::open(path.parent().context("File has no parent")?, false, false)?;
    directory
        .read(
            path.file_name()
                .and_then(OsStr::to_str)
                .context("File needs a Unicode name")?,
            limit,
        )?
        .context("Install file is missing")
}

pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
