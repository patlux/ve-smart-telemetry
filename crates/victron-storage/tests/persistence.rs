//! Restart continuity and collector key/value state tests.

mod common;

use common::{db_path, open_at, T0};

use victron_storage::{EnergyOutcome, StorageError};

#[test]
fn restart_preserves_spool_energy_and_kv_state() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = db_path(&dir);

    {
        let storage = open_at(&path);
        storage
            .enqueue_batch("d1", b"queued-payload".to_vec(), T0)
            .unwrap();
        storage
            .set_state("last_success_unixtime", &T0.to_string(), T0)
            .unwrap();
        storage
            .integrate_energy(&common::sample("solar-charger", 100.0, T0))
            .unwrap();
        assert!(matches!(
            storage
                .integrate_energy(&common::sample("solar-charger", 100.0, T0 + 10_000))
                .unwrap(),
            EnergyOutcome::Integrated { .. }
        ));
    } // storage dropped: simulated process exit

    let storage = open_at(&path);
    let batch = storage.claim_oldest_batch(T0 + 100_000).unwrap().unwrap();
    assert_eq!(batch.payload, b"queued-payload".to_vec());
    assert_eq!(
        batch.attempts, 1,
        "claim after restart continues the retry bookkeeping"
    );

    assert_eq!(
        storage
            .get_state("last_success_unixtime")
            .unwrap()
            .as_deref(),
        Some(T0.to_string().as_str())
    );

    let energy = storage.get_energy("solar-charger").unwrap().unwrap();
    assert_eq!(energy.last_sample_at_ms, Some(T0 + 10_000));
    assert!(energy.total_kwh > 0.0);
}

#[test]
fn kv_roundtrip_upsert_and_typed_accessors() {
    let (_dir, storage) = common::open_tmp();

    storage.set_state("a", "hello", T0).unwrap();
    assert_eq!(storage.get_state("a").unwrap().as_deref(), Some("hello"));

    // Upsert overwrites value and timestamp.
    storage.set_state("a", "world", T0 + 1000).unwrap();
    assert_eq!(storage.get_state("a").unwrap().as_deref(), Some("world"));

    // Missing keys return None.
    assert_eq!(storage.get_state("missing").unwrap(), None);

    storage.set_state_i64("counter", 41, T0).unwrap();
    storage.set_state_i64("counter", 42, T0 + 1000).unwrap();
    assert_eq!(storage.get_state_i64("counter").unwrap(), Some(42));

    // Listing returns all entries sorted by key.
    let entries = storage.get_state_entries().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].key, "a");
    assert_eq!(entries[1].key, "counter");
    assert!(entries.iter().all(|e| e.updated_at_ms > 0));

    // Non-numeric stored under a numeric key reports inconsistent state.
    storage.set_state("text", "not-a-number", T0).unwrap();
    assert!(matches!(
        storage.get_state_i64("text"),
        Err(StorageError::Inconsistent(_))
    ));
}

#[test]
fn kv_state_survives_restart() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = db_path(&dir);

    {
        let storage = open_at(&path);
        storage.set_state_i64("cycle_count", 7, T0).unwrap();
    }

    let storage = open_at(&path);
    assert_eq!(storage.get_state_i64("cycle_count").unwrap(), Some(7));
}
