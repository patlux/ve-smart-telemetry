//! Energy integration tests: trapezoid math, skip reasons, per-device
//! isolation, restart double-count guard and reset semantics.

mod common;

use common::{db_path, open_at, open_tmp, raw_conn, sample, T0};

use victron_storage::{EnergyOutcome, SkipReason};

#[test]
fn integration_uses_trapezoid_and_matches_expected_math() {
    let (_dir, storage) = open_tmp();

    // First sample: anchor only, no energy.
    assert_eq!(
        storage.integrate_energy(&sample("d", 100.0, T0)).unwrap(),
        EnergyOutcome::Skipped {
            reason: SkipReason::FirstSample
        }
    );

    // 10 seconds later at the same power: avg 100 W over 10 s.
    let outcome = storage
        .integrate_energy(&sample("d", 100.0, T0 + 10_000))
        .unwrap();
    let EnergyOutcome::Integrated {
        delta_kwh,
        total_kwh,
        elapsed_ms,
    } = outcome
    else {
        panic!("expected integration");
    };
    assert_eq!(elapsed_ms, 10_000);
    let expected = 100.0 * 10.0 / 3_600_000.0;
    assert!((delta_kwh - expected).abs() < 1e-12);
    assert_eq!(total_kwh, delta_kwh);

    // Ramp 100 W -> 300 W over 5 s: avg 200 W.
    let outcome = storage
        .integrate_energy(&sample("d", 300.0, T0 + 15_000))
        .unwrap();
    let EnergyOutcome::Integrated {
        delta_kwh,
        total_kwh,
        ..
    } = outcome
    else {
        panic!("expected integration");
    };
    let expected = 200.0 * 5.0 / 3_600_000.0;
    assert!((delta_kwh - expected).abs() < 1e-12);
    assert!((total_kwh - (100.0 * 10.0 / 3_600_000.0 + expected)).abs() < 1e-12);
}

#[test]
fn integration_skips_gaps_over_threshold_and_resets_anchor() {
    let (_dir, storage) = open_tmp();
    let threshold = storage.config().energy_gap_threshold_ms; // 5 min

    storage.integrate_energy(&sample("d", 100.0, T0)).unwrap();

    // A gap beyond the threshold: skipped, anchor reset, no energy added.
    let outcome = storage
        .integrate_energy(&sample("d", 100.0, T0 + threshold + 1000))
        .unwrap();
    assert_eq!(
        outcome,
        EnergyOutcome::Skipped {
            reason: SkipReason::GapTooLarge {
                gap_ms: threshold + 1000,
                threshold_ms: threshold,
            }
        }
    );
    assert_eq!(storage.get_energy("d").unwrap().unwrap().total_kwh, 0.0);

    // The next sample integrates from the reset anchor, not from the gap start.
    let outcome = storage
        .integrate_energy(&sample("d", 100.0, T0 + threshold + 11_000))
        .unwrap();
    let EnergyOutcome::Integrated {
        delta_kwh,
        elapsed_ms,
        ..
    } = outcome
    else {
        panic!("expected integration after anchor reset");
    };
    assert_eq!(elapsed_ms, 10_000);
    assert!((delta_kwh - 100.0 * 10.0 / 3_600_000.0).abs() < 1e-12);
}

#[test]
fn integration_skips_backward_and_duplicate_timestamps() {
    let (_dir, storage) = open_tmp();
    storage
        .integrate_energy(&sample("d", 100.0, T0 + 5000))
        .unwrap();

    let outcome = storage.integrate_energy(&sample("d", 100.0, T0)).unwrap();
    assert_eq!(
        outcome,
        EnergyOutcome::Skipped {
            reason: SkipReason::BackwardTime
        }
    );

    // Duplicate of the anchor sample (the restart double-count guard).
    let outcome = storage
        .integrate_energy(&sample("d", 100.0, T0 + 5000))
        .unwrap();
    assert_eq!(
        outcome,
        EnergyOutcome::Skipped {
            reason: SkipReason::BackwardTime
        }
    );

    // The anchor was not moved: state is unchanged and total stayed 0.
    let energy = storage.get_energy("d").unwrap().unwrap();
    assert_eq!(energy.last_sample_at_ms, Some(T0 + 5000));
    assert_eq!(energy.total_kwh, 0.0);
}

#[test]
fn integration_skips_invalid_power_and_keeps_anchor() {
    let (_dir, storage) = open_tmp();
    storage.integrate_energy(&sample("d", 100.0, T0)).unwrap();

    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        let outcome = storage
            .integrate_energy(&sample("d", bad, T0 + 10_000))
            .unwrap();
        assert_eq!(
            outcome,
            EnergyOutcome::Skipped {
                reason: SkipReason::InvalidPower
            },
            "power {bad} must be rejected"
        );
    }

    // Above the sanity bound (default 30 kW, the domain plan's conservative
    // PV maximum) is also invalid.
    let outcome = storage
        .integrate_energy(&sample("d", 100_000.0, T0 + 10_000))
        .unwrap();
    assert_eq!(
        outcome,
        EnergyOutcome::Skipped {
            reason: SkipReason::InvalidPower
        }
    );

    // Anchor untouched by all invalid samples.
    let energy = storage.get_energy("d").unwrap().unwrap();
    assert_eq!(energy.last_sample_at_ms, Some(T0));
    assert_eq!(energy.last_power_watts, Some(100.0));
    assert_eq!(energy.total_kwh, 0.0);
}

#[test]
fn integration_state_is_isolated_per_device() {
    let (_dir, storage) = open_tmp();
    storage.integrate_energy(&sample("pv", 100.0, T0)).unwrap();
    storage
        .integrate_energy(&sample("battery", 200.0, T0))
        .unwrap();

    storage
        .integrate_energy(&sample("pv", 100.0, T0 + 10_000))
        .unwrap();
    storage
        .integrate_energy(&sample("battery", 200.0, T0 + 10_000))
        .unwrap();

    let pv = storage.get_energy("pv").unwrap().unwrap();
    let battery = storage.get_energy("battery").unwrap().unwrap();
    let pv_expected = 100.0 * 10.0 / 3_600_000.0;
    let battery_expected = 200.0 * 10.0 / 3_600_000.0;
    assert!((pv.total_kwh - pv_expected).abs() < 1e-12);
    assert!((battery.total_kwh - battery_expected).abs() < 1e-12);
    assert!((pv.total_kwh - battery.total_kwh).abs() > 1e-9);
}

#[test]
fn integration_does_not_double_count_across_restart() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = db_path(&dir);

    let total_after_first_run;
    {
        let storage = open_at(&path);
        storage.integrate_energy(&sample("d", 100.0, T0)).unwrap();
        let outcome = storage
            .integrate_energy(&sample("d", 100.0, T0 + 10_000))
            .unwrap();
        let EnergyOutcome::Integrated { total_kwh, .. } = outcome else {
            panic!("expected integration");
        };
        total_after_first_run = total_kwh;
    }

    // Restart; the service re-processes the same last sample (e.g. because the
    // process crashed between commit and metric rendering).
    let storage = open_at(&path);
    let outcome = storage
        .integrate_energy(&sample("d", 100.0, T0 + 10_000))
        .unwrap();
    assert_eq!(
        outcome,
        EnergyOutcome::Skipped {
            reason: SkipReason::BackwardTime
        }
    );

    let energy = storage.get_energy("d").unwrap().unwrap();
    assert!((energy.total_kwh - total_after_first_run).abs() < 1e-12);
}

#[test]
fn reset_energy_clears_anchor_and_sets_total() {
    let (_dir, storage) = open_tmp();
    storage.integrate_energy(&sample("d", 100.0, T0)).unwrap();
    storage
        .integrate_energy(&sample("d", 100.0, T0 + 10_000))
        .unwrap();

    storage.reset_energy("d", 42.5, T0 + 20_000).unwrap();
    let energy = storage.get_energy("d").unwrap().unwrap();
    assert_eq!(energy.total_kwh, 42.5);
    assert_eq!(energy.last_sample_at_ms, None);

    // After a reset the next sample anchors again instead of integrating
    // against stale state.
    let outcome = storage
        .integrate_energy(&sample("d", 50.0, T0 + 30_000))
        .unwrap();
    assert_eq!(
        outcome,
        EnergyOutcome::Skipped {
            reason: SkipReason::FirstSample
        }
    );
}

#[test]
fn invalid_first_sample_creates_no_row() {
    let (_dir, storage) = open_tmp();

    for bad in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -1.0,
        100_000.0, // above the default 30 kW bound
    ] {
        let outcome = storage.integrate_energy(&sample("d", bad, T0)).unwrap();
        assert_eq!(
            outcome,
            EnergyOutcome::Skipped {
                reason: SkipReason::InvalidPower
            },
            "first power {bad} must be rejected"
        );
        // The invalid first sample must not have created or mutated any row:
        // a poisoned anchor would make every later integration measure
        // against garbage.
        assert!(
            storage.get_energy("d").unwrap().is_none(),
            "no energy_state row may exist after an invalid first sample ({bad})"
        );
    }

    // A valid first sample afterwards still anchors normally.
    let outcome = storage.integrate_energy(&sample("d", 100.0, T0)).unwrap();
    assert_eq!(
        outcome,
        EnergyOutcome::Skipped {
            reason: SkipReason::FirstSample
        }
    );
    let energy = storage.get_energy("d").unwrap().unwrap();
    assert_eq!(energy.last_power_watts, Some(100.0));
    assert_eq!(energy.last_sample_at_ms, Some(T0));
    assert_eq!(energy.total_kwh, 0.0);
}

#[test]
fn invalid_first_sample_is_rejected_even_before_any_transaction() {
    // Force-invalid samples must never open a write transaction: the schema
    // row count stays zero and no side tables are touched.
    let (_dir, storage) = open_tmp();
    for bad in [f64::NAN, -0.5, 60_000.0] {
        assert_eq!(
            storage.integrate_energy(&sample("other", bad, T0)).unwrap(),
            EnergyOutcome::Skipped {
                reason: SkipReason::InvalidPower
            }
        );
    }
    assert_eq!(storage.get_energy("other").unwrap(), None);
}

#[test]
fn default_max_power_accepts_full_supported_family() {
    let (_dir, storage) = open_tmp();

    // The domain plan's conservative PV range is [0.0, 30_000.0] W
    // (250 V x 100 A = 25 kW worst case plus margin). The default sanity
    // bound must accept every value in that range.
    storage
        .integrate_energy(&sample("d", 25_000.0, T0))
        .unwrap();
    let outcome = storage
        .integrate_energy(&sample("d", 30_000.0, T0 + 10_000))
        .unwrap();
    assert!(
        matches!(outcome, EnergyOutcome::Integrated { .. }),
        "30 kW is inside the supported family and must integrate"
    );

    // Still above the conservative maximum: rejected, anchor kept.
    let outcome = storage
        .integrate_energy(&sample("d", 30_000.5, T0 + 20_000))
        .unwrap();
    assert_eq!(
        outcome,
        EnergyOutcome::Skipped {
            reason: SkipReason::InvalidPower
        }
    );
    let energy = storage.get_energy("d").unwrap().unwrap();
    assert_eq!(energy.last_power_watts, Some(30_000.0));
}

#[test]
fn reset_energy_rejects_negative_and_non_finite_totals() {
    let (_dir, storage) = open_tmp();
    storage.integrate_energy(&sample("d", 100.0, T0)).unwrap();
    storage
        .integrate_energy(&sample("d", 100.0, T0 + 10_000))
        .unwrap();
    let before = storage.get_energy("d").unwrap().unwrap();

    for bad in [-1.0f64, -0.001, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let err = storage.reset_energy("d", bad, T0 + 20_000).unwrap_err();
        assert!(
            matches!(err, victron_storage::StorageError::InvalidArgument(_)),
            "reset with {bad} must be rejected as an argument error"
        );
        assert_eq!(
            storage.get_energy("d").unwrap().unwrap(),
            before,
            "state must be untouched after rejected reset ({bad})"
        );
    }
}

#[test]
fn energy_state_check_rejects_negative_cumulative_energy() {
    let (dir, storage) = open_tmp();
    storage.integrate_energy(&sample("d", 100.0, T0)).unwrap();
    assert_eq!(storage.user_version().unwrap(), 2);

    // Bypass the facade: the schema CHECK itself must reject a negative
    // cumulative total, even through a raw upsert.
    let conn = raw_conn(&db_path(&dir));
    let err = conn
        .execute(
            "INSERT INTO energy_state (device, total_kwh, last_power_watts, last_sample_at_ms, updated_at_ms)
             VALUES ('d', -1.0, NULL, NULL, ?1)
             ON CONFLICT(device) DO UPDATE SET total_kwh = excluded.total_kwh",
            rusqlite::params![T0],
        )
        .unwrap_err();
    assert!(err.to_string().contains("CHECK"));
}

#[test]
fn failed_energy_write_leaves_no_partial_state() {
    let (_dir, storage) = open_tmp();
    storage.integrate_energy(&sample("d", 100.0, T0)).unwrap();
    let before = storage.get_energy("d").unwrap().unwrap();

    // total_kwh beyond the schema's upper CHECK bound (finite and >= 0, so
    // it passes code validation): the constraint fires inside the transaction
    // and the row must be untouched.
    let err = storage.reset_energy("d", 2.0e9, T0 + 5000).unwrap_err();
    assert!(matches!(err, victron_storage::StorageError::Sqlite(_)));

    let after = storage.get_energy("d").unwrap().unwrap();
    assert_eq!(before, after);
}
