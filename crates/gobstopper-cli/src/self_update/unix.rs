//! Gobstopper's native transaction preserves its release archive and Apple
//! signature contract. Only this process writes installed files or receipts.
use super::{paths, product, released_tag, InitialInstall};
use anyhow::{bail, ensure, Context, Result};
use hraness_cli_update::{
    run_bounded, Asset, CurlGithub, InstallReceipt, InstallRequest, InstallationKind, Installer,
    Product, Release, ReleaseSource,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[path = "unix/files.rs"]
mod files;
pub(crate) use files::Directory as ManagedDirectory;
use files::{digest, read_path, Directory, Stage};

#[cfg(target_os = "macos")]
const MACOS_REQUIREMENT: &str = "=anchor apple generic and identifier \"dev.hraness.gobstopper\" and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"8AAP53VTW3\"";

const ARCHIVE_LIMIT: usize = 128 * 1024 * 1024;
const BINARY_LIMIT: usize = 256 * 1024 * 1024;
const CHECKSUM_LIMIT: usize = 1024;
const RECEIPT_LIMIT: usize = 64 * 1024;
// Known releases before installer ownership receipts. Compare verified bytes,
// never execute an unknown installed binary to discover its claimed version.
const LEGACY_RELEASES: &[&str] = &["v0.8.1", "v0.8.0", "v0.7.5"];

pub(super) struct NativeInstaller;

impl Installer for NativeInstaller {
    fn install(&self, request: &InstallRequest<'_>) -> hraness_cli_update::Result<()> {
        install_update(request).map_err(|error| {
            hraness_cli_update::Error::new(
                hraness_cli_update::ErrorCode::Installer,
                format!("{error:#}"),
            )
        })
    }
}

struct Published {
    release: Release,
    archive: Asset,
    checksum: Asset,
    archive_sha256: String,
    checksum_sha256: String,
}

#[derive(Deserialize)]
struct AssetDigest {
    id: u64,
    name: String,
    digest: Option<String>,
}

#[derive(Deserialize)]
struct DigestList {
    assets: Vec<AssetDigest>,
}

impl Published {
    fn parse(profile: &Product, expected_tag: &str, bytes: &[u8]) -> Result<Self> {
        let release: Release =
            serde_json::from_slice(bytes).context("Read canonical release metadata")?;
        release.validate(profile)?;
        ensure!(
            release.tag_name == expected_tag,
            "Canonical release tag differs from the requested version"
        );
        let raw: DigestList = serde_json::from_slice(bytes)?;
        let names = profile.asset_names(expected_tag)?;
        ensure!(
            names.len() == 2,
            "Gobstopper requires an archive and checksum"
        );
        let archive = release
            .asset(&names[0])
            .context("Missing release archive")?
            .clone();
        let checksum = release
            .asset(&names[1])
            .context("Missing release checksum")?
            .clone();
        ensure!(
            archive.size <= ARCHIVE_LIMIT as u64 && checksum.size <= CHECKSUM_LIMIT as u64,
            "Release assets exceed the native install size limits"
        );
        let published_digest = |asset: &Asset| -> Result<String> {
            let rows: Vec<_> = raw
                .assets
                .iter()
                .filter(|row| row.id == asset.id && row.name == asset.name)
                .collect();
            ensure!(
                rows.len() == 1,
                "Canonical release asset digest is ambiguous"
            );
            let hash = rows[0]
                .digest
                .as_deref()
                .and_then(|value| value.strip_prefix("sha256:"))
                .context("Canonical release is missing its asset SHA-256")?;
            ensure!(valid_digest(hash), "Canonical asset SHA-256 is invalid");
            Ok(hash.to_owned())
        };
        let archive_sha256 = published_digest(&archive)?;
        let checksum_sha256 = published_digest(&checksum)?;
        Ok(Self {
            release,
            archive,
            checksum,
            archive_sha256,
            checksum_sha256,
        })
    }

    fn fetch(profile: &Product, tag: &str) -> Result<Self> {
        let version = profile.version(tag)?;
        ensure!(
            profile.accepts(&version),
            "Release is outside Gobstopper's stable channel"
        );
        let url = format!(
            "https://api.github.com/repos/{}/releases/tags/{}",
            profile.repository,
            tag.replace('+', "%2B")
        );
        let mut command = Command::new("/usr/bin/curl");
        // No redirect, credential lookup, curl config, or replaceable authority.
        command.args([
            "--disable",
            "--silent",
            "--show-error",
            "--fail",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--connect-timeout",
            "5",
            "--max-time",
            "30",
            "--max-filesize",
            "2097152",
            "--header",
            "Accept: application/vnd.github+json",
            "--user-agent",
            "gobstopper-native-update",
            "--write-out",
            "\n%{http_code}",
            "--url",
            &url,
        ]);
        let output = run_bounded(
            &mut command,
            2 * 1024 * 1024 + 4,
            8192,
            Duration::from_secs(32),
        )?;
        ensure!(
            output.status.success(),
            "Could not read canonical GitHub release metadata; installation was not changed"
        );
        let bytes = output
            .stdout
            .strip_suffix(b"\n200")
            .context("GitHub did not return an exact HTTP 200 release response")?;
        Self::parse(profile, tag, bytes)
    }

    fn matches_selected(&self, selected: &Release) -> Result<()> {
        ensure!(
            self.release.id == selected.id && self.release.tag_name == selected.tag_name,
            "Selected release changed during verification"
        );
        for asset in [&self.archive, &self.checksum] {
            let selected_asset = selected
                .asset(&asset.name)
                .context("Selected release asset disappeared")?;
            ensure!(
                asset.id == selected_asset.id
                    && asset.size == selected_asset.size
                    && asset.browser_download_url == selected_asset.browser_download_url,
                "Selected release asset changed during verification"
            );
        }
        Ok(())
    }

    fn download(&self, profile: &Product, stage: &Stage<'_>) -> Result<()> {
        let source = CurlGithub::new("/usr/bin/curl")?;
        source.download_verified(
            profile,
            &self.release,
            &self.archive,
            &self.archive_sha256,
            &stage.directory.path.join("archive"),
            ARCHIVE_LIMIT,
        )?;
        source.download_verified(
            profile,
            &self.release,
            &self.checksum,
            &self.checksum_sha256,
            &stage.directory.path.join("checksum"),
            CHECKSUM_LIMIT,
        )?;
        self.verify_staged(stage)
    }

    fn import(&self, stage: &Stage<'_>, archive: &Path, checksum: &Path) -> Result<()> {
        stage
            .directory
            .write_new("archive", &read_path(archive, ARCHIVE_LIMIT)?, false)?;
        stage
            .directory
            .write_new("checksum", &read_path(checksum, CHECKSUM_LIMIT)?, false)?;
        self.verify_staged(stage)
    }

    fn verify_staged(&self, stage: &Stage<'_>) -> Result<()> {
        let archive = stage
            .directory
            .read("archive", ARCHIVE_LIMIT)?
            .context("Missing staged archive")?;
        let checksum = stage
            .directory
            .read("checksum", CHECKSUM_LIMIT)?
            .context("Missing staged checksum")?;
        ensure!(
            archive.len() as u64 == self.archive.size && digest(&archive) == self.archive_sha256,
            "Release archive differs from its canonical size or SHA-256"
        );
        ensure!(
            checksum.len() as u64 == self.checksum.size
                && digest(&checksum) == self.checksum_sha256,
            "Release checksum differs from its canonical size or SHA-256"
        );
        verify_checksum(&checksum, &self.archive.name, &self.archive_sha256)
    }

    fn receipt(&self, executable: &Path, binary_sha256: String, pinned: bool) -> InstallReceipt {
        InstallReceipt {
            schema: InstallReceipt::SCHEMA.into(),
            product: "gobstopper".into(),
            repository: "hraness/gobstopper".into(),
            kind: InstallationKind::NativeRelease,
            executable: executable.into(),
            binary_sha256,
            release_tag: self.release.tag_name.clone(),
            build_sha: None,
            release_id: self.release.id,
            archive_name: self.archive.name.clone(),
            archive_sha256: self.archive_sha256.clone(),
            platform: super::platform().into(),
            pinned,
        }
    }
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn verify_checksum(bytes: &[u8], archive: &str, expected: &str) -> Result<()> {
    let text = std::str::from_utf8(bytes)?.trim_end_matches(['\r', '\n']);
    let plain = format!("{expected}  {archive}");
    let binary = format!("{expected} *{archive}");
    ensure!(
        text == plain || text == binary,
        "Checksum must name only the exact release archive with its canonical SHA-256"
    );
    Ok(())
}

fn tar(archive: &Path, mode: &str, limit: usize) -> Result<Vec<u8>> {
    let mut command = Command::new("/usr/bin/tar");
    command.env("LC_ALL", "C").env_remove("TAR_OPTIONS");
    // bsdtar understands the mac-ext switch; GNU tar does not. This is chosen
    // at compile time, never from archive contents or a replaceable executable.
    #[cfg(target_os = "macos")]
    command.args(["--options", "!mac-ext"]);
    command.arg(mode).arg(archive);
    if mode == "-xzOf" {
        command.arg("gobstopper");
    }
    let output = run_bounded(&mut command, limit, 8192, Duration::from_secs(30))?;
    ensure!(
        output.status.success(),
        "Release archive could not be inspected or extracted"
    );
    Ok(output.stdout)
}

fn unpack(stage: &Stage<'_>) -> Result<String> {
    let archive = stage.directory.path.join("archive");
    ensure!(
        tar(&archive, "-tzf", 4096)? == b"gobstopper\n",
        "Release archive must contain exactly gobstopper"
    );
    let listing = tar(&archive, "-tvzf", 8192)?;
    ensure!(
        listing.first() == Some(&b'-')
            && listing.iter().filter(|byte| **byte == b'\n').count() == 1,
        "Release archive must contain one regular file, without links"
    );
    let binary = tar(&archive, "-xzOf", BINARY_LIMIT)?;
    ensure!(!binary.is_empty(), "Release binary is empty");
    let hash = digest(&binary);
    stage.directory.write_new("gobstopper", &binary, true)?;
    Ok(hash)
}

fn verify_candidate(stage: &Stage<'_>, tag: &str) -> Result<()> {
    let candidate = stage.directory.path.join("gobstopper");
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("/usr/bin/codesign");
        command
            .args([
                "--verify",
                "--strict",
                "--all-architectures",
                "--test-requirement",
                MACOS_REQUIREMENT,
            ])
            .arg(&candidate);
        ensure!(
            run_bounded(&mut command, 8192, 8192, Duration::from_secs(30))?
                .status
                .success(),
            "Release does not have Gobstopper's required Apple Developer ID signature"
        );
    }
    let mut command = Command::new(&candidate);
    command.arg("--version");
    let output = run_bounded(&mut command, 4096, 4096, Duration::from_secs(10))?;
    ensure!(
        output.status.success()
            && output.stdout == format!("gobstopper {}\n", tag.trim_start_matches('v')).as_bytes(),
        "Release executable reports the wrong version"
    );
    let mut command = Command::new(&candidate);
    command.arg("__build-identity");
    let output = run_bounded(&mut command, 4096, 4096, Duration::from_secs(10))?;
    ensure!(
        output.status.success(),
        "Release executable cannot report its embedded identity"
    );
    let identity: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    ensure!(
        identity["schema"] == "gobstopper.build.v1"
            && identity["releaseTag"] == tag
            && identity["platform"] == super::platform(),
        "Release executable is not an official build for this tag and platform"
    );
    Ok(())
}

fn eligible_destination(executable: &Path) -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "Install Gobstopper as your ordinary user; root-owned installations do not self-update"
    );
    ensure!(executable.is_absolute(), "Install prefix must be absolute");
    for component in executable.components() {
        ensure!(
            !matches!(
                component.as_os_str().to_str(),
                Some("Cellar" | ".cargo" | "target" | "node_modules" | ".git")
            ),
            "Cargo, Homebrew and source installations must use their original update workflow"
        );
    }
    for parent in executable.ancestors().skip(1) {
        ensure!(
            !(parent.join(".git").exists() && parent.join("Cargo.toml").exists()),
            "Install destination is inside a source checkout; use its source workflow"
        );
    }
    Ok(())
}

fn install_update(request: &InstallRequest<'_>) -> Result<()> {
    let target = &request.installation.receipt.executable;
    eligible_destination(target)?;
    let bin = Directory::open(
        target.parent().context("Install target has no parent")?,
        false,
        false,
    )?;
    let state = Directory::open(
        request
            .paths
            .receipt
            .parent()
            .context("Receipt has no parent")?,
        false,
        true,
    )?;
    let mut stage = Stage::new(&bin)?;
    let published = Published::fetch(request.product, &request.release.tag_name)?;
    published.matches_selected(request.release)?;
    published.download(request.product, &stage)?;
    let binary_sha = unpack(&stage)?;
    verify_candidate(&stage, &published.release.tag_name)?;
    let receipt = published.receipt(target, binary_sha, false);
    let old = snapshot(&bin, &state, &stage)?;
    request.revalidate()?;
    replace(
        &bin,
        &state,
        &mut stage,
        &old,
        &receipt,
        || {
            request.revalidate()?;
            Ok(())
        },
        || {
            request.publish_receipt(&receipt)?;
            Ok(())
        },
    )
}

pub(crate) struct UpgradeTarget {
    pub current: String,
    pub target: String,
    pub executable: PathBuf,
    receipt: InstallReceipt,
    receipt_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct PreparedUpgrade {
    stage_device: u64,
    stage_inode: u64,
    from_receipt_sha256: String,
    to_receipt_sha256: String,
    pub(crate) from: InstallReceipt,
    pub(crate) to: InstallReceipt,
}

pub(crate) struct UpgradeLease {
    _file: std::fs::File,
}

fn validate_upgrade_receipt(
    profile: &Product,
    receipt: &InstallReceipt,
    executable: &Path,
) -> Result<()> {
    ensure!(
        executable.is_absolute()
            && executable.file_name() == Some(std::ffi::OsStr::new("gobstopper"))
            && receipt.schema == InstallReceipt::SCHEMA
            && receipt.product == profile.id
            && receipt.repository == profile.repository
            && receipt.kind == InstallationKind::NativeRelease
            && receipt.platform == profile.platform
            && receipt.executable == executable
            && receipt.release_id != 0
            && valid_digest(&receipt.binary_sha256)
            && valid_digest(&receipt.archive_sha256)
            && receipt.build_sha.as_deref().is_none_or(|sha| {
                sha.len() == 40
                    && sha
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            && profile.accepts(&profile.version(&receipt.release_tag)?)
            && profile.asset_names(&receipt.release_tag)?.first() == Some(&receipt.archive_name),
        "Install receipt does not match the managed native release"
    );
    Ok(())
}

fn inspect_upgrade_receipt(executable: &Path) -> Result<(InstallReceipt, Vec<u8>)> {
    let bin = Directory::open_owned(
        executable
            .parent()
            .context("Install target has no parent")?,
        false,
        false,
    )?;
    let state_path = paths(executable)?.receipt;
    let state = Directory::open_owned(
        state_path.parent().context("Receipt has no parent")?,
        false,
        true,
    )?;
    let bytes = state
        .read_private("install.json", RECEIPT_LIMIT)?
        .context("Missing verified native release receipt")?;
    let receipt: InstallReceipt = serde_json::from_slice(&bytes)?;
    validate_upgrade_receipt(&product(), &receipt, executable)?;
    bin.verify_executable("gobstopper")?;
    ensure!(
        bin.read("gobstopper", BINARY_LIMIT)?
            .as_deref()
            .map(digest)
            .as_deref()
            == Some(&receipt.binary_sha256),
        "Installed executable differs from its native release receipt"
    );
    Ok((receipt, bytes))
}

fn select_upgrade_target(
    profile: &Product,
    receipt: &InstallReceipt,
    version: Option<&str>,
    releases: impl FnOnce() -> Result<Vec<Release>>,
) -> Result<String> {
    if receipt.pinned && version.is_none() {
        bail!("This installation is pinned; select --version explicitly");
    }
    if let Some(version) = version {
        let tag = format!("v{}", version.strip_prefix('v').unwrap_or(version));
        ensure!(
            profile.accepts(&profile.version(&tag)?),
            "Release is outside Gobstopper's stable channel"
        );
        return Ok(tag);
    }
    let latest = releases()?
        .into_iter()
        .filter_map(|release| {
            let version = release.validate(profile).ok()?;
            Some((version, release.tag_name))
        })
        .max_by(|left, right| left.0.cmp(&right.0))
        .context("No supported Gobstopper release is available")?;
    if latest.0 > profile.version(&receipt.release_tag)? {
        Ok(latest.1)
    } else {
        Ok(receipt.release_tag.clone())
    }
}

pub(super) fn upgrade_target(executable: &Path, version: Option<&str>) -> Result<UpgradeTarget> {
    eligible_destination(executable)?;
    let (receipt, bytes) = inspect_upgrade_receipt(executable)?;
    ensure!(
        released_tag()? == receipt.release_tag
            && std::env::current_exe()?.canonicalize()? == executable,
        "Run proxy upgrade from the managed service's installed release executable"
    );
    crate::proxy_agent::upgrade_controller_identity(executable)?;
    let profile = product();
    let target = select_upgrade_target(&profile, &receipt, version, || {
        Ok(CurlGithub::new("/usr/bin/curl")?.releases(&profile)?)
    })?;
    Ok(UpgradeTarget {
        current: receipt.release_tag.clone(),
        target,
        executable: executable.to_path_buf(),
        receipt,
        receipt_sha256: digest(&bytes),
    })
}

pub(super) fn planned_upgrade_stage(executable: &Path) -> Result<PathBuf> {
    let bin = Directory::open_owned(
        executable
            .parent()
            .context("Install target has no parent")?,
        false,
        false,
    )?;
    Ok(Stage::planned(&bin))
}

pub(super) fn prepare_upgrade(
    selected: &UpgradeTarget,
    explicit: bool,
    stage_path: &Path,
) -> Result<PreparedUpgrade> {
    eligible_destination(&selected.executable)?;
    let (current, current_bytes) = inspect_upgrade_receipt(&selected.executable)?;
    ensure!(
        current == selected.receipt && digest(&current_bytes) == selected.receipt_sha256,
        "Installed release changed before staging"
    );
    let profile = product();
    let bin = Directory::open_owned(
        selected
            .executable
            .parent()
            .context("Install target has no parent")?,
        false,
        false,
    )?;
    let mut stage = Stage::create(&bin, stage_path)?;
    let published = Published::fetch(&profile, &selected.target)?;
    published.download(&profile, &stage)?;
    let binary_sha = unpack(&stage)?;
    verify_candidate(&stage, &selected.target)?;
    let receipt = published.receipt(
        &selected.executable,
        binary_sha,
        explicit || selected.receipt.pinned,
    );
    let bytes = serde_json::to_vec(&receipt)?;
    stage.directory.write_new("new-receipt", &bytes, false)?;
    let (stage_device, stage_inode) = stage.directory.identity()?;
    let prepared = PreparedUpgrade {
        stage_device,
        stage_inode,
        from_receipt_sha256: selected.receipt_sha256.clone(),
        to_receipt_sha256: digest(&bytes),
        from: selected.receipt.clone(),
        to: receipt,
    };
    verify_prepared_upgrade(&selected.executable, stage_path, &prepared)?;
    stage.preserve = true;
    Ok(prepared)
}

fn verify_upgrade_stage(
    stage: &Stage<'_>,
    executable: &Path,
    prepared: &PreparedUpgrade,
) -> Result<()> {
    stage.validate_contents()?;
    ensure!(
        stage.directory.identity()? == (prepared.stage_device, prepared.stage_inode),
        "Upgrade staging directory changed"
    );
    validate_upgrade_receipt(&product(), &prepared.from, executable)?;
    validate_upgrade_receipt(&product(), &prepared.to, executable)?;
    let bytes = stage
        .directory
        .read_private("new-receipt", RECEIPT_LIMIT)?
        .context("Missing upgrade receipt")?;
    let receipt: InstallReceipt = serde_json::from_slice(&bytes)?;
    ensure!(
        digest(&bytes) == prepared.to_receipt_sha256 && receipt == prepared.to,
        "Staged upgrade receipt changed after verification"
    );
    Ok(())
}

pub(super) fn verify_prepared_upgrade(
    executable: &Path,
    stage_path: &Path,
    prepared: &PreparedUpgrade,
) -> Result<()> {
    let bin = Directory::open_owned(
        executable
            .parent()
            .context("Install target has no parent")?,
        false,
        false,
    )?;
    let stage = Stage::existing(&bin, stage_path)?;
    verify_upgrade_stage(&stage, executable, prepared)?;
    stage.directory.verify_executable("gobstopper")?;
    ensure!(
        stage
            .directory
            .read("gobstopper", BINARY_LIMIT)?
            .as_deref()
            .map(digest)
            .as_deref()
            == Some(&prepared.to.binary_sha256),
        "Staged binary changed after release verification"
    );
    let (receipt, bytes) = inspect_upgrade_receipt(executable)?;
    ensure!(
        receipt == prepared.from && digest(&bytes) == prepared.from_receipt_sha256,
        "Managed installation changed while upgrade was staged"
    );
    Ok(())
}

pub(super) fn replace_upgrade(
    executable: &Path,
    stage_path: &Path,
    prepared: &PreparedUpgrade,
) -> Result<UpgradeLease> {
    let profile = product();
    let bin = Directory::open_owned(
        executable
            .parent()
            .context("Install target has no parent")?,
        false,
        false,
    )?;
    let state_path = paths(executable)?.receipt;
    let state = Directory::open_owned(
        state_path.parent().context("Receipt has no parent")?,
        false,
        true,
    )?;
    let mut stage = Stage::existing(&bin, stage_path)?;
    let lock = state.lock()?;
    verify_prepared_upgrade(executable, stage_path, prepared)?;
    let old = snapshot(&bin, &state, &stage)?;
    ensure!(
        old.binary_sha256.as_deref() == Some(&prepared.from.binary_sha256)
            && old.receipt.as_deref().map(digest).as_deref() == Some(&prepared.from_receipt_sha256),
        "Previous release changed before its backup was recorded"
    );
    replace(
        &bin,
        &state,
        &mut stage,
        &old,
        &prepared.to,
        || {
            lock.validate()?;
            verify_prepared_upgrade(executable, stage_path, prepared)
        },
        || {
            prepared.to.write_verified(&profile, &state_path)?;
            Ok(())
        },
    )?;
    match lock.into_shared() {
        Ok(file) => Ok(UpgradeLease { _file: file }),
        Err(error) => {
            restore_upgrade(executable, stage_path, prepared)?;
            Err(error)
        }
    }
}

pub(super) fn restore_upgrade(
    executable: &Path,
    stage_path: &Path,
    prepared: &PreparedUpgrade,
) -> Result<UpgradeLease> {
    let bin = Directory::open_owned(
        executable
            .parent()
            .context("Install target has no parent")?,
        false,
        false,
    )?;
    let state_path = paths(executable)?.receipt;
    let state = Directory::open_owned(
        state_path.parent().context("Receipt has no parent")?,
        false,
        true,
    )?;
    let stage = Stage::existing(&bin, stage_path)?;
    let lock = state.lock()?;
    verify_upgrade_stage(&stage, executable, prepared)?;
    let installed = bin
        .read("gobstopper", BINARY_LIMIT)?
        .context("Installed executable disappeared; backup preserved")?;
    bin.verify_executable("gobstopper")?;
    let installed_receipt = state
        .read_private("install.json", RECEIPT_LIMIT)?
        .context("Installed receipt disappeared; backup preserved")?;
    ensure!(
        [&prepared.from.binary_sha256, &prepared.to.binary_sha256].contains(&&digest(&installed))
            && [&prepared.from_receipt_sha256, &prepared.to_receipt_sha256]
                .contains(&&digest(&installed_receipt)),
        "Installed release changed since upgrade; backup preserved"
    );
    let previous = stage
        .directory
        .read("previous", BINARY_LIMIT)?
        .context("Missing previous executable")?;
    stage.directory.verify_executable("previous")?;
    let old_receipt = stage
        .directory
        .read_private("old-receipt", RECEIPT_LIMIT)?
        .context("Missing previous receipt")?;
    let old: InstallReceipt = serde_json::from_slice(&old_receipt)?;
    ensure!(
        old == prepared.from
            && digest(&old_receipt) == prepared.from_receipt_sha256
            && digest(&previous) == prepared.from.binary_sha256,
        "Backup differs from the reserved previous release"
    );
    lock.validate()?;
    publish_restore_copy(&stage, &bin, "gobstopper", &previous, true)?;
    publish_restore_copy(&stage, &state, "install.json", &old_receipt, false)?;
    verify_installed_upgrade(executable, prepared, false)?;
    Ok(UpgradeLease {
        _file: lock.into_shared()?,
    })
}

fn publish_restore_copy(
    stage: &Stage<'_>,
    destination: &Directory,
    target: &str,
    bytes: &[u8],
    executable: bool,
) -> Result<()> {
    let temporary = if executable {
        "restore-binary"
    } else {
        "restore-receipt"
    };
    let existing = if executable {
        stage.directory.read(temporary, BINARY_LIMIT)?
    } else {
        stage.directory.read_private(temporary, RECEIPT_LIMIT)?
    };
    if let Some(existing) = existing {
        ensure!(
            existing == bytes,
            "Interrupted restoration copy changed; backup preserved"
        );
    } else {
        stage.directory.write_new(temporary, bytes, executable)?;
    }
    if executable {
        stage
            .directory
            .copy_mode(temporary, &stage.directory, "previous")?;
        stage.directory.verify_executable(temporary)?;
    }
    stage.directory.rename(temporary, destination, target)
}

pub(super) fn verify_installed_upgrade(
    executable: &Path,
    prepared: &PreparedUpgrade,
    target: bool,
) -> Result<()> {
    let (receipt, bytes) = inspect_upgrade_receipt(executable)?;
    let (expected, expected_sha256) = if target {
        (&prepared.to, &prepared.to_receipt_sha256)
    } else {
        (&prepared.from, &prepared.from_receipt_sha256)
    };
    ensure!(
        &receipt == expected && digest(&bytes) == *expected_sha256,
        "Installed release differs from the reserved upgrade outcome"
    );
    Ok(())
}

pub(super) fn discard_bound_upgrade(
    executable: &Path,
    stage_path: &Path,
    prepared: &PreparedUpgrade,
) -> Result<()> {
    let bin = Directory::open_owned(
        executable
            .parent()
            .context("Install target has no parent")?,
        false,
        false,
    )?;
    let stage = Stage::existing(&bin, stage_path)?;
    verify_upgrade_stage(&stage, executable, prepared)?;
    stage.remove_checked()
}

pub(super) fn protect_previous_upgrade(
    executable: &Path,
    prepared: &PreparedUpgrade,
) -> Result<UpgradeLease> {
    let state_path = paths(executable)?.receipt;
    let state = Directory::open_owned(
        state_path.parent().context("Receipt has no parent")?,
        false,
        true,
    )?;
    let lock = state.lock()?;
    let (receipt, bytes) = inspect_upgrade_receipt(executable)?;
    ensure!(
        receipt == prepared.from && digest(&bytes) == prepared.from_receipt_sha256,
        "Previous executable and receipt differ from the reserved release"
    );
    Ok(UpgradeLease {
        _file: lock.into_shared()?,
    })
}

#[cfg(test)]
pub(super) fn verify_previous_upgrade(executable: &Path, version: &str) -> Result<()> {
    let (receipt, _) = inspect_upgrade_receipt(executable)?;
    ensure!(
        receipt.release_tag == version,
        "Previous executable and receipt were not both restored"
    );
    Ok(())
}

pub(super) fn discard_upgrade(executable: &Path, stage_path: &Path) -> Result<()> {
    let bin = Directory::open_owned(
        executable
            .parent()
            .context("Install target has no parent")?,
        false,
        false,
    )?;
    let stage = Stage::existing(&bin, stage_path)?;
    stage.remove_checked()
}

struct Previous {
    binary_sha256: Option<String>,
    receipt: Option<Vec<u8>>,
}

fn snapshot(bin: &Directory, state: &Directory, stage: &Stage<'_>) -> Result<Previous> {
    let binary = bin.read("gobstopper", BINARY_LIMIT)?;
    if let Some(bytes) = &binary {
        stage.directory.write_new("previous", bytes, true)?;
        stage.directory.copy_mode("previous", bin, "gobstopper")?;
    }
    let receipt = state.read("install.json", RECEIPT_LIMIT)?;
    if let Some(bytes) = &receipt {
        stage.directory.write_new("old-receipt", bytes, false)?;
    }
    Ok(Previous {
        binary_sha256: binary.as_deref().map(digest),
        receipt,
    })
}

/// Keep binary replacement, receipt publication and rollback in the lock owner.
/// If rollback fails, retain the private backup and fail closed on the next run.
fn replace(
    bin: &Directory,
    state: &Directory,
    stage: &mut Stage<'_>,
    old: &Previous,
    receipt: &InstallReceipt,
    revalidate: impl FnOnce() -> Result<()>,
    publish: impl FnOnce() -> Result<()>,
) -> Result<()> {
    ensure!(
        bin.read("gobstopper", BINARY_LIMIT)?.as_deref().map(digest) == old.binary_sha256,
        "Installed bytes changed before replacement"
    );
    ensure!(
        state.read("install.json", RECEIPT_LIMIT)? == old.receipt,
        "Install receipt changed before replacement"
    );
    ensure!(
        stage
            .directory
            .read("gobstopper", BINARY_LIMIT)?
            .as_deref()
            .map(digest)
            .as_deref()
            == Some(&receipt.binary_sha256),
        "Staged binary changed after verification"
    );
    revalidate()?;
    let result: Result<()> = (|| {
        stage.directory.rename("gobstopper", bin, "gobstopper")?;
        publish()?;
        Ok(())
    })();
    if let Err(error) = result {
        let rollback: Result<()> = (|| {
            let now = bin.read("gobstopper", BINARY_LIMIT)?.as_deref().map(digest);
            ensure!(
                now == old.binary_sha256 || now.as_deref() == Some(&receipt.binary_sha256),
                "Installed binary changed outside the transaction; backup was retained"
            );
            if old.binary_sha256.is_some() {
                stage.directory.rename("previous", bin, "gobstopper")?;
            } else {
                bin.remove("gobstopper", false)?;
            }
            if old.receipt.is_some() {
                stage
                    .directory
                    .rename("old-receipt", state, "install.json")?;
            } else {
                state.remove("install.json", false)?;
            }
            Ok(())
        })();
        if let Err(rollback) = rollback {
            stage.preserve = true;
            bail!("Installation failed: {error:#}; restoration failed: {rollback:#}. Verified backup retained at {}", stage.directory.path.display());
        }
        return Err(error).context("Installation failed; original files were restored");
    }
    Ok(())
}

fn matches_legacy_release(
    current_hash: &str,
    mut fetch_hash: impl FnMut(&str) -> Result<String>,
) -> bool {
    LEGACY_RELEASES
        .iter()
        .any(|tag| fetch_hash(tag).is_ok_and(|hash| hash == current_hash))
}

fn verify_previous(
    profile: &Product,
    published: &Published,
    bin: &Directory,
    current: Option<&[u8]>,
    receipt: Option<&[u8]>,
    explicit_pin: bool,
) -> Result<bool> {
    let Some(binary) = current else {
        ensure!(
            receipt.is_none(),
            "An install receipt exists without its executable"
        );
        return Ok(explicit_pin);
    };
    if let Some(bytes) = receipt {
        let old: InstallReceipt =
            serde_json::from_slice(bytes).context("Read previous native install receipt")?;
        ensure!(
            old.schema == InstallReceipt::SCHEMA
                && old.product == profile.id
                && old.repository == profile.repository
                && old.kind == InstallationKind::NativeRelease
                && old.platform == profile.platform
                && old.executable == bin.path.join("gobstopper")
                && old.binary_sha256 == digest(binary)
                && old.release_id != 0
                && valid_digest(&old.archive_sha256)
                && profile
                    .asset_names(&old.release_tag)?
                    .contains(&old.archive_name),
            "Previous install receipt does not match the installed native release"
        );
        ensure!(!old.pinned || explicit_pin || old.release_tag == published.release.tag_name, "This install is pinned; set GOBSTOPPER_VERSION to explicitly select a replacement version");
        return Ok(old.pinned || explicit_pin);
    }
    // Each allowed predecessor is independently checked against its canonical
    // immutable release. A reserved or unavailable newer predecessor must not
    // prevent migration of an older verified copy.
    ensure!(
        matches_legacy_release(&digest(binary), |tag| {
            let legacy = Published::fetch(profile, tag)?;
            let stage = Stage::new(bin)?;
            legacy.download(profile, &stage)?;
            unpack(&stage)
        }),
        "Existing file is not a verified 0.8.1, 0.8.0 or 0.7.5 release; use its source/package-manager update workflow or select a new GOBSTOPPER_INSTALL_PREFIX"
    );
    Ok(explicit_pin)
}

pub(super) fn initial_install(args: &InitialInstall) -> Result<()> {
    let tag = released_tag()?;
    let profile = product();
    let target = args.prefix.join("bin/gobstopper");
    eligible_destination(&target)?;
    let bin = Directory::open(
        target.parent().context("Install target has no parent")?,
        true,
        false,
    )?;
    let mut stage = Stage::new(&bin)?;
    let published = Published::fetch(&profile, tag)?;
    published.import(&stage, &args.archive, &args.checksum)?;
    let binary_sha = unpack(&stage)?;
    let running = std::env::current_exe()?.canonicalize()?;
    ensure!(
        digest(&read_path(&running, BINARY_LIMIT)?) == binary_sha,
        "Installer executable differs from the verified release archive"
    );
    verify_candidate(&stage, tag)?;
    let pinned = enroll_verified(
        &profile,
        &published,
        &bin,
        &mut stage,
        &target,
        &binary_sha,
        args.pinned,
    )?;
    println!(
        "Installed {} ({}); automatic updates {}",
        target.display(),
        tag,
        if pinned {
            "off for this pinned version"
        } else {
            "enabled by default"
        }
    );
    Ok(())
}

fn enroll_verified(
    profile: &Product,
    published: &Published,
    bin: &Directory,
    stage: &mut Stage<'_>,
    target: &Path,
    binary_sha: &str,
    explicit_pin: bool,
) -> Result<bool> {
    let update_paths = paths(target)?;
    let coordination = update_paths
        .receipt
        .parent()
        .context("Receipt has no parent")?;
    let state_name = ".hraness-cli-update-gobstopper";
    // Verify an old unmanaged installation before creating the coordination
    // authority; a failed ownership check must not disable the old executable.
    let current = bin.read("gobstopper", BINARY_LIMIT)?;
    let old_receipt = match std::fs::symlink_metadata(coordination) {
        Ok(_) => Directory::open(coordination, false, true)?.read("install.json", RECEIPT_LIMIT)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let pinned =
        if old_receipt.is_none() && current.as_deref().map(digest).as_deref() == Some(binary_sha) {
            explicit_pin
        } else {
            verify_previous(
                profile,
                published,
                bin,
                current.as_deref(),
                old_receipt.as_deref(),
                explicit_pin,
            )?
        };
    let created_state = bin.mkdir(state_name)?;
    let state = Directory::open(coordination, false, true)?;
    let lock = state.lock()?;
    let outcome = (|| {
        let old = snapshot(bin, &state, stage)?;
        ensure!(
            old.binary_sha256 == current.as_deref().map(digest) && old.receipt == old_receipt,
            "Installation changed while its release identity was checked; retry installation"
        );
        let receipt = published.receipt(target, binary_sha.to_owned(), pinned);
        replace(
            bin,
            &state,
            stage,
            &old,
            &receipt,
            || lock.validate(),
            || {
                receipt.write_verified(profile, &update_paths.receipt)?;
                Ok(())
            },
        )
    })();
    if outcome.is_err() && created_state && !stage.preserve {
        // No managed install survived this failed first enrollment. Remove only
        // our freshly created empty authority, while still holding its lock.
        let _ = state.remove("activity.lock", false);
        let _ = bin.remove(state_name, true);
    }
    outcome?;
    Ok(pinned)
}

#[cfg(test)]
pub(crate) mod tests {
    #[test]
    #[cfg(target_os = "macos")]
    fn macos_requirement_is_compilable_inline_source() {
        let mut command = Command::new("/usr/bin/csreq");
        command.args(["-r", MACOS_REQUIREMENT, "-t"]);
        let output = run_bounded(&mut command, 8192, 8192, Duration::from_secs(10)).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    use super::*;
    use hraness_cli_update::{
        ReleaseSource, RunningIdentity, StartupContext, StartupOutcome, Updater,
    };
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SERIAL: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn legacy_migration_uses_only_verified_known_predecessors() {
        let expected = "a".repeat(64);
        let mut requests = Vec::new();
        assert!(matches_legacy_release(&expected, |tag| {
            requests.push(tag.to_string());
            if tag != "v0.7.5" {
                bail!("release is not published yet");
            }
            Ok(expected.clone())
        }));
        assert_eq!(requests, ["v0.8.1", "v0.8.0", "v0.7.5"]);

        requests.clear();
        assert!(matches_legacy_release(&expected, |tag| {
            requests.push(tag.to_string());
            Ok(expected.clone())
        }));
        assert_eq!(requests, ["v0.8.1"]);

        requests.clear();
        assert!(!matches_legacy_release(&expected, |tag| {
            requests.push(tag.to_string());
            Ok("b".repeat(64))
        }));
        assert_eq!(requests, ["v0.8.1", "v0.8.0", "v0.7.5"]);
    }

    struct Fixture {
        root: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let base = std::env::temp_dir().canonicalize().unwrap();
            let root = base.join(format!(
                "gobstopper-native-test-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            Self { root }
        }
        fn bin(&self) -> Directory {
            Directory::open(&self.root.join("bin"), true, false).unwrap()
        }
        fn state(&self) -> Directory {
            Directory::open(
                &self.root.join("bin/.hraness-cli-update-gobstopper"),
                true,
                true,
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// An install whose v1.0.0 executable and receipt were replaced from a
    /// preserved stage, as the controller leaves them before starting.
    pub(crate) struct ReplacedInstall {
        _fixture: Fixture,
        pub(crate) executable: PathBuf,
        pub(crate) stage: PathBuf,
        pub(crate) prepared: PreparedUpgrade,
    }

    pub(crate) fn staged_install() -> ReplacedInstall {
        let fixture = Fixture::new();
        let bin = fixture.bin();
        bin.write_new("gobstopper", b"old release", true).unwrap();
        let executable = bin.path.join("gobstopper");
        fixture.state();
        let previous = published().receipt(&executable, digest(b"old release"), false);
        previous
            .write_verified(&product(), &paths(&executable).unwrap().receipt)
            .unwrap();
        let mut stage = Stage::new(&bin).unwrap();
        stage
            .directory
            .write_new("gobstopper", b"new release", true)
            .unwrap();
        let next = published_for("v1.0.1").receipt(&executable, digest(b"new release"), true);
        let next_bytes = serde_json::to_vec(&next).unwrap();
        stage
            .directory
            .write_new("new-receipt", &next_bytes, false)
            .unwrap();
        let (stage_device, stage_inode) = stage.directory.identity().unwrap();
        let prepared = PreparedUpgrade {
            stage_device,
            stage_inode,
            from_receipt_sha256: digest(&fs::read(paths(&executable).unwrap().receipt).unwrap()),
            to_receipt_sha256: digest(&next_bytes),
            from: previous,
            to: next,
        };
        stage.preserve = true;
        verify_prepared_upgrade(&executable, &stage.directory.path, &prepared).unwrap();
        ReplacedInstall {
            stage: stage.directory.path.clone(),
            executable,
            prepared,
            _fixture: fixture,
        }
    }

    pub(crate) fn replaced_install() -> ReplacedInstall {
        let install = staged_install();
        replace_upgrade(&install.executable, &install.stage, &install.prepared).unwrap();
        assert_eq!(fs::read(&install.executable).unwrap(), b"new release");
        install
    }

    #[test]
    fn managed_upgrade_explicit_pins_are_supported_without_implicit_downgrades() {
        let profile = product();
        let mut receipt =
            published().receipt(Path::new("/fixture/bin/gobstopper"), "a".repeat(64), true);
        assert_eq!(
            select_upgrade_target(&profile, &receipt, Some("1.0.1"), || panic!(
                "an explicit version must not query latest"
            ))
            .unwrap(),
            "v1.0.1"
        );
        assert!(select_upgrade_target(&profile, &receipt, None, || panic!(
            "a pin must refuse before querying latest"
        ))
        .is_err());
        for version in ["1.0.1-beta.1", "vv1.0.1", "1.0.1/other"] {
            assert!(
                select_upgrade_target(&profile, &receipt, Some(version), || panic!(
                    "invalid explicit selection queried latest"
                ))
                .is_err()
            );
        }
        receipt.pinned = false;
        assert_eq!(
            select_upgrade_target(&profile, &receipt, None, || Ok(vec![
                published_for("v0.9.9").release
            ]))
            .unwrap(),
            "v1.0.0"
        );
    }

    #[test]
    fn managed_upgrade_rejects_changed_staged_receipts_before_any_installed_write() {
        for field in [
            "release_tag",
            "binary_sha256",
            "executable",
            "pinned",
            "repository",
        ] {
            let install = staged_install();
            let original = fs::read(&install.executable).unwrap();
            let receipt_path = paths(&install.executable).unwrap().receipt;
            let original_receipt = fs::read(&receipt_path).unwrap();
            let new_receipt = install.stage.join("new-receipt");
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&new_receipt).unwrap()).unwrap();
            value[field] = if field == "pinned" {
                false.into()
            } else {
                "foreign".into()
            };
            fs::write(&new_receipt, serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(
                replace_upgrade(&install.executable, &install.stage, &install.prepared).is_err(),
                "{field}"
            );
            assert_eq!(fs::read(&install.executable).unwrap(), original);
            assert_eq!(fs::read(receipt_path).unwrap(), original_receipt);
            assert!(!install.stage.join("previous").exists());
        }
    }

    #[test]
    fn managed_upgrade_rejects_symlinks_shared_files_and_unsafe_stage_permissions() {
        for variant in [
            "symlink",
            "hardlink",
            "shared_directory",
            "writable_binary",
            "setuid_binary",
            "readable_receipt",
        ] {
            let install = staged_install();
            let candidate = install.stage.join("gobstopper");
            match variant {
                "symlink" => {
                    fs::remove_file(&candidate).unwrap();
                    symlink(&install.executable, &candidate).unwrap();
                }
                "hardlink" => {
                    fs::remove_file(&candidate).unwrap();
                    fs::hard_link(&install.executable, &candidate).unwrap();
                }
                "shared_directory" => {
                    fs::set_permissions(&install.stage, fs::Permissions::from_mode(0o755)).unwrap()
                }
                "writable_binary" => {
                    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o777)).unwrap()
                }
                "setuid_binary" => {
                    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o4755)).unwrap()
                }
                "readable_receipt" => fs::set_permissions(
                    install.stage.join("new-receipt"),
                    fs::Permissions::from_mode(0o644),
                )
                .unwrap(),
                _ => unreachable!(),
            }
            assert!(
                verify_prepared_upgrade(&install.executable, &install.stage, &install.prepared)
                    .is_err(),
                "{variant}"
            );
            assert_eq!(fs::read(&install.executable).unwrap(), b"old release");
        }
    }

    #[test]
    fn managed_upgrade_cleanup_preserves_unrecognized_or_linked_entries() {
        for linked in [false, true] {
            let install = staged_install();
            if linked {
                symlink(&install.executable, install.stage.join("archive")).unwrap();
            } else {
                fs::write(install.stage.join("unrelated"), b"preserve").unwrap();
            }
            assert!(discard_upgrade(&install.executable, &install.stage).is_err());
            assert!(install.stage.join("new-receipt").is_file());
            assert!(install.stage.join("gobstopper").is_file());
            assert_eq!(fs::read(&install.executable).unwrap(), b"old release");
        }
    }

    #[test]
    fn managed_upgrade_rollback_resumes_each_partial_pair_without_consuming_backups() {
        for restored in ["neither", "binary", "receipt", "both"] {
            let install = replaced_install();
            let bin =
                Directory::open_owned(install.executable.parent().unwrap(), false, false).unwrap();
            let stage = Stage::existing(&bin, &install.stage).unwrap();
            let receipt_path = paths(&install.executable).unwrap().receipt;
            let state = Directory::open_owned(receipt_path.parent().unwrap(), false, true).unwrap();
            let binary = stage
                .directory
                .read("previous", BINARY_LIMIT)
                .unwrap()
                .unwrap();
            let receipt = stage
                .directory
                .read_private("old-receipt", RECEIPT_LIMIT)
                .unwrap()
                .unwrap();
            if matches!(restored, "binary" | "both") {
                publish_restore_copy(&stage, &bin, "gobstopper", &binary, true).unwrap();
            }
            if matches!(restored, "receipt" | "both") {
                publish_restore_copy(&stage, &state, "install.json", &receipt, false).unwrap();
            }
            drop(restore_upgrade(&install.executable, &install.stage, &install.prepared).unwrap());
            verify_installed_upgrade(&install.executable, &install.prepared, false).unwrap();
            assert_eq!(fs::read(install.stage.join("previous")).unwrap(), binary);
            assert_eq!(
                fs::read(install.stage.join("old-receipt")).unwrap(),
                receipt
            );
            drop(restore_upgrade(&install.executable, &install.stage, &install.prepared).unwrap());
            verify_installed_upgrade(&install.executable, &install.prepared, false).unwrap();
        }
    }

    #[test]
    fn managed_upgrade_rollback_rejects_foreign_executable_and_interrupted_copy() {
        for changed in ["binary", "copy"] {
            let install = replaced_install();
            let path = if changed == "binary" {
                install.executable.clone()
            } else {
                install.stage.join("restore-binary")
            };
            fs::write(&path, b"foreign executable").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            let receipt_path = paths(&install.executable).unwrap().receipt;
            let receipt = fs::read(&receipt_path).unwrap();
            assert!(
                restore_upgrade(&install.executable, &install.stage, &install.prepared).is_err()
            );
            assert_eq!(fs::read(&path).unwrap(), b"foreign executable");
            assert_eq!(fs::read(receipt_path).unwrap(), receipt);
            assert_eq!(
                fs::read(install.stage.join("previous")).unwrap(),
                b"old release"
            );
            assert!(install.stage.join("old-receipt").exists());
        }
    }

    #[test]
    fn managed_upgrade_rollback_preserves_externally_changed_receipt_and_backup() {
        let install = replaced_install();
        let path = paths(&install.executable).unwrap().receipt;
        let mut receipt: InstallReceipt =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        receipt.pinned = !receipt.pinned;
        let edited = serde_json::to_vec(&receipt).unwrap();
        fs::write(&path, &edited).unwrap();
        assert!(restore_upgrade(&install.executable, &install.stage, &install.prepared).is_err());
        assert_eq!(fs::read(&path).unwrap(), edited);
        assert_eq!(fs::read(&install.executable).unwrap(), b"new release");
        assert_eq!(
            fs::read(install.stage.join("previous")).unwrap(),
            b"old release"
        );
    }

    #[test]
    fn managed_upgrade_rollback_rejects_rebound_previous_receipt() {
        let install = replaced_install();
        let old = install.stage.join("old-receipt");
        let mut receipt: InstallReceipt = serde_json::from_slice(&fs::read(&old).unwrap()).unwrap();
        receipt.executable = PathBuf::from("/foreign/bin/gobstopper");
        fs::write(&old, serde_json::to_vec(&receipt).unwrap()).unwrap();
        assert!(restore_upgrade(&install.executable, &install.stage, &install.prepared).is_err());
        assert_eq!(fs::read(&install.executable).unwrap(), b"new release");
        assert!(install.stage.join("previous").is_file());
    }

    #[test]
    fn descriptor_paths_reject_dot_aliases_before_creating_directories() {
        let fixture = Fixture::new();
        let bin = fixture.bin();
        assert!(Directory::open(&bin.path.join("./nested"), true, false).is_err());
        assert!(!bin.path.join("nested").exists());
    }

    #[test]
    fn planned_upgrade_stage_precedes_creation_and_can_be_removed() {
        let fixture = Fixture::new();
        let bin = fixture.bin();
        let executable = bin.path.join("gobstopper");
        let path = planned_upgrade_stage(&executable).unwrap();
        assert!(!path.exists());
        let mut stage = Stage::create(&bin, &path).unwrap();
        stage
            .directory
            .write_new("archive", b"partial", false)
            .unwrap();
        stage.preserve = true;
        assert!(path.exists());
        drop(stage);
        discard_upgrade(&executable, &path).unwrap();
        assert!(!path.exists());
    }

    fn metadata() -> serde_json::Value {
        let names = product().asset_names("v1.0.0").unwrap();
        serde_json::json!({
            "id":42,"tag_name":"v1.0.0","draft":false,"prerelease":false,"immutable":true,
            "assets": names.iter().enumerate().map(|(index, name)| serde_json::json!({
                "id": index + 1, "name":name, "size":123,
                "browser_download_url":format!("https://github.com/hraness/gobstopper/releases/download/v1.0.0/{name}"),
                "digest":format!("sha256:{}", "a".repeat(64)),
            })).collect::<Vec<_>>()
        })
    }

    fn published() -> Published {
        published_for("v1.0.0")
    }

    fn published_for(tag: &str) -> Published {
        let mut value = metadata();
        value["tag_name"] = tag.into();
        if tag != "v1.0.0" {
            value["id"] = 43.into();
        }
        let names = product().asset_names(tag).unwrap();
        for (index, name) in names.iter().enumerate() {
            value["assets"][index]["name"] = name.clone().into();
            value["assets"][index]["browser_download_url"] =
                format!("https://github.com/hraness/gobstopper/releases/download/{tag}/{name}")
                    .into();
        }
        Published::parse(&product(), tag, &serde_json::to_vec(&value).unwrap()).unwrap()
    }

    pub(crate) fn assert_installation_busy(executable: &Path) {
        let receipt = paths(executable).unwrap().receipt;
        let state = Directory::open_owned(receipt.parent().unwrap(), false, true).unwrap();
        assert!(
            state.lock().is_err(),
            "restart must hold a shared installation lease"
        );
    }

    #[test]
    fn canonical_release_proof_requires_immutable_assets_and_digests() {
        published();
        let mut variants = vec![];
        let mut value = metadata();
        value["immutable"] = false.into();
        variants.push(value);
        let mut value = metadata();
        value["assets"][0]["digest"] = serde_json::Value::Null;
        variants.push(value);
        let mut value = metadata();
        value["assets"][0]["digest"] = format!("sha256:{}", "A".repeat(64)).into();
        variants.push(value);
        let mut value = metadata();
        value["assets"][0]["browser_download_url"] = "https://example.test/gobstopper".into();
        variants.push(value);
        let mut value = metadata();
        value["tag_name"] = "v1.0.1".into();
        variants.push(value);
        for value in variants {
            assert!(
                Published::parse(&product(), "v1.0.0", &serde_json::to_vec(&value).unwrap())
                    .is_err()
            );
        }
    }

    #[test]
    fn checksum_binds_one_exact_archive() {
        let hash = "b".repeat(64);
        for text in [
            format!("{hash}  exact.tar.gz\n"),
            format!("{hash} *exact.tar.gz\r\n"),
        ] {
            verify_checksum(text.as_bytes(), "exact.tar.gz", &hash).unwrap();
        }
        for text in [
            format!("{hash}  other.tar.gz\n"),
            format!("{hash}  exact.tar.gz\n{hash}  other.tar.gz\n"),
            "b".repeat(64),
        ] {
            assert!(verify_checksum(text.as_bytes(), "exact.tar.gz", &hash).is_err());
        }
    }

    #[test]
    fn local_archive_cannot_claim_canonical_release_ownership() {
        let fixture = Fixture::new();
        let bin = fixture.bin();
        let stage = Stage::new(&bin).unwrap();
        stage
            .directory
            .write_new("archive", b"foreign archive", false)
            .unwrap();
        stage
            .directory
            .write_new("checksum", b"foreign checksum", false)
            .unwrap();
        assert!(published().verify_staged(&stage).is_err());
        assert!(fixture
            .bin()
            .read("gobstopper", BINARY_LIMIT)
            .unwrap()
            .is_none());
    }

    fn make_archive(source: &Path, archive: &Path, members: &[&str]) {
        let mut command = Command::new("/usr/bin/tar");
        command
            .env("COPYFILE_DISABLE", "1")
            .env_remove("TAR_OPTIONS")
            .arg("-czf")
            .arg(archive)
            .arg("-C")
            .arg(source)
            .args(members);
        assert!(
            run_bounded(&mut command, 8192, 8192, Duration::from_secs(10))
                .unwrap()
                .status
                .success()
        );
    }

    #[test]
    fn archive_refuses_links_and_extra_members_without_execution() {
        let fixture = Fixture::new();
        let bin = fixture.bin();
        let source = fixture.root.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("gobstopper"), b"not executed").unwrap();
        fs::write(source.join("extra"), b"extra").unwrap();
        {
            let stage = Stage::new(&bin).unwrap();
            make_archive(
                &source,
                &stage.directory.path.join("archive"),
                &["gobstopper"],
            );
            assert_eq!(unpack(&stage).unwrap(), digest(b"not executed"));
        }
        {
            let stage = Stage::new(&bin).unwrap();
            make_archive(
                &source,
                &stage.directory.path.join("archive"),
                &["gobstopper", "extra"],
            );
            assert!(unpack(&stage).is_err());
        }
        fs::remove_file(source.join("gobstopper")).unwrap();
        symlink("/bin/sh", source.join("gobstopper")).unwrap();
        let stage = Stage::new(&bin).unwrap();
        make_archive(
            &source,
            &stage.directory.path.join("archive"),
            &["gobstopper"],
        );
        assert!(unpack(&stage).is_err());
    }

    #[test]
    fn rollback_restores_binary_and_receipt_after_publication_failure() {
        let fixture = Fixture::new();
        let bin = fixture.bin();
        let state = fixture.state();
        bin.write_new("gobstopper", b"old binary", true).unwrap();
        fs::set_permissions(
            bin.path.join("gobstopper"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        state
            .write_new("install.json", b"old receipt", false)
            .unwrap();
        let mut stage = Stage::new(&bin).unwrap();
        stage
            .directory
            .write_new("gobstopper", b"new binary", true)
            .unwrap();
        let old = snapshot(&bin, &state, &stage).unwrap();
        let receipt =
            published().receipt(&bin.path.join("gobstopper"), digest(b"new binary"), false);
        let result = replace(
            &bin,
            &state,
            &mut stage,
            &old,
            &receipt,
            || Ok(()),
            || {
                state.remove("install.json", false)?;
                state.write_new("install.json", b"partially published receipt", false)?;
                bail!("injected receipt publication failure")
            },
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("original files were restored"));
        assert_eq!(
            bin.read("gobstopper", BINARY_LIMIT).unwrap().unwrap(),
            b"old binary"
        );
        assert_eq!(
            state.read("install.json", RECEIPT_LIMIT).unwrap().unwrap(),
            b"old receipt"
        );
    }

    #[test]
    fn failed_final_revalidation_performs_no_installed_writes() {
        let fixture = Fixture::new();
        let bin = fixture.bin();
        let state = fixture.state();
        bin.write_new("gobstopper", b"old", true).unwrap();
        state.write_new("install.json", b"old", false).unwrap();
        let mut stage = Stage::new(&bin).unwrap();
        stage
            .directory
            .write_new("gobstopper", b"new", true)
            .unwrap();
        let old = snapshot(&bin, &state, &stage).unwrap();
        let receipt = published().receipt(&bin.path.join("gobstopper"), digest(b"new"), false);
        assert!(replace(
            &bin,
            &state,
            &mut stage,
            &old,
            &receipt,
            || bail!("policy disabled"),
            || panic!("publication must not run")
        )
        .is_err());
        assert_eq!(
            bin.read("gobstopper", BINARY_LIMIT).unwrap().unwrap(),
            b"old"
        );
        assert_eq!(
            state.read("install.json", RECEIPT_LIMIT).unwrap().unwrap(),
            b"old"
        );
    }

    #[test]
    fn installed_symlinks_and_shared_files_are_never_replaced() {
        let fixture = Fixture::new();
        let bin = fixture.bin();
        let foreign = fixture.root.join("foreign");
        fs::write(&foreign, b"preserve").unwrap();
        symlink(&foreign, bin.path.join("gobstopper")).unwrap();
        assert!(bin.read("gobstopper", BINARY_LIMIT).is_err());
        fs::remove_file(bin.path.join("gobstopper")).unwrap();
        fs::hard_link(&foreign, bin.path.join("gobstopper")).unwrap();
        assert!(bin.read("gobstopper", BINARY_LIMIT).is_err());
        assert_eq!(fs::read(&foreign).unwrap(), b"preserve");
    }

    #[test]
    fn manager_and_source_destinations_are_ineligible() {
        for path in [
            "/opt/homebrew/Cellar/gobstopper/1/bin/gobstopper",
            "/home/user/.cargo/bin/gobstopper",
            "/work/target/release/gobstopper",
        ] {
            assert!(eligible_destination(Path::new(path)).is_err());
        }
        let fixture = Fixture::new();
        fs::create_dir(fixture.root.join(".git")).unwrap();
        fs::write(fixture.root.join("Cargo.toml"), b"").unwrap();
        assert!(eligible_destination(&fixture.root.join("bin/gobstopper")).is_err());
    }

    #[test]
    fn initial_enrollment_refuses_an_unpublished_source_build() {
        if matches!(product().running_identity, RunningIdentity::Source) {
            let args = InitialInstall {
                archive: "/missing".into(),
                checksum: "/missing".into(),
                prefix: "/missing".into(),
                pinned: false,
            };
            assert!(initial_install(&args)
                .unwrap_err()
                .to_string()
                .contains("source build"));
        }
    }

    #[test]
    fn reinstall_preserves_pins_and_rejects_foreign_receipts() {
        let fixture = Fixture::new();
        let bin = fixture.bin();
        let published = published();
        let profile = product();
        let binary = b"verified binary";
        let mut receipt = published.receipt(&bin.path.join("gobstopper"), digest(binary), true);
        let bytes = serde_json::to_vec(&receipt).unwrap();
        assert!(verify_previous(
            &profile,
            &published,
            &bin,
            Some(binary),
            Some(&bytes),
            false
        )
        .unwrap());
        receipt.release_tag = "v0.9.0".into();
        receipt.archive_name = profile.asset_names("v0.9.0").unwrap()[0].clone();
        let bytes = serde_json::to_vec(&receipt).unwrap();
        assert!(verify_previous(
            &profile,
            &published,
            &bin,
            Some(binary),
            Some(&bytes),
            false
        )
        .is_err());
        assert!(
            verify_previous(&profile, &published, &bin, Some(binary), Some(&bytes), true).unwrap()
        );
        receipt.kind = InstallationKind::Cargo;
        assert!(verify_previous(
            &profile,
            &published,
            &bin,
            Some(binary),
            Some(&serde_json::to_vec(&receipt).unwrap()),
            true
        )
        .is_err());
    }

    struct NeverNetwork;
    impl ReleaseSource for NeverNetwork {
        fn releases(&self, _: &Product) -> hraness_cli_update::Result<Vec<Release>> {
            panic!("offline command must not fetch")
        }
    }

    #[test]
    fn initial_install_keeps_preferences_separate_from_ownership() {
        let fixture = Fixture::new();
        let bin = fixture.bin();
        bin.write_new("gobstopper", b"source bytes", true).unwrap();
        let target = bin.path.join("gobstopper");
        let profile = product();
        let update_paths = paths(&target).unwrap();
        let updater =
            Updater::for_executable(profile.clone(), update_paths.clone(), target.clone()).unwrap();
        updater
            .execute(
                hraness_cli_update::CommandAction::Disable,
                &NeverNetwork,
                &NativeInstaller,
            )
            .unwrap();
        assert!(update_paths.state_dir.exists());
        assert!(!update_paths.receipt.parent().unwrap().exists());
        bin.remove("gobstopper", false).unwrap();
        let mut stage = Stage::new(&bin).unwrap();
        stage
            .directory
            .write_new("gobstopper", b"verified native bytes", true)
            .unwrap();
        let pinned = enroll_verified(
            &profile,
            &published(),
            &bin,
            &mut stage,
            &target,
            &digest(b"verified native bytes"),
            false,
        )
        .unwrap();
        assert!(!pinned);
        let receipt: InstallReceipt =
            serde_json::from_slice(&fs::read(&update_paths.receipt).unwrap()).unwrap();
        assert_eq!(receipt.binary_sha256, digest(b"verified native bytes"));
        assert!(update_paths
            .receipt
            .parent()
            .unwrap()
            .join("activity.lock")
            .exists());
        let status = updater
            .execute(
                hraness_cli_update::CommandAction::Status,
                &NeverNetwork,
                &NativeInstaller,
            )
            .unwrap();
        assert_eq!(status.policy, hraness_cli_update::Policy::Disabled);
    }

    #[test]
    fn installer_lock_survives_service_drain_and_manual_helper_lifetimes() {
        let fixture = Fixture::new();
        let bin = fixture.bin();
        let state = fixture.state();
        bin.write_new("gobstopper", b"release bytes", true).unwrap();
        let target = bin.path.join("gobstopper");
        let paths = paths(&target).unwrap();
        let mut profile = product();
        profile.running_identity = RunningIdentity::Release {
            release_tag: "v1.0.0",
            build_sha: None,
        };
        let receipt = published().receipt(&target, digest(b"release bytes"), false);
        receipt.write_verified(&profile, &paths.receipt).unwrap();
        let updater = Updater::for_executable(profile, paths, target).unwrap();
        let mut context = StartupContext::from_process();
        context.args = vec!["proxy".into(), "serve".into()];
        context.no_update = true;
        let outcome = updater
            .startup(&context, &NeverNetwork, &NativeInstaller)
            .unwrap();
        let StartupOutcome::Continue {
            lease: Some(lease), ..
        } = outcome
        else {
            panic!("command must retain an active lease")
        };
        assert!(state.lock().is_err());
        let mut admission = crate::proxy_drain::Admission::default();
        let now = std::time::Instant::now();
        let mut request = serde_json::json!({
            "owner": "0123456789abcdef", "epoch": 0, "wait_secs": 600
        });
        admission.control("acquire", &request, 1, now).unwrap();
        assert!(state.lock().is_err());
        request["epoch"] = serde_json::json!(1);
        admission.control("commit", &request, 0, now).unwrap();
        assert!(state.lock().is_err());
        // A manual service helper keeps its own installation lease after the
        // drained service exits, including while it replaces service files.
        context.args = vec!["proxy".into(), "repair".into()];
        let StartupOutcome::Continue {
            lease: Some(helper),
            ..
        } = updater
            .startup(&context, &NeverNetwork, &NativeInstaller)
            .unwrap()
        else {
            panic!("service helper must retain an active lease")
        };
        drop(lease);
        assert!(state.lock().is_err());
        drop(helper);
        state.lock().unwrap();
    }

    #[test]
    fn detached_replacement_can_rollback_after_service_lease_exits() {
        let fixture = Fixture::new();
        let bin = fixture.bin();
        bin.write_new("gobstopper", b"old release", true).unwrap();
        let executable = bin.path.join("gobstopper");
        let update_paths = paths(&executable).unwrap();
        let state = fixture.state();
        let mut profile = product();
        profile.running_identity = RunningIdentity::Release {
            release_tag: "v1.0.0",
            build_sha: None,
        };
        let previous = published().receipt(&executable, digest(b"old release"), false);
        previous
            .write_verified(&profile, &update_paths.receipt)
            .unwrap();
        let updater = Updater::for_executable(profile, update_paths, executable.clone()).unwrap();
        let mut context = StartupContext::from_process();
        context.args = vec!["proxy".into(), "serve".into()];
        context.no_update = true;
        let StartupOutcome::Continue {
            lease: Some(lease), ..
        } = updater
            .startup(&context, &NeverNetwork, &NativeInstaller)
            .unwrap()
        else {
            panic!("service startup must hold an activity lease")
        };

        let mut stage = Stage::new(&bin).unwrap();
        stage
            .directory
            .write_new("gobstopper", b"new release", true)
            .unwrap();
        let next = published_for("v1.0.1").receipt(&executable, digest(b"new release"), true);
        stage
            .directory
            .write_new("new-receipt", &serde_json::to_vec(&next).unwrap(), false)
            .unwrap();
        stage.preserve = true;
        let (stage_device, stage_inode) = stage.directory.identity().unwrap();
        let prepared = PreparedUpgrade {
            stage_device,
            stage_inode,
            from_receipt_sha256: digest(&fs::read(paths(&executable).unwrap().receipt).unwrap()),
            to_receipt_sha256: digest(&serde_json::to_vec(&next).unwrap()),
            from: previous.clone(),
            to: next,
        };
        assert!(state.lock().is_err());
        drop(lease);
        verify_previous_upgrade(&executable, "v1.0.0").unwrap();
        let restarted = replace_upgrade(&executable, &stage.directory.path, &prepared).unwrap();
        assert!(state.lock().is_err());
        drop(restarted);
        assert_eq!(
            bin.read("gobstopper", BINARY_LIMIT).unwrap().unwrap(),
            b"new release"
        );
        let restarted = restore_upgrade(&executable, &stage.directory.path, &prepared).unwrap();
        assert!(state.lock().is_err());
        drop(restarted);
        verify_previous_upgrade(&executable, "v1.0.0").unwrap();
        stage
            .directory
            .write_new("gobstopper", b"invalid release", true)
            .unwrap();
        assert!(replace_upgrade(&executable, &stage.directory.path, &prepared).is_err());
        verify_previous_upgrade(&executable, "v1.0.0").unwrap();
        assert_eq!(
            bin.read("gobstopper", BINARY_LIMIT).unwrap().unwrap(),
            b"old release"
        );
        let restored: InstallReceipt =
            serde_json::from_slice(&fs::read(state.path.join("install.json")).unwrap()).unwrap();
        assert_eq!(restored.binary_sha256, previous.binary_sha256);
    }
}
