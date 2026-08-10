//! `victron-cli discover` — scan for the configured Victron device.

use clap::Args;

use crate::CliError;

#[derive(Debug, Args)]
pub struct Discover {
    /// BlueZ alias of the bonded Victron device (e.g. `Solar Charger`).
    #[arg(long, value_name = "ALIAS")]
    pub device: String,

    /// BlueZ adapter (default `hci0`).
    #[arg(long, default_value = "hci0")]
    pub adapter: String,

    /// Scan window in seconds.
    #[arg(long, default_value_t = 10)]
    pub timeout_seconds: u64,
}

impl Discover {
    pub fn run(&self) -> Result<(), CliError> {
        let _ = (&self.device, &self.adapter, self.timeout_seconds);
        Err(crate::not_wired(
            "requires victron-bluez (BLE scan matching VE.Smart service UUID \
             306b0001-...dfd0/dfd1 or manufacturer id 0x02e1); bluez lane wiring pending.",
        ))
    }
}
