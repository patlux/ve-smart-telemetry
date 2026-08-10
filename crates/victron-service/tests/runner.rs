//! Runner-level integration tests: cycle success/failure, backoff, shutdown
//! robustness and solar-activity cadence switching.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use victron_service::*;

#[tokio::test(start_paused = true)]
async fn success_cycle_persists_delivers_and_records_health() {
    let mut h = Harness::new(base_config());
    let ctx = h.take_ctx();
    let summary = h
        .drive(async {
            let handle = h.spawn_runner(ctx);

            // c0 immediate; then the runner sleeps active_interval (15 s) per
            // cycle.
            for _ in 0..2 {
                tokio::time::advance(Duration::from_secs(15)).await;
                tokio::task::yield_now().await;
            }

            {
                let b = h.ble_calls.lock().unwrap();
                assert_eq!(b.discover, 2);
                assert_eq!(b.connect, 2);
                assert_eq!(b.request, 2);
                assert_eq!(b.disconnect, 2);
                assert_eq!(b.frames_seen.len(), 2);
                assert_eq!(
                    b.frames_seen[0],
                    vec![vec![0xfa, 0x80, 0xff], vec![0xf9, 0x80]]
                );
                assert_eq!(
                    b.subscribe_payloads[0],
                    vec![0x03, 0x03, 0x00],
                    "subscribe CBOR carries instance 3"
                );
            }
            assert_eq!(h.samples_persisted().await, 2);
            {
                let s = h.storage.lock().unwrap();
                // The second cycle committed at FIXED_TS + 15 s (the fake
                // advances the observation time per cycle).
                assert_eq!(s.last_success, Some(t(FIXED_TS + 15)));
                let e = s.energy.clone().expect("energy state persisted");
                assert_eq!(e.last_power_watts, Some(150.0));
                assert_eq!(e.last_sample_at, Some(t(FIXED_TS + 15)));
            }
            assert_eq!(h.delivery_calls.lock().unwrap().delivered.len(), 2);

            h.shutdown_tx.send(true).unwrap();
            handle.await.expect("runner task panicked").expect("run ok")
        })
        .await;

    assert!(summary.graceful);
    assert_eq!(summary.cycles, 2);
    assert_eq!(summary.cycles_succeeded, 2);
    assert_eq!(summary.health.last_success, Some(t(FIXED_TS + 15)));
    assert_eq!(summary.health.deliveries_succeeded_total, 2);
}

#[tokio::test(start_paused = true)]
async fn idle_shutdown_hard_disconnects_a_retained_session() {
    let mut h = Harness::new(base_config());
    h.ble.lock().unwrap().retain_on_finish = true;
    let ctx = h.take_ctx();
    let summary = h
        .drive(async {
            let handle = h.spawn_runner(ctx);
            tokio::task::yield_now().await;

            assert_eq!(h.samples_persisted().await, 1);
            assert_eq!(
                h.ble_calls.lock().unwrap().disconnect,
                0,
                "successful cycle retained the healthy session"
            );

            h.shutdown_tx.send(true).unwrap();
            tokio::task::yield_now().await;
            handle.await.expect("runner task panicked").expect("run ok")
        })
        .await;

    assert!(summary.graceful);
    assert_eq!(summary.cycles_succeeded, 1);
    assert_eq!(
        h.ble_calls.lock().unwrap().disconnect,
        1,
        "shutdown from idle must hard-close a retained session"
    );
}

#[tokio::test(start_paused = true)]
async fn ble_timeout_fails_phase_then_backoff_and_recovers() {
    let mut h = Harness::new(base_config());
    // First request hangs (gate never released) and hits the phase timeout;
    // the gate is consumed once, so the second request proceeds normally.
    let (_release_tx, _entered_rx) = h.ble.lock().unwrap().install_request_gate();
    let ctx = h.take_ctx();
    let summary = h
        .drive(async {
            let handle = h.spawn_runner(ctx);
            tokio::task::yield_now().await;

            // c0: request hangs; phase_timeout (12 s) fires -> Failure(Request).
            tokio::time::advance(Duration::from_secs(12)).await;
            tokio::task::yield_now().await;
            assert_eq!(
                h.samples_persisted().await,
                0,
                "nothing persisted on timeout"
            );

            // backoff(1)=5 s + active interval 15 s = 20 s -> c1 succeeds.
            tokio::time::advance(Duration::from_secs(20)).await;
            tokio::task::yield_now().await;
            assert_eq!(h.samples_persisted().await, 1);

            h.shutdown_tx.send(true).unwrap();
            handle.await.expect("runner task panicked").expect("run ok")
        })
        .await;

    assert!(summary.graceful);
    assert_eq!(summary.cycles, 2);
    assert_eq!(summary.cycles_succeeded, 1);
    assert_eq!(summary.health.ble_session_failures_total, 1);
    assert_eq!(
        summary.health.consecutive_failures, 0,
        "reset after success"
    );
}

#[tokio::test(start_paused = true)]
async fn ble_contention_fails_then_backoff_and_recovers() {
    let mut h = Harness::new(base_config());
    h.ble
        .lock()
        .unwrap()
        .connect_script
        .push_back(Err(BleError::Contention));
    let ctx = h.take_ctx();
    let summary = h
        .drive(async {
            let handle = h.spawn_runner(ctx);
            tokio::task::yield_now().await;

            // c0 fails at Connect immediately, then sleeps interval +
            // backoff(1) = 20 s.
            tokio::time::advance(Duration::from_secs(20)).await;
            tokio::task::yield_now().await;

            assert_eq!(h.ble_calls.lock().unwrap().connect, 2);
            assert_eq!(h.ble_calls.lock().unwrap().request, 1);

            h.shutdown_tx.send(true).unwrap();
            handle.await.expect("runner task panicked").expect("run ok")
        })
        .await;

    assert_eq!(summary.cycles, 2);
    assert_eq!(summary.cycles_succeeded, 1);
    assert_eq!(summary.health.ble_connect_failures_total, 1);
}

#[tokio::test(start_paused = true)]
async fn malformed_response_aborts_without_persisting() {
    let mut h = Harness::new(base_config());
    h.protocol
        .lock()
        .unwrap()
        .parse_script
        .push_back(ParseBehavior::Err(ProtocolError::Malformed));
    let ctx = h.take_ctx();
    let summary = h
        .drive(async {
            let handle = h.spawn_runner(ctx);
            tokio::task::yield_now().await;

            // backoff + interval = 20 s -> second cycle parses fine.
            tokio::time::advance(Duration::from_secs(20)).await;
            tokio::task::yield_now().await;
            assert_eq!(
                h.samples_persisted().await,
                1,
                "only the valid cycle persisted"
            );

            h.shutdown_tx.send(true).unwrap();
            handle.await.expect("runner task panicked").expect("run ok")
        })
        .await;

    assert_eq!(summary.cycles, 2);
    assert_eq!(summary.cycles_succeeded, 1);
    assert_eq!(summary.health.protocol_errors_total, 1);
    assert_eq!(summary.health.samples_dropped_total, 0);
}

#[tokio::test(start_paused = true)]
async fn graceful_shutdown_between_acquisition_and_delivery_persists_sample() {
    let mut h = Harness::new(base_config());
    let (release_tx, mut entered_rx) = h.ble.lock().unwrap().install_request_gate();
    let ctx = h.take_ctx();
    let summary = h
        .drive(async {
            let handle = h.spawn_runner(ctx);
            // Wait until the runner is blocked inside request_values.
            entered_rx
                .recv()
                .await
                .expect("runner reached request phase");
            tokio::task::yield_now().await;

            // Shutdown arrives while the request is in flight.
            h.shutdown_tx.send(true).unwrap();
            // Release the request; the runner completes acquisition +
            // persistence, then stops before delivery.
            release_tx.send(()).unwrap();

            handle.await.expect("runner task panicked").expect("run ok")
        })
        .await;

    assert_eq!(
        h.samples_persisted().await,
        1,
        "acquired sample durably persisted"
    );
    assert_eq!(h.storage.lock().unwrap().last_success, Some(t(FIXED_TS)));
    assert_eq!(
        h.delivery_calls.lock().unwrap().delivered.len(),
        0,
        "delivery skipped on graceful shutdown"
    );
    assert!(
        h.ble_calls.lock().unwrap().disconnect >= 1,
        "session torn down"
    );
    assert!(summary.graceful);
    assert_eq!(summary.cycles, 1);
    assert_eq!(summary.cycles_succeeded, 1);
}

#[tokio::test(start_paused = true)]
async fn closed_shutdown_sender_exits_gracefully_without_busy_loop() {
    let mut h = Harness::new(base_config());
    let ctx = h.take_ctx();
    let shutdown_tx = h.detach_shutdown_sender();
    let summary = h
        .drive(async {
            let handle = h.spawn_runner(ctx);
            tokio::task::yield_now().await;
            // The sender disappears without ever sending `true`: the closed
            // channel must be treated as shutdown, not polled forever.
            drop(shutdown_tx);
            // Give the runner a moment to notice; it must exit on its own.
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
            handle.await.expect("runner task panicked").expect("run ok")
        })
        .await;

    assert!(summary.graceful, "closed channel must count as shutdown");
    assert_eq!(
        summary.cycles, 1,
        "no busy loop: exactly the first cycle ran"
    );
}

#[tokio::test(start_paused = true)]
async fn failed_cycle_tears_down_through_disconnecting_and_backoff() {
    let mut h = Harness::new(base_config());
    h.ble
        .lock()
        .unwrap()
        .connect_script
        .push_back(Err(BleError::Contention));
    let ctx = h.take_ctx();
    let summary = h
        .drive(async {
            let handle = h.spawn_runner(ctx);
            tokio::task::yield_now().await;
            // c0 fails at Connect; the teardown walks Disconnecting -> Backoff.
            tokio::time::advance(Duration::from_secs(20)).await;
            tokio::task::yield_now().await;
            h.shutdown_tx.send(true).unwrap();
            handle.await.expect("runner task panicked").expect("run ok")
        })
        .await;

    // The state machine reflected the best-effort disconnect.
    assert!(h.observer.contains(CyclePhase::Disconnecting));
    assert!(h.observer.contains(CyclePhase::Backoff));
    assert!(
        h.ble_calls.lock().unwrap().disconnect >= 1,
        "best-effort disconnect attempted after failure"
    );
    assert_eq!(summary.cycles, 2);
    assert_eq!(summary.cycles_succeeded, 1);
}

#[tokio::test(start_paused = true)]
async fn interval_switching_active_then_idle_changes_poll_cadence() {
    let mut h = Harness::new(base_config());
    let mut ctx = h.take_ctx();
    ctx.interval = Arc::new(SwitchPolicy {
        seq: Mutex::new(
            [IntervalKind::Active, IntervalKind::Idle]
                .into_iter()
                .collect(),
        ),
    });
    let summary = h
        .drive(async {
            let handle = h.spawn_runner(ctx);
            tokio::task::yield_now().await;

            // c0 immediate -> 1 sample. Next wait: Active = 15 s.
            assert_eq!(h.samples_persisted().await, 1);
            tokio::time::advance(Duration::from_secs(14)).await;
            tokio::task::yield_now().await;
            assert_eq!(
                h.samples_persisted().await,
                1,
                "still inside the 15 s active wait"
            );
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
            assert_eq!(
                h.samples_persisted().await,
                2,
                "active interval fired at 15 s"
            );

            // Next wait: Idle = 60 s.
            tokio::time::advance(Duration::from_secs(59)).await;
            tokio::task::yield_now().await;
            assert_eq!(
                h.samples_persisted().await,
                2,
                "still inside the 60 s idle wait"
            );
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
            assert_eq!(
                h.samples_persisted().await,
                3,
                "idle interval fired at 60 s"
            );

            h.shutdown_tx.send(true).unwrap();
            handle.await.expect("runner task panicked").expect("run ok")
        })
        .await;

    assert!(summary.graceful);
    assert_eq!(summary.cycles, 3);
    assert_eq!(summary.health.consecutive_failures, 0);
}
