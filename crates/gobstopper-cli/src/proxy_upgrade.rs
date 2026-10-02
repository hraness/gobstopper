use anyhow::{bail, ensure, Context, Result};
use clap::Args;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Args)]
pub(crate) struct ControllerArgs {
    #[arg(long)]
    pub stage: PathBuf,
    #[arg(long)]
    pub state_dir: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Journal {
    schema: u32,
    stage: String,
    stage_dir: PathBuf,
    executable: PathBuf,
    from_version: String,
    to_version: String,
    reason: Option<String>,
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
    journal.stage = stage.to_owned();
    save(journal)
}

pub(crate) fn run(version: Option<&str>, wait: bool, print: bool) -> Result<()> {
    let (executable, live_version) = crate::proxy_agent::upgrade_probe(print)?;
    if let Some(prior) = read_journal()? {
        if !matches!(prior.stage.as_str(), "healthy" | "failed" | "rolled_back") {
            bail!(
                "an upgrade is pending at stage {}; inspect proxy doctor",
                prior.stage
            );
        }
    }
    let selected = crate::self_update::upgrade_target(&executable, version)?;
    ensure!(
        live_version == selected.current.trim_start_matches('v'),
        "running proxy version differs from the installed release receipt"
    );
    if selected.current == selected.target {
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
    let job = crate::proxy_agent::upgrade_job(&std::env::current_exe()?, &args.stage)?;
    let result = controller_inner(args);
    let cleanup = crate::proxy_agent::upgrade_cleanup(&job);
    result?;
    cleanup
}

fn controller_inner(args: &ControllerArgs) -> Result<()> {
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
    let mut journal = read_journal()?.context("No staged upgrade journal")?;
    ensure!(
        journal.schema == 1 && journal.stage == "started" && journal.stage_dir == args.stage,
        "Upgrade journal does not match this controller"
    );
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
    if let Err(error) = advance(journal, "stopped").and_then(|()| advance(journal, "replacing")) {
        return rollback(journal, &mut control, error, false);
    }
    if let Err(error) = crate::self_update::replace_upgrade(&journal.executable, &journal.stage_dir)
    {
        return rollback(journal, &mut control, error, false);
    }
    if let Err(error) = advance(journal, "replaced").and_then(|()| advance(journal, "starting")) {
        return rollback(journal, &mut control, error, true);
    }
    if let Err(error) = control.start(journal.to_version.trim_start_matches('v')) {
        return rollback(journal, &mut control, error, true);
    }
    advance(journal, "healthy")?;
    if let Err(error) = crate::self_update::discard_upgrade(&journal.executable, &journal.stage_dir)
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
            control.stop()?;
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
    journal.reason = Some(format!("{error:#}"));
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
}
