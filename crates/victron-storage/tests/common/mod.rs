//! Shared helpers for the integration test binaries (each file in `tests/`
//! is its own crate, so helpers live in a `common` submodule).

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use tempfile::TempDir;

use victron_storage::{EnergySample, Storage, StorageConfig};

/// Fixed "now" for deterministic tests.
pub const T0: i64 = 1_700_000_000_000;

pub fn cfg() -> StorageConfig {
    StorageConfig::conservative()
}

pub fn open_tmp() -> (TempDir, Storage) {
    let dir = TempDir::new().expect("tempdir");
    let storage = Storage::open(&dir.path().join("state.sqlite3"), cfg()).expect("open");
    (dir, storage)
}

pub fn open_at(path: &Path) -> Storage {
    Storage::open(path, cfg()).expect("open")
}

pub fn db_path(dir: &TempDir) -> PathBuf {
    dir.path().join("state.sqlite3")
}

pub fn raw_conn(path: &Path) -> Connection {
    Connection::open(path).expect("raw open")
}

pub fn sample(device: &str, power_watts: f64, at_ms: i64) -> EnergySample {
    EnergySample {
        device: device.to_string(),
        power_watts,
        sample_at_ms: at_ms,
    }
}
