//! `victron-cli`: diagnostic executable for the Victron VE.Smart BLE
//! collector.
//!
//! Command tree:
//!
//! ```text
//! victron-cli adapters
//! victron-cli discover --device <alias> [--adapter hci0] [--timeout-seconds 10]
//! victron-cli read-once --device <alias> [--instance 3] [--timeout-seconds 8]
//! victron-cli decode-fixture <path>
//! victron-cli render-metrics <fixture> [--device <name>] [--instance 3]
//! victron-cli check-victoriametrics [--url ...] [--timeout-ms 3000]
//! ```
//!
//! Commands that depend on sibling crates being built in parallel
//! (`victron-bluez`, `victron-protocol`, `victron-domain`, `victron-metrics`)
//! exit `3` with a precise "not wired" message instead of faking success.
//! `check-victoriametrics` is already real but is a **transport probe only**
//! (strict plaintext `http://` parsing + TCP connect; no import-path
//! validation until `victron-metrics` is integrated).

mod commands;
mod exit;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "victron-cli",
    version,
    about = "Diagnostics for the Victron VE.Smart BLE collector"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List BLE adapters (BlueZ). Not wired yet.
    Adapters(commands::adapters::Adapters),
    /// Discover the configured Victron device. Not wired yet.
    Discover(commands::discover::Discover),
    /// One-shot read from the device. Not wired yet.
    ReadOnce(commands::read_once::ReadOnce),
    /// Decode a captured notification fixture. Not wired yet.
    DecodeFixture(commands::decode_fixture::DecodeFixture),
    /// Render a fixture as Prometheus text. Not wired yet.
    RenderMetrics(commands::render_metrics::RenderMetrics),
    /// Probe VictoriaMetrics reachability (transport only).
    #[command(name = "check-victoriametrics")]
    CheckVictoriaMetrics(commands::check_victoriametrics::CheckVictoriaMetrics),
}

/// A CLI-level failure with a fixed exit code.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("not wired: {0}")]
    NotWired(&'static str),
    #[error("{0}")]
    Runtime(String),
}

impl CliError {
    pub fn exit_code(&self) -> u8 {
        match self {
            CliError::NotWired(_) => exit::NOT_WIRED,
            CliError::Runtime(_) => exit::RUNTIME,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result: Result<(), CliError> = match cli.command {
        Command::Adapters(c) => c.run(),
        Command::Discover(c) => c.run(),
        Command::ReadOnce(c) => c.run(),
        Command::DecodeFixture(c) => c.run(),
        Command::RenderMetrics(c) => c.run(),
        Command::CheckVictoriaMetrics(c) => c.run(),
    };
    match result {
        Ok(()) => ExitCode::from(exit::OK),
        Err(err) => {
            eprintln!("victron-cli: {err}");
            ExitCode::from(err.exit_code())
        }
    }
}

/// Convenience for NotWired errors with a consistent message shape.
pub fn not_wired(what: &'static str) -> CliError {
    CliError::NotWired(what)
}

/// Convenience for runtime failures.
pub fn runtime(msg: impl Into<String>) -> CliError {
    CliError::Runtime(msg.into())
}
