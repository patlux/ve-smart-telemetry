use tempfile::TempDir;
use victron_storage::{
    AcquisitionCommit, AcquisitionCommitOutcome, AcquisitionEnergyState, Storage, StorageConfig,
    StorageError,
};

fn open() -> (TempDir, Storage) {
    let dir = tempfile::tempdir().unwrap();
    let storage =
        Storage::open(&dir.path().join("state.sqlite3"), StorageConfig::default()).unwrap();
    (dir, storage)
}

fn state(total: f64, power: Option<f64>, at: Option<i64>) -> AcquisitionEnergyState {
    AcquisitionEnergyState {
        total_kwh: total,
        last_power_watts: power,
        last_sample_at_ms: at,
    }
}

fn commit(
    observed_at_ms: i64,
    expected_energy: Option<AcquisitionEnergyState>,
    next_energy: AcquisitionEnergyState,
    payload: &[u8],
) -> AcquisitionCommit {
    AcquisitionCommit {
        device: "solar-charger".into(),
        observed_at_ms,
        expected_energy,
        next_energy,
        payload: payload.to_vec(),
    }
}

#[test]
fn commits_energy_identity_and_spool_atomically() {
    let (_dir, storage) = open();
    let next = state(0.25, Some(125.0), Some(1_700_000_000_000));

    let outcome = storage
        .commit_acquisition(&commit(
            1_700_000_000_000,
            None,
            next.clone(),
            b"metric 1 1700000000000\n",
        ))
        .unwrap();
    assert!(matches!(
        outcome,
        AcquisitionCommitOutcome::Committed { batch_id: 1 }
    ));
    assert_eq!(
        storage.last_acquisition_success("solar-charger").unwrap(),
        Some(1_700_000_000_000)
    );
    let energy = storage.get_energy("solar-charger").unwrap().unwrap();
    assert_eq!(energy.total_kwh, next.total_kwh);
    assert_eq!(energy.last_power_watts, next.last_power_watts);
    assert_eq!(energy.last_sample_at_ms, next.last_sample_at_ms);
    assert_eq!(storage.spool_stats().unwrap().queued_batches, 1);
}

#[test]
fn duplicate_or_older_observation_is_a_complete_noop() {
    let (_dir, storage) = open();
    let first = state(0.25, Some(125.0), Some(2_000));
    storage
        .commit_acquisition(&commit(2_000, None, first.clone(), b"first"))
        .unwrap();

    for timestamp in [2_000, 1_999] {
        let outcome = storage
            .commit_acquisition(&commit(
                timestamp,
                Some(first.clone()),
                state(99.0, Some(999.0), Some(timestamp)),
                b"duplicate",
            ))
            .unwrap();
        assert_eq!(outcome, AcquisitionCommitOutcome::AlreadyCommitted);
    }

    let energy = storage.get_energy("solar-charger").unwrap().unwrap();
    assert_eq!(energy.total_kwh, first.total_kwh);
    assert_eq!(storage.spool_stats().unwrap().queued_batches, 1);
}

#[test]
fn optimistic_anchor_conflict_rolls_back_everything() {
    let (_dir, storage) = open();
    let stored = state(1.0, Some(100.0), Some(1_000));
    storage
        .commit_acquisition(&commit(1_000, None, stored.clone(), b"first"))
        .unwrap();

    let err = storage
        .commit_acquisition(&commit(
            2_000,
            Some(state(0.5, Some(50.0), Some(1_000))),
            state(1.1, Some(110.0), Some(2_000)),
            b"must-not-persist",
        ))
        .unwrap_err();
    assert!(matches!(err, StorageError::EnergyAnchorConflict));
    assert_eq!(
        storage.last_acquisition_success("solar-charger").unwrap(),
        Some(1_000)
    );
    assert_eq!(
        storage
            .get_energy("solar-charger")
            .unwrap()
            .unwrap()
            .total_kwh,
        stored.total_kwh
    );
    assert_eq!(storage.spool_stats().unwrap().queued_batches, 1);
}

#[test]
fn invalid_payload_or_state_writes_nothing() {
    let (_dir, storage) = open();
    let invalids = [
        commit(1_000, None, state(0.0, None, Some(1_001)), b"payload"),
        commit(1_000, None, state(f64::NAN, None, None), b"payload"),
        commit(1_000, None, state(0.0, None, None), b""),
    ];
    for invalid in invalids {
        assert!(matches!(
            storage.commit_acquisition(&invalid),
            Err(StorageError::InvalidArgument(_))
        ));
    }
    assert_eq!(storage.get_energy("solar-charger").unwrap(), None);
    assert_eq!(storage.spool_stats().unwrap().queued_batches, 0);
}
