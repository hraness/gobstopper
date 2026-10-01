//! Executable maintenance runs before application configuration or provider I/O.
//! Release identity is embedded by the official packaging scripts, never inferred
//! from a receipt, a pathname, the runtime environment, or CARGO's version alone.
use anyhow::{bail, Result};
use clap::{Args, ValueEnum};
use hraness_cli_update::{
    ActiveLease, Channel, CommandAction, CurlGithub, Paths, Product, RunningIdentity,
    StartupContext, StartupOutcome, UpdateResult, Updater,
};
use std::path::{Path, PathBuf};

#[cfg(unix)]
#[path = "self_update/unix.rs"]
mod native;

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
    let report = updater.execute_with_context(
        action,
        &StartupContext::from_process(),
        &source,
        &installer,
    )?;
    print_result(&report, args.json)
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
