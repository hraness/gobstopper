//! Estimate calibration: learn how far the four-characters-per-token
//! estimate runs below the provider's own count.
//!
//! The proxy sizes a request at four characters per token. On Claude Code
//! traffic the provider reported 13% to 38% more input than that, so a
//! request estimated at the threshold was already well past it. After each
//! response the proxy compares the provider-reported input with its own
//! estimate of the request it forwarded, and keeps a running ratio. The
//! engine divides the threshold by that ratio (see
//! [`super::Engine::prepare_calibrated`]).
//!
//! The ratio is in thousandths (`1000` is 1.0) so the arithmetic is exact
//! and replays are deterministic. The applied ratio is clamped to
//! [`MIN_RATIO_PERMILLE`]..=[`MAX_RATIO_PERMILLE`]: it can only lower the
//! threshold, never raise it, and never by more than half. Until
//! [`MIN_SAMPLES`] responses have reported usage it is exactly 1.0.

use serde_json::Value;

/// The applied ratio's lower bound: 1.0, so calibration never compacts later
/// than the uncalibrated estimate would.
pub const MIN_RATIO_PERMILLE: u32 = 1000;
/// The applied ratio's upper bound: 2.0, so calibration never more than
/// halves the threshold.
pub const MAX_RATIO_PERMILLE: u32 = 2000;
/// Samples needed before the learned ratio is applied.
pub const MIN_SAMPLES: u64 = 5;
/// Requests estimated below this many tokens are not sampled: fixed framing
/// the estimate does not see dominates their ratio.
pub const MIN_SAMPLE_EST_TOKENS: u64 = 1_000;
/// A sample outside this range (in thousandths) is ignored as unrelated to
/// the request, such as usage from a different request or a count that
/// leaves most of the request out.
const SAMPLE_RANGE_PERMILLE: std::ops::RangeInclusive<u64> = 250..=4000;
/// The running estimate moves one eighth of the way to each new sample.
const WEIGHT_SHIFT: u32 = 3;

/// A running ratio of provider-reported to estimated input tokens.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Calibration {
    samples: u64,
    /// Exponentially weighted mean of the accepted samples, in thousandths.
    mean_permille: u64,
}

impl Calibration {
    /// Record one response: `est_tokens` is the proxy's estimate of the
    /// request it forwarded and `reported_tokens` the provider's input count
    /// for it. Returns whether the sample was accepted.
    pub fn observe(&mut self, est_tokens: u64, reported_tokens: u64) -> bool {
        if est_tokens < MIN_SAMPLE_EST_TOKENS {
            return false;
        }
        let sample = u128::from(reported_tokens) * 1000 / u128::from(est_tokens);
        let Ok(sample) = u64::try_from(sample) else {
            return false;
        };
        if !SAMPLE_RANGE_PERMILLE.contains(&sample) {
            return false;
        }
        self.mean_permille = if self.samples == 0 {
            sample
        } else if sample >= self.mean_permille {
            self.mean_permille + ((sample - self.mean_permille) >> WEIGHT_SHIFT)
        } else {
            self.mean_permille - ((self.mean_permille - sample) >> WEIGHT_SHIFT)
        };
        self.samples += 1;
        true
    }

    /// Accepted samples so far.
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// The measured ratio in thousandths, unclamped; `None` before any sample.
    pub fn measured_permille(&self) -> Option<u64> {
        (self.samples > 0).then_some(self.mean_permille)
    }

    /// The ratio to apply, in thousandths: 1000 until [`MIN_SAMPLES`]
    /// samples, then the measured ratio clamped to 1000..=2000.
    pub fn ratio_permille(&self) -> u32 {
        if self.samples < MIN_SAMPLES {
            return MIN_RATIO_PERMILLE;
        }
        self.mean_permille
            .clamp(u64::from(MIN_RATIO_PERMILLE), u64::from(MAX_RATIO_PERMILLE)) as u32
    }
}

/// The threshold compared against the uncalibrated estimate: `threshold`
/// divided by the ratio, with the ratio clamped to 1000..=2000 thousandths.
/// At 1000 it is `threshold` exactly; it is never above it.
pub fn calibrated_threshold(threshold_tokens: u64, ratio_permille: u32) -> u64 {
    let ratio = ratio_permille.clamp(MIN_RATIO_PERMILLE, MAX_RATIO_PERMILLE);
    (u128::from(threshold_tokens) * 1000 / u128::from(ratio)) as u64
}

/// The input tokens an Anthropic Messages `usage` object reports:
/// `input_tokens` plus `cache_creation_input_tokens` and
/// `cache_read_input_tokens` when present. `None` without a numeric
/// `input_tokens`, or when a cache field is present but not a number.
pub fn reported_input_tokens(usage: &Value) -> Option<u64> {
    let input = usage.get("input_tokens")?.as_u64()?;
    let mut total = input;
    for key in ["cache_creation_input_tokens", "cache_read_input_tokens"] {
        match usage.get(key) {
            None | Some(Value::Null) => {}
            Some(value) => total = total.checked_add(value.as_u64()?)?,
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_ratio_is_one_until_enough_samples() {
        let mut calibration = Calibration::default();
        assert_eq!(calibration.ratio_permille(), 1000);
        assert_eq!(calibration.measured_permille(), None);
        for _ in 0..MIN_SAMPLES - 1 {
            assert!(calibration.observe(100_000, 125_000));
            assert_eq!(calibration.ratio_permille(), 1000);
        }
        assert_eq!(calibration.measured_permille(), Some(1250));
        assert!(calibration.observe(100_000, 125_000));
        assert_eq!(calibration.samples(), MIN_SAMPLES);
        assert_eq!(calibration.ratio_permille(), 1250);
    }

    #[test]
    fn the_applied_ratio_is_clamped_to_one_and_two() {
        let mut low = Calibration::default();
        let mut high = Calibration::default();
        for _ in 0..MIN_SAMPLES {
            assert!(low.observe(100_000, 80_000));
            assert!(high.observe(100_000, 300_000));
        }
        assert_eq!(low.measured_permille(), Some(800));
        assert_eq!(low.ratio_permille(), 1000);
        assert_eq!(high.measured_permille(), Some(3000));
        assert_eq!(high.ratio_permille(), 2000);
    }

    #[test]
    fn small_requests_and_outliers_are_not_sampled() {
        let mut calibration = Calibration::default();
        assert!(!calibration.observe(999, 2_000));
        assert!(!calibration.observe(0, 0));
        assert!(!calibration.observe(100_000, 10_000));
        assert!(!calibration.observe(100_000, 500_000));
        assert!(!calibration.observe(u64::MAX / 8, u64::MAX));
        assert!(calibration.observe(100_000, 400_000));
        assert_eq!(calibration.samples(), 1);
    }

    #[test]
    fn the_mean_moves_an_eighth_of_the_way_to_each_sample() {
        let mut calibration = Calibration::default();
        calibration.observe(100_000, 120_000);
        calibration.observe(100_000, 200_000);
        assert_eq!(calibration.measured_permille(), Some(1200 + 800 / 8));
        calibration.observe(100_000, 100_000);
        assert_eq!(calibration.measured_permille(), Some(1300 - 300 / 8));
    }

    #[test]
    fn the_calibrated_threshold_is_never_above_the_threshold() {
        assert_eq!(calibrated_threshold(128_000, 1000), 128_000);
        assert_eq!(calibrated_threshold(128_000, 0), 128_000);
        assert_eq!(calibrated_threshold(128_000, 999), 128_000);
        assert_eq!(calibrated_threshold(128_000, 1250), 102_400);
        assert_eq!(calibrated_threshold(128_000, 2000), 64_000);
        assert_eq!(calibrated_threshold(128_000, u32::MAX), 64_000);
        assert_eq!(calibrated_threshold(u64::MAX, 1000), u64::MAX);
        for ratio in (0..3000).step_by(7) {
            assert!(calibrated_threshold(256_000, ratio) <= 256_000);
        }
    }

    #[test]
    fn reported_input_sums_uncached_and_cache_fields() {
        let usage = json!({
            "input_tokens": 12,
            "cache_creation_input_tokens": 3_000,
            "cache_read_input_tokens": 140_000,
            "output_tokens": 50,
        });
        assert_eq!(reported_input_tokens(&usage), Some(143_012));
        assert_eq!(reported_input_tokens(&json!({"input_tokens": 7})), Some(7));
        assert_eq!(
            reported_input_tokens(&json!({"input_tokens": 7, "cache_read_input_tokens": null})),
            Some(7)
        );
        assert_eq!(reported_input_tokens(&json!({"output_tokens": 7})), None);
        assert_eq!(reported_input_tokens(&json!({"input_tokens": "7"})), None);
        assert_eq!(reported_input_tokens(&json!({"input_tokens": -1})), None);
        assert_eq!(
            reported_input_tokens(&json!({"input_tokens": 7, "cache_read_input_tokens": "x"})),
            None
        );
        assert_eq!(
            reported_input_tokens(&json!({"input_tokens": u64::MAX, "cache_read_input_tokens": 1})),
            None
        );
        assert_eq!(reported_input_tokens(&json!(null)), None);
    }
}
