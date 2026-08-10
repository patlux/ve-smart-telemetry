//! `victron-cli read-once` — one read-only acquisition from the live device.

use std::time::Duration;

use clap::Args;
use serde_json::{json, Value};
use victron_protocol::{Request, Response, VregValue};

use super::common::{decoded_vreg_json, print_or_write_json, transport_config};
use crate::{runtime, CliError};

const PRIMARY_REGISTERS: &[u16] = &[
    0xedbb, 0xedbd, 0xedbc, 0xed8d, 0xed8c, 0x0201, 0xeda8, 0xedad, 0xedaa,
];
const FALLBACK_REGISTERS: &[u16] = &[0xed8f, 0xed8e];

#[derive(Debug, Args)]
pub struct ReadOnce {
    /// BlueZ alias of the bonded Victron device.
    #[arg(long, value_name = "ALIAS")]
    pub device: String,

    /// BlueZ adapter.
    #[arg(long, default_value = "hci0")]
    pub adapter: String,

    /// VE.Smart instance (>= 1).
    #[arg(long, default_value_t = 3)]
    pub instance: u16,

    /// Discovery timeout in seconds.
    #[arg(long, default_value_t = 12)]
    pub discovery_timeout_seconds: u64,

    /// Connect timeout in seconds.
    #[arg(long, default_value_t = 30)]
    pub connect_timeout_seconds: u64,

    /// Response timeout in seconds.
    #[arg(long, default_value_t = 10)]
    pub timeout_seconds: u64,

    /// Do not request legacy candidate fallback registers.
    #[arg(long)]
    pub no_fallbacks: bool,

    /// Include individual VREG raw bytes in JSON. Never includes BLE frames.
    #[arg(long)]
    pub raw: bool,
}

impl ReadOnce {
    pub async fn run(&self) -> Result<(), CliError> {
        if self.instance == 0 {
            return Err(runtime("instance 0 is the keep-alive pseudo-instance"));
        }
        let timeout = Duration::from_secs(self.timeout_seconds);
        let config = transport_config(
            &self.device,
            &self.adapter,
            Duration::from_secs(self.discovery_timeout_seconds),
            timeout,
            Duration::from_secs(self.connect_timeout_seconds),
        )?;
        let mut session = victron_client::VeSmartBleSession::new(config);
        let result = self.read(&mut session, timeout).await;
        session.close_read_only().await;
        let value = result?;
        print_or_write_json(&value, None)
    }

    async fn read(
        &self,
        session: &mut victron_client::VeSmartBleSession,
        timeout: Duration,
    ) -> Result<Value, CliError> {
        session
            .open_read_only()
            .await
            .map_err(|error| runtime(error.to_string()))?;
        session
            .subscribe_read_only(self.instance)
            .await
            .map_err(|error| runtime(error.to_string()))?;

        let mut registers = PRIMARY_REGISTERS.to_vec();
        if !self.no_fallbacks {
            registers.extend_from_slice(FALLBACK_REGISTERS);
        }
        let responses = session
            .request_read_only(
                &Request::GetValues {
                    instance: self.instance,
                    registers,
                },
                timeout,
            )
            .await
            .map_err(|error| runtime(error.to_string()))?;
        let rssi = session
            .rssi_read_only()
            .await
            .map_err(|error| runtime(error.to_string()))?;

        let rows = responses
            .into_iter()
            .filter_map(|response| match response {
                Response::Value { register, data, .. } => {
                    let decoded = VregValue::new(register, data.clone()).decode();
                    let mut value =
                        decoded_vreg_json(&decoded, self.raw.then_some(data.as_slice()));
                    if !self.raw {
                        value.as_object_mut()?.remove("raw");
                    }
                    Some(value)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        if rows.is_empty() {
            return Err(runtime("no matching VE.Smart values received"));
        }
        Ok(json!({
            "ok": true,
            "device": self.device,
            "instance": self.instance,
            "rssiDbm": rssi,
            "valueCount": rows.len(),
            "rows": rows,
        }))
    }
}
