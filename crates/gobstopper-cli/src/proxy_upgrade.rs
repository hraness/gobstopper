use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub(crate) struct ControllerArgs {
    #[arg(long)]
    pub stage: PathBuf,
    #[arg(long)]
    pub state_dir: PathBuf,
    #[arg(long)]
    pub upgrade_id: String,
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

    pub(crate) fn run(
        _version: Option<&str>,
        _wait: bool,
        _print: bool,
        _allow_dependent_caller: bool,
    ) -> anyhow::Result<()> {
        anyhow::bail!("detached upgrade is unsupported on this platform")
    }

    pub(crate) fn controller(_args: &ControllerArgs) -> anyhow::Result<()> {
        anyhow::bail!("detached upgrade is unsupported on this platform")
    }

    #[cfg(test)]
    #[test]
    fn upgrade_and_controller_report_unsupported_platform() {
        assert!(run(None, false, false, false)
            .unwrap_err()
            .to_string()
            .contains("unsupported"));
        let args = ControllerArgs {
            stage: PathBuf::new(),
            state_dir: PathBuf::new(),
            upgrade_id: String::new(),
        };
        assert!(controller(&args)
            .unwrap_err()
            .to_string()
            .contains("unsupported"));
    }
}

#[cfg(unix)]
pub(crate) use platform::{controller, reconcile, require_no_pending_at, run};

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
        upgrade_id: String,
        #[serde(default)]
        revision: u64,
        #[serde(default)]
        service: Option<crate::proxy_agent::UpgradeServiceBinding>,
        #[serde(default)]
        prepared: Option<crate::self_update::PreparedUpgrade>,
        #[serde(default)]
        recovery_required: bool,
        #[serde(default)]
        owner_identity: Option<crate::proxy_agent::ProcessIdentity>,
        #[serde(default)]
        controller_identity: Option<crate::proxy_agent::ProcessIdentity>,
        #[serde(default)]
        started_identity: Option<crate::proxy_agent::ProcessIdentity>,
        #[serde(default)]
        starting_at_unix_ms: Option<u128>,
        #[serde(default)]
        rollback_deadline_unix_ms: Option<u128>,
    }

    fn read_journal_at(root: &Path) -> Result<Option<Journal>> {
        let Some(bytes) = crate::proxy_agent::upgrade_read_journal_at(root)? else {
            return Ok(None);
        };
        let journal: Journal =
            serde_json::from_slice(&bytes).context("Upgrade journal is damaged")?;
        ensure!(
            journal.schema == 1
                && matches!(
                    journal.stage.as_str(),
                    "staging"
                        | "started"
                        | "draining"
                        | "stopped"
                        | "replacing"
                        | "replaced"
                        | "starting"
                        | "healthy"
                        | "failed"
                        | "rolled_back"
                ),
            "Upgrade journal has an unsupported schema or stage; files were preserved"
        );
        Ok(Some(journal))
    }

    fn read_journal() -> Result<Option<Journal>> {
        let path = crate::proxy_agent::upgrade_journal_path()?;
        read_journal_at(path.parent().context("Upgrade journal has no parent")?)
    }

    fn same_reservation(previous: &Journal, journal: &Journal) -> bool {
        !journal.upgrade_id.is_empty()
            && previous.upgrade_id == journal.upgrade_id
            && previous.revision == journal.revision
            && previous.executable == journal.executable
            && previous.owner_identity == journal.owner_identity
            && previous.service == journal.service
            && previous
                .controller_identity
                .is_none_or(|identity| journal.controller_identity == Some(identity))
            && (previous.stage == "staging"
                || (previous.stage_dir == journal.stage_dir
                    && previous.prepared == journal.prepared
                    && previous.from_version == journal.from_version
                    && previous.to_version == journal.to_version))
    }

    fn save(journal: &mut Journal) -> Result<()> {
        let prior = read_journal()?.context("Upgrade reservation disappeared")?;
        ensure!(
            same_reservation(&prior, journal),
            "Upgrade journal changed; this controller no longer owns the operation"
        );
        let revision = journal
            .revision
            .checked_add(1)
            .context("Upgrade revision overflow")?;
        let mut next = journal.clone();
        next.revision = revision;
        let bytes = serde_json::to_vec_pretty(&next)?;
        let written = crate::proxy_agent::upgrade_write_journal(&bytes);
        if written.is_ok()
            || read_journal()?.as_ref().is_some_and(|saved| {
                serde_json::to_vec_pretty(saved).is_ok_and(|saved| saved == bytes)
            })
        {
            journal.revision = revision;
        }
        written
    }

    fn owned_reservation(upgrade_id: &str) -> Result<Journal> {
        let journal = read_journal()?.context("Missing upgrade reservation")?;
        ensure!(
            journal.upgrade_id == upgrade_id && !upgrade_id.is_empty(),
            "Upgrade reservation belongs to another operation"
        );
        Ok(journal)
    }

    pub(crate) fn require_no_pending_at(root: &Path) -> Result<()> {
        reject_pending(read_journal_at(root)?.as_ref())
    }

    fn advance(journal: &mut Journal, stage: &str) -> Result<()> {
        advance_recorded(journal, stage, &mut save)
    }

    fn advance_recorded(
        journal: &mut Journal,
        stage: &str,
        persist: &mut impl FnMut(&mut Journal) -> Result<()>,
    ) -> Result<()> {
        if stage == "starting" {
            let now = now_unix_ms()?;
            journal.starting_at_unix_ms = Some(now);
            journal.rollback_deadline_unix_ms = Some(
                now.checked_add(25_000)
                    .context("Rollback deadline overflow")?,
            );
        }
        if matches!(stage, "healthy" | "rolled_back") {
            journal.recovery_required = false;
        }
        if stage == "healthy" {
            journal.reason = None;
        }
        journal.stage = stage.to_owned();
        persist(journal)
    }

    fn rollback_stop_reason(error: &anyhow::Error) -> Option<&'static str> {
        let failure = error.to_string();
        [
            "stop_refused_answered_status",
            "identity_changed",
            "rollback_deadline_expired",
            "start_unacknowledged",
            "kernel_identity_unavailable",
            "stop_refused_unresolved_drain",
            "stop_refused_unresponsive",
        ]
        .into_iter()
        .find(|reason| failure.starts_with(reason))
    }

    fn now_unix_ms() -> Result<u128> {
        Ok(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis())
    }

    fn repair_outcome(
        journal: &Journal,
        doctor: &serde_json::Value,
        owner_alive: bool,
    ) -> Option<&'static str> {
        if owner_alive {
            return None;
        }
        let bound = journal.service.as_ref().is_none_or(|service| {
            doctor["service_id"] == service.service_id
                && doctor["port"] == service.port
                && doctor["executable"]
                    .as_str()
                    .is_some_and(|executable| Path::new(executable) == journal.executable)
        });
        if bound
            && !journal.to_version.is_empty()
            && doctor["healthy"] == true
            && doctor["live_version"] == journal.to_version.trim_start_matches('v')
        {
            Some("healthy")
        } else if journal.stage == "started" && journal.controller_identity.is_none() {
            None
        } else {
            Some("failed")
        }
    }

    fn repair_outcome_with_probe(
        journal: &Journal,
        doctor: &serde_json::Value,
        owner_alive: bool,
        absent: impl FnOnce() -> Result<bool>,
    ) -> Result<Option<&'static str>> {
        let outcome = repair_outcome(journal, doctor, owner_alive);
        if outcome.is_none()
            && !owner_alive
            && journal.stage == "started"
            && journal.owner_identity.is_some()
            && journal.controller_identity.is_none()
            && absent()?
        {
            return Ok(Some("failed"));
        }
        Ok(outcome)
    }

    fn cleanup_abandoned_stage(journal: &mut Journal) {
        if matches!(journal.stage.as_str(), "staging" | "started") && journal.stage_dir.exists() {
            let removed = match journal.prepared.as_ref() {
                Some(prepared) => crate::self_update::discard_bound_upgrade(
                    &journal.executable,
                    &journal.stage_dir,
                    prepared,
                ),
                None => {
                    crate::self_update::discard_upgrade(&journal.executable, &journal.stage_dir)
                }
            };
            match removed {
                Ok(()) => journal.reason = Some("controller_exited; staged files removed".into()),
                Err(error) => {
                    journal.reason = Some(format!(
                        "controller_exited; staged cleanup failed: {error:#}"
                    ))
                }
            }
        }
    }

    pub(crate) fn reconcile() -> Result<()> {
        let Some(mut journal) = read_journal()? else {
            return Ok(());
        };
        if matches!(journal.stage.as_str(), "healthy" | "failed" | "rolled_back")
            && !journal.recovery_required
        {
            return Ok(());
        }
        ensure!(
            !journal.upgrade_id.is_empty(),
            "Interrupted upgrade has no reservation identity; files were preserved"
        );
        let owner = journal.controller_identity.or(journal.owner_identity);
        let owner_alive = owner.is_some_and(crate::proxy_agent::process_may_be_alive);
        if owner_alive {
            bail!(PendingUpgrade);
        }
        let doctor = crate::proxy_agent::inspect()?;
        let outcome = repair_outcome_with_probe(&journal, &doctor, owner_alive, || {
            let job = crate::proxy_agent::upgrade_job(
                &journal.executable,
                &journal.stage_dir,
                &journal.upgrade_id,
            )?;
            crate::proxy_agent::upgrade_job_absent(&job)
        })?;
        match outcome {
            Some("healthy") => {
                let prepared = journal
                    .prepared
                    .as_ref()
                    .context("Interrupted upgrade has no verified release binding")?;
                let service = journal
                    .service
                    .as_ref()
                    .context("Interrupted upgrade has no service binding")?;
                ensure!(
                    &crate::proxy_agent::upgrade_service_binding(&journal.executable)? == service,
                    "Upgrade recovery service configuration changed"
                );
                let (executable, version) = crate::proxy_agent::upgrade_probe(true)?;
                ensure!(
                    executable == journal.executable
                        && version == journal.to_version.trim_start_matches('v'),
                    "Upgrade recovery process identity changed"
                );
                crate::self_update::verify_installed_upgrade(&journal.executable, prepared, true)?;
                advance(&mut journal, "healthy")?;
                if let Err(error) = crate::self_update::discard_bound_upgrade(
                    &journal.executable,
                    &journal.stage_dir,
                    journal.prepared.as_ref().unwrap(),
                ) {
                    eprintln!("Upgrade recovery completed; staged cleanup failed: {error:#}");
                }
                println!("Upgrade recovery found the target service healthy.");
            }
            Some("failed") if matches!(journal.stage.as_str(), "staging" | "started") => {
                journal.reason = Some("controller_exited".into());
                cleanup_abandoned_stage(&mut journal);
                advance(&mut journal, "failed")?;
                eprintln!(
                    "Upgrade controller exited: {}; stage path: {}",
                    journal.reason.as_deref().unwrap_or("unknown outcome"),
                    journal.stage_dir.display()
                );
            }
            Some("failed") => {
                if !journal.recovery_required {
                    journal.reason = Some("controller_exited_after_stop".into());
                    journal.recovery_required = true;
                    advance(&mut journal, "failed")?;
                }
                bail!(PendingUpgrade);
            }
            _ => bail!(PendingUpgrade),
        }
        Ok(())
    }

    fn reject_pending(prior: Option<&Journal>) -> Result<()> {
        if prior.is_some_and(|journal| {
            journal.recovery_required
                || !matches!(journal.stage.as_str(), "healthy" | "failed" | "rolled_back")
        }) {
            return Err(PendingUpgrade.into());
        }
        Ok(())
    }

    pub(crate) fn run(
        version: Option<&str>,
        wait: bool,
        print: bool,
        allow_dependent_caller: bool,
    ) -> Result<()> {
        if !print {
            crate::proxy_caller::refuse(
                crate::proxy_agent::status_port(None)?,
                allow_dependent_caller,
            )?;
        }
        crate::self_update::released_tag()?;
        if print {
            reject_pending(read_journal()?.as_ref())?;
            let (executable, live_version) = crate::proxy_agent::upgrade_probe(true)?;
            return run_selected(version, wait, true, executable, live_version, None);
        }
        let journal = crate::proxy_agent::upgrade_claim(|| {
            let prior = read_journal()?;
            reject_pending(prior.as_ref())?;
            if let Some(prior) = prior.as_ref() {
                if let Some(controller) = prior.controller_identity {
                    let job = crate::proxy_agent::upgrade_job(
                        &prior.executable,
                        &prior.stage_dir,
                        &prior.upgrade_id,
                    )?;
                    crate::proxy_agent::upgrade_cleanup_finished(&job, controller)?;
                }
            }
            let (executable, live_version) = crate::proxy_agent::upgrade_probe(true)?;
            let service = crate::proxy_agent::upgrade_service_binding(&executable)?;
            ensure!(
                std::env::current_exe()?.canonicalize()? == executable,
                "Run proxy upgrade from the managed service's installed release executable"
            );
            let journal = Journal {
                schema: 1,
                upgrade_id: crate::proxy_agent::unique_id(),
                revision: 0,
                service: Some(service),
                prepared: None,
                recovery_required: false,
                stage: "staging".into(),
                stage_dir: PathBuf::new(),
                executable,
                from_version: live_version,
                to_version: String::new(),
                reason: None,
                owner_identity: Some(
                    crate::proxy_agent::process_identity(std::process::id())
                        .context("Cannot identify upgrade owner")?,
                ),
                controller_identity: None,
                started_identity: None,
                starting_at_unix_ms: None,
                rollback_deadline_unix_ms: None,
            };
            crate::proxy_agent::upgrade_write_journal(&serde_json::to_vec_pretty(&journal)?)?;
            Ok(journal)
        })?;
        let result = run_selected(
            version,
            wait,
            false,
            journal.executable.clone(),
            journal.from_version.clone(),
            Some(&journal.upgrade_id),
        );
        if let Err(error) = &result {
            let recorded = crate::proxy_agent::upgrade_claim(|| {
                let mut current = owned_reservation(&journal.upgrade_id)?;
                if current.stage == "staging" && current.controller_identity.is_none() {
                    current.reason = Some(format!("{error:#}"));
                    cleanup_abandoned_stage(&mut current);
                    advance(&mut current, "failed")?;
                }
                Ok(())
            });
            if let Err(record_error) = recorded {
                eprintln!(
                    "Upgrade outcome could not be recorded: {record_error:#}; inspect proxy doctor"
                );
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
        upgrade_id: Option<&str>,
    ) -> Result<()> {
        let selected = crate::self_update::upgrade_target(&executable, version)?;
        ensure!(
            live_version == selected.current.trim_start_matches('v'),
            "running proxy version differs from the installed release receipt"
        );
        if selected.current == selected.target {
            if !print {
                crate::proxy_agent::upgrade_claim(|| {
                    let mut journal =
                        owned_reservation(upgrade_id.context("Missing upgrade reservation ID")?)?;
                    ensure!(
                        journal.stage == "staging",
                        "Upgrade reservation is no longer staging"
                    );
                    let (current_executable, version) = crate::proxy_agent::upgrade_probe(true)?;
                    ensure!(
                        current_executable == executable
                            && version == selected.target.trim_start_matches('v')
                            && journal.service.as_ref()
                                == Some(&crate::proxy_agent::upgrade_service_binding(&executable)?),
                        "Managed service changed while the installed version was checked"
                    );
                    journal.to_version = selected.target.clone();
                    advance(&mut journal, "healthy")
                })?;
            }
            println!("Managed proxy already runs {}.", selected.target);
            return Ok(());
        }
        let stage = crate::self_update::planned_upgrade_stage(&executable)?;
        let preview_id = crate::proxy_agent::unique_id();
        let job_id = upgrade_id.unwrap_or(&preview_id);
        let planned_job = crate::proxy_agent::upgrade_job(&executable, &stage, job_id)?;
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
        let upgrade_id = upgrade_id.context("Missing upgrade reservation ID")?;
        crate::proxy_agent::upgrade_claim(|| {
            let mut journal = owned_reservation(upgrade_id)?;
            ensure!(
                journal.stage == "staging" && journal.controller_identity.is_none(),
                "Upgrade reservation is no longer staging"
            );
            ensure!(
                journal.service.as_ref()
                    == Some(&crate::proxy_agent::upgrade_service_binding(&executable)?),
                "Managed service changed while upgrade was selected"
            );
            journal.stage_dir = stage.clone();
            journal.from_version = selected.current.clone();
            journal.to_version = selected.target.clone();
            save(&mut journal)
        })?;
        let prepared = crate::self_update::prepare_upgrade(&selected, version.is_some(), &stage)?;
        let job = crate::proxy_agent::upgrade_job(&executable, &stage, upgrade_id)?;
        crate::proxy_agent::upgrade_claim(|| {
            let mut journal = owned_reservation(upgrade_id)?;
            ensure!(
                journal.stage == "staging" && journal.stage_dir == stage,
                "Upgrade reservation changed during staging"
            );
            ensure!(
                journal.service.as_ref()
                    == Some(&crate::proxy_agent::upgrade_service_binding(&executable)?),
                "Managed service changed during staging"
            );
            crate::self_update::verify_prepared_upgrade(&executable, &stage, &prepared)?;
            journal.prepared = Some(prepared);
            advance(&mut journal, "started")
        })?;
        if let Err(error) = crate::proxy_agent::upgrade_launch(&job, &executable, &stage) {
            let _ = crate::proxy_agent::upgrade_claim(|| {
                let mut journal = owned_reservation(upgrade_id)?;
                if journal.stage == "started" && journal.controller_identity.is_none() {
                    journal.reason = Some(format!("launch_unacknowledged: {error:#}"));
                    save(&mut journal)?;
                }
                Ok(())
            });
            return Err(error).context("Upgrade launch could not be acknowledged; staged files were preserved. Inspect proxy doctor before retrying");
        }
        println!("Detached upgrade job: sh.gobstopper.upgrade (Linux: gobstopper-upgrade)\nLog: {}\nFollow with: gobstopper proxy doctor", job.log.display());
        if wait {
            let deadline = Instant::now() + Duration::from_secs(900);
            loop {
                let status = crate::proxy_agent::inspect()?;
                ensure!(
                    status["upgrade"]["upgrade_id"] == upgrade_id,
                    "Upgrade reservation changed while waiting; inspect proxy doctor"
                );
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

    fn validate_controller(
        args: &ControllerArgs,
        journal: &Journal,
        executable: &Path,
        release_tag: &str,
    ) -> Result<()> {
        let service = journal
            .service
            .as_ref()
            .context("Upgrade journal has no service binding")?;
        let prepared = journal
            .prepared
            .as_ref()
            .context("Upgrade journal has no verified stage binding")?;
        ensure!(
            journal.schema == 1
                && journal.stage == "started"
                && journal.controller_identity.is_none()
                && journal.stage_dir == args.stage
                && journal.upgrade_id == args.upgrade_id
                && args.upgrade_id.len() == 32
                && args.upgrade_id.bytes().all(|byte| byte.is_ascii_hexdigit())
                && service.state_dir == args.state_dir
                && service.executable == journal.executable
                && journal.executable == executable
                && journal.from_version == release_tag
                && prepared.from.executable == journal.executable
                && prepared.to.executable == journal.executable
                && prepared.from.release_tag == journal.from_version
                && prepared.to.release_tag == journal.to_version,
            "Upgrade journal does not match this installed release controller"
        );
        Ok(())
    }

    pub(crate) fn controller(args: &ControllerArgs) -> Result<()> {
        let release_tag = crate::self_update::released_tag()?;
        ensure!(
            args.state_dir.is_absolute() && args.stage.is_absolute(),
            "Controller paths must be absolute"
        );
        let config = args
            .state_dir
            .parent()
            .and_then(Path::parent)
            .context("Invalid service state directory")?;
        ensure!(
            config.join("gobstopper/service") == args.state_dir,
            "Invalid service state directory"
        );
        std::env::set_var("XDG_CONFIG_HOME", config);
        ensure!(
            crate::proxy_agent::upgrade_journal_path()?.parent() == Some(args.state_dir.as_path()),
            "Controller service state differs from its job definition"
        );
        let executable = std::env::current_exe()?.canonicalize()?;
        let initial = read_journal()?.context("No staged upgrade journal")?;
        validate_controller(args, &initial, &executable, release_tag)?;
        crate::self_update::verify_prepared_upgrade(
            &initial.executable,
            &initial.stage_dir,
            initial.prepared.as_ref().unwrap(),
        )?;
        let mut control = crate::proxy_agent::UpgradeControl::open(
            initial.service.as_ref().unwrap(),
            &initial.from_version,
        )?;
        let mut journal = owned_reservation(&args.upgrade_id)?;
        validate_controller(args, &journal, &executable, release_tag)?;
        crate::self_update::verify_prepared_upgrade(
            &journal.executable,
            &journal.stage_dir,
            journal.prepared.as_ref().unwrap(),
        )?;
        let controller_identity =
            crate::proxy_agent::upgrade_controller_identity(&journal.executable)?;
        journal.controller_identity = Some(controller_identity);
        save(&mut journal)?;
        let job =
            crate::proxy_agent::upgrade_job(&journal.executable, &args.stage, &args.upgrade_id)?;
        let result = controller_inner(&mut journal, &mut control, &job);
        let cleanup = crate::proxy_agent::upgrade_cleanup(&job, controller_identity);
        if let Err(error) = cleanup {
            eprintln!("Upgrade job cleanup failed: {error:#}");
        }
        result
    }

    fn controller_inner(
        journal: &mut Journal,
        control: &mut crate::proxy_agent::UpgradeControl,
        job: &crate::proxy_agent::UpgradeJob,
    ) -> Result<()> {
        let result = (|| {
            crate::proxy_agent::upgrade_unlink_on_entry(job)?;
            execute(journal, control, &mut save)
        })();
        if let Err(error) = &result {
            if !matches!(journal.stage.as_str(), "failed" | "rolled_back")
                && !matches!(
                    journal.reason.as_deref(),
                    Some("start_unacknowledged" | "kernel_identity_unavailable")
                )
            {
                journal.recovery_required =
                    !matches!(journal.stage.as_str(), "started" | "draining");
                journal.reason = Some(format!("{error:#}"));
                let _ = advance(journal, "failed");
            }
        }
        result
    }

    trait UpgradeService {
        fn stop(&mut self) -> Result<()>;
        fn start(&mut self, version: &str, deadline: Instant) -> Result<()>;
        fn started_identity(&self) -> Option<crate::proxy_agent::ProcessIdentity>;
        fn started_pid(&self) -> Option<u32>;
        fn start_submitted(&self) -> bool;
        fn stop_after_failed_start(
            &mut self,
            version: &str,
            readiness: Instant,
            rollback: Instant,
        ) -> Result<bool>;
        fn restart_previous(&mut self) -> Result<()>;
    }

    impl UpgradeService for crate::proxy_agent::UpgradeControl {
        fn stop(&mut self) -> Result<()> {
            crate::proxy_agent::UpgradeControl::stop(self)
        }
        fn start(&mut self, version: &str, deadline: Instant) -> Result<()> {
            crate::proxy_agent::UpgradeControl::start(self, version, deadline)
        }
        fn started_identity(&self) -> Option<crate::proxy_agent::ProcessIdentity> {
            crate::proxy_agent::UpgradeControl::started_identity(self)
        }
        fn started_pid(&self) -> Option<u32> {
            crate::proxy_agent::UpgradeControl::started_pid(self)
        }
        fn start_submitted(&self) -> bool {
            crate::proxy_agent::UpgradeControl::start_submitted(self)
        }
        fn stop_after_failed_start(
            &mut self,
            version: &str,
            readiness: Instant,
            rollback: Instant,
        ) -> Result<bool> {
            crate::proxy_agent::UpgradeControl::stop_after_failed_start(
                self, version, readiness, rollback,
            )
        }
        fn restart_previous(&mut self) -> Result<()> {
            crate::proxy_agent::UpgradeControl::restart_previous(self)
        }
    }

    fn startup_deadline(journal: &Journal, budget: u128) -> Result<Instant> {
        let starting = journal
            .starting_at_unix_ms
            .context("Missing startup time")?;
        let remaining = starting
            .checked_add(budget)
            .context("Startup deadline overflow")?
            .saturating_sub(now_unix_ms()?);
        Ok(Instant::now() + Duration::from_millis(u64::try_from(remaining)?))
    }

    fn execute(
        journal: &mut Journal,
        control: &mut impl UpgradeService,
        persist: &mut impl FnMut(&mut Journal) -> Result<()>,
    ) -> Result<()> {
        let prepared = journal
            .prepared
            .clone()
            .context("Missing verified upgrade binding")?;
        crate::self_update::verify_prepared_upgrade(
            &journal.executable,
            &journal.stage_dir,
            &prepared,
        )?;
        advance_recorded(journal, "draining", persist)?;
        if let Err(error) = control.stop() {
            journal.reason = Some(format!("{error:#}"));
            advance_recorded(journal, "failed", persist)?;
            return Err(error);
        }
        if let Err(error) = advance_recorded(journal, "stopped", persist)
            .and_then(|()| advance_recorded(journal, "replacing", persist))
        {
            return rollback(journal, control, error, false, false, persist);
        }
        let lease = match crate::self_update::replace_upgrade(
            &journal.executable,
            &journal.stage_dir,
            &prepared,
        ) {
            Ok(lease) => lease,
            Err(error) => return rollback(journal, control, error, false, false, persist),
        };
        if let Err(error) = advance_recorded(journal, "replaced", persist)
            .and_then(|()| advance_recorded(journal, "starting", persist))
        {
            drop(lease);
            return rollback(journal, control, error, true, false, persist);
        }
        let started = control.start(
            journal.to_version.trim_start_matches('v'),
            startup_deadline(journal, 5_000)?,
        );
        journal.started_identity = control.started_identity();
        if let Err(error) = persist(journal) {
            let attempted_start = control.start_submitted();
            drop(lease);
            return rollback(journal, control, error, true, attempted_start, persist);
        }
        if let Err(error) = started {
            if control.start_submitted() && control.started_identity().is_none() {
                journal.reason = Some(
                    if control.started_pid().is_none() {
                        "start_unacknowledged"
                    } else {
                        "kernel_identity_unavailable"
                    }
                    .into(),
                );
                journal.recovery_required = true;
                persist(journal)?;
                return Err(error);
            }
            let attempted_start = control.start_submitted();
            drop(lease);
            return rollback(journal, control, error, true, attempted_start, persist);
        }
        crate::self_update::verify_installed_upgrade(&journal.executable, &prepared, true)?;
        advance_recorded(journal, "healthy", persist)?;
        if let Err(error) = crate::self_update::discard_bound_upgrade(
            &journal.executable,
            &journal.stage_dir,
            &prepared,
        ) {
            eprintln!("Upgrade succeeded; staged backup cleanup failed: {error:#}");
        }
        Ok(())
    }

    fn recover_started_upgrade<Control, Lease>(
        journal: &mut Journal,
        control: &mut Control,
        stop: impl FnOnce(&mut Control) -> Result<bool>,
        restore: impl FnOnce() -> Result<Lease>,
        restart: impl FnOnce(&mut Control) -> Result<()>,
        record: impl FnOnce(&mut Journal, &str) -> Result<()>,
    ) -> Result<bool> {
        if stop(control)? {
            record(journal, "healthy")?;
            return Ok(true);
        }
        journal.reason = Some("new_version_unresponsive".into());
        let _lease = restore()?;
        restart(control)?;
        record(journal, "rolled_back")?;
        Ok(false)
    }

    fn rollback(
        journal: &mut Journal,
        control: &mut impl UpgradeService,
        error: anyhow::Error,
        replaced: bool,
        attempted_start: bool,
        persist: &mut impl FnMut(&mut Journal) -> Result<()>,
    ) -> Result<()> {
        let prepared = journal
            .prepared
            .clone()
            .context("Missing verified rollback binding")?;
        let restored: Result<()> = (|| {
            if replaced && attempted_start {
                let remaining = journal
                    .rollback_deadline_unix_ms
                    .context("Missing rollback deadline")?
                    .saturating_sub(now_unix_ms()?);
                ensure!(remaining > 0, "rollback_deadline_expired");
                let rollback_deadline =
                    Instant::now() + Duration::from_millis(u64::try_from(remaining)?);
                let readiness_remaining = journal
                    .starting_at_unix_ms
                    .context("Missing startup time")?
                    .saturating_add(5_000)
                    .saturating_sub(now_unix_ms()?);
                let readiness_deadline =
                    Instant::now() + Duration::from_millis(u64::try_from(readiness_remaining)?);
                let version = journal.to_version.trim_start_matches('v').to_owned();
                let executable = journal.executable.clone();
                let stage_dir = journal.stage_dir.clone();
                recover_started_upgrade(
                    journal,
                    control,
                    |control| {
                        control.stop_after_failed_start(
                            &version,
                            readiness_deadline,
                            rollback_deadline,
                        )
                    },
                    || crate::self_update::restore_upgrade(&executable, &stage_dir, &prepared),
                    |control| control.restart_previous(),
                    |journal, stage| {
                        crate::self_update::verify_installed_upgrade(
                            &journal.executable,
                            &prepared,
                            stage == "healthy",
                        )?;
                        advance_recorded(journal, stage, persist)
                    },
                )?;
                return Ok(());
            }
            let _lease = if replaced {
                crate::self_update::restore_upgrade(
                    &journal.executable,
                    &journal.stage_dir,
                    &prepared,
                )?
            } else {
                crate::self_update::protect_previous_upgrade(&journal.executable, &prepared)?
            };
            control.restart_previous()?;
            Ok(())
        })();
        if restored.is_ok() && journal.stage == "healthy" {
            if let Err(cleanup_error) = crate::self_update::discard_bound_upgrade(
                &journal.executable,
                &journal.stage_dir,
                &prepared,
            ) {
                eprintln!("Upgrade succeeded; staged backup cleanup failed: {cleanup_error:#}");
            }
            return Ok(());
        }
        if journal.reason.is_none() {
            journal.reason = Some(format!("{error:#}"));
        }
        match restored {
            Ok(()) => {
                crate::self_update::verify_installed_upgrade(
                    &journal.executable,
                    &prepared,
                    false,
                )?;
                if journal.stage != "rolled_back" {
                    advance_recorded(journal, "rolled_back", persist)?;
                }
                if let Err(cleanup_error) = crate::self_update::discard_bound_upgrade(
                    &journal.executable,
                    &journal.stage_dir,
                    &prepared,
                ) {
                    eprintln!(
                        "Previous proxy restored; staged backup cleanup failed: {cleanup_error:#}"
                    );
                }
                bail!("upgrade failed and previous proxy was restored: {error:#}")
            }
            Err(rollback_error) => {
                if let Some(reason) = rollback_stop_reason(&rollback_error) {
                    journal.reason = Some(reason.to_owned());
                    eprintln!("Upgrade rollback refused: {reason}: {rollback_error:#}");
                }
                journal.recovery_required = true;
                advance_recorded(journal, "failed", persist)?;
                bail!("upgrade failed: {error:#}; restoration cannot be proven: {rollback_error:#}; inspect proxy doctor and repair")
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn dead_reserved_owner_cleans_journaled_stage() {
            use std::os::unix::fs::PermissionsExt;
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "gobstopper-abandoned-stage-{}-{}",
                std::process::id(),
                now_unix_ms().unwrap()
            ));
            let bin = root.join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            let stage = bin.join(".gobstopper-update-test");
            std::fs::create_dir(&stage).unwrap();
            std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::write(stage.join("archive"), b"partial").unwrap();
            std::fs::set_permissions(
                stage.join("archive"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            let mut journal = Journal {
                schema: 1,
                upgrade_id: "test-upgrade".into(),
                revision: 0,
                service: None,
                prepared: None,
                recovery_required: false,
                stage: "staging".into(),
                stage_dir: stage.clone(),
                executable: bin.join("gobstopper"),
                from_version: "v1".into(),
                to_version: "v2".into(),
                reason: None,
                owner_identity: None,
                controller_identity: None,
                started_identity: None,
                starting_at_unix_ms: None,
                rollback_deadline_unix_ms: None,
            };
            assert_eq!(
                repair_outcome(&journal, &serde_json::json!({"healthy": false}), false),
                Some("failed")
            );
            cleanup_abandoned_stage(&mut journal);
            assert!(!stage.exists());
            assert!(journal
                .reason
                .as_deref()
                .unwrap()
                .contains("staged files removed"));
            std::fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn journal_stages_and_doctor_fields_serialize() {
            let mut journal = Journal {
                schema: 1,
                upgrade_id: "test-upgrade".into(),
                revision: 0,
                service: None,
                prepared: None,
                recovery_required: false,
                stage: "started".into(),
                stage_dir: "/tmp/stage".into(),
                executable: "/tmp/bin/gobstopper".into(),
                from_version: "v1.0.0".into(),
                to_version: "v1.0.1".into(),
                reason: None,
                owner_identity: None,
                controller_identity: None,
                started_identity: None,
                starting_at_unix_ms: None,
                rollback_deadline_unix_ms: None,
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
                upgrade_id: "test-upgrade".into(),
                revision: 0,
                service: None,
                prepared: None,
                recovery_required: false,
                stage: "staging".into(),
                stage_dir: PathBuf::new(),
                executable: PathBuf::new(),
                from_version: String::new(),
                to_version: String::new(),
                reason: None,
                owner_identity: None,
                controller_identity: None,
                started_identity: None,
                starting_at_unix_ms: None,
                rollback_deadline_unix_ms: None,
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
                upgrade_id: "test-upgrade".into(),
                revision: 0,
                service: None,
                prepared: None,
                recovery_required: false,
                stage: "starting".into(),
                stage_dir: "/tmp/stage".into(),
                executable: PathBuf::new(),
                from_version: "v1.0.0".into(),
                to_version: "v1.0.1".into(),
                reason: None,
                owner_identity: None,
                controller_identity: Some(crate::proxy_agent::ProcessIdentity {
                    pid: 42,
                    birth: 1,
                    birth_microseconds: 0,
                }),
                started_identity: None,
                starting_at_unix_ms: None,
                rollback_deadline_unix_ms: None,
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

        #[test]
        fn unacknowledged_launch_requires_a_dead_owner_and_verified_manager_absence() {
            let install = crate::self_update::staged_install();
            let journal = fixture_journal(
                &install.executable,
                &install.stage,
                install.prepared.clone(),
                8260,
            );
            let doctor = serde_json::json!({"healthy": false});
            assert_eq!(
                repair_outcome_with_probe(&journal, &doctor, false, || Ok(true)).unwrap(),
                Some("failed")
            );
            assert_eq!(
                repair_outcome_with_probe(&journal, &doctor, false, || Ok(false)).unwrap(),
                None
            );
            assert!(
                repair_outcome_with_probe(&journal, &doctor, false, || bail!(
                    "manager unavailable"
                ))
                .is_err()
            );
            assert_eq!(
                repair_outcome_with_probe(&journal, &doctor, true, || panic!(
                    "live owner must not be reconciled"
                ))
                .unwrap(),
                None
            );
            let mut missing_owner = journal.clone();
            missing_owner.owner_identity = None;
            assert_eq!(
                repair_outcome_with_probe(&missing_owner, &doctor, false, || panic!(
                    "missing owner identity cannot prove exit"
                ))
                .unwrap(),
                None
            );
            let mut acknowledged = journal;
            acknowledged.controller_identity = Some(crate::proxy_agent::ProcessIdentity {
                pid: 42,
                birth: 1,
                birth_microseconds: 0,
            });
            assert_eq!(
                repair_outcome_with_probe(&acknowledged, &doctor, false, || panic!(
                    "acknowledged controller needs no absence probe"
                ))
                .unwrap(),
                Some("failed")
            );
        }

        #[test]
        fn wrong_version_target_is_drained_restored_and_rolled_back() {
            struct FakeControl {
                service: crate::proxy_agent::tests::FakeService,
                drained: bool,
                restarted: bool,
            }
            let install = crate::self_update::replaced_install();
            let mut journal = Journal {
                schema: 1,
                upgrade_id: "test-upgrade".into(),
                revision: 0,
                service: None,
                prepared: None,
                recovery_required: false,
                stage: "starting".into(),
                stage_dir: install.stage.clone(),
                executable: install.executable.clone(),
                from_version: "v1.0.0".into(),
                to_version: "v1.0.1".into(),
                reason: None,
                owner_identity: None,
                controller_identity: None,
                started_identity: None,
                starting_at_unix_ms: None,
                rollback_deadline_unix_ms: None,
            };
            let mut control = FakeControl {
                service: crate::proxy_agent::tests::FakeService::start(|| "1.0.0"),
                drained: false,
                restarted: false,
            };
            let kept = recover_started_upgrade(
                &mut journal,
                &mut control,
                |control| {
                    crate::proxy_agent::stop_after_failed_start_on(
                        control.service.port,
                        "1.0.1",
                        |_| true,
                        Instant::now() + Duration::from_millis(300),
                        Instant::now() + Duration::from_secs(5),
                        || {
                            control.service.stop();
                            control.drained = true;
                            Ok(())
                        },
                        |_| panic!("an answering service must not be force-stopped"),
                    )
                },
                || {
                    crate::self_update::restore_upgrade(
                        &install.executable,
                        &install.stage,
                        &install.prepared,
                    )
                },
                |control| {
                    assert!(control.drained, "restart precedes the drained stop");
                    control.restarted = true;
                    Ok(())
                },
                |journal, stage| {
                    journal.stage = stage.into();
                    Ok(())
                },
            )
            .unwrap();
            assert!(!kept);
            assert!(control.drained && control.restarted);
            crate::self_update::verify_previous_upgrade(&install.executable, "v1.0.0").unwrap();
            assert_eq!(std::fs::read(&install.executable).unwrap(), b"old release");
            assert_eq!(journal.stage, "rolled_back");
            assert_eq!(journal.reason.as_deref(), Some("new_version_unresponsive"));

            journal.stage = "starting".into();
            journal.reason = None;
            let kept = recover_started_upgrade(
                &mut journal,
                &mut control,
                |_| Ok(true),
                || -> Result<()> { panic!("a healthy target must not be restored") },
                |_| panic!("a healthy target must not be restarted"),
                |journal, stage| {
                    journal.stage = stage.into();
                    Ok(())
                },
            )
            .unwrap();
            assert!(kept);
            assert_eq!(journal.stage, "healthy");
            assert_eq!(journal.reason, None);
        }

        struct FakeUpgradeService {
            service: Option<crate::proxy_agent::tests::FakeService>,
            executable: PathBuf,
            port: u16,
            mode: &'static str,
            actions: Vec<&'static str>,
            submitted: bool,
            identity: Option<crate::proxy_agent::ProcessIdentity>,
        }

        impl FakeUpgradeService {
            fn new(executable: &Path, mode: &'static str) -> Self {
                let service = crate::proxy_agent::tests::FakeService::start_at(
                    0,
                    || "1.0.0",
                    executable.into(),
                );
                Self {
                    port: service.port,
                    service: Some(service),
                    executable: executable.into(),
                    mode,
                    actions: Vec::new(),
                    submitted: false,
                    identity: None,
                }
            }
        }

        impl UpgradeService for FakeUpgradeService {
            fn stop(&mut self) -> Result<()> {
                self.actions.push("stop_old");
                self.service.as_mut().unwrap().stop();
                self.service = None;
                if self.mode == "external_change" {
                    std::fs::write(&self.executable, b"foreign edit")?;
                }
                Ok(())
            }
            fn start(&mut self, _version: &str, _deadline: Instant) -> Result<()> {
                crate::self_update::assert_installation_busy(&self.executable);
                assert_eq!(std::fs::read(&self.executable)?, b"new release");
                self.actions.push("start_new");
                if self.mode == "before_dispatch" {
                    bail!("injected pre-dispatch failure");
                }
                self.submitted = true;
                if self.mode == "unacknowledged" {
                    bail!("injected unknown start");
                }
                self.identity = crate::proxy_agent::process_identity(std::process::id());
                if self.mode == "silent" {
                    bail!("injected silent start");
                }
                let mode = self.mode;
                self.service = Some(crate::proxy_agent::tests::FakeService::start_at(
                    self.port,
                    move || {
                        if mode == "wrong_version" {
                            "1.0.0"
                        } else {
                            "1.0.1"
                        }
                    },
                    self.executable.clone(),
                ));
                if self.mode == "wrong_version" || self.mode == "healthy_after_error" {
                    bail!("injected manager error");
                }
                Ok(())
            }
            fn started_identity(&self) -> Option<crate::proxy_agent::ProcessIdentity> {
                self.identity
            }
            fn started_pid(&self) -> Option<u32> {
                self.identity.map(|identity| identity.pid)
            }
            fn start_submitted(&self) -> bool {
                self.submitted
            }
            fn stop_after_failed_start(
                &mut self,
                version: &str,
                readiness: Instant,
                rollback: Instant,
            ) -> Result<bool> {
                let executable = self.executable.clone();
                crate::proxy_agent::stop_after_failed_start_on(
                    self.port,
                    version,
                    |live| {
                        live["service_id"] == "aabbcc"
                            && live["executable"]
                                .as_str()
                                .is_some_and(|path| Path::new(path) == executable)
                    },
                    readiness,
                    rollback,
                    || {
                        self.actions.push("stop_new");
                        self.service.as_mut().unwrap().stop();
                        self.service = None;
                        Ok(())
                    },
                    |_| bail!("stop_refused_unresponsive: fixture cannot prove held admission"),
                )
            }
            fn restart_previous(&mut self) -> Result<()> {
                crate::self_update::assert_installation_busy(&self.executable);
                assert_eq!(std::fs::read(&self.executable)?, b"old release");
                self.actions.push("restart_old");
                self.service = Some(crate::proxy_agent::tests::FakeService::start_at(
                    self.port,
                    || "1.0.0",
                    self.executable.clone(),
                ));
                Ok(())
            }
        }

        fn fixture_journal(
            executable: &Path,
            stage: &Path,
            prepared: crate::self_update::PreparedUpgrade,
            port: u16,
        ) -> Journal {
            Journal {
                schema: 1,
                upgrade_id: crate::proxy_agent::unique_id(),
                revision: 0,
                service: Some(crate::proxy_agent::tests::fixture_upgrade_service(
                    executable, port,
                )),
                prepared: Some(prepared),
                recovery_required: false,
                stage: "started".into(),
                stage_dir: stage.into(),
                executable: executable.into(),
                from_version: "v1.0.0".into(),
                to_version: "v1.0.1".into(),
                reason: None,
                owner_identity: crate::proxy_agent::process_identity(std::process::id()),
                controller_identity: None,
                started_identity: None,
                starting_at_unix_ms: None,
                rollback_deadline_unix_ms: None,
            }
        }

        #[test]
        fn fake_release_and_service_complete_the_owned_upgrade_transaction() {
            for mode in [
                "healthy",
                "healthy_after_error",
                "wrong_version",
                "before_dispatch",
            ] {
                let install = crate::self_update::staged_install();
                let mut control = FakeUpgradeService::new(&install.executable, mode);
                let mut journal = fixture_journal(
                    &install.executable,
                    &install.stage,
                    install.prepared.clone(),
                    control.port,
                );
                let root = journal.service.as_ref().unwrap().state_dir.clone();
                let manifest = std::fs::read(root.join("manifest.json")).unwrap();
                let directory =
                    crate::self_update::ManagedDirectory::open_owned(&root, false, false).unwrap();
                let mut stages = Vec::new();
                let mut persist = |journal: &mut Journal| {
                    stages.push(journal.stage.clone());
                    directory.write_atomic("upgrade.json", &serde_json::to_vec_pretty(journal)?)
                };
                let result = execute(&mut journal, &mut control, &mut persist);
                let kept = matches!(mode, "healthy" | "healthy_after_error");
                assert_eq!(result.is_ok(), kept, "{mode}: {result:?}");
                assert_eq!(journal.stage, if kept { "healthy" } else { "rolled_back" });
                assert!(!journal.recovery_required);
                crate::self_update::verify_installed_upgrade(
                    &install.executable,
                    &install.prepared,
                    kept,
                )
                .unwrap();
                assert_eq!(std::fs::read(root.join("manifest.json")).unwrap(), manifest);
                assert!(!install.stage.exists());
                assert_eq!(stages.first().map(String::as_str), Some("draining"));
                assert_eq!(
                    stages.last().map(String::as_str),
                    Some(journal.stage.as_str())
                );
                let expected = match mode {
                    "wrong_version" => vec!["stop_old", "start_new", "stop_new", "restart_old"],
                    "before_dispatch" => vec!["stop_old", "start_new", "restart_old"],
                    _ => vec!["stop_old", "start_new"],
                };
                assert_eq!(control.actions, expected);
            }
        }

        #[test]
        fn uncertain_or_silent_start_preserves_backup_and_blocks_another_operation() {
            for mode in ["unacknowledged", "silent", "external_change"] {
                let install = crate::self_update::staged_install();
                let mut control = FakeUpgradeService::new(&install.executable, mode);
                let mut journal = fixture_journal(
                    &install.executable,
                    &install.stage,
                    install.prepared.clone(),
                    control.port,
                );
                let result = execute(&mut journal, &mut control, &mut |_| Ok(()));
                assert!(result.is_err(), "{mode}");
                assert!(journal.recovery_required);
                assert!(reject_pending(Some(&journal)).is_err());
                assert!(install.stage.exists());
                assert!(!control.actions.contains(&"restart_old"));
                if mode == "external_change" {
                    assert_eq!(std::fs::read(&install.executable).unwrap(), b"foreign edit");
                } else {
                    assert_eq!(
                        std::fs::read(install.stage.join("previous")).unwrap(),
                        b"old release"
                    );
                }
            }
        }

        #[test]
        fn failed_healthy_journal_write_never_discards_the_recovery_backup() {
            let install = crate::self_update::staged_install();
            let mut control = FakeUpgradeService::new(&install.executable, "healthy_after_error");
            let mut journal = fixture_journal(
                &install.executable,
                &install.stage,
                install.prepared.clone(),
                control.port,
            );
            let mut persist = |journal: &mut Journal| {
                if journal.stage == "healthy" {
                    bail!("injected healthy record failure");
                }
                Ok(())
            };
            assert!(execute(&mut journal, &mut control, &mut persist).is_err());
            assert_eq!(journal.stage, "failed");
            assert!(journal.recovery_required);
            assert_eq!(
                std::fs::read(install.stage.join("previous")).unwrap(),
                b"old release"
            );
            assert_eq!(std::fs::read(&install.executable).unwrap(), b"new release");
            assert_eq!(control.actions, ["stop_old", "start_new"]);
        }

        #[test]
        fn stale_reservation_and_foreign_controller_are_rejected() {
            let install = crate::self_update::staged_install();
            let mut journal = fixture_journal(
                &install.executable,
                &install.stage,
                install.prepared.clone(),
                8260,
            );
            let args = ControllerArgs {
                stage: install.stage.clone(),
                state_dir: journal.service.as_ref().unwrap().state_dir.clone(),
                upgrade_id: journal.upgrade_id.clone(),
            };
            validate_controller(&args, &journal, &install.executable, "v1.0.0").unwrap();
            assert!(validate_controller(
                &args,
                &journal,
                Path::new("/foreign/gobstopper"),
                "v1.0.0"
            )
            .is_err());
            assert!(validate_controller(&args, &journal, &install.executable, "v1.0.1").is_err());
            let original = journal.clone();
            journal.revision += 1;
            assert!(!same_reservation(&original, &journal));
            journal = original.clone();
            journal.upgrade_id = crate::proxy_agent::unique_id();
            assert!(!same_reservation(&original, &journal));
            journal = original.clone();
            journal.to_version = "v2.0.0".into();
            assert!(!same_reservation(&original, &journal));
            journal = original;
            journal.controller_identity = journal.owner_identity;
            assert!(validate_controller(&args, &journal, &install.executable, "v1.0.0").is_err());
        }

        #[test]
        fn unacknowledged_controller_cannot_be_reconciled_from_owner_death() {
            let install = crate::self_update::staged_install();
            let journal = fixture_journal(
                &install.executable,
                &install.stage,
                install.prepared.clone(),
                8260,
            );
            assert_eq!(
                repair_outcome(&journal, &serde_json::json!({"healthy": false}), false),
                None
            );
            assert!(install.stage.exists());
        }

        #[test]
        fn rollback_stop_reasons_are_stable() {
            for reason in [
                "stop_refused_answered_status",
                "identity_changed",
                "rollback_deadline_expired",
                "start_unacknowledged",
                "kernel_identity_unavailable",
                "stop_refused_unresolved_drain",
                "stop_refused_unresponsive",
            ] {
                let error = anyhow::anyhow!("{reason}: details");
                assert_eq!(rollback_stop_reason(&error), Some(reason));
            }
            assert_eq!(rollback_stop_reason(&anyhow::anyhow!("unrelated")), None);
        }
    }
}
