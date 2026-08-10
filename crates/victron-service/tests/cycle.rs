//! Cycle-level persistence contract tests: atomic all-or-nothing commits,
//! idempotent duplicate handling, pre-epoch rejection, delayed processing,
//! and truthful render context.

mod common;

use std::time::{Duration, SystemTime};

use common::*;
use victron_domain::{ConnectionHealth, Quality};
use victron_service::*;

/// Run exactly one cycle and return its outcome plus the post-cycle health
/// counters (the context is consumed by the cycle).
async fn one_cycle(h: &mut Harness) -> (CycleOutcome, HealthCounters) {
    let mut ctx = h.take_ctx();
    let outcome = run_cycle(&mut ctx).await;
    let health = ctx.health.clone();
    (outcome, health)
}

#[tokio::test(start_paused = true)]
async fn commit_failure_is_all_or_nothing() {
    let mut h = Harness::new(base_config());
    // Inject a storage failure for the commit: nothing may be applied.
    h.storage
        .lock()
        .unwrap()
        .commit_script
        .push_back(Err(StorageError::Io("injected".into())));
    let (outcome, _health) = one_cycle(&mut h).await;

    match outcome {
        CycleOutcome::Failure { phase, error } => {
            assert_eq!(phase, CyclePhase::Persisting);
            assert!(matches!(error, CycleError::Persist(StorageError::Io(_))));
        }
        other => panic!("expected persist failure, got {other:?}"),
    }
    // All-or-nothing: no energy, no identity, no batch.
    let s = h.storage.lock().unwrap();
    assert_eq!(s.energy, None, "energy must not advance on a failed commit");
    assert_eq!(s.last_success, None, "identity must not be recorded");
    assert_eq!(s.enqueues, 0, "no batch may be enqueued");
    assert_eq!(s.spool_depth(), 0);
}

#[tokio::test(start_paused = true)]
async fn duplicate_observed_at_is_idempotent_and_never_enqueues_twice() {
    let mut h = Harness::new(base_config());
    // The sample was already committed at FIXED_TS.
    h.storage.lock().unwrap().last_success = Some(t(FIXED_TS));
    h.storage.lock().unwrap().energy = Some(EnergyState {
        total_kwh: 1.0,
        last_power_watts: Some(150.0),
        last_sample_at: Some(t(FIXED_TS)),
    });
    let (outcome, _health) = one_cycle(&mut h).await;

    match outcome {
        CycleOutcome::Success(result) => {
            assert!(result.duplicate, "replay must be flagged as duplicate");
            assert_eq!(result.delivery, DeliveryStatus::Skipped);
        }
        other => panic!("expected success (idempotent replay), got {other:?}"),
    }
    // Nothing was rendered or enqueued; the durable state is untouched.
    assert_eq!(
        h.renderer.lock().unwrap().captured.len(),
        0,
        "no render for a duplicate"
    );
    let s = h.storage.lock().unwrap();
    assert_eq!(s.enqueues, 0, "no second batch");
    assert_eq!(s.last_success, Some(t(FIXED_TS)), "identity unchanged");
    assert_eq!(s.energy.as_ref().unwrap().total_kwh, 1.0, "no double-count");
}

#[tokio::test(start_paused = true)]
async fn pre_epoch_timestamp_is_rejected_at_the_seam() {
    let mut h = Harness::new(base_config());
    h.protocol.lock().unwrap().observed_at = SystemTime::UNIX_EPOCH - Duration::from_secs(1);
    let (outcome, _health) = one_cycle(&mut h).await;

    match outcome {
        CycleOutcome::Failure { phase, error } => {
            assert_eq!(phase, CyclePhase::Persisting);
            assert!(matches!(
                error,
                CycleError::Persist(StorageError::InvalidTimestamp(_))
            ));
        }
        other => panic!("expected pre-epoch rejection, got {other:?}"),
    }
    let s = h.storage.lock().unwrap();
    assert_eq!(s.enqueues, 0, "nothing persisted for a pre-epoch sample");
    assert_eq!(s.last_success, None);
}

#[tokio::test(start_paused = true)]
async fn delayed_processing_integrates_from_observed_at_not_wall_clock() {
    let mut h = Harness::new(base_config());
    // Durable anchor at t=0; the sample is observed at t=15 but only
    // processed at wall clock t=600. The integration interval must be the
    // 15 s between observed timestamps, not the 600 s processing delay.
    h.storage.lock().unwrap().energy = Some(EnergyState {
        total_kwh: 0.0,
        last_power_watts: Some(200.0),
        last_sample_at: Some(t(0)),
    });
    h.protocol.lock().unwrap().observed_at = t(15);
    h.clock.set(600);

    let (outcome, _health) = one_cycle(&mut h).await;
    match outcome {
        CycleOutcome::Success(result) => {
            assert_eq!(result.energy.kind, EnergyKind::Integrated);
            // (200 W + 150 W) / 2 * 15 s / 3_600_000
            let expected = 175.0 * 15.0 / 3_600_000.0;
            assert!(
                (result.resolved_yield_kwh - expected).abs() < 1e-12,
                "integrated over the observed 15 s gap, not the 600 s delay"
            );
        }
        other => panic!("expected integrated success, got {other:?}"),
    }
    let s = h.storage.lock().unwrap();
    assert_eq!(s.last_success, Some(t(15)), "identity is the observed time");
    assert_eq!(s.energy.as_ref().unwrap().last_sample_at, Some(t(15)));
}

#[tokio::test(start_paused = true)]
async fn render_context_reflects_the_current_success_and_projected_spool() {
    let mut h = Harness::new(base_config());
    // One batch already queued (oldest age 10 s at clock 600).
    h.storage.lock().unwrap().records.push(Record {
        id: 1,
        payload: b"old".to_vec(),
        attempts: 0,
        created_at: t(590),
        next_attempt_at: None,
        claim_deadline: None,
    });
    h.storage.lock().unwrap().next_id = 2;
    h.protocol.lock().unwrap().observed_at = t(595);
    h.protocol.lock().unwrap().connection_health = Some(ConnectionHealth::Up);
    h.protocol.lock().unwrap().pv_power_quality = Quality::ConfirmedNative;
    h.clock.set(600);

    let (outcome, _health) = one_cycle(&mut h).await;
    assert!(matches!(outcome, CycleOutcome::Success(_)), "{outcome:?}");

    let captured = h.renderer.lock().unwrap().captured.clone();
    assert_eq!(captured.len(), 1);
    let c = &captured[0];
    assert_eq!(c.device, "solar-charger");
    assert_eq!(c.observed_at, t(595));
    // The committed sample IS a success: last_success is projected to it.
    assert_eq!(c.last_success, Some(t(595)));
    // Sample age at render time (clock 600 - observed 595).
    assert_eq!(c.sample_age, Some(Duration::from_secs(5)));
    // BLE link state as actually known (Up), never synthesized.
    assert_eq!(c.ble_up, Some(true));
    assert_eq!(c.ble_rssi_dbm, None, "unknown RSSI stays unknown");
    // Spool health projected: 1 old batch + the newly enqueued one.
    assert_eq!(c.spool_depth, 2);
    assert_eq!(c.spool_oldest_age, Some(Duration::from_secs(10)));
    // The committed batch is durable; the old batch was delivered in-cycle,
    // so the spool holds exactly the new batch.
    let s = h.storage.lock().unwrap();
    assert_eq!(
        s.spool_depth(),
        1,
        "old batch delivered in-cycle; new batch queued"
    );
    assert_eq!(s.enqueues, 1);
    assert_eq!(h.delivery_calls.lock().unwrap().delivered.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn energy_gap_600_seconds_is_cumulative_seconds_in_health_and_render() {
    let mut h = Harness::new(base_config());
    // Anchor at t=0; the next sample is observed at t=600 -> gap 600 s
    // exceeds the 300 s maximum and is skipped, reported as seconds.
    h.storage.lock().unwrap().energy = Some(EnergyState {
        total_kwh: 1.0,
        last_power_watts: Some(200.0),
        last_sample_at: Some(t(0)),
    });
    h.protocol.lock().unwrap().observed_at = t(600);

    let (outcome, health) = one_cycle(&mut h).await;
    match outcome {
        CycleOutcome::Success(result) => {
            assert_eq!(result.energy.kind, EnergyKind::Skipped);
            assert_eq!(
                result.energy.skipped_gap_seconds,
                Some(Duration::from_secs(600))
            );
        }
        other => panic!("expected skipped-gap success, got {other:?}"),
    }

    // The render context carried the cumulative 600 s.
    let captured = h.renderer.lock().unwrap().captured.clone();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].energy_gap_skipped_seconds, 600);

    // The health counter is cumulative seconds, not events.
    assert_eq!(health.energy_gap_skipped_seconds(), 600);
    assert_eq!(health.snapshot().energy_gap_skipped_seconds, 600);
}

#[tokio::test(start_paused = true)]
async fn failed_commit_does_not_record_energy_gap_or_success() {
    let mut h = Harness::new(base_config());
    h.storage.lock().unwrap().energy = Some(EnergyState {
        total_kwh: 1.0,
        last_power_watts: Some(200.0),
        last_sample_at: Some(t(0)),
    });
    h.protocol.lock().unwrap().observed_at = t(600);
    // The commit fails after the gap was computed: neither the gap counter
    // nor the success may be recorded (the sample is not durable).
    h.storage
        .lock()
        .unwrap()
        .commit_script
        .push_back(Err(StorageError::Io("injected".into())));

    let (outcome, health) = one_cycle(&mut h).await;
    assert!(
        matches!(outcome, CycleOutcome::Failure { .. }),
        "{outcome:?}"
    );

    assert_eq!(
        health.energy_gap_skipped_seconds(),
        0,
        "a failed/uncommitted sample must not advance health"
    );
    assert_eq!(health.last_success(), None);
    assert_eq!(health.snapshot().cycles_succeeded, 0);
}
