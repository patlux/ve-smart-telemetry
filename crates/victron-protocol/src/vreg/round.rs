//! Deterministic ties-to-even rounding for hundredths, matching the proven
//! Python decoder's `round(value / 100)` used for `0xedbc`/`0x2027`.

/// Round `v / 100` to the nearest integer, ties to even.
///
/// Python's `round()` rounds half to even, while Rust's `f64::round` rounds
/// half away from zero. This integer-only implementation is exact for every
/// `i64` input (no float rounding), so it matches Python bit for bit,
/// including the `.5` boundaries (e.g. `150 → 2`, `250 → 2`, `350 → 4`,
/// `-150 → -2`, `-250 → -2`, `-350 → -4`).
pub(crate) fn round_hundredths_ties_even(v: i64) -> i64 {
    let q = v / 100; // truncating division
    let r = v % 100; // remainder carries the sign of v
    match r {
        0 => q,
        r if r.abs() < 50 => q,
        r if r.abs() > 50 => q + v.signum(),
        // |r| == 50: ties to even.
        _ => {
            if q % 2 == 0 {
                q
            } else {
                q + v.signum()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::round_hundredths_ties_even;

    #[test]
    fn ties_to_even_positive() {
        // Python: round(150/100)=2, round(250/100)=2, round(350/100)=4.
        assert_eq!(round_hundredths_ties_even(150), 2);
        assert_eq!(round_hundredths_ties_even(250), 2);
        assert_eq!(round_hundredths_ties_even(350), 4);
        assert_eq!(round_hundredths_ties_even(450), 4);
        assert_eq!(round_hundredths_ties_even(550), 6);
    }

    #[test]
    fn ties_to_even_negative() {
        // Python: round(-150/100)=-2, round(-250/100)=-2, round(-350/100)=-4.
        assert_eq!(round_hundredths_ties_even(-150), -2);
        assert_eq!(round_hundredths_ties_even(-250), -2);
        assert_eq!(round_hundredths_ties_even(-350), -4);
        assert_eq!(round_hundredths_ties_even(-450), -4);
        assert_eq!(round_hundredths_ties_even(-550), -6);
    }

    #[test]
    fn non_ties_round_to_nearest() {
        // Golden values from `python3 -c 'print(round(v/100))'`.
        for (v, want) in [
            (149, 1),
            (151, 2),
            (249, 2),
            (251, 3),
            (99, 1),
            (101, 1),
            (-149, -1),
            (-151, -2),
            (-249, -2),
            (-251, -3),
            (-99, -1),
            (-101, -1),
            (0, 0),
            (1, 0),
            (-1, 0),
            (5, 0),
            (-5, 0),
            (15, 0),
            (-15, 0),
            (100, 1),
        ] {
            assert_eq!(round_hundredths_ties_even(v), want, "v={v}");
        }
    }

    #[test]
    fn extremes_do_not_overflow() {
        assert_eq!(round_hundredths_ties_even(i64::MAX), 92_233_720_368_547_758);
        assert_eq!(
            round_hundredths_ties_even(i64::MIN),
            -92_233_720_368_547_758
        );
    }
}
