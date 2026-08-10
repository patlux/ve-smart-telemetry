//! Connection setup, idempotent schema migration and collector key/value state.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::{JournalMode, StorageConfig, StorageError};

/// Highest schema version this binary knows how to migrate to.
pub(crate) fn schema_version() -> i64 {
    MIGRATIONS.len() as i64
}

/// Migration scripts, indexed by target version (`scripts[i]` migrates to
/// version `i + 1`). Scripts are idempotent (`CREATE ... IF NOT EXISTS`) and
/// each is applied inside its own transaction together with the
/// `user_version` bump, so a partially applied migration can never persist.
const MIGRATIONS: &[&str] = &[MIGRATION_V1, MIGRATION_V2];

const MIGRATION_V1: &str = "
-- Collector key/value state (last delivered timestamp, counters, ...).
CREATE TABLE IF NOT EXISTS collector_state (
    key            TEXT PRIMARY KEY,
    value          TEXT NOT NULL,
    updated_at_ms  INTEGER NOT NULL CHECK (updated_at_ms > 0)
) STRICT;

-- Durable outbound delivery spool, ordered FIFO by (created_at_ms, id).
-- Delivered batches are deleted; this table only ever holds undelivered rows,
-- keeping the database bounded.
CREATE TABLE IF NOT EXISTS spool_batch (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    device             TEXT    NOT NULL,
    created_at_ms      INTEGER NOT NULL CHECK (created_at_ms > 0),
    payload            BLOB    NOT NULL CHECK (length(payload) > 0),
    attempts           INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at_ms INTEGER NOT NULL DEFAULT 0 CHECK (next_attempt_at_ms >= 0)
) STRICT;

CREATE INDEX IF NOT EXISTS idx_spool_batch_fifo
    ON spool_batch (created_at_ms, id);

-- Transaction-safe per-device energy integration state. The accumulator and
-- the last-sample anchor commit together, so re-processing a sample after a
-- crash can never double-count. The cumulative total is never negative: the
-- integration accumulator only ever adds non-negative deltas and resets
-- reject negative totals.
CREATE TABLE IF NOT EXISTS energy_state (
    device             TEXT    PRIMARY KEY,
    total_kwh          REAL    NOT NULL CHECK (total_kwh >= 0.0 AND total_kwh <= 1.0e9),
    last_power_watts   REAL,
    last_sample_at_ms  INTEGER,
    updated_at_ms      INTEGER NOT NULL CHECK (updated_at_ms > 0)
) STRICT;
";

const MIGRATION_V2: &str = "
-- Rebuild energy_state with a non-negative cumulative-energy CHECK. The
-- initial schema allowed negative totals (CHECK BETWEEN -1.0e6 AND 1.0e9),
-- which the integration accumulator can never legitimately produce and the
-- hardened reset path now rejects. SQLite cannot alter CHECK constraints, so
-- the table is rebuilt. A legacy row with a negative total fails this
-- migration loudly instead of being silently carried over.
CREATE TABLE energy_state_v2 (
    device             TEXT    PRIMARY KEY,
    total_kwh          REAL    NOT NULL CHECK (total_kwh >= 0.0 AND total_kwh <= 1.0e9),
    last_power_watts   REAL,
    last_sample_at_ms  INTEGER,
    updated_at_ms      INTEGER NOT NULL CHECK (updated_at_ms > 0)
) STRICT;

INSERT INTO energy_state_v2 (device, total_kwh, last_power_watts, last_sample_at_ms, updated_at_ms)
    SELECT device, total_kwh, last_power_watts, last_sample_at_ms, updated_at_ms
    FROM energy_state;

DROP TABLE energy_state;

ALTER TABLE energy_state_v2 RENAME TO energy_state;
";

/// Opens the database, applies journaling/synchronous pragmas and runs any
/// pending migrations.
pub(crate) fn open_connection(
    path: &Path,
    cfg: &StorageConfig,
) -> Result<Connection, StorageError> {
    let mut conn = Connection::open(path)?;
    configure_pragmas(&conn, cfg)?;
    migrate(&mut conn)?;
    Ok(conn)
}

/// Conservative, explicit pragma configuration. The busy_timeout is set first
/// so the journal-mode switch (which may briefly need the write lock) can wait
/// instead of failing.
fn configure_pragmas(conn: &Connection, cfg: &StorageConfig) -> Result<(), StorageError> {
    conn.pragma_update(None, "busy_timeout", cfg.busy_timeout_ms)?;

    let (journal, synchronous) = match cfg.journal {
        JournalMode::Conservative => ("DELETE", "FULL"),
        JournalMode::Wal => ("WAL", "NORMAL"),
    };
    conn.pragma_update(None, "journal_mode", journal)?;
    conn.pragma_update(None, "synchronous", synchronous)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

/// Applies pending migrations idempotently. Each migration runs in its own
/// `BEGIN IMMEDIATE` transaction together with the `user_version` update.
fn migrate(conn: &mut Connection) -> Result<(), StorageError> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let supported = schema_version();
    if current > supported {
        return Err(StorageError::DatabaseTooNew {
            found: current,
            supported,
        });
    }
    if current == supported {
        return Ok(());
    }

    for (idx, script) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let target = (idx + 1) as i64;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(script)?;
        tx.pragma_update(None, "user_version", target)?;
        tx.commit()?;
    }
    Ok(())
}

/// One collector_state row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvEntry {
    pub key: String,
    pub value: String,
    pub updated_at_ms: i64,
}

pub(crate) fn get_kv(conn: &Connection, key: &str) -> Result<Option<String>, StorageError> {
    Ok(conn
        .query_row(
            "SELECT value FROM collector_state WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()?)
}

pub(crate) fn get_kv_i64(conn: &Connection, key: &str) -> Result<Option<i64>, StorageError> {
    match get_kv(conn, key)? {
        None => Ok(None),
        Some(raw) => raw.parse::<i64>().map(Some).map_err(|_| {
            StorageError::Inconsistent(format!("collector_state[{key}] is not an integer"))
        }),
    }
}

pub(crate) fn set_kv(
    conn: &Connection,
    key: &str,
    value: &str,
    now_ms: i64,
) -> Result<(), StorageError> {
    if key.is_empty() {
        return Err(StorageError::InvalidArgument(
            "key must not be empty".into(),
        ));
    }
    if now_ms <= 0 {
        return Err(StorageError::InvalidArgument("now_ms must be > 0".into()));
    }
    conn.execute(
        "INSERT INTO collector_state (key, value, updated_at_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at_ms = excluded.updated_at_ms",
        params![key, value, now_ms],
    )?;
    Ok(())
}

pub(crate) fn list_kv(conn: &Connection) -> Result<Vec<KvEntry>, StorageError> {
    let mut stmt =
        conn.prepare("SELECT key, value, updated_at_ms FROM collector_state ORDER BY key")?;
    let rows = stmt.query_map([], |row| {
        Ok(KvEntry {
            key: row.get(0)?,
            value: row.get(1)?,
            updated_at_ms: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Atomic `+= 1` on a numeric counter stored in collector_state. Must be
/// called inside an active transaction to be atomic with the caller's write.
pub(crate) fn bump_counter(conn: &Connection, key: &str, now_ms: i64) -> Result<(), StorageError> {
    let current = get_kv_i64(conn, key)?.unwrap_or(0);
    set_kv(conn, key, &(current + 1).to_string(), now_ms)
}

/// Current wall-clock time in milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
