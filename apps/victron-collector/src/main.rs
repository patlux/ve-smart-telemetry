//! `victron-collector`: production daemon for the Victron VE.Smart BLE
//! collector.
//!
//! This binary only wires: parse validated TOML, initialise logging, assemble
//! concrete adapters, start the `victron-service` runner, handle SIGTERM/
//! SIGINT. **No business logic lives here.**
//!
//! Concrete sibling adapters (`victron-bluez`, `victron-protocol`,
//! `victron-domain`, `victron-storage`, `victron-metrics`) are not wired yet;
//! every cycle fails fast with a precise `NotWired` error and the runner
//! backs off. `--check-config` validates configuration without running.
//!
//! Shutdown signal-handler installation failure is a startup error: the
//! daemon never runs without a working shutdown path.

mod adapters;
mod config;
mod logging;
mod shutdown;
mod watchdog;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use tokio::sync::watch;
use victron_service::{
    run, run_cycle, CycleContext, CycleOutcome, CyclePorts, PhaseObserver, RunError,
};

/// Exit codes shared with `victron-cli`.
pub mod exit {
    pub const OK: u8 = 0;
    pub const RUNTIME: u8 = 1;
    pub const CONFIG: u8 = 2;
    pub const NOT_WIRED: u8 = 3;
}

#[derive(Debug, Parser)]
#[command(
    name = "victron-collector",
    version,
    about = "Victron VE.Smart BLE collector daemon (push to VictoriaMetrics)"
)]
struct Cli {
    /// Path to the validated TOML configuration.
    #[arg(
        long,
        value_name = "PATH",
        default_value = "/etc/victron-collector/config.toml"
    )]
    config: PathBuf,

    /// Validate configuration and exit without running.
    #[arg(long)]
    check_config: bool,

    /// Run exactly one acquisition cycle and exit (diagnostics).
    #[arg(long)]
    run_once: bool,

    /// Default log level when RUST_LOG is unset.
    #[arg(long, default_value = "info")]
    log_level: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Err(error) = logging::init(&cli.log_level) {
        // Logging cannot report its own initialization failure. stderr is
        // still captured by systemd/journald and by diagnostic CLI callers.
        eprintln!("victron-collector: logging setup error: {error}");
        return ExitCode::from(exit::RUNTIME);
    }
    match run_daemon(cli).await {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            let exit_code = error.exit_code();
            tracing::error!(%error, exit_code, "collector terminated");
            ExitCode::from(exit_code)
        }
    }
}

enum DaemonError {
    Config(config::ConfigError),
    Shutdown(shutdown::ShutdownError),
    Watchdog(std::io::Error),
    Run(victron_service::CycleError),
    RunLoop(RunError),
}

impl DaemonError {
    fn exit_code(&self) -> u8 {
        match self {
            DaemonError::Config(_) => exit::CONFIG,
            DaemonError::Shutdown(_) | DaemonError::Watchdog(_) | DaemonError::RunLoop(_) => {
                exit::RUNTIME
            }
            DaemonError::Run(e) => match e {
                // A cycle failing purely because wiring is pending is a
                // NotWired condition for scripting/CI.
                victron_service::CycleError::Plan(victron_service::ProtocolError::NotWired(_))
                | victron_service::CycleError::Discover(victron_service::BleError::NotWired(_))
                | victron_service::CycleError::Connect(victron_service::BleError::NotWired(_))
                | victron_service::CycleError::Negotiate(victron_service::BleError::NotWired(_))
                | victron_service::CycleError::Subscribe(victron_service::BleError::NotWired(_))
                | victron_service::CycleError::Request(victron_service::BleError::NotWired(_))
                | victron_service::CycleError::Disconnect(victron_service::BleError::NotWired(_)) => {
                    exit::NOT_WIRED
                }
                _ => exit::RUNTIME,
            },
        }
    }
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DaemonError::Config(e) => write!(f, "configuration error: {e}"),
            DaemonError::Shutdown(e) => write!(f, "shutdown setup error: {e}"),
            DaemonError::Watchdog(e) => write!(f, "systemd watchdog setup error: {e}"),
            DaemonError::Run(e) => write!(f, "cycle error: {e}"),
            DaemonError::RunLoop(e) => write!(f, "run loop error: {e}"),
        }
    }
}

async fn run_daemon(cli: Cli) -> Result<u8, DaemonError> {
    let cfg = config::Config::load(&cli.config).map_err(DaemonError::Config)?;

    if cli.check_config {
        println!(
            "config OK: device={} instance={} url={}",
            cfg.device.name, cfg.device.instance, cfg.victoria_metrics.url
        );
        return Ok(exit::OK);
    }

    let service_config = cfg
        .service_config()
        .map_err(|e| DaemonError::Config(config::ConfigError::Service(e)))?;

    // --- Concrete adapters ---
    let selector = victron_bluez::discovery::DeviceSelector::new(
        Some(cfg.device.bluez_alias.clone()),
        None,
    )
    .map_err(|error| DaemonError::Config(config::ConfigError::Bluetooth(error.to_string())))?;
    let ble_config = victron_bluez::TransportConfig {
        adapter: Some(cfg.device.adapter.clone()),
        selector,
        power_policy: victron_bluez::adapter::PowerPolicy::RequireManual,
        // The charger advertises a connectable slot much less frequently
        // than its non-connectable telemetry beacons. Reserve one response
        // window for deterministic cancellation/cleanup before the service's
        // outer phase deadline can drop the open future.
        connect_timeout: service_config
            .phase_timeout
            .saturating_sub(service_config.response_timeout),
        discovery_timeout: service_config.phase_timeout,
        notification_timeout: service_config.response_timeout,
        // Keep individual D-Bus calls strictly below the service's outer
        // phase deadline. Equal inner/outer deadlines hide the concrete
        // operation that stalled because the outer phase timeout wins first.
        operation_timeout: service_config.response_timeout,
        write_chunk_size: victron_protocol::control::MIN_ATT_CHUNK_SIZE,
        require_advertisement_evidence: false,
    };
    let ble = victron_client::VeSmartBleSession::new(ble_config);
    let protocol = adapters::protocol::VeSmartProtocol::new(
        victron_domain::DeviceId::new(&cfg.device.name).map_err(|error| {
            DaemonError::Config(config::ConfigError::DeviceIdentity(error.to_string()))
        })?,
    );
    let storage_config = victron_storage::StorageConfig {
        max_spool_attempts: cfg.poll.spool_max_attempts,
        spool_inflight_ms: duration_ms(service_config.spool_claim_ttl),
        max_spool_batches: cfg.storage.maximum_spool_batches,
        max_spool_age_ms: days_ms(cfg.storage.maximum_spool_age_days),
        energy_gap_threshold_ms: duration_ms(service_config.maximum_energy_gap),
        ..victron_storage::StorageConfig::default()
    };
    let storage = adapters::storage::SqliteStorage::open(
        &cfg.storage.path,
        cfg.device.name.clone(),
        storage_config,
    )
    .map_err(|e| DaemonError::Run(victron_service::CycleError::Persist(e)))?;
    let delivery = adapters::delivery::VictoriaMetricsDelivery::new(
        &cfg.victoria_metrics.url,
        std::time::Duration::from_secs(cfg.victoria_metrics.request_timeout_seconds),
    )
    .map_err(|e| DaemonError::Config(config::ConfigError::Delivery(e.to_string())))?;
    let renderer = adapters::delivery::PrometheusRenderer;
    let clock = Arc::new(adapters::SystemClock);
    let watchdog_observer = watchdog::ProgressObserver::new(
        service_config.phase_timeout,
        service_config
            .idle_interval
            .saturating_add(service_config.backoff_cap)
            .saturating_add(service_config.phase_timeout),
        std::time::Duration::from_secs(cfg.victoria_metrics.request_timeout_seconds),
    );
    let observer: Arc<dyn PhaseObserver> = watchdog_observer.clone();
    let interval = Arc::new(adapters::SolarActivityPolicy::new(
        cfg.poll.solar_active_threshold_watts,
    ));

    let ports = CyclePorts::new(
        Box::new(ble),
        Arc::new(protocol),
        Box::new(storage),
        Box::new(delivery),
        Arc::new(renderer),
    );
    let backoff = Arc::new(service_config.backoff());

    tracing::info!(
        device = %service_config.device_name,
        instance = service_config.instance,
        adapter = %cfg.device.adapter,
        phase_timeout_ms = service_config.phase_timeout.as_millis() as u64,
        response_timeout_ms = service_config.response_timeout.as_millis() as u64,
        "starting VE.Smart Telemetry collector"
    );

    if cli.run_once {
        let mut ctx = CycleContext::new(
            service_config,
            ports,
            clock,
            watch::channel(false).1,
            observer,
            backoff,
            interval,
        );
        match run_cycle(&mut ctx).await {
            CycleOutcome::Success(result) => {
                println!(
                    "cycle OK: sample={} samples_persisted delivery={:?}",
                    result
                        .sample
                        .observed_at()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                    result.delivery
                );
                Ok(exit::OK)
            }
            CycleOutcome::Failure { phase: _, error } => Err(DaemonError::Run(error)),
            CycleOutcome::ShutdownGraceful { .. } => Ok(exit::OK),
        }
    } else {
        let shutdown_rx = shutdown::install().await.map_err(DaemonError::Shutdown)?;
        watchdog::start(watchdog_observer, shutdown_rx.clone()).map_err(DaemonError::Watchdog)?;
        let ctx = CycleContext::new(
            service_config,
            ports,
            clock,
            shutdown_rx,
            observer,
            backoff,
            interval,
        );
        let summary = run(ctx).await.map_err(DaemonError::RunLoop)?;
        watchdog::stopping();
        tracing::info!(
            cycles = summary.cycles,
            cycles_succeeded = summary.cycles_succeeded,
            graceful = summary.graceful,
            "collector stopped"
        );
        if summary.graceful {
            Ok(exit::OK)
        } else {
            Ok(exit::RUNTIME)
        }
    }
}

fn duration_ms(duration: std::time::Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn days_ms(days: u64) -> i64 {
    let millis = days.saturating_mul(24 * 60 * 60 * 1_000);
    i64::try_from(millis).unwrap_or(i64::MAX)
}
