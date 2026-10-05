//! Executable maintenance runs before application configuration or provider I/O.
//! Release identity is embedded by the official packaging scripts, never inferred
//! from a receipt, a pathname, the runtime environment, or CARGO's version alone.
use anyhow::{bail, Result};
use clap::{Args, ValueEnum};
use hraness_cli_update::{
    ActiveLease, Channel, CommandAction, CurlGithub, Paths, Product, RunningIdentity,
    StartupContext, StartupOutcome, UpdateResult, UpdateStatus, Updater,
};
use std::path::{Path, PathBuf};

#[cfg(unix)]
#[path = "self_update/unix.rs"]
mod native;

#[cfg(unix)]
pub(crate) use native::{ManagedDirectory, PreparedUpgrade, UpgradeLease, UpgradeTarget};

#[cfg(unix)]
pub(crate) fn upgrade_target(executable: &Path, version: Option<&str>) -> Result<UpgradeTarget> {
    native::upgrade_target(executable, version)
}

#[cfg(unix)]
pub(crate) fn planned_upgrade_stage(executable: &Path) -> Result<PathBuf> {
    native::planned_upgrade_stage(executable)
}

#[cfg(unix)]
pub(crate) fn prepare_upgrade(
    selected: &UpgradeTarget,
    explicit: bool,
    stage: &Path,
) -> Result<PreparedUpgrade> {
    native::prepare_upgrade(selected, explicit, stage)
}

#[cfg(unix)]
pub(crate) fn verify_prepared_upgrade(
    executable: &Path,
    stage: &Path,
    prepared: &PreparedUpgrade,
) -> Result<()> {
    native::verify_prepared_upgrade(executable, stage, prepared)
}

#[cfg(unix)]
pub(crate) fn replace_upgrade(
    executable: &Path,
    stage: &Path,
    prepared: &PreparedUpgrade,
) -> Result<UpgradeLease> {
    native::replace_upgrade(executable, stage, prepared)
}

#[cfg(unix)]
pub(crate) fn restore_upgrade(
    executable: &Path,
    stage: &Path,
    prepared: &PreparedUpgrade,
) -> Result<UpgradeLease> {
    native::restore_upgrade(executable, stage, prepared)
}

#[cfg(unix)]
pub(crate) fn protect_previous_upgrade(
    executable: &Path,
    prepared: &PreparedUpgrade,
) -> Result<UpgradeLease> {
    native::protect_previous_upgrade(executable, prepared)
}

#[cfg(unix)]
pub(crate) fn verify_installed_upgrade(
    executable: &Path,
    prepared: &PreparedUpgrade,
    target: bool,
) -> Result<()> {
    native::verify_installed_upgrade(executable, prepared, target)
}

#[cfg(unix)]
pub(crate) fn discard_bound_upgrade(
    executable: &Path,
    stage: &Path,
    prepared: &PreparedUpgrade,
) -> Result<()> {
    native::discard_bound_upgrade(executable, stage, prepared)
}

#[cfg(all(unix, test))]
pub(crate) fn verify_previous_upgrade(executable: &Path, version: &str) -> Result<()> {
    native::verify_previous_upgrade(executable, version)
}

#[cfg(all(unix, test))]
pub(crate) use native::tests::{assert_installation_busy, replaced_install, staged_install};

#[cfg(unix)]
pub(crate) fn discard_upgrade(executable: &Path, stage: &Path) -> Result<()> {
    native::discard_upgrade(executable, stage)
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum Action {
    Install,
    Check,
    Status,
    Enable,
    Disable,
}

#[derive(Args)]
pub(crate) struct UpdateArgs {
    /// Install, check, inspect settings, enable automatic updates, or disable them.
    #[arg(value_enum, default_value = "install")]
    action: Action,
    /// Print the update result as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
pub(crate) struct InitialInstall {
    #[arg(long)]
    pub archive: PathBuf,
    #[arg(long)]
    pub checksum: PathBuf,
    #[arg(long)]
    pub prefix: PathBuf,
    /// An explicitly selected version remains fixed.
    #[arg(long)]
    pub pinned: bool,
}

pub(crate) fn product() -> Product {
    Product {
        id: "gobstopper".into(), repository: "hraness/gobstopper".into(), tag_prefix: "v".into(),
        channel: Channel::Stable,
        running_identity: match option_env!("GOBSTOPPER_COMPILED_RELEASE_TAG") {
            Some(tag) if !tag.is_empty() => RunningIdentity::Release { release_tag: tag, build_sha: None },
            _ => RunningIdentity::Source,
        },
        executable_name: if cfg!(windows) { "gobstopper.exe" } else { "gobstopper" }.into(),
        platform: platform().into(),
        required_assets: if cfg!(windows) {
            vec!["gobstopper-{version}-{platform}.zip".into(), "gobstopper-{version}-{platform}.zip.sha256".into()]
        } else {
            vec!["gobstopper-{version}-{platform}.tar.gz".into(), "gobstopper-{version}-{platform}.tar.gz.sha256".into()]
        },
        require_immutable: true,
        manual_instructions: "Re-run Gobstopper's verified installer for native releases. Use cargo install for Cargo copies. Windows updates use install.ps1; source builds stay under their source workflow.".into(),
    }
}

fn platform() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "darwin-aarch64"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux-x86_64"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "linux-aarch64"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "windows-x86_64"
    } else {
        "unsupported"
    }
}

pub(crate) fn paths(executable: &Path) -> Result<Paths> {
    let directory = executable
        .parent()
        .ok_or_else(|| anyhow::anyhow!("executable has no parent"))?
        .join(".hraness-cli-update-gobstopper");
    Ok(Paths {
        receipt: directory.join("install.json"),
        // A source/Cargo user may save an opt-out. Preferences must not create
        // the separate managed-install activity authority without a receipt.
        state_dir: executable
            .parent()
            .unwrap()
            .join(".hraness-cli-update-gobstopper-preferences"),
    })
}

fn updater() -> Result<Updater> {
    let executable = std::env::current_exe()?.canonicalize()?;
    Ok(Updater::new(product(), paths(&executable)?)?)
}

fn client() -> Result<CurlGithub> {
    Ok(CurlGithub::new(if cfg!(windows) {
        "C:/Windows/System32/curl.exe"
    } else {
        "/usr/bin/curl"
    })?)
}

#[cfg(not(unix))]
struct UnsupportedInstaller;
#[cfg(not(unix))]
impl hraness_cli_update::Installer for UnsupportedInstaller {
    fn install(
        &self,
        _: &hraness_cli_update::InstallRequest<'_>,
    ) -> hraness_cli_update::Result<()> {
        Err(hraness_cli_update::Error::new(
            hraness_cli_update::ErrorCode::Unsupported,
            "Use the verified Windows installer.",
        ))
    }
}

fn print_result(report: &UpdateResult, json: bool) -> Result<()> {
    if json {
        println!("{}", report.json()?);
    } else {
        println!(
            "Gobstopper updates: {:?} (automatic policy: {:?})",
            report.status, report.policy
        );
        if let Some(reason) = &report.reason {
            println!("{reason}");
        }
        if let Some(instructions) = &report.instructions {
            println!("{instructions}");
        }
        if let Some(current) = &report.current {
            println!("Installed: {current}");
        }
        if let Some(available) = &report.latest {
            println!("Available: {available}");
        }
    }
    Ok(())
}

pub(crate) fn explicit(args: &UpdateArgs) -> Result<()> {
    let updater = updater()?;
    let action = match args.action {
        Action::Install => CommandAction::Install,
        Action::Check => CommandAction::Check,
        Action::Status => CommandAction::Status,
        Action::Enable => CommandAction::Enable,
        Action::Disable => CommandAction::Disable,
    };
    let source = client()?;
    #[cfg(unix)]
    let installer = native::NativeInstaller;
    #[cfg(not(unix))]
    let installer = UnsupportedInstaller;
    let mut report = updater.execute_with_context(
        action,
        &StartupContext::from_process(),
        &source,
        &installer,
    )?;
    if report.status == UpdateStatus::Busy {
        report.instructions = Some(busy_instructions(
            crate::proxy_agent::installed(),
            report.instructions.take(),
        ));
    }
    print_result(&report, args.json)
}

/// `update` reports Busy whenever another command holds the installation's
/// activity lock — which a managed proxy does for its entire lifetime, so a
/// bare retry can never succeed while the service runs. Say who is holding it
/// and name the path that actually works.
fn busy_instructions(service_installed: bool, existing: Option<String>) -> String {
    let hint = if service_installed {
        "A managed Gobstopper service holds this installation's update lock for \
        its entire lifetime. `gobstopper proxy upgrade` pauses requests, \
        replaces the executable, and restarts the service."
    } else {
        "Running Gobstopper commands hold this installation's update lock for \
        their entire lifetime. Retry after they finish, or stop them first."
    };
    match existing {
        Some(previous) => format!("{previous}\n{hint}"),
        None => hint.into(),
    }
}

/// A verified install receipt beside this executable means another running
/// command holds its activity lock for life — continuous daemons on a managed
/// path permanently block updates.
pub(crate) fn installation_enrolled(executable: &Path) -> bool {
    executable.is_absolute()
        && paths(executable)
            .map(|paths| paths.receipt.is_file())
            .unwrap_or(false)
}

/// Machine-readable enrollment for `proxy doctor`: whether the service's
/// executable is a managed release installation and what its update policy is.
/// Reads local state only; never touches the network.
pub(crate) fn installation_report(executable: &Path) -> serde_json::Value {
    let report = (|| -> Result<serde_json::Value> {
        let updater =
            Updater::for_executable(product(), paths(executable)?, executable.to_path_buf())?;
        #[cfg(unix)]
        let installer = native::NativeInstaller;
        #[cfg(not(unix))]
        let installer = UnsupportedInstaller;
        let policy = updater
            .execute(CommandAction::Status, &client()?, &installer)
            .ok()
            .map(|report| report.policy);
        match updater.inspect() {
            Ok(installation) => Ok(serde_json::json!({
                "managed": true,
                "kind": installation.receipt.kind,
                "release_tag": installation.receipt.release_tag,
                "pinned": installation.receipt.pinned,
                "policy": policy,
            })),
            Err(_) => Ok(serde_json::json!({
                "managed": false,
                "policy": policy,
            })),
        }
    })();
    report.unwrap_or_else(|_| serde_json::json!({"managed": false}))
}

/// unix replacement fails fast while any command holds the activity lock.
#[cfg(unix)]
pub(crate) fn is_installation_busy(error: &anyhow::Error) -> bool {
    native::is_installation_busy(error)
}

#[cfg(not(unix))]
pub(crate) fn is_installation_busy(_: &anyhow::Error) -> bool {
    false
}

pub(crate) fn startup(offline: bool, no_update: bool) -> Result<Option<ActiveLease>> {
    let mut context = StartupContext::from_process();
    context.offline = offline;
    context.no_update |= no_update;
    #[cfg(unix)]
    {
        context.no_update |= unsafe { libc::geteuid() } == 0;
    }
    let updater = updater()?;
    let source = client()?;
    #[cfg(unix)]
    let installer = native::NativeInstaller;
    #[cfg(not(unix))]
    let installer = UnsupportedInstaller;
    match updater.startup(&context, &source, &installer)? {
        StartupOutcome::Continue { lease, .. } => Ok(lease),
        StartupOutcome::Reenter(reentry) => reentry.run_and_exit(),
    }
}

pub(crate) fn initial_install(args: &InitialInstall) -> Result<()> {
    #[cfg(unix)]
    {
        native::initial_install(args)
    }
    #[cfg(not(unix))]
    {
        let _ = args;
        bail!("Use Gobstopper's verified Windows installer; in-process native updating is not supported on Windows.");
    }
}

pub(crate) fn build_identity() {
    let identity = match product().running_identity {
        RunningIdentity::Release { release_tag, .. } => Some(release_tag),
        RunningIdentity::Source => None,
    };
    println!(
        "{}",
        serde_json::json!({"schema":"gobstopper.build.v1","version":env!("CARGO_PKG_VERSION"),"releaseTag":identity,"platform":platform()})
    );
}

#[cfg(unix)]
pub(crate) fn released_tag() -> Result<&'static str> {
    match product().running_identity {
        RunningIdentity::Release { release_tag, .. } => Ok(release_tag),
        RunningIdentity::Source => {
            bail!("This source build cannot enroll as an official release installation.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_instructions_names_the_service_upgrade_path() {
        let text = busy_instructions(true, None);
        assert!(text.contains("proxy upgrade"));
        assert!(text.contains("managed Gobstopper service"));
        let text = busy_instructions(true, Some("prior".into()));
        assert!(text.starts_with("prior\n"));
    }

    #[test]
    fn busy_instructions_without_a_service_points_at_holders() {
        let text = busy_instructions(false, None);
        assert!(text.contains("Retry after they finish"));
        assert!(!text.contains("proxy upgrade"));
    }

    #[test]
    fn installation_enrolled_requires_an_existing_receipt() {
        let root =
            std::env::temp_dir().join(format!("gobstopper-enrolled-test-{}", std::process::id()));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("gobstopper");
        std::fs::write(&exe, b"binary").unwrap();
        assert!(!installation_enrolled(&exe));
        let receipt_dir = bin.join(".hraness-cli-update-gobstopper");
        std::fs::create_dir_all(&receipt_dir).unwrap();
        std::fs::write(receipt_dir.join("install.json"), b"{}").unwrap();
        assert!(installation_enrolled(&exe));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
