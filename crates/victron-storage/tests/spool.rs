//! Spool tests: FIFO replay, lease/in-flight, retries, pruning, stats and
//! crash rollback.

mod common;

use common::{cfg, db_path, open_at, open_tmp, raw_conn, T0};

use victron_storage::{PruneStats, RetryOutcome, Storage, StorageConfig, StorageError};

#[test]
fn fifo_replay_returns_oldest_first() {
    let (_dir, storage) = open_tmp();
    let a = storage
        .enqueue_batch("d1", b"first".to_vec(), T0 + 3000)
        .unwrap();
    let b = storage
        .enqueue_batch("d1", b"second".to_vec(), T0 + 1000)
        .unwrap();
    let c = storage
        .enqueue_batch("d1", b"third".to_vec(), T0 + 2000)
        .unwrap();

    // Peek is non-mutating and shows the same batch repeatedly.
    let peek = storage.peek_oldest_batch(T0 + 10_000).unwrap().unwrap();
    assert_eq!(peek.id, b);
    assert_eq!(peek.created_at_ms, T0 + 1000);
    assert_eq!(peek.payload, b"second".to_vec());
    let again = storage.peek_oldest_batch(T0 + 10_000).unwrap().unwrap();
    assert_eq!(again.attempts, 0, "peek must not mutate");

    // Claim order follows (created_at_ms, id), regardless of enqueue order.
    let first = storage.claim_oldest_batch(T0 + 10_000).unwrap().unwrap();
    assert_eq!(first.id, b);
    let second = storage.claim_oldest_batch(T0 + 10_000).unwrap().unwrap();
    assert_eq!(second.id, c);
    let third = storage.claim_oldest_batch(T0 + 10_000).unwrap().unwrap();
    assert_eq!(third.id, a);
    assert!(storage.claim_oldest_batch(T0 + 10_000).unwrap().is_none());
}

#[test]
fn enqueue_validates_inputs() {
    let (_dir, storage) = open_tmp();
    assert!(matches!(
        storage.enqueue_batch("", b"x".to_vec(), T0),
        Err(StorageError::InvalidArgument(_))
    ));
    assert!(matches!(
        storage.enqueue_batch("d", b"".to_vec(), T0),
        Err(StorageError::InvalidArgument(_))
    ));
    assert!(matches!(
        storage.enqueue_batch("d", b"x".to_vec(), 0),
        Err(StorageError::InvalidArgument(_))
    ));
}

#[test]
fn empty_payload_is_rejected_by_code_and_constraint() {
    let (dir, storage) = open_tmp();
    // API-level validation.
    assert!(matches!(
        storage.enqueue_batch("d", b"".to_vec(), T0),
        Err(StorageError::InvalidArgument(_))
    ));
    // Database-level CHECK (bypassing the facade) also rejects it.
    let conn = raw_conn(&db_path(&dir));
    let err = conn
        .execute(
            "INSERT INTO spool_batch (device, created_at_ms, payload, next_attempt_at_ms)
             VALUES ('d', ?1, X'', ?1)",
            rusqlite::params![T0],
        )
        .unwrap_err();
    assert!(err.to_string().contains("CHECK"));
}

#[test]
fn claim_schedules_inflight_retry_and_blocks_reclaim_until_window() {
    let (_dir, storage) = open_tmp();
    let id = storage
        .enqueue_batch("d1", b"payload".to_vec(), T0)
        .unwrap();

    let claimed = storage.claim_oldest_batch(T0).unwrap().unwrap();
    assert_eq!(claimed.id, id);
    assert_eq!(claimed.attempts, 1);
    assert_eq!(claimed.next_attempt_at_ms, T0 + cfg().spool_inflight_ms);

    // Still inside the in-flight window: nothing is claimable.
    assert!(storage.claim_oldest_batch(T0 + 59_000).unwrap().is_none());
    assert_eq!(storage.spool_stats().unwrap().queued_batches, 1);

    // After the window (simulated crash without delivery) it is claimable
    // again with attempts bumped.
    let reclaimed = storage
        .claim_oldest_batch(T0 + cfg().spool_inflight_ms)
        .unwrap()
        .unwrap();
    assert_eq!(reclaimed.id, id);
    assert_eq!(reclaimed.attempts, 2);
}

#[test]
fn mark_delivered_removes_batch_and_counts() {
    let (_dir, storage) = open_tmp();
    let id = storage
        .enqueue_batch("d1", b"payload".to_vec(), T0)
        .unwrap();
    storage.claim_oldest_batch(T0).unwrap().unwrap();

    assert!(storage.mark_batch_delivered(id).unwrap());
    assert_eq!(storage.spool_stats().unwrap().queued_batches, 0);

    // Second delivery call for the same id is a no-op, not an error.
    assert!(!storage.mark_batch_delivered(id).unwrap());

    assert_eq!(
        storage.get_state_i64("spool.delivered_total").unwrap(),
        Some(1)
    );
}

#[test]
fn retry_uses_exponential_backoff_and_respects_next_attempt() {
    let (_dir, storage) = open_tmp();
    let id = storage
        .enqueue_batch("d1", b"payload".to_vec(), T0)
        .unwrap();
    storage.claim_oldest_batch(T0).unwrap().unwrap();

    // Attempt 1 -> base backoff.
    let outcome = storage.record_batch_retry(id, T0 + 1000).unwrap();
    let RetryOutcome::Retried {
        next_attempt_at_ms,
        attempts,
    } = outcome
    else {
        panic!("expected retry");
    };
    assert_eq!(attempts, 1);
    assert_eq!(next_attempt_at_ms, T0 + 1000 + cfg().spool_retry_base_ms);

    // Not claimable before its scheduled next attempt.
    assert!(storage
        .claim_oldest_batch(next_attempt_at_ms - 1)
        .unwrap()
        .is_none());

    // Claim again -> attempt 2, then retry doubles the backoff.
    let claimed = storage
        .claim_oldest_batch(next_attempt_at_ms)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.attempts, 2);
    let outcome = storage.record_batch_retry(id, next_attempt_at_ms).unwrap();
    let RetryOutcome::Retried {
        next_attempt_at_ms: next_after_second,
        ..
    } = outcome
    else {
        panic!("expected retry");
    };
    assert_eq!(
        next_after_second,
        next_attempt_at_ms + 2 * cfg().spool_retry_base_ms
    );
}

#[test]
fn retry_drops_batch_once_attempt_budget_is_exhausted() {
    let storage_cfg = StorageConfig {
        max_spool_attempts: 3,
        ..cfg()
    };
    let dir = tempfile::TempDir::new().unwrap();
    let storage = Storage::open(&db_path(&dir), storage_cfg).unwrap();

    let id = storage
        .enqueue_batch("d1", b"payload".to_vec(), T0)
        .unwrap();

    let mut now = T0;
    for expected_attempts in 1..=3u32 {
        let claimed = storage.claim_oldest_batch(now).unwrap().unwrap();
        assert_eq!(claimed.id, id);
        assert_eq!(claimed.attempts, expected_attempts);
        let outcome = storage.record_batch_retry(id, now).unwrap();
        match outcome {
            RetryOutcome::Retried {
                next_attempt_at_ms,
                attempts,
            } => {
                assert_eq!(attempts, expected_attempts);
                assert!(expected_attempts < 3);
                now = next_attempt_at_ms;
            }
            RetryOutcome::Dropped { attempts } => {
                assert_eq!(expected_attempts, 3);
                assert_eq!(attempts, 3);
            }
        }
    }

    assert_eq!(storage.spool_stats().unwrap().queued_batches, 0);
    assert_eq!(
        storage.get_state_i64("spool.dropped_total").unwrap(),
        Some(1)
    );
}

#[test]
fn explicit_drop_removes_batch_and_counts_only_drops() {
    let (_dir, store) = open_tmp();
    let id = store
        .enqueue_batch("dev", b"payload".to_vec(), 1_000)
        .unwrap();
    assert!(store.drop_batch(id).unwrap());
    assert!(!store.drop_batch(id).unwrap());
    assert_eq!(store.get_state_i64("spool.dropped_total").unwrap(), Some(1));
    assert_eq!(store.get_state_i64("spool.delivered_total").unwrap(), None);
    assert_eq!(store.spool_stats().unwrap().queued_batches, 0);
}

#[test]
fn retry_on_unknown_batch_is_an_error_and_leaves_counters_alone() {
    let (_dir, storage) = open_tmp();
    let err = storage.record_batch_retry(999, T0).unwrap_err();
    assert!(matches!(err, StorageError::Inconsistent(_)));
    assert_eq!(storage.get_state("spool.dropped_total").unwrap(), None);
}

#[test]
fn prune_keeps_newest_batches_by_count() {
    let storage_cfg = StorageConfig {
        max_spool_batches: 5,
        ..cfg()
    };
    let dir = tempfile::TempDir::new().unwrap();
    let storage = Storage::open(&db_path(&dir), storage_cfg).unwrap();

    for i in 0..100i64 {
        storage
            .enqueue_batch("d1", format!("batch-{i}").into_bytes(), T0 + i * 1000)
            .unwrap();
    }

    let stats = storage.prune_spool(T0 + 1_000_000).unwrap();
    assert_eq!(stats.removed, 95);
    assert_eq!(stats.remaining, 5);

    let kept = storage.spool_stats().unwrap();
    assert_eq!(kept.queued_batches, 5);
    // The newest five are kept: created_at 95_000..99_000.
    assert_eq!(kept.oldest_created_at_ms, Some(T0 + 95_000));
}

#[test]
fn prune_removes_batches_older_than_age_bound() {
    let storage_cfg = StorageConfig {
        max_spool_age_ms: 60_000,
        ..cfg()
    };
    let dir = tempfile::TempDir::new().unwrap();
    let storage = Storage::open(&db_path(&dir), storage_cfg).unwrap();

    storage.enqueue_batch("d1", b"old".to_vec(), T0).unwrap();
    storage
        .enqueue_batch("d1", b"new".to_vec(), T0 + 90_000)
        .unwrap();

    let stats = storage.prune_spool(T0 + 120_000).unwrap();
    assert_eq!(
        stats,
        PruneStats {
            removed: 1,
            remaining: 1
        }
    );
    let oldest = storage.spool_stats().unwrap().oldest_created_at_ms;
    assert_eq!(oldest, Some(T0 + 90_000));
}

#[test]
fn crash_between_claim_and_delivery_rolls_back_to_claimable_batch() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = db_path(&dir);

    let id;
    {
        let storage = open_at(&path);
        id = storage
            .enqueue_batch("d1", b"payload".to_vec(), T0)
            .unwrap();
        let claimed = storage.claim_oldest_batch(T0).unwrap().unwrap();
        assert_eq!(claimed.id, id);
        // Simulated crash: neither mark_delivered nor record_retry ran.
    }

    // After the in-flight window, the batch is claimable again with the
    // attempt counted; nothing was lost, nothing was double-deleted.
    let storage = open_at(&path);
    let reclaimed = storage
        .claim_oldest_batch(T0 + cfg().spool_inflight_ms)
        .unwrap()
        .unwrap();
    assert_eq!(reclaimed.id, id);
    assert_eq!(reclaimed.attempts, 2);
    assert_eq!(storage.spool_stats().unwrap().queued_batches, 1);
}

#[test]
fn spool_stats_reflect_depth_and_age() {
    let (_dir, storage) = open_tmp();
    let empty = storage.spool_stats().unwrap();
    assert_eq!(empty.queued_batches, 0);
    assert_eq!(empty.oldest_created_at_ms, None);
    assert_eq!(empty.total_attempts, 0);

    storage
        .enqueue_batch("d1", b"one".to_vec(), T0 + 10_000)
        .unwrap();
    storage
        .enqueue_batch("d1", b"two".to_vec(), T0 + 20_000)
        .unwrap();
    let stats = storage.spool_stats().unwrap();
    assert_eq!(stats.queued_batches, 2);
    assert_eq!(stats.oldest_created_at_ms, Some(T0 + 10_000));

    storage.claim_oldest_batch(T0 + 30_000).unwrap();
    let stats = storage.spool_stats().unwrap();
    assert_eq!(stats.total_attempts, 1);
}
