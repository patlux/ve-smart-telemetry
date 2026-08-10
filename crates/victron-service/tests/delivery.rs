//! Delivery ownership + bounded retry integration tests (direct, no runner
//! loop): exclusive claims, TTL reclamation, exact attempts 1..max, permanent
//! drops, and the guarantee that a drop never increments a delivered counter.

mod common;

use std::time::Duration;

use common::*;
use victron_service::*;

#[tokio::test(start_paused = true)]
async fn spool_ownership_claim_is_exclusive_and_ttl_reclaims_after_crash() {
    let mut h = Harness::new(base_config());
    let mut ctx = h.take_ctx();
    ctx.ports
        .storage
        .commit_acquisition(AcquisitionCommit {
            device: DeviceId::new("solar-charger").unwrap(),
            observed_at: t(0),
            expected_energy: None,
            next_energy: EnergyState {
                total_kwh: 0.0,
                last_power_watts: None,
                last_sample_at: Some(t(0)),
            },
            payload: b"batch-A".to_vec(),
        })
        .await
        .unwrap();

    // Claim once: while claimed, claim_next must not hand it out again.
    let claim = ctx
        .ports
        .storage
        .spool_claim_next(Duration::from_secs(120), t(10))
        .await
        .unwrap()
        .expect("batch claimable");
    assert_eq!(claim.payload, b"batch-A");
    assert_eq!(
        claim.attempts, 1,
        "freshly enqueued batch is claimed as attempt 1"
    );
    assert!(
        ctx.ports
            .storage
            .spool_claim_next(Duration::from_secs(120), t(10))
            .await
            .unwrap()
            .is_none(),
        "unexpired claim must stay exclusive"
    );

    // Simulate a crash: the claim is abandoned and expires after the TTL.
    h.storage.lock().unwrap().expire_claims(t(10));
    let reclaimed = ctx
        .ports
        .storage
        .spool_claim_next(Duration::from_secs(120), t(200))
        .await
        .unwrap()
        .expect("expired claim is reclaimable");
    assert_eq!(reclaimed.id, claim.id);

    // Exactly-once: a single successful completion removes the batch.
    ctx.ports.storage.spool_complete(&reclaimed).await.unwrap();
    assert_eq!(h.spool_depth().await, 0);
    // Double completion of the same claim is a storage-level error.
    assert!(matches!(
        ctx.ports.storage.spool_complete(&reclaimed).await,
        Err(StorageError::Corrupt)
    ));
}

#[tokio::test(start_paused = true)]
async fn bounded_retries_drop_batch_after_max_attempts() {
    let mut h = Harness::new(base_config());
    let mut ctx = h.take_ctx();
    ctx.ports
        .storage
        .commit_acquisition(AcquisitionCommit {
            device: DeviceId::new("solar-charger").unwrap(),
            observed_at: t(0),
            expected_energy: None,
            next_energy: EnergyState {
                total_kwh: 0.0,
                last_power_watts: None,
                last_sample_at: Some(t(0)),
            },
            payload: b"batch-A".to_vec(),
        })
        .await
        .unwrap();
    for _ in 0..5 {
        h.delivery
            .lock()
            .unwrap()
            .script
            .push_back(Err(DeliveryError::Timeout));
    }

    // Attempts 1..4 fail and re-queue with deterministic deadlines; the
    // claimed batch reports the exact 1-based attempt.
    for attempt in 1..=4u32 {
        let claim = ctx
            .ports
            .storage
            .spool_claim_next(Duration::from_secs(120), h.clock.now())
            .await
            .unwrap()
            .expect("claim");
        assert_eq!(claim.attempts, attempt, "claim reports the current attempt");
        let status = deliver_claim(&mut ctx, claim).await;
        assert_eq!(
            status,
            DeliveryStatus::Queued { attempts: attempt },
            "attempt {attempt} re-queued"
        );
        h.clock.advance(h.storage.lock().unwrap().retry_delay);
    }
    // Attempt 5 fails; the current attempt reached spool_max_attempts=5 ->
    // drop.
    let claim = ctx
        .ports
        .storage
        .spool_claim_next(Duration::from_secs(120), h.clock.now())
        .await
        .unwrap()
        .expect("claim before drop");
    assert_eq!(claim.attempts, 5, "fifth attempt is the current attempt");
    let status = deliver_claim(&mut ctx, claim).await;
    assert_eq!(status, DeliveryStatus::Dropped { attempts: 5 });
    assert_eq!(h.spool_depth().await, 0);

    let s = ctx.health.snapshot();
    assert_eq!(s.deliveries_failed_total, 5);
    assert_eq!(s.deliveries_succeeded_total, 0);
    assert_eq!(s.spool_dropped_total, 1);
}

#[tokio::test(start_paused = true)]
async fn permanent_delivery_error_drops_immediately() {
    let mut h = Harness::new(base_config());
    let mut ctx = h.take_ctx();
    ctx.ports
        .storage
        .commit_acquisition(AcquisitionCommit {
            device: DeviceId::new("solar-charger").unwrap(),
            observed_at: t(0),
            expected_energy: None,
            next_energy: EnergyState {
                total_kwh: 0.0,
                last_power_watts: None,
                last_sample_at: Some(t(0)),
            },
            payload: b"batch-A".to_vec(),
        })
        .await
        .unwrap();
    h.delivery
        .lock()
        .unwrap()
        .script
        .push_back(Err(DeliveryError::Http { status: 400 }));

    let claim = ctx
        .ports
        .storage
        .spool_claim_next(Duration::from_secs(120), h.clock.now())
        .await
        .unwrap()
        .expect("claim");
    let status = deliver_claim(&mut ctx, claim).await;
    assert_eq!(status, DeliveryStatus::Dropped { attempts: 1 });
    assert_eq!(h.spool_depth().await, 0);
    assert_eq!(ctx.health.snapshot().spool_dropped_total, 1);
}

#[tokio::test(start_paused = true)]
async fn drop_never_increments_a_delivered_counter() {
    let mut h = Harness::new(base_config());
    let mut ctx = h.take_ctx();
    // Two batches: one permanently rejected, one delivered. The second
    // commit's expected anchor is the state left by the first commit.
    ctx.ports
        .storage
        .commit_acquisition(AcquisitionCommit {
            device: DeviceId::new("solar-charger").unwrap(),
            observed_at: t(0),
            expected_energy: None,
            next_energy: EnergyState {
                total_kwh: 0.0,
                last_power_watts: None,
                last_sample_at: Some(t(0)),
            },
            payload: b"batch-A".to_vec(),
        })
        .await
        .unwrap();
    let after_first = ctx.ports.storage.energy_state().await.unwrap();
    ctx.ports
        .storage
        .commit_acquisition(AcquisitionCommit {
            device: DeviceId::new("solar-charger").unwrap(),
            observed_at: t(1),
            expected_energy: after_first,
            next_energy: EnergyState {
                total_kwh: 0.0,
                last_power_watts: None,
                last_sample_at: Some(t(1)),
            },
            payload: b"batch-B".to_vec(),
        })
        .await
        .unwrap();
    h.delivery
        .lock()
        .unwrap()
        .script
        .push_back(Err(DeliveryError::Http { status: 400 }));

    // First claim: permanent 400 -> drop (never a delivery).
    let claim = ctx
        .ports
        .storage
        .spool_claim_next(Duration::from_secs(120), h.clock.now())
        .await
        .unwrap()
        .expect("claim");
    let status = deliver_claim(&mut ctx, claim).await;
    assert_eq!(status, DeliveryStatus::Dropped { attempts: 1 });

    // Second claim: delivered.
    let claim = ctx
        .ports
        .storage
        .spool_claim_next(Duration::from_secs(120), h.clock.now())
        .await
        .unwrap()
        .expect("claim");
    let status = deliver_claim(&mut ctx, claim).await;
    assert_eq!(status, DeliveryStatus::Delivered);

    let s = ctx.health.snapshot();
    assert_eq!(s.deliveries_succeeded_total, 1);
    assert_eq!(s.deliveries_failed_total, 1);
    assert_eq!(s.spool_dropped_total, 1);
    // The storage-level counters agree: one delivered, one dropped.
    let st = h.storage.lock().unwrap();
    assert_eq!(st.delivered, 1);
    assert_eq!(st.dropped, 1);
}

#[tokio::test(start_paused = true)]
async fn delivery_outage_replays_oldest_first_with_exactly_once_ownership() {
    let mut h = Harness::new(base_config());
    for _ in 0..2 {
        h.delivery
            .lock()
            .unwrap()
            .script
            .push_back(Err(DeliveryError::Timeout));
    }
    let ctx = h.take_ctx();
    let summary = h
        .drive(async {
            let handle = h.spawn_runner(ctx);
            tokio::task::yield_now().await;

            // c0 (immediate): batch-1 enqueued; in-cycle delivery fails ->
            // retry at now(0)+30 s. Runner sleeps 15 s, then c1 runs: batch-2
            // enqueued; its in-cycle delivery fails too (old batches are not
            // due yet: clock=0).
            tokio::time::advance(Duration::from_secs(15)).await;
            tokio::task::yield_now().await;
            assert_eq!(h.samples_persisted().await, 2, "c0 and c1 both persisted");
            assert_eq!(h.spool_depth().await, 2, "both batches waiting for retry");

            // c2 runs: batch-3 is delivered fresh (script ok), overtaking the
            // not-yet-due retries (freshness first); the two old batches stay
            // queued.
            tokio::time::advance(Duration::from_secs(15)).await;
            tokio::task::yield_now().await;
            assert_eq!(h.samples_persisted().await, 3);
            assert_eq!(h.spool_depth().await, 2);

            // Make both retry deadlines due; the next drain replays
            // oldest-first (batch-1 before batch-2).
            h.clock.set(60);
            tokio::time::advance(Duration::from_secs(15)).await;
            tokio::task::yield_now().await;
            assert_eq!(h.spool_depth().await, 0, "spool fully drained");

            let payloads: Vec<String> = h
                .delivery_calls
                .lock()
                .unwrap()
                .delivered
                .iter()
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect();
            // Fresh in-cycle delivery first (batch-3), then the replayed
            // retries in enqueue order (batch-1, batch-2), then the next
            // fresh batch (batch-4) from the cycle that runs at the end of
            // the final advance window.
            assert_eq!(payloads, vec!["batch-3", "batch-1", "batch-2", "batch-4"]);

            h.shutdown_tx.send(true).unwrap();
            handle.await.expect("runner task panicked").expect("run ok")
        })
        .await;

    assert!(summary.graceful);
    assert_eq!(summary.health.deliveries_failed_total, 2);
    assert_eq!(summary.health.deliveries_succeeded_total, 4);
    assert_eq!(summary.health.spool_dropped_total, 0);
    assert!(summary.cycles >= 3);
}
