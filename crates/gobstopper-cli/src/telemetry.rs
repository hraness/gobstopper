//! Decision-time provenance. Historical events are never relabeled by current config.

use crate::{config::Config, hooks, native_operations};
use gobstopper_adapters::{copy, detect};
use gobstopper_core::events::{Cohort, CompactionEvent};
use gobstopper_core::SessionHandle;

pub(crate) struct EventContext<'a> {
    config: &'a Config,
    config_sha256: String,
    rollout_enabled: bool,
}

impl<'a> EventContext<'a> {
    /// The config digest covers the parsed configuration, including ordered maps.
    /// Its versioned encoding is bound to the separately recorded binary digest.
    /// CLI overrides remain represented by each event's strategy and threshold.
    pub(crate) fn new(config: &'a Config, rollout_enabled: bool) -> Self {
        Self {
            config,
            config_sha256: copy::sha256(
                format!("gobstopper-config-debug-v1:{config:?}").as_bytes(),
            ),
            rollout_enabled,
        }
    }

    pub(crate) fn annotate(&self, event: &mut CompactionEvent, handle: &SessionHandle) {
        event.binary_sha256 = native_operations::artifact_sha256().ok().map(str::to_owned);
        event.config_sha256 = Some(self.config_sha256.clone());
        event.source_identity_sha256 = detect::source_identity(handle).ok();
        let cohort = self
            .rollout_enabled
            .then(|| {
                hooks::rollout_cohort(self.config, handle.provider.as_str(), &handle.session_id)
            })
            .flatten();
        event.decision_cohort = Some(match cohort {
            Some(true) => Cohort::Treatment,
            Some(false) => Cohort::Control,
            None => Cohort::Ungated,
        });
        event.rollout_percent =
            cohort.and_then(|_| self.config.rollout.get(handle.provider.as_str()).copied());
        // A rollout is not itself a registered experiment. Do not invent a study ID.
        event.experiment_sha256 = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gobstopper_core::Provider;

    fn event() -> CompactionEvent {
        CompactionEvent::new(
            Provider::Devin,
            "session",
            "watch-apply",
            "provider_compact",
            "blocked",
            100,
            0,
            0,
            0,
            0,
            Some("native_unqualified".into()),
        )
    }

    #[test]
    fn provenance_records_actual_gate_and_frozen_configuration() {
        let handle = SessionHandle {
            provider: Provider::Devin,
            session_id: "session".into(),
            path: std::env::current_exe().unwrap(),
            cwd: None,
            age_secs: 0,
        };
        let mut config = Config::default();
        config.rollout.insert("devin".into(), 0);
        let mut control = event();
        EventContext::new(&config, true).annotate(&mut control, &handle);
        assert_eq!(control.decision_cohort, Some(Cohort::Control));
        assert_eq!(control.rollout_percent, Some(0));
        assert!(control.binary_sha256.is_some());
        assert_eq!(
            control.source_identity_sha256,
            detect::source_identity(&handle).ok()
        );
        let mut manual = event();
        EventContext::new(&config, false).annotate(&mut manual, &handle);
        assert_eq!(manual.decision_cohort, Some(Cohort::Ungated));
        assert_eq!(manual.rollout_percent, None);
        assert_eq!(manual.config_sha256, control.config_sha256);
        config.rollout.insert("devin".into(), 100);
        let mut treatment = event();
        EventContext::new(&config, true).annotate(&mut treatment, &handle);
        assert_eq!(treatment.decision_cohort, Some(Cohort::Treatment));
        assert_ne!(treatment.config_sha256, control.config_sha256);
        assert_eq!(control.decision_cohort, Some(Cohort::Control));
        assert!(treatment.experiment_sha256.is_none());
    }
}
