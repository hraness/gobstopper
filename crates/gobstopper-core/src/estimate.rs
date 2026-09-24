/// Rough chars-per-token divisor used when a transcript carries no
/// provider usage accounting. Deliberately conservative (over-estimates).
pub const CHARS_PER_TOKEN: u64 = 4;

pub fn estimate_tokens(text_len: usize) -> u64 {
    estimate_token_bytes(text_len as u64)
}

/// Preserve the provider's full byte-count domain on 32-bit and 64-bit hosts.
pub fn estimate_token_bytes(bytes: u64) -> u64 {
    bytes.div_ceil(CHARS_PER_TOKEN).max(1)
}

/// Shared saturating aggregate for untrusted provider estimates. A total that
/// exceeds u64 stays conservative instead of wrapping or panicking.
pub fn add_tokens(total: u64, additional: u64) -> u64 {
    total.saturating_add(additional)
}

pub fn elision_savings(est_tokens: u64, bytes: Option<u64>, parts: u32) -> u64 {
    if !crate::admission::is_elidable(est_tokens, bytes) {
        return 0;
    }
    let bytes = bytes.unwrap_or(0);
    let stub_budget = u64::from(parts.max(1)) * 96;
    estimate_token_bytes(bytes)
        .saturating_sub(estimate_token_bytes(stub_budget))
        .min(est_tokens)
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn token_rounding_full_u64() {
        let bytes: u64 = kani::any();
        let expected = ((bytes as u128 + 3) / 4).max(1) as u64;
        assert_eq!(
            estimate_token_bytes(bytes),
            expected,
            "token estimate agrees with wide oracle"
        );
        let text_len: usize = kani::any();
        assert_eq!(
            estimate_tokens(text_len),
            ((text_len as u128 + 3) / 4).max(1) as u64
        );
        kani::cover!(bytes == 0 && expected == 1, "empty estimate convention");
        kani::cover!(
            bytes == u64::MAX && expected > 0,
            "maximum bytes do not overflow"
        );
    }

    #[kani::proof]
    fn aggregate_saturates_full_u64() {
        let before: u64 = kani::any();
        let added: u64 = kani::any();
        let expected = (before as u128 + added as u128).min(u64::MAX as u128) as u64;
        let actual = add_tokens(before, added);
        assert_eq!(
            actual, expected,
            "aggregate agrees with wide saturation oracle"
        );
        assert!(actual >= before && actual >= added);
        kani::cover!(
            before as u128 + added as u128 > u64::MAX as u128,
            "overflow saturation reachable"
        );
        kani::cover!(actual == 0, "zero aggregate reachable");
    }

    #[kani::proof]
    fn savings_never_exceed_live_payload_or_context() {
        let tokens: u64 = kani::any();
        let bytes: Option<u64> = kani::any();
        let parts: u32 = kani::any();
        let actual = elision_savings(tokens, bytes, parts);
        let expected = match bytes {
            Some(bytes) if tokens > 0 && bytes > 0 => {
                let payload = ((bytes as u128 + 3) / 4).max(1);
                let stub = (u128::from(parts.max(1)) * 96 + 3) / 4;
                payload.saturating_sub(stub).min(tokens as u128) as u64
            }
            _ => 0,
        };
        assert_eq!(
            actual, expected,
            "savings agree with independent wide oracle"
        );
        assert!(actual <= tokens, "savings cannot exceed live item estimate");
        kani::cover!(
            actual > 0 && bytes == Some(u64::MAX),
            "maximum payload has savings"
        );
        kani::cover!(
            actual == 0 && parts == u32::MAX,
            "large stub budget is safe"
        );
        kani::cover!(actual == tokens && tokens > 0, "estimate cap reachable");
    }
}
