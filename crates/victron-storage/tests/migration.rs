//! Migration, pragma and configuration tests against temp databases.

mod common;

use common::{cfg, db_path, open_at, open_tmp, raw_conn};

use victron_storage::{Storage, StorageConfig, StorageError, SynchronousMode};

#[test]
fn migration_fresh_database_lands_on_latest_version() {
    let (dir, storage) = open_tmp();
    assert_eq!(storage.user_version().unwrap(), 2);

    // All tables exist with the expected shape.
    let conn = raw_conn(&db_path(&dir));
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for expected in ["collector_state", "energy_state", "spool_batch"] {
        assert!(
            tables.iter().any(|t| t == expected),
            "missing table {expected}"
        );
    }
}

#[test]
fn migration_is_idempotent_across_reopens() {
    let (dir, storage) = open_tmp();
    assert_eq!(storage.user_version().unwrap(), 2);
    drop(storage);

    // Reopen the same file repeatedly: no error, version stays at the latest.
    for _ in 0..3 {
        let storage = open_at(&db_path(&dir));
        assert_eq!(storage.user_version().unwrap(), 2);
    }
}

#[test]
fn migration_rejects_newer_database() {
    let (dir, storage) = open_tmp();
    assert_eq!(storage.user_version().unwrap(), 2);
    drop(storage);

    // Simulate a database written by a future binary.
    let conn = raw_conn(&db_path(&dir));
    conn.pragma_update(None, "user_version", 99).unwrap();
    drop(conn);

    let err = Storage::open(&db_path(&dir), cfg())
        .err()
        .expect("must fail");
    match err {
        StorageError::DatabaseTooNew { found, supported } => {
            assert_eq!(found, 99);
            assert_eq!(supported, 2);
        }
        other => panic!("expected DatabaseTooNew, got {other:?}"),
    }
}

#[test]
fn migration_v2_rebuilds_energy_state_with_non_negative_check() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = db_path(&dir);

    // Simulate a database written by the initial implementation: V1 schema
    // whose energy_state CHECK still allowed negative cumulative totals.
    let conn = raw_conn(&path);
    conn.execute_batch(
        "CREATE TABLE energy_state (
            device             TEXT    PRIMARY KEY,
            total_kwh          REAL    NOT NULL CHECK (total_kwh BETWEEN -1.0e6 AND 1.0e9),
            last_power_watts   REAL,
            last_sample_at_ms  INTEGER,
            updated_at_ms      INTEGER NOT NULL CHECK (updated_at_ms > 0)
        ) STRICT;
        INSERT INTO energy_state (device, total_kwh, last_power_watts, last_sample_at_ms, updated_at_ms)
        VALUES ('legacy', 12.5, 300.0, 1700000000000, 1700000000000);
        PRAGMA user_version = 1;",
    )
    .unwrap();
    drop(conn);

    // Reopen through Storage: the V2 migration must rebuild the table and
    // preserve the existing row.
    let storage = open_at(&path);
    assert_eq!(storage.user_version().unwrap(), 2);
    let energy = storage.get_energy("legacy").unwrap().unwrap();
    assert_eq!(energy.total_kwh, 12.5);
    assert_eq!(energy.last_power_watts, Some(300.0));

    // The hardened CHECK is now in effect on the migrated table.
    let conn = raw_conn(&path);
    let err = conn
        .execute(
            "INSERT INTO energy_state (device, total_kwh, last_power_watts, last_sample_at_ms, updated_at_ms)
             VALUES ('other', -1.0, NULL, NULL, 1700000000000)",
            rusqlite::params![],
        )
        .unwrap_err();
    assert!(err.to_string().contains("CHECK"));
}

#[test]
fn migration_v2_fails_loudly_on_legacy_negative_total() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = db_path(&dir);

    // A legacy row that the old CHECK allowed but the hardened one cannot
    // represent: the migration must fail loudly instead of clamping.
    let conn = raw_conn(&path);
    conn.execute_batch(
        "CREATE TABLE energy_state (
            device             TEXT    PRIMARY KEY,
            total_kwh          REAL    NOT NULL CHECK (total_kwh BETWEEN -1.0e6 AND 1.0e9),
            last_power_watts   REAL,
            last_sample_at_ms  INTEGER,
            updated_at_ms      INTEGER NOT NULL CHECK (updated_at_ms > 0)
        ) STRICT;
        INSERT INTO energy_state (device, total_kwh, last_power_watts, last_sample_at_ms, updated_at_ms)
        VALUES ('bad', -5.0, NULL, NULL, 1700000000000);
        PRAGMA user_version = 1;",
    )
    .unwrap();
    drop(conn);

    let err = match Storage::open(&path, cfg()) {
        Ok(_) => panic!("open of a legacy negative-total DB must fail"),
        Err(e) => e,
    };
    assert!(matches!(err, StorageError::Sqlite(_)));
    // The failed migration rolled back: the table is untouched.
    let conn = raw_conn(&path);
    let total: f64 = conn
        .query_row(
            "SELECT total_kwh FROM energy_state WHERE device = 'bad'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(total, -5.0);
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
}

#[test]
fn journaling_pragmas_are_conservative_by_default() {
    let (_dir, storage) = open_tmp();
    assert_eq!(storage.journal_mode().unwrap(), "delete");
    assert_eq!(storage.synchronous().unwrap(), SynchronousMode::Full);
}

#[test]
fn wal_journaling_is_an_explicit_opt_in() {
    let dir = tempfile::TempDir::new().unwrap();
    let storage = Storage::open(&db_path(&dir), StorageConfig::wal()).unwrap();
    assert_eq!(storage.journal_mode().unwrap(), "wal");
    assert_eq!(storage.synchronous().unwrap(), SynchronousMode::Normal);
}

#[test]
fn config_validation_rejects_impossible_settings() {
    let bad = StorageConfig {
        max_spool_attempts: 0,
        ..cfg()
    };
    assert!(bad.validate().is_err());

    let bad = StorageConfig {
        energy_gap_threshold_ms: 0,
        ..cfg()
    };
    assert!(bad.validate().is_err());

    let bad = StorageConfig {
        energy_max_power_watts: Some(100.0),
        energy_min_power_watts: 200.0,
        ..cfg()
    };
    assert!(bad.validate().is_err());

    // A config that allows an immediate retry still requires a positive delay.
    assert!(StorageConfig::conservative().validate().is_ok());
}

#[test]
fn config_validation_rejects_non_finite_power_bounds() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let min_bad = StorageConfig {
            energy_min_power_watts: bad,
            ..cfg()
        };
        assert!(min_bad.validate().is_err(), "min {bad} must be rejected");

        let max_bad = StorageConfig {
            energy_max_power_watts: Some(bad),
            ..cfg()
        };
        assert!(max_bad.validate().is_err(), "max {bad} must be rejected");
    }

    // An empty interval (max == min) is not usable for integration.
    let equal = StorageConfig {
        energy_max_power_watts: Some(50.0),
        energy_min_power_watts: 50.0,
        ..cfg()
    };
    assert!(equal.validate().is_err());

    let valid = StorageConfig {
        energy_max_power_watts: Some(50.0),
        energy_min_power_watts: 0.0,
        ..cfg()
    };
    assert!(valid.validate().is_ok());
}

#[test]
fn config_validation_rejects_retry_and_spool_bound_overflow() {
    // Non-positive retry ceiling.
    for bad in [0i64, -1, i64::MIN] {
        let cfg_bad = StorageConfig {
            spool_retry_max_ms: bad,
            ..cfg()
        };
        assert!(
            cfg_bad.validate().is_err(),
            "retry max {bad} must be rejected"
        );
    }

    // Ceiling below the base backoff.
    let ceiling_below_base = StorageConfig {
        spool_retry_base_ms: 60_000,
        spool_retry_max_ms: 30_000,
        ..cfg()
    };
    assert!(ceiling_below_base.validate().is_err());
    let equal_ok = StorageConfig {
        spool_retry_base_ms: 30_000,
        spool_retry_max_ms: 30_000,
        ..cfg()
    };
    assert!(equal_ok.validate().is_ok());

    // Non-positive in-flight window.
    for bad in [0i64, -5] {
        let cfg_bad = StorageConfig {
            spool_inflight_ms: bad,
            ..cfg()
        };
        assert!(
            cfg_bad.validate().is_err(),
            "inflight {bad} must be rejected"
        );
    }

    // Non-positive spool age bound.
    for bad in [0i64, -1] {
        let cfg_bad = StorageConfig {
            max_spool_age_ms: bad,
            ..cfg()
        };
        assert!(
            cfg_bad.validate().is_err(),
            "spool age {bad} must be rejected"
        );
    }

    // A zero count bound would empty the spool; a bound above i64::MAX cannot
    // be represented in the SQLite INTEGER used by the prune OFFSET.
    let zero_batches = StorageConfig {
        max_spool_batches: 0,
        ..cfg()
    };
    assert!(zero_batches.validate().is_err());
    let overflow_batches = StorageConfig {
        max_spool_batches: u64::MAX,
        ..cfg()
    };
    assert!(overflow_batches.validate().is_err());
    let max_ok = StorageConfig {
        max_spool_batches: i64::MAX as u64,
        ..cfg()
    };
    assert!(max_ok.validate().is_ok());
}
