//! Solar-activity cadence tests: the runner switches active/idle intervals
//! from the last committed sample's confirmed PV power (active -> idle ->
//! active), with the first cycle using the active cadence.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use victron_service::*;

fn solar_config() -> ServiceConfig {
    ServiceConfig {
        solar_active_threshold_watts: 5.0,
        ..ServiceConfig::default()
    }
}

#[tokio::test(start_paused = true)]
async fn cadence_follows_committed_solar_activity_active_idle_active() {
    let mut h = Harness::new(solar_config());
    // Confirmed PV power per cycle: 150 W (active), 0 W (idle), 100 W
    // (active again).
    h.protocol
        .lock()
        .unwrap()
        .pv_power_script
        .extend([150.0, 0.0, 100.0]);
    let mut ctx = h.take_ctx();
    ctx.interval = Arc::new(SolarActivityPolicy::new(5.0));
    let summary = h
        .drive(async {
            let handle = h.spawn_runner(ctx);
            tokio::task::yield_now().await;

            // c0 immediate (first cycle uses the active cadence) -> 1 sample.
            assert_eq!(h.samples_persisted().await, 1);

            // Sample 1 was active (150 W): next wait is the 15 s active
            // interval.
            tokio::time::advance(Duration::from_secs(14)).await;
            tokio::task::yield_now().await;
            assert_eq!(
                h.samples_persisted().await,
                1,
                "still inside the active wait"
            );
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
            assert_eq!(
                h.samples_persisted().await,
                2,
                "active interval fired at 15 s"
            );

            // Sample 2 was idle (0 W): next wait is the 60 s idle interval.
            tokio::time::advance(Duration::from_secs(59)).await;
            tokio::task::yield_now().await;
            assert_eq!(h.samples_persisted().await, 2, "still inside the idle wait");
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
            assert_eq!(
                h.samples_persisted().await,
                3,
                "idle interval fired at 60 s"
            );

            // Sample 3 was active again (100 W): back to the 15 s cadence.
            tokio::time::advance(Duration::from_secs(14)).await;
            tokio::task::yield_now().await;
            assert_eq!(
                h.samples_persisted().await,
                3,
                "still inside the active wait"
            );
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
            assert_eq!(h.samples_persisted().await, 4, "active again at 15 s");

            h.shutdown_tx.send(true).unwrap();
            handle.await.expect("runner task panicked").expect("run ok")
        })
        .await;

    assert!(summary.graceful);
    assert_eq!(summary.cycles, 4);
    assert_eq!(summary.cycles_succeeded, 4);
    assert_eq!(summary.health.consecutive_failures, 0);
}

#[tokio::test(start_paused = true)]
async fn candidate_power_does_not_switch_to_active_cadence() {
    let mut h = Harness::new(solar_config());
    // The device reports 150 W but only as Candidate: not evidence of solar
    // activity, so the cadence must stay idle after the first cycle.
    h.protocol.lock().unwrap().pv_power_quality = Quality::Candidate;
    let mut ctx = h.take_ctx();
    ctx.interval = Arc::new(SolarActivityPolicy::new(5.0));
    let summary = h
        .drive(async {
            let handle = h.spawn_runner(ctx);
            tokio::task::yield_now().await;

            // c0 immediate -> 1 sample.
            assert_eq!(h.samples_persisted().await, 1);
            // Sample 1 was candidate: idle cadence (60 s) applies next.
            tokio::time::advance(Duration::from_secs(59)).await;
            tokio::task::yield_now().await;
            assert_eq!(h.samples_persisted().await, 1, "still inside the idle wait");
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
            assert_eq!(
                h.samples_persisted().await,
                2,
                "idle interval fired at 60 s"
            );

            h.shutdown_tx.send(true).unwrap();
            handle.await.expect("runner task panicked").expect("run ok")
        })
        .await;

    assert!(summary.graceful);
    assert_eq!(summary.cycles, 2);
}
