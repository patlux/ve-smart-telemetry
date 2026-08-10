//! Trust and provenance of a measurement value.

/// How much a [`crate::Measurement`] value can be trusted.
///
/// Ordered from most trustworthy (rank 0) to least trustworthy (rank 3):
///
/// 1. `ConfirmedNative` — read directly from the device and confirmed
///    against the VictronConnect UI (e.g. PV voltage VREG `0xEDBB`).
/// 2. `Candidate` — read directly from the device, but the wire mapping is
///    not yet validated against the app UI (e.g. battery current).
/// 3. `Derived` — computed from other measurements of the same sample
///    (e.g. PV current = PV power / PV voltage). Even when its inputs are
///    confirmed, derivation introduces an assumption, so it ranks below a
///    native reading.
/// 4. `LocallyIntegrated` — produced by the durable local fallback energy
///    integration (trapezoidal kWh accumulation), the least preferred source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Quality {
    /// Read directly from the device and confirmed against the UI.
    ConfirmedNative,
    /// Read directly from the device; mapping not yet confirmed against the UI.
    Candidate,
    /// Computed from other measurements of this sample.
    Derived,
    /// Produced by the durable local fallback integration.
    LocallyIntegrated,
}

impl Quality {
    /// Precedence rank: 0 is the most trustworthy source.
    pub const fn rank(self) -> u8 {
        match self {
            Quality::ConfirmedNative => 0,
            Quality::Candidate => 1,
            Quality::Derived => 2,
            Quality::LocallyIntegrated => 3,
        }
    }

    /// Whether this quality is strictly more trustworthy than `other`.
    pub fn is_better_than(self, other: Quality) -> bool {
        self.rank() < other.rank()
    }

    /// The more trustworthy of `self` and `other`; on a tie, `self`.
    pub fn prefer(self, other: Quality) -> Quality {
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Quality::*;

    #[test]
    fn ranks_are_ordered() {
        assert!(ConfirmedNative.is_better_than(Candidate));
        assert!(Candidate.is_better_than(Derived));
        assert!(Derived.is_better_than(LocallyIntegrated));
        assert!(!LocallyIntegrated.is_better_than(Derived));
        assert!(!Derived.is_better_than(Derived));
    }

    #[test]
    fn numeric_ranks() {
        assert_eq!(ConfirmedNative.rank(), 0);
        assert_eq!(Candidate.rank(), 1);
        assert_eq!(Derived.rank(), 2);
        assert_eq!(LocallyIntegrated.rank(), 3);
    }

    #[test]
    fn prefer_picks_more_trustworthy() {
        assert_eq!(ConfirmedNative.prefer(Derived), ConfirmedNative);
        assert_eq!(Derived.prefer(Candidate), Candidate);
        assert_eq!(LocallyIntegrated.prefer(ConfirmedNative), ConfirmedNative);
        assert_eq!(Candidate.prefer(ConfirmedNative), ConfirmedNative);
    }

    #[test]
    fn prefer_tie_returns_self() {
        assert_eq!(ConfirmedNative.prefer(ConfirmedNative), ConfirmedNative);
        assert_eq!(
            LocallyIntegrated.prefer(LocallyIntegrated),
            LocallyIntegrated
        );
    }

    #[test]
    fn quality_is_copy() {
        let q = Candidate;
        let q2 = q; // copy, not move
        assert_eq!(q, q2);
    }
}
