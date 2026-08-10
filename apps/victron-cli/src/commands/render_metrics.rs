//! `victron-cli render-metrics` — render a fixture as Prometheus text.

use std::path::PathBuf;

use clap::Args;

use crate::CliError;

#[derive(Debug, Args)]
pub struct RenderMetrics {
    /// Path to a decoded fixture (domain sample JSON).
    #[arg(value_name = "FIXTURE")]
    pub fixture: PathBuf,

    /// Device label for the `device` metric label.
    #[arg(long, default_value = "fixture")]
    pub device: String,

    /// VE.Smart instance.
    #[arg(long, default_value_t = 3)]
    pub instance: u16,
}

impl RenderMetrics {
    pub fn run(&self) -> Result<(), CliError> {
        let _ = (&self.fixture, &self.device, self.instance);
        Err(crate::not_wired(
            "requires victron-domain + victron-metrics (canonical sample + \
             Prometheus text with explicit timestamps); domain/metrics lane \
             wiring pending.",
        ))
    }
}
