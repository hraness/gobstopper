//! Adaptive trigger/floor tuning from observed session telemetry.
//!
//! The tuner is pure and idempotent: every call recomputes from the
//! configured base policy, so repeated evaluations (watch pre-check then
//! stage, plan then apply) never compound. Reasons are closed-vocab
//! strings so the adjustment is auditable in plan output and telemetry.
//! Inputs are numeric only — no transcript content.

use crate::strategy::PolicyConfig;

/// Trigger is capped at this fraction of the provider-advertised window,
/// so gobstopper fires well before the provider's own ~90% compaction
/// threshold even when the configured trigger was sized for a larger
/// window.
const WINDOW_TRIGGER_FRACTION: f64 = 0.25;
/// The floor may not exceed this share of the trigger — the sawtooth
/// needs amplitude or a compaction cycle reclaims nothing.
const FLOOR_TRIGGER_FRACTION: f64 = 0.5;
/// An applied compaction reclaiming under this share of pre-context is
/// low-yield: the trigger fired before enough stale output accumulated.
const LOW_YIELD_RATIO: f64 = 0.10;
/// Backoff applied after repeated low-yield compactions.
const LOW_YIELD_BACKOFF: f64 = 1.5;
/// How far past-yield evidence may raise the trigger above configured.
const MAX_TRIGGER_BOOST: f64 = 2.0;
/// Sessions whose elidable share exceeds this accumulate tool output
/// quickly — compacting earlier pays off.
const HIGH_ELIDABLE_RATIO: f64 = 0.5;
/// Tightening applied to high-elidable sessions.
const HIGH_ELIDABLE_TIGHTEN: f64 = 0.8;
/// Absolute guard rails on the adjusted policy.
const MIN_TRIGGER: u64 = 10_000;
const MIN_FLOOR: u64 = 1_000;

/// Numeric observations the tuner reads. No transcript content.
#[derive(Debug, Clone, Default)]
pub struct AdaptiveSample {
    /// Provider-advertised context window for the session's model, when
    /// the transcript carries a usage record.
    pub model_context_window: Option<u64>,
    /// Current context occupancy estimate.
    pub context_tokens: u64,
    /// Estimated tokens recoverable by eliding every elidable item.
    /// `None` when the caller has not parsed the transcript (watch loop).
    pub elidable_tokens: Option<u64>,
    /// Most recent applied compactions for this session as
    /// `(context_tokens_before, est_reclaimed_tokens)`, newest first.
    pub recent: Vec<(u64, u64)>,
}

/// The adjusted policy plus the closed-vocab reasons that produced it.
#[derive(Debug, Clone)]
pub struct AdaptiveOutcome {
    pub policy: PolicyConfig,
    pub reasons: Vec<&'static str>,
}

/// Adjust `base` for the observed session. Every rule works against the
/// configured base or the running adjusted value inside one call only —
/// nothing is persisted, so callers may recompute freely.
pub fn adapt(base: &PolicyConfig, sample: &AdaptiveSample) -> AdaptiveOutcome {
    let mut trigger = base.trigger_tokens;
    let mut floor = base.floor_tokens;
    let mut reasons = Vec::new();

    // Window cap: on smaller windows the provider's own compaction fires
    // before an oversized configured trigger ever would.
    if let Some(window) = sample.model_context_window.filter(|w| *w > 0) {
        let cap = ((window as f64) * WINDOW_TRIGGER_FRACTION).round() as u64;
        if trigger > cap {
            trigger = cap.max(MIN_TRIGGER);
            reasons.push("trigger_capped_to_window_fraction");
        }
    }

    // Telemetry backoff: the last few applied compactions all reclaimed
    // under LOW_YIELD_RATIO of pre-context — the trigger is firing before
    // enough stale output exists to be worth a cycle. Bounded at
    // MAX_TRIGGER_BOOST times the configured trigger so it cannot run away.
    let recent: &[(u64, u64)] = &sample.recent[..sample.recent.len().min(3)];
    if recent.len() >= 2
        && recent.iter().all(|(before, reclaimed)| {
            *before > 0 && (*reclaimed as f64) < (*before as f64) * LOW_YIELD_RATIO
        })
    {
        let bound = ((base.trigger_tokens as f64) * MAX_TRIGGER_BOOST).round() as u64;
        let raised = ((trigger as f64) * LOW_YIELD_BACKOFF).round() as u64;
        if raised > trigger {
            trigger = raised.min(bound);
            reasons.push("trigger_raised_low_yield");
        }
    }

    // High-elidable tightening: most of the window is reclaimable tool
    // output, so a lower trigger keeps occupancy down at no recall cost.
    if let Some(elidable) = sample.elidable_tokens {
        if sample.context_tokens > 0
            && (elidable as f64) > (sample.context_tokens as f64) * HIGH_ELIDABLE_RATIO
        {
            let tightened = ((trigger as f64) * HIGH_ELIDABLE_TIGHTEN).round() as u64;
            if tightened < trigger {
                trigger = tightened;
                reasons.push("trigger_lowered_high_elidable");
            }
        }
    }

    // Floor consistency: keep at least 2x sawtooth amplitude, and keep
    // both values inside absolute guard rails.
    let floor_cap = ((trigger as f64) * FLOOR_TRIGGER_FRACTION) as u64;
    if floor > floor_cap {
        floor = floor_cap.max(MIN_FLOOR);
        reasons.push("floor_capped_to_trigger_half");
    }
    trigger = trigger.max(MIN_TRIGGER).max(floor.saturating_mul(2));
    floor = floor.clamp(MIN_FLOOR, trigger / 2);

    AdaptiveOutcome {
        policy: PolicyConfig {
            trigger_tokens: trigger,
            floor_tokens: floor,
            ..base.clone()
        },
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> PolicyConfig {
        PolicyConfig::default() // trigger 250_000, floor 40_000
    }

    fn sample(window: Option<u64>, context: u64, elidable: Option<u64>) -> AdaptiveSample {
        AdaptiveSample {
            model_context_window: window,
            context_tokens: context,
            elidable_tokens: elidable,
            recent: Vec::new(),
        }
    }

    #[test]
    fn no_signals_leaves_policy_unchanged() {
        let out = adapt(&base(), &sample(Some(1_000_000), 300_000, Some(50_000)));
        assert_eq!(out.policy.trigger_tokens, 250_000);
        assert_eq!(out.policy.floor_tokens, 40_000);
        assert!(out.reasons.is_empty());
    }

    #[test]
    fn small_window_caps_trigger_and_floor() {
        // 200k window: trigger caps at 50k, floor then caps at 25k.
        let out = adapt(&base(), &sample(Some(200_000), 150_000, None));
        assert_eq!(out.policy.trigger_tokens, 50_000);
        assert_eq!(out.policy.floor_tokens, 25_000);
        assert!(out.reasons.contains(&"trigger_capped_to_window_fraction"));
        assert!(out.reasons.contains(&"floor_capped_to_trigger_half"));
    }

    #[test]
    fn repeated_low_yield_backs_off_bounded() {
        let mut s = sample(None, 260_000, None);
        s.recent = vec![(260_000, 10_000), (255_000, 8_000)];
        let out = adapt(&base(), &s);
        assert_eq!(out.policy.trigger_tokens, 375_000); // 250k * 1.5
        assert!(out.reasons.contains(&"trigger_raised_low_yield"));

        // Backoff is bounded at 2x the configured trigger.
        s.recent = vec![(500_000, 20_000), (490_000, 15_000), (480_000, 12_000)];
        let big = PolicyConfig {
            trigger_tokens: 400_000,
            ..base()
        };
        let out = adapt(&big, &s);
        assert_eq!(out.policy.trigger_tokens, 600_000); // 400k*1.5 under 800k bound
    }

    #[test]
    fn single_low_yield_event_does_not_back_off() {
        let mut s = sample(None, 260_000, None);
        s.recent = vec![(260_000, 10_000)];
        let out = adapt(&base(), &s);
        assert_eq!(out.policy.trigger_tokens, 250_000);
        assert!(out.reasons.is_empty());
    }

    #[test]
    fn high_elidable_share_tightens_trigger() {
        let out = adapt(&base(), &sample(None, 300_000, Some(200_000)));
        assert_eq!(out.policy.trigger_tokens, 200_000); // 250k * 0.8
        assert!(out.reasons.contains(&"trigger_lowered_high_elidable"));
    }

    #[test]
    fn missing_elidable_skips_tightening() {
        let out = adapt(&base(), &sample(None, 300_000, None));
        assert_eq!(out.policy.trigger_tokens, 250_000);
        assert!(out.reasons.is_empty());
    }

    #[test]
    fn adjustment_is_idempotent_from_configured_base() {
        let s = sample(Some(200_000), 150_000, Some(120_000));
        let first = adapt(&base(), &s);
        let second = adapt(&base(), &s);
        assert_eq!(first.policy.trigger_tokens, second.policy.trigger_tokens);
        assert_eq!(first.policy.floor_tokens, second.policy.floor_tokens);
        assert_eq!(first.reasons, second.reasons);
    }

    #[test]
    fn floor_never_reaches_trigger() {
        let out = adapt(&base(), &sample(Some(40_000), 30_000, None));
        assert!(out.policy.floor_tokens < out.policy.trigger_tokens);
        assert!(out.policy.floor_tokens >= MIN_FLOOR);
    }
}
