//! `victron-cli read-once` — one acquisition cycle from the live device.

use clap::Args;

use crate::CliError;

#[derive(Debug, Args)]
pub struct ReadOnce {
    /// BlueZ alias of the bonded Victron device (e.g. `Solar Charger`).
    #[arg(long, value_name = "ALIAS")]
    pub device: String,

    /// VE.Smart instance (>= 1).
    #[arg(long, default_value_t = 3)]
    pub instance: u16,

    /// Response timeout in seconds.
    #[arg(long, default_value_t = 8)]
    pub timeout_seconds: u64,

    /// Print raw notification bytes (debug only; redacts nothing sensitive).
    #[arg(long)]
    pub raw: bool,
}

impl ReadOnce {
    pub fn run(&self) -> Result<(), CliError> {
        if self.instance == 0 {
            return Err(crate::runtime(
                "instance 0 is the keep-alive pseudo-instance",
            ));
        }
        let _ = (&self.device, self.timeout_seconds, self.raw);
        Err(crate::not_wired(
            "requires victron-bluez + victron-protocol + victron-domain \
             (session, negotiation fa80ff/f980, subscribe, getValues, VREG \
             decoding); bluez/protocol lane wiring pending.",
        ))
    }
}
