//! Small total admission kernels shared by production validation and proofs.
//!
//! Collection construction, provider parsing and payload authority are separate
//! boundaries. No proof-only capacity replaces these production limits.

pub const MAX_EDITS: usize = 64;
pub const MAX_ITEMS: usize = 100_000;
pub const MAX_DIGEST_BYTES: usize = 32 * 1024;
pub const MAX_POLICY_TOKENS: u64 = 10_000_000;
pub const MAX_INTERVAL_SECS: u64 = 86_400;

pub fn plan_bounds(edits: usize, items: usize) -> bool {
    edits <= MAX_EDITS && items <= MAX_ITEMS
}

pub fn add_digest_bytes(total: usize, additional: usize) -> Option<usize> {
    let next = total.checked_add(additional)?;
    (next <= MAX_DIGEST_BYTES).then_some(next)
}

pub fn is_elidable(est_tokens: u64, bytes: Option<u64>) -> bool {
    est_tokens > 0 && bytes.is_some_and(|bytes| bytes > 0)
}

/// Exclude the protected suffix without subtraction underflow, for any usize.
pub fn unprotected_len(eligible: usize, keep_recent: usize) -> usize {
    eligible.saturating_sub(keep_recent)
}

pub fn target_allowed(est_tokens: u64, bytes: Option<u64>, protected: bool) -> bool {
    is_elidable(est_tokens, bytes) && !protected
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditClass {
    Elide,
    Digest,
    ProviderControl,
    CacheControl,
}

/// The state is updated only after every condition succeeds. This is admission
/// metadata, not a transcript mutation: rejected edits leave it byte-for-byte
/// unchanged. Its private fields cannot be fabricated by ordinary callers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EditAdmission {
    edits: usize,
    digests: usize,
    control: bool,
}

impl EditAdmission {
    fn valid(&self) -> bool {
        self.edits <= MAX_EDITS
            && self.digests <= 1
            && self.digests <= self.edits
            && (!self.control || (self.edits == 1 && self.digests == 0))
    }

    pub fn admit(&mut self, class: EditClass) -> Result<(), &'static str> {
        if !self.valid() || self.edits >= MAX_EDITS || self.control {
            return Err("invalid edit composition");
        }
        let control = matches!(class, EditClass::ProviderControl | EditClass::CacheControl);
        if (control && self.edits != 0) || (class == EditClass::Digest && self.digests != 0) {
            return Err("invalid edit composition");
        }
        let next = Self {
            edits: self.edits + 1,
            digests: self.digests + usize::from(class == EditClass::Digest),
            control,
        };
        *self = next;
        Ok(())
    }
}

/// Check the index ordering that production establishes by sorting its copied
/// identity list. Strict order simultaneously rejects duplicate physical IDs.
pub fn strictly_increasing(indexes: &[usize]) -> bool {
    let mut position = 1;
    while position < indexes.len() {
        if indexes[position - 1] >= indexes[position] {
            return false;
        }
        position += 1;
    }
    true
}

/// Exact lookup in a sorted identity list. The midpoint uses a difference so
/// it cannot overflow, even at the platform's complete usize range.
pub fn find_index(indexes: &[usize], target: usize) -> Option<usize> {
    let mut low = 0;
    let mut high = indexes.len();
    while low < high {
        let middle = low + (high - low) / 2;
        match indexes[middle].cmp(&target) {
            std::cmp::Ordering::Less => low = middle + 1,
            std::cmp::Ordering::Greater => high = middle,
            std::cmp::Ordering::Equal => return Some(middle),
        }
    }
    None
}

#[cfg(kani)]
mod proofs {
    use super::*;

    fn class(raw: u8) -> EditClass {
        match raw % 4 {
            0 => EditClass::Elide,
            1 => EditClass::Digest,
            2 => EditClass::ProviderControl,
            _ => EditClass::CacheControl,
        }
    }

    #[kani::proof]
    fn production_plan_bounds() {
        let edits: usize = kani::any();
        let items: usize = kani::any();
        let valid = plan_bounds(edits, items);
        assert_eq!(
            valid,
            edits < 65 && items < 100_001,
            "exact production plan bounds"
        );
        kani::cover!(
            valid && edits == MAX_EDITS && items == MAX_ITEMS,
            "real capacity admitted"
        );
        kani::cover!(!valid && edits == MAX_EDITS + 1, "edit capacity rejected");
        kani::cover!(!valid && items == usize::MAX, "full width rejected");
    }

    #[kani::proof]
    fn digest_byte_admission() {
        let total: usize = kani::any();
        let part: usize = kani::any();
        let actual = add_digest_bytes(total, part);
        let wide = total as u128 + part as u128;
        let expected = if wide <= 32_768 {
            Some(wide as usize)
        } else {
            None
        };
        assert_eq!(
            actual, expected,
            "digest byte bound agrees with wide oracle"
        );
        kani::cover!(actual == Some(MAX_DIGEST_BYTES), "digest maximum admitted");
        kani::cover!(actual.is_none() && total == usize::MAX, "overflow refused");
    }

    #[kani::proof]
    fn eligibility_and_protection() {
        let tokens: u64 = kani::any();
        let bytes: Option<u64> = kani::any();
        let protected: bool = kani::any();
        let expected = matches!(bytes, Some(1..=u64::MAX)) && tokens != 0 && !protected;
        let actual = target_allowed(tokens, bytes, protected);
        assert_eq!(
            actual, expected,
            "only live positive unprotected payload is eligible"
        );
        kani::cover!(actual && tokens == u64::MAX, "full token domain admitted");
        kani::cover!(!actual && bytes == Some(0), "zero payload refused");
        kani::cover!(
            !actual && protected && tokens > 0 && bytes == Some(u64::MAX),
            "protected maximum refused"
        );
    }

    #[kani::proof]
    fn protected_suffix_boundary() {
        let eligible: usize = kani::any();
        let keep: usize = kani::any();
        let count = unprotected_len(eligible, keep);
        let expected = if keep >= eligible { 0 } else { eligible - keep };
        assert_eq!(
            count, expected,
            "recent suffix never enters candidate prefix"
        );
        assert!(count <= eligible);
        kani::cover!(count == 0 && keep == usize::MAX, "all protected");
        kani::cover!(count == usize::MAX && keep == 0, "full width prefix");
    }

    /// Arbitrary private state as well as arbitrary input. Invalid states are
    /// rejected without mutation; no assume excludes them from the proof.
    #[kani::proof]
    fn edit_step_is_atomic_and_exclusive() {
        let mut state = EditAdmission {
            edits: kani::any(),
            digests: kani::any(),
            control: kani::any(),
        };
        let before = state;
        let class = class(kani::any());
        let external_control =
            matches!(class, EditClass::ProviderControl | EditClass::CacheControl);
        let oracle_valid = before.edits < 65
            && before.digests < 2
            && before.digests <= before.edits
            && (!before.control || (before.edits == 1 && before.digests == 0));
        let expected = oracle_valid
            && before.edits < 64
            && !before.control
            && (!external_control || before.edits == 0)
            && (class != EditClass::Digest || before.digests == 0);
        let result = state.admit(class);
        assert_eq!(
            result.is_ok(),
            expected,
            "edit admission matches full domain oracle"
        );
        if result.is_ok() {
            assert!(
                state.valid(),
                "successful step preserves admission invariant"
            );
            assert_eq!(state.edits, before.edits + 1);
            assert!(
                !state.control || state.edits == 1,
                "provider controls are unmixed"
            );
        } else {
            assert_eq!(state, before, "rejection preserves admission state");
        }
        kani::cover!(
            result.is_ok() && state.edits == MAX_EDITS,
            "production capacity reachable"
        );
        kani::cover!(
            result.is_err() && before.edits == usize::MAX,
            "invalid state rejected"
        );
        kani::cover!(
            result.is_ok() && state.control,
            "exclusive control admitted"
        );
        kani::cover!(
            result.is_err() && before.control,
            "control cannot be followed"
        );
    }

    #[kani::proof]
    #[kani::unwind(5)]
    fn edit_sequences_four() {
        let raw: [u8; 4] = kani::any();
        let mut state = EditAdmission::default();
        let mut accepted = 0;
        let mut controls = 0;
        let mut digests = 0;
        for raw in raw {
            let class = class(raw);
            let before = state;
            if state.admit(class).is_ok() {
                accepted += 1;
                controls += usize::from(matches!(
                    class,
                    EditClass::ProviderControl | EditClass::CacheControl
                ));
                digests += usize::from(class == EditClass::Digest);
            } else {
                assert_eq!(state, before);
            }
            assert!(
                controls == 0 || accepted == 1,
                "accepted sequence has no mixed control"
            );
            assert!(digests <= 1, "accepted sequence has at most one digest");
            assert_eq!(state.edits, accepted);
        }
        kani::cover!(accepted == 4, "four ordinary edits reachable");
        kani::cover!(
            controls == 1 && accepted == 1,
            "control exclusions reachable"
        );
    }

    fn indexes<const N: usize>() -> (bool, Option<usize>, usize, bool) {
        let indexes: [usize; N] = kani::any();
        let target: usize = kani::any();
        let ordered = strictly_increasing(&indexes);
        let mut oracle_ordered = true;
        let mut expected = None;
        for i in 0..N {
            if indexes[i] == target {
                expected = Some(i);
            }
            for j in 0..i {
                if indexes[j] >= indexes[i] {
                    oracle_ordered = false;
                }
            }
        }
        assert_eq!(
            ordered, oracle_ordered,
            "strict order rejects duplicate and reversed IDs"
        );
        let found = find_index(&indexes, target);
        if let Some(position) = found {
            assert!(position < N);
            assert_eq!(
                indexes[position], target,
                "lookup cannot invent an identity"
            );
        }
        if ordered {
            assert_eq!(
                found, expected,
                "ordered lookup matches independent linear search"
            );
        }
        let duplicate = N > 1 && indexes[0] == indexes[1];
        (ordered, found, target, duplicate)
    }

    #[kani::proof]
    #[kani::unwind(2)]
    fn indexes_empty() {
        let (ordered, found, _, _) = indexes::<0>();
        kani::cover!(ordered && found.is_none(), "empty identity set checked");
    }
    #[kani::proof]
    #[kani::unwind(3)]
    fn indexes_one() {
        let (ordered, found, target, _) = indexes::<1>();
        kani::cover!(
            ordered && found.is_some() && target == usize::MAX,
            "maximum identity found"
        );
        kani::cover!(ordered && found.is_none(), "unknown identity rejected");
    }
    #[kani::proof]
    #[kani::unwind(4)]
    fn indexes_two() {
        let (ordered, found, target, duplicate) = indexes::<2>();
        kani::cover!(
            ordered && found.is_some() && target == usize::MAX,
            "maximum identity found"
        );
        kani::cover!(ordered && found.is_none(), "unknown identity rejected");
        kani::cover!(!ordered && duplicate, "duplicate identity rejected");
    }
    #[kani::proof]
    #[kani::unwind(6)]
    fn indexes_four() {
        let (ordered, found, target, duplicate) = indexes::<4>();
        kani::cover!(
            ordered && found.is_some() && target == usize::MAX,
            "maximum identity found"
        );
        kani::cover!(ordered && found.is_none(), "unknown identity rejected");
        kani::cover!(!ordered && duplicate, "duplicate identity rejected");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_capacity_and_rejection_identity() {
        let mut admission = EditAdmission::default();
        for _ in 0..MAX_EDITS {
            admission.admit(EditClass::Elide).unwrap();
        }
        let before = admission;
        assert!(admission.admit(EditClass::Elide).is_err());
        assert_eq!(admission, before);
        assert!(plan_bounds(MAX_EDITS, MAX_ITEMS));
        assert!(!plan_bounds(MAX_EDITS + 1, MAX_ITEMS));
        assert!(!plan_bounds(MAX_EDITS, MAX_ITEMS + 1));
    }

    #[test]
    fn exclusive_controls_and_digest_limit() {
        for class in [EditClass::ProviderControl, EditClass::CacheControl] {
            let mut admission = EditAdmission::default();
            admission.admit(class).unwrap();
            let before = admission;
            assert!(admission.admit(EditClass::Elide).is_err());
            assert_eq!(admission, before);
        }
        let mut admission = EditAdmission::default();
        admission.admit(EditClass::Digest).unwrap();
        let before = admission;
        assert!(admission.admit(EditClass::Digest).is_err());
        assert!(admission.admit(EditClass::ProviderControl).is_err());
        assert_eq!(admission, before);
    }

    #[test]
    fn exact_full_width_indexes_and_byte_bounds() {
        let indexes = [0, 7, usize::MAX];
        assert!(strictly_increasing(&indexes));
        assert_eq!(find_index(&indexes, usize::MAX), Some(2));
        assert_eq!(find_index(&indexes, 8), None);
        assert!(!strictly_increasing(&[7, 7]));
        assert_eq!(
            add_digest_bytes(MAX_DIGEST_BYTES, 0),
            Some(MAX_DIGEST_BYTES)
        );
        assert_eq!(add_digest_bytes(MAX_DIGEST_BYTES, 1), None);
        assert_eq!(add_digest_bytes(usize::MAX, 1), None);
        assert!(!is_elidable(1, Some(0)));
        assert!(!is_elidable(0, Some(u64::MAX)));
    }
}
