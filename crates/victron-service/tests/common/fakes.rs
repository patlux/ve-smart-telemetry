//! BLE, protocol, delivery, and renderer fakes for service tests.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use tokio::sync::mpsc;
use victron_domain::{ChargerState, ConnectionHealth, DeviceId, Quality};
use victron_service::*;

use super::{t, FIXED_TS};

// ---------------------------------------------------------------------------
// BLE session fake
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct BleCalls {
    pub discover: u32,
    pub connect: u32,
    pub negotiate: u32,
    pub subscribe: u32,
    pub request: u32,
    pub disconnect: u32,
    pub frames_seen: Vec<Vec<Vec<u8>>>,
    pub subscribe_payloads: Vec<Vec<u8>>,
    pub request_payloads: Vec<Vec<u8>>,
}

/// Scripted BLE session. Scripts pop front; an empty script means success
/// (or the default fixture for `request_values`).
pub struct FakeBle {
    pub calls: Arc<Mutex<BleCalls>>,
    pub connect_script: VecDeque<Result<(), BleError>>,
    pub fixture: Vec<u8>,
    /// Model a concrete adapter that retains healthy sessions after success.
    pub retain_on_finish: bool,
    /// Signal "entered request_values" to the test.
    pub entered_tx: Option<mpsc::UnboundedSender<()>>,
    /// Receiver the test releases to let request_values finish.
    pub release_rx: Option<mpsc::UnboundedReceiver<()>>,
}

impl FakeBle {
    pub fn new(calls: Arc<Mutex<BleCalls>>) -> Self {
        Self {
            calls,
            connect_script: VecDeque::new(),
            fixture: b"fixture-bytes".to_vec(),
            retain_on_finish: false,
            entered_tx: None,
            release_rx: None,
        }
    }

    /// When set, request_values signals `entered_tx` then blocks until the
    /// test sends on the returned sender. The gate is consumed once; later
    /// calls proceed immediately.
    pub fn install_request_gate(
        &mut self,
    ) -> (mpsc::UnboundedSender<()>, mpsc::UnboundedReceiver<()>) {
        let (entered_tx, entered_rx) = mpsc::unbounded_channel();
        let (release_tx, release_rx) = mpsc::unbounded_channel();
        self.entered_tx = Some(entered_tx);
        self.release_rx = Some(release_rx);
        (release_tx, entered_rx)
    }
}

/// Local newtype so the foreign trait can be implemented (orphan rule).
pub struct SharedBle(pub Arc<Mutex<FakeBle>>);

#[async_trait(?Send)]
impl BleSession for SharedBle {
    async fn discover(&mut self) -> Result<(), BleError> {
        self.0.lock().unwrap().calls.lock().unwrap().discover += 1;
        Ok(())
    }

    async fn connect(&mut self) -> Result<(), BleError> {
        self.0.lock().unwrap().calls.lock().unwrap().connect += 1;
        let next = self.0.lock().unwrap().connect_script.pop_front();
        match next {
            Some(r) => r,
            None => Ok(()),
        }
    }

    async fn negotiate(&mut self, frames: &[Vec<u8>]) -> Result<(), BleError> {
        let fake = self.0.lock().unwrap();
        let mut c = fake.calls.lock().unwrap();
        c.negotiate += 1;
        c.frames_seen.push(frames.to_vec());
        Ok(())
    }

    async fn subscribe(&mut self, instance: u16, payload: &[u8]) -> Result<(), BleError> {
        let fake = self.0.lock().unwrap();
        let mut c = fake.calls.lock().unwrap();
        c.subscribe += 1;
        c.subscribe_payloads.push(payload.to_vec());
        assert_eq!(instance, 3, "expected configured instance");
        Ok(())
    }

    async fn request_values(
        &mut self,
        payload: &[u8],
        _timeout: Duration,
    ) -> Result<Vec<u8>, BleError> {
        {
            let fake = self.0.lock().unwrap();
            let mut c = fake.calls.lock().unwrap();
            c.request += 1;
            c.request_payloads.push(payload.to_vec());
            if let Some(tx) = &fake.entered_tx {
                let _ = tx.send(());
            }
        }
        // Take the gate receiver out, drop the lock, then wait.
        let gate = self.0.lock().unwrap().release_rx.take();
        if let Some(mut rx) = gate {
            let _ = rx.recv().await;
        }
        Ok(self.0.lock().unwrap().fixture.clone())
    }

    async fn finish_cycle(&mut self) -> Result<(), BleError> {
        if self.0.lock().unwrap().retain_on_finish {
            Ok(())
        } else {
            self.disconnect().await
        }
    }

    async fn disconnect(&mut self) -> Result<(), BleError> {
        self.0.lock().unwrap().calls.lock().unwrap().disconnect += 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Protocol fake (produces canonical domain samples)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseBehavior {
    Err(ProtocolError),
}

pub struct FakeProtocol {
    pub parse_script: VecDeque<ParseBehavior>,
    pub pv_power_watts: f64,
    pub pv_power_quality: Quality,
    pub pv_power_script: VecDeque<f64>,
    pub yield_total_kwh: Option<f64>,
    /// Observation time of the next produced sample; advanced by
    /// `observed_at_step` on every translate call so consecutive cycles get
    /// distinct timestamps (idempotent commits would otherwise treat the
    /// second cycle as a duplicate).
    pub observed_at: SystemTime,
    pub observed_at_step: Duration,
    pub connection_health: Option<ConnectionHealth>,
    pub charger_state: Option<ChargerState>,
}

impl FakeProtocol {
    pub fn new() -> Self {
        Self {
            parse_script: VecDeque::new(),
            pv_power_watts: 150.0,
            pv_power_quality: Quality::ConfirmedNative,
            pv_power_script: VecDeque::new(),
            yield_total_kwh: None,
            observed_at: t(FIXED_TS),
            observed_at_step: Duration::from_secs(15),
            connection_health: None,
            charger_state: Some(ChargerState::Bulk),
        }
    }
}

pub struct SharedProtocol(pub Arc<Mutex<FakeProtocol>>);

impl ProtocolAdapter for SharedProtocol {
    fn vregs(&self) -> &[u16] {
        &[0xedbb, 0xedbc]
    }

    fn acquire_plan(&self, instance: u16, _vregs: &[u16]) -> Result<AcquirePlan, ProtocolError> {
        let mut subscribe = vec![0x03];
        subscribe.extend_from_slice(&instance.to_le_bytes());
        Ok(AcquirePlan {
            negotiation_frames: vec![vec![0xfa, 0x80, 0xff], vec![0xf9, 0x80]],
            subscribe_payload: subscribe,
            values_payload: vec![0x05, 0x03, 0x00, 0x82, 0x19, 0xed, 0xbb, 0x19, 0xed, 0xbc],
        })
    }

    fn parse_response(
        &self,
        _instance: u16,
        _bytes: &[u8],
    ) -> Result<Vec<RawValue>, ProtocolError> {
        let next = self.0.lock().unwrap().parse_script.pop_front();
        match next {
            Some(ParseBehavior::Err(e)) => Err(e),
            None => Ok(vec![
                RawValue {
                    vreg: 0xedbc,
                    raw: vec![0x00],
                },
                RawValue {
                    vreg: 0xedbb,
                    raw: vec![0x00],
                },
            ]),
        }
    }

    fn translate(&self, _instance: u16, _values: &[RawValue]) -> Result<Sample, ProtocolError> {
        let mut fake = self.0.lock().unwrap();
        let power = fake
            .pv_power_script
            .pop_front()
            .unwrap_or(fake.pv_power_watts);
        let observed_at = fake.observed_at;
        fake.observed_at = observed_at + fake.observed_at_step;
        let device = DeviceId::new("solar-charger").expect("valid test device");
        let mut b = Sample::builder(device, observed_at)
            .pv_power_watts(power, fake.pv_power_quality)
            .expect("valid test power")
            .pv_voltage_volts(48.1, Quality::ConfirmedNative)
            .expect("valid test voltage")
            .battery_voltage_volts(13.2, Quality::Candidate)
            .expect("valid test battery voltage");
        if let Some(y) = fake.yield_total_kwh {
            b = b
                .yield_total_kwh(y, Quality::ConfirmedNative)
                .expect("valid test yield");
        }
        if let Some(h) = fake.connection_health {
            b = b.connection_health(h);
        }
        if let Some(s) = fake.charger_state {
            b = b.charger_state(s);
        }
        Ok(b.build())
    }
}

// ---------------------------------------------------------------------------
// Delivery fake
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct DeliveryCalls {
    pub delivered: Vec<Vec<u8>>,
    pub failures: u32,
}

pub struct FakeDelivery {
    pub script: VecDeque<Result<(), DeliveryError>>,
    pub calls: Arc<Mutex<DeliveryCalls>>,
}

impl FakeDelivery {
    pub fn new(calls: Arc<Mutex<DeliveryCalls>>) -> Self {
        Self {
            script: VecDeque::new(),
            calls,
        }
    }
}

pub struct SharedDelivery(pub Arc<Mutex<FakeDelivery>>);

#[async_trait]
impl MetricsDelivery for SharedDelivery {
    async fn deliver(&mut self, payload: &[u8]) -> Result<(), DeliveryError> {
        let behavior = self.0.lock().unwrap().script.pop_front();
        match behavior {
            Some(Ok(())) => {
                self.0
                    .lock()
                    .unwrap()
                    .calls
                    .lock()
                    .unwrap()
                    .delivered
                    .push(payload.to_vec());
                Ok(())
            }
            Some(Err(e)) => {
                self.0.lock().unwrap().calls.lock().unwrap().failures += 1;
                Err(e)
            }
            None => {
                self.0
                    .lock()
                    .unwrap()
                    .calls
                    .lock()
                    .unwrap()
                    .delivered
                    .push(payload.to_vec());
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Renderer fake (captures the render context for truthfulness assertions)
// ---------------------------------------------------------------------------

/// What the renderer saw for one batch.
#[derive(Debug, Clone)]
pub struct CapturedRender {
    pub device: String,
    pub observed_at: SystemTime,
    pub resolved_yield_kwh: f64,
    pub energy_kind: EnergyKind,
    pub ble_up: Option<bool>,
    pub ble_rssi_dbm: Option<i32>,
    pub last_success: Option<SystemTime>,
    pub sample_age: Option<Duration>,
    pub spool_depth: usize,
    pub spool_oldest_age: Option<Duration>,
    pub energy_gap_skipped_seconds: u64,
    pub health: HealthSnapshot,
}

pub struct FakeRenderer {
    pub captured: Vec<CapturedRender>,
    pub counter: u64,
}

impl FakeRenderer {
    pub fn new() -> Self {
        Self {
            captured: Vec::new(),
            counter: 0,
        }
    }
}

pub struct SharedRenderer(pub Arc<Mutex<FakeRenderer>>);

impl BatchRenderer for SharedRenderer {
    fn render(&self, ctx: &RenderContext<'_>) -> Result<Vec<u8>, RenderError> {
        let mut fake = self.0.lock().unwrap();
        fake.captured.push(CapturedRender {
            device: ctx.device.as_str().to_string(),
            observed_at: ctx.sample.observed_at(),
            resolved_yield_kwh: ctx.resolved_yield_kwh,
            energy_kind: ctx.energy_kind,
            ble_up: ctx.ble_up,
            ble_rssi_dbm: ctx.ble_rssi_dbm,
            last_success: ctx.last_success,
            sample_age: ctx.sample_age,
            spool_depth: ctx.spool_depth,
            spool_oldest_age: ctx.spool_oldest_age,
            energy_gap_skipped_seconds: ctx.energy_gap_skipped_seconds,
            health: ctx.health.clone(),
        });
        fake.counter += 1;
        Ok(format!("batch-{}", fake.counter).into_bytes())
    }
}
