use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub(crate) struct ControllerArgs {
    #[arg(long)]
    pub stage: PathBuf,
    #[arg(long)]
    pub state_dir: PathBuf,
}

#[derive(Debug)]
pub(crate) struct PendingUpgrade;

impl std::fmt::Display for PendingUpgrade {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "an upgrade is pending; see gobstopper proxy doctor"
        )
    }
}

impl std::error::Error for PendingUpgrade {}

#[cfg(not(unix))]
pub(crate) use unsupported::{controller, run};

#[cfg(any(not(unix), test))]
mod unsupported {
    use super::*;

    pub(super) fn run(_version: Option<&str>, _wait: bool, _print: bool) -> anyhow::Result<()> {
        anyhow::bail!("detached upgrade is unsupported on this platform")
    }

    pub(super) fn controller(_args: &ControllerArgs) -> anyhow::Result<()> {
        anyhow::bail!("detached upgrade is unsupported on this platform")
    }

    #[cfg(test)]
    #[test]
    fn upgrade_and_controller_report_unsupported_platform() {
        assert!(run(None, false, false)
            .unwrap_err()
            .to_string()
            .contains("unsupported"));
        let args = ControllerArgs {
            stage: PathBuf::new(),
            state_dir: PathBuf::new(),
        };
        assert!(controller(&args)
            .unwrap_err()
            .to_string()
            .contains("unsupported"));
    }
}

#[cfg(unix)]
pub(crate) use platform::{controller, reconcile, run};

#[cfg(unix)]
mod platform {
    use super::*;
    use anyhow::{bail, ensure, Context, Result};
    use serde::{Deserialize, Serialize};
    use std::path::Path;
    use std::time::{Duration, Instant};

    #[derive(Clone, Debug, Deserialize, Serialize)]
    struct Journal {
        schema: u32,
        stage: String,
        stage_dir: PathBuf,
        executable: PathBuf,
        from_version: String,
        to_version: String,
        reason: Option<String>,
        #[serde(default)]
        owner_pid: Option<u32>,
        #[serde(default)]
        controller_pid: Option<u32>,
        #[serde(default)]
        starting_at_unix_ms: Option<u128>,
    }

    fn read_journal() -> Result<Option<Journal>> {
        let path = crate::proxy_agent::upgrade_journal_path()?;
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(
                serde_json::from_slice(&bytes).context("Upgrade journal is damaged")?,
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn save(journal: &Journal) -> Result<()> {
        crate::proxy_agent::upgrade_write_journal(&serde_json::to_vec_pretty(journal)?)
    }

    fn advance(journal: &mut Journal, stage: &str) -> Result<()> {
        if stage == "starting" {
            journal.starting_at_unix_ms = Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_millis(),
            );
        }
        journal.stage = stage.to_owned();
        save(journal)
    }

    fn repair_outcome(
        journal: &Journal,
        doctor: &serde_json::Value,
        owner_alive: bool,
    ) -> Option<&'static str> {
        if !journal.to_version.is_empty()
            && doctor["healthy"] == true
            && doctor["live_version"] == journal.to_version.trim_start_matches('v')
        {
            Some("healthy")
        } else if !owner_alive {
            Some("failed")
        } else {
            None
        }
    }

    fn pid_alive(pid: u32) -> bool {
        (unsafe { libc::kill(pid as libc::pid_t, 0) == 0 })
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    pub(crate) fn reconcile() -> Result<()> {
        let Some(mut journal) = read_journal()? else {
            return Ok(());
        };
        if matches!(journal.stage.as_str(), "healthy" | "failed" | "rolled_back") {
            return Ok(());
        }
        let doctor = crate::proxy_agent::inspect()?;
        let owner = journal.controller_pid.or(journal.owner_pid);
        match repair_outcome(&journal, &doctor, owner.is_some_and(pid_alive)) {
            Some("healthy") => {
                advance(&mut journal, "healthy")?;
                println!("Upgrade recovery found the target service healthy.");
            }
            Some("failed") => {
                journal.reason = Some("controller_exited".into());
                advance(&mut journal, "failed")?;
                eprintln!(
                    "Upgrade controller exited; staged files and any backup remain at {}",
                    if journal.stage_dir.as_os_str().is_empty() {
                        "(stage was not created)".to_owned()
                    } else {
                        journal.stage_dir.display().to_string()
                    }
                );
            }
            _ => bail!(PendingUpgrade),
        }
        Ok(())
    }

    fn reject_pending(prior: Option<&Journal>) -> Result<()> {
        if prior.is_some_and(|journal| {
            !matches!(journal.stage.as_str(), "healthy" | "failed" | "rolled_back")
        }) {
            return Err(PendingUpgrade.into());
        }
        Ok(())
    }

    pub(crate) fn run(version: Option<&str>, wait: bool, print: bool) -> Result<()> {
        if print {
            let (executable, live_version) = crate::proxy_agent::upgrade_probe(true)?;
            reject_pending(read_journal()?.as_ref())?;
            return run_selected(version, wait, true, executable, live_version);
        }
        let (executable, live_version) = crate::proxy_agent::upgrade_claim(|| {
            let (executable, live_version) = crate::proxy_agent::upgrade_probe(true)?;
            reject_pending(read_journal()?.as_ref())?;
            save(&Journal {
                schema: 1,
                stage: "staging".into(),
                stage_dir: PathBuf::new(),
                executable: executable.clone(),
                from_version: live_version.clone(),
                to_version: String::new(),
                reason: None,
                owner_pid: Some(std::process::id()),
                controller_pid: None,
                starting_at_unix_ms: None,
            })?;
            Ok((executable, live_version))
        })?;
        let result = run_selected(version, wait, false, executable, live_version);
        if let Err(error) = &result {
            if let Some(mut journal) = read_journal()? {
                if journal.stage == "staging" {
                    journal.reason = Some(format!("{error:#}"));
                    advance(&mut journal, "failed")?;
                }
            }
        }
        result
    }

    fn run_selected(
        version: Option<&str>,
        wait: bool,
        print: bool,
        executable: PathBuf,
        live_version: String,
    ) -> Result<()> {
        let selected = crate::self_update::upgrade_target(&executable, version)?;
        ensure!(
            live_version == selected.current.trim_start_matches('v'),
            "running proxy version differs from the installed release receipt"
        );
        if selected.current == selected.target {
            if !print {
                let mut journal = read_journal()?.context("Missing upgrade reservation")?;
                journal.to_version = selected.target.clone();
                advance(&mut journal, "healthy")?;
            }
            println!("Managed proxy already runs {}.", selected.target);
            return Ok(());
        }
        let stage = executable
            .parent()
            .context("executable directory")?
            .join(format!(".gobstopper-update-{}-0", std::process::id()));
        let planned_job = crate::proxy_agent::upgrade_job(&executable, &stage)?;
        if print {
            println!(
                "{} -> {}\nStaging: {}\nJob: {}\n{}",
                selected.current,
                selected.target,
                stage.display(),
                planned_job.definition.display(),
                planned_job.text
            );
            return Ok(());
        }
        let stage = crate::self_update::prepare_upgrade(&selected, version.is_some())?;
        let job = crate::proxy_agent::upgrade_job(&executable, &stage)?;
        let mut journal = Journal {
            schema: 1,
            stage: "started".into(),
            stage_dir: stage.clone(),
            executable: executable.clone(),
            from_version: selected.current,
            to_version: selected.target,
            reason: None,
            owner_pid: Some(std::process::id()),
            controller_pid: None,
            starting_at_unix_ms: None,
        };
        save(&journal)?;
        if let Err(error) = crate::proxy_agent::upgrade_launch(&job, &executable, &stage) {
            journal.reason = Some(format!("{error:#}"));
            advance(&mut journal, "failed")?;
            let _ = crate::self_update::discard_upgrade(&executable, &stage);
            return Err(error);
        }
        println!("Detached upgrade job: sh.gobstopper.upgrade (Linux: gobstopper-upgrade)\nLog: {}\nFollow with: gobstopper proxy doctor", job.log.display());
        if wait {
            let deadline = Instant::now() + Duration::from_secs(900);
            loop {
                let status = crate::proxy_agent::inspect()?;
                let stage = status["upgrade"]["stage"].as_str().unwrap_or("unknown");
                match stage {
                    "healthy" => return Ok(()),
                    "failed" | "rolled_back" => bail!(
                        "proxy upgrade {stage}: {}",
                        status["upgrade"]["reason"]
                            .as_str()
                            .unwrap_or("unknown reason")
                    ),
                    _ if Instant::now() >= deadline => bail!(
                        "upgrade is still {stage}; inspect proxy doctor and {}",
                        job.log.display()
                    ),
                    _ => std::thread::sleep(Duration::from_millis(200)),
                }
            }
        }
        Ok(())
    }

    pub(crate) fn controller(args: &ControllerArgs) -> Result<()> {
        let config = args
            .state_dir
            .parent()
            .and_then(Path::parent)
            .context("Invalid service state directory")?;
        std::env::set_var("XDG_CONFIG_HOME", config);
        ensure!(
            crate::proxy_agent::upgrade_journal_path()?.parent() == Some(args.state_dir.as_path()),
            "Controller service state differs from its job definition"
        );
        let job = crate::proxy_agent::upgrade_job(&std::env::current_exe()?, &args.stage)?;
        crate::proxy_agent::upgrade_unlink_on_start(&job)?;
        let result = controller_inner(args);
        let cleanup = crate::proxy_agent::upgrade_cleanup(&job);
        result?;
        cleanup
    }

    fn controller_inner(args: &ControllerArgs) -> Result<()> {
        let mut journal = read_journal()?.context("No staged upgrade journal")?;
        ensure!(
            journal.schema == 1 && journal.stage == "started" && journal.stage_dir == args.stage,
            "Upgrade journal does not match this controller"
        );
        journal.controller_pid = Some(std::process::id());
        save(&journal)?;
        let result = execute(&mut journal);
        if let Err(error) = &result {
            if !matches!(journal.stage.as_str(), "failed" | "rolled_back") {
                journal.reason = Some(format!("{error:#}"));
                let _ = advance(&mut journal, "failed");
            }
        }
        result
    }

    fn execute(journal: &mut Journal) -> Result<()> {
        let mut control = crate::proxy_agent::UpgradeControl::open(&journal.executable)?;
        advance(journal, "draining")?;
        if let Err(error) = control.stop() {
            journal.reason = Some(format!("{error:#}"));
            advance(journal, "failed")?;
            return Err(error);
        }
        if let Err(error) = advance(journal, "stopped").and_then(|()| advance(journal, "replacing"))
        {
            return rollback(journal, &mut control, error, false);
        }
        if let Err(error) =
            crate::self_update::replace_upgrade(&journal.executable, &journal.stage_dir)
        {
            return rollback(journal, &mut control, error, false);
        }
        if let Err(error) = advance(journal, "replaced").and_then(|()| advance(journal, "starting"))
        {
            return rollback(journal, &mut control, error, true);
        }
        if let Err(error) = control.start(
            journal.to_version.trim_start_matches('v'),
            journal
                .starting_at_unix_ms
                .context("Missing upgrade start time")?,
        ) {
            return rollback(journal, &mut control, error, true);
        }
        advance(journal, "healthy")?;
        if let Err(error) =
            crate::self_update::discard_upgrade(&journal.executable, &journal.stage_dir)
        {
            eprintln!("Upgrade succeeded; staged backup cleanup failed: {error:#}");
        }
        Ok(())
    }

    fn rollback(
        journal: &mut Journal,
        control: &mut crate::proxy_agent::UpgradeControl,
        error: anyhow::Error,
        replaced: bool,
    ) -> Result<()> {
        let restored: Result<()> = (|| {
            if replaced {
                if control.stop().is_err() {
                    control.stop_started_unresponsive()?;
                    journal.reason = Some("new_version_unresponsive".into());
                }
                crate::self_update::restore_upgrade(&journal.executable, &journal.stage_dir)?;
            } else {
                crate::self_update::verify_previous_upgrade(
                    &journal.executable,
                    &journal.from_version,
                )?;
            }
            control.restart_previous()?;
            Ok(())
        })();
        if journal.reason.is_none() {
            journal.reason = Some(format!("{error:#}"));
        }
        match restored {
            Ok(()) => {
                advance(journal, "rolled_back")?;
                if let Err(cleanup_error) =
                    crate::self_update::discard_upgrade(&journal.executable, &journal.stage_dir)
                {
                    eprintln!(
                        "Previous proxy restored; staged backup cleanup failed: {cleanup_error:#}"
                    );
                }
                bail!("upgrade failed and previous proxy was restored: {error:#}")
            }
            Err(rollback_error) => {
                advance(journal, "failed")?;
                bail!("upgrade failed: {error:#}; restoration cannot be proven: {rollback_error:#}; inspect proxy doctor and repair")
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn journal_stages_and_doctor_fields_serialize() {
            let mut journal = Journal {
                schema: 1,
                stage: "started".into(),
                stage_dir: "/tmp/stage".into(),
                executable: "/tmp/bin/gobstopper".into(),
                from_version: "v1.0.0".into(),
                to_version: "v1.0.1".into(),
                reason: None,
                owner_pid: None,
                controller_pid: None,
                starting_at_unix_ms: None,
            };
            for stage in [
                "draining",
                "stopped",
                "replacing",
                "replaced",
                "starting",
                "healthy",
            ] {
                journal.stage = stage.into();
                let doctor: serde_json::Value = serde_json::to_value(&journal).unwrap();
                assert_eq!(doctor["stage"], stage);
                assert_eq!(doctor["to_version"], "v1.0.1");
            }
            journal.stage = "rolled_back".into();
            journal.reason = Some("startup failed".into());
            let doctor: serde_json::Value = serde_json::to_value(&journal).unwrap();
            assert_eq!(doctor["stage"], "rolled_back");
            assert_eq!(doctor["reason"], "startup failed");
        }

        #[test]
        fn second_upgrade_refuses_while_staging_or_running() {
            let mut journal = Journal {
                schema: 1,
                stage: "staging".into(),
                stage_dir: PathBuf::new(),
                executable: PathBuf::new(),
                from_version: String::new(),
                to_version: String::new(),
                reason: None,
                owner_pid: None,
                controller_pid: None,
                starting_at_unix_ms: None,
            };
            for stage in ["staging", "started", "draining", "starting"] {
                journal.stage = stage.into();
                let error = reject_pending(Some(&journal)).unwrap_err();
                assert!(error.downcast_ref::<PendingUpgrade>().is_some());
                assert_eq!(
                    error.to_string(),
                    "an upgrade is pending; see gobstopper proxy doctor"
                );
            }
            journal.stage = "failed".into();
            assert!(reject_pending(Some(&journal)).is_ok());
        }

        #[test]
        fn repair_reconciles_healthy_target_and_dead_controller() {
            let journal = Journal {
                schema: 1,
                stage: "starting".into(),
                stage_dir: "/tmp/stage".into(),
                executable: PathBuf::new(),
                from_version: "v1.0.0".into(),
                to_version: "v1.0.1".into(),
                reason: None,
                owner_pid: None,
                controller_pid: Some(42),
                starting_at_unix_ms: None,
            };
            assert_eq!(
                repair_outcome(
                    &journal,
                    &serde_json::json!({"healthy": true, "live_version": "1.0.1"}),
                    false
                ),
                Some("healthy")
            );
            assert_eq!(
                repair_outcome(&journal, &serde_json::json!({"healthy": false}), false),
                Some("failed")
            );
            assert_eq!(
                repair_outcome(&journal, &serde_json::json!({"healthy": false}), true),
                None
            );
        }
    }
}
