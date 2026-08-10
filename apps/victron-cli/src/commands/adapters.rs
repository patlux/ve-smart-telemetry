//! `victron-cli adapters` — list BlueZ adapters and bonded devices.

use clap::Args;

use crate::CliError;

#[derive(Debug, Args)]
pub struct Adapters {
    /// BlueZ adapter to inspect (default: all).
    #[arg(long)]
    pub adapter: Option<String>,
}

impl Adapters {
    pub fn run(&self) -> Result<(), CliError> {
        let _ = self.adapter;
        Err(crate::not_wired(
            "requires victron-bluez (BlueZ D-Bus adapter enumeration via bluer); \
             bluez lane wiring pending. Expected adapter hci0 with bonded device \
             'Solar Charger' (pair once via bluetoothctl — PIN never stored here).",
        ))
    }
}
