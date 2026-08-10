//! Concrete [`BleTransport`] implementation over BlueZ D-Bus.

use std::pin::Pin;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bluer::gatt::remote::Characteristic;
use bluer::{Adapter, Device, Session};
use futures::stream::Stream;
use futures::StreamExt;

use crate::adapter::{self, PowerPolicy};
use crate::discovery::{self, DeviceSelector};
use crate::error::{from_bluer, BleError};
use crate::gatt::{self, VeSmartGatt};
use crate::timeout::bounded;
use crate::transport::{BleTransport, Notification, NotificationSource};

/// Configuration for the BlueZ transport.
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// Adapter name; `None` picks the default (`hci0`).
    pub adapter: Option<String>,
    /// Bonded Victron device identity. Must be non-empty (validated before
    /// any D-Bus action).
    pub selector: DeviceSelector,
    /// How to handle a powered-off adapter.
    pub power_policy: PowerPolicy,
    /// Deadline for `Device::connect`.
    pub connect_timeout: Duration,
    /// Deadline for the bounded discovery scan.
    pub discovery_timeout: Duration,
    /// Deadline for `next_notification`.
    pub notification_timeout: Duration,
    /// Single coherent deadline for every other BlueZ operation: adapter and
    /// device property reads, GATT service resolution, notify subscription,
    /// Control reads, writes, RSSI, disconnect/cleanup.
    pub operation_timeout: Duration,
    /// Maximum bytes per characteristic write (protocol layer chunks above).
    pub write_chunk_size: usize,
    /// Require Victron advertisement evidence before connecting to a
    /// selector-matched bonded device.
    pub require_advertisement_evidence: bool,
}

impl TransportConfig {
    /// Pure validation, run at the start of [`BleTransport::open`] before any
    /// D-Bus action.
    ///
    /// The [`Default`] instance is a convenience test/config seam: it carries
    /// an empty selector, so it must be configured before use. `validate`
    /// rejects it loudly instead of letting the transport connect to nothing.
    pub fn validate(&self) -> Result<(), BleError> {
        self.selector.validate()?;
        if self.connect_timeout.is_zero()
            || self.discovery_timeout.is_zero()
            || self.notification_timeout.is_zero()
            || self.operation_timeout.is_zero()
        {
            return Err(BleError::InvalidConfig {
                detail: "timeouts must be positive",
            });
        }
        if self.write_chunk_size == 0 {
            return Err(BleError::InvalidConfig {
                detail: "write_chunk_size must be positive",
            });
        }
        Ok(())
    }
}

impl Default for TransportConfig {
    fn default() -> Self {
        TransportConfig {
            adapter: Some(adapter::DEFAULT_ADAPTER.to_string()),
            selector: DeviceSelector {
                alias: None,
                address: None,
            },
            power_policy: PowerPolicy::RequireManual,
            connect_timeout: Duration::from_secs(15),
            discovery_timeout: Duration::from_secs(12),
            notification_timeout: Duration::from_secs(15),
            operation_timeout: Duration::from_secs(20),
            write_chunk_size: 20,
            require_advertisement_evidence: true,
        }
    }
}

/// Notification stream type produced by [`Characteristic::notify`].
type NotifyStream = Pin<Box<dyn Stream<Item = Vec<u8>> + Send>>;

/// Transactional open state.
///
/// Resources are only committed once every step of [`open`] succeeded;
/// [`SessionState::clear`] resets everything so a failed open leaves the
/// transport closed and reusable with no stale session/adapter/device/GATT
/// fields. Pure, unit-testable without BlueZ.
#[derive(Default)]
struct SessionState {
    session: Option<Session>,
    adapter: Option<Adapter>,
    device: Option<Device>,
    gatt: Option<VeSmartGatt>,
    connected_by_us: bool,
    open: bool,
}

impl SessionState {
    fn is_open(&self) -> bool {
        self.open
    }

    /// True when any BlueZ resource is held. Used to assert that a failed open
    /// leaves no stale session/adapter/device/GATT behind.
    fn has_resources(&self) -> bool {
        self.session.is_some()
            || self.adapter.is_some()
            || self.device.is_some()
            || self.gatt.is_some()
    }

    fn commit(&mut self, session: Session, adapter: Adapter, device: Device, gatt: VeSmartGatt) {
        self.session = Some(session);
        self.adapter = Some(adapter);
        self.device = Some(device);
        self.gatt = Some(gatt);
        self.connected_by_us = true;
        self.open = true;
    }

    fn clear(&mut self) {
        self.session = None;
        self.adapter = None;
        self.device = None;
        self.gatt = None;
        self.connected_by_us = false;
        self.open = false;
    }
}

/// Concrete BlueZ transport.
pub struct BluezTransport {
    config: TransportConfig,
    state: SessionState,
    control_notifications: Option<NotifyStream>,
    last_data_notifications: Option<NotifyStream>,
    data_notifications: Option<NotifyStream>,
}

impl BluezTransport {
    /// Create a transport from configuration. Does not touch the bus until
    /// [`open`](BleTransport::open).
    pub fn new(config: TransportConfig) -> Self {
        BluezTransport {
            config,
            state: SessionState::default(),
            control_notifications: None,
            last_data_notifications: None,
            data_notifications: None,
        }
    }

    fn require_open(&self) -> Result<(), BleError> {
        if self.state.is_open() {
            Ok(())
        } else {
            Err(BleError::InvalidState {
                operation: "transport",
            })
        }
    }

    fn op_timeout(&self) -> Duration {
        self.config.operation_timeout
    }

    async fn subscribe_one(
        &self,
        characteristic: &Characteristic,
    ) -> Result<NotifyStream, BleError> {
        let stream = bounded("notify-subscribe", self.op_timeout(), async {
            characteristic.notify().await.map_err(|e| from_bluer(&e))
        })
        .await?;
        Ok(Box::pin(stream))
    }

    async fn connect_with_retry(&self, device: &Device) -> Result<(), BleError> {
        let started_at = Instant::now();
        let deadline = started_at + self.config.connect_timeout;
        let mut attempt = 0u32;
        loop {
            attempt = attempt.saturating_add(1);
            let remaining =
                deadline
                    .checked_duration_since(Instant::now())
                    .ok_or(BleError::Timeout {
                        operation: "connect",
                    })?;
            match tokio::time::timeout(remaining, device.connect()).await {
                Ok(Ok(())) => {
                    tracing::debug!(
                        operation = "connect",
                        attempt,
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        "BLE connection established"
                    );
                    return Ok(());
                }
                Ok(Err(err)) if retryable_connect_kind(&err.kind) => {
                    tracing::debug!(
                        operation = "connect",
                        attempt,
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        error_class = ?from_bluer(&err).class(),
                        "transient BLE connection failure; retrying within deadline"
                    );
                    let _ = bounded("connect-retry-cancel", self.op_timeout(), async {
                        device.disconnect().await.map_err(|e| from_bluer(&e))
                    })
                    .await;
                    let remaining = deadline.checked_duration_since(Instant::now()).ok_or(
                        BleError::Timeout {
                            operation: "connect",
                        },
                    )?;
                    tokio::time::sleep(remaining.min(Duration::from_secs(1))).await;
                }
                Ok(Err(err)) => {
                    let error = from_bluer(&err);
                    tracing::debug!(
                        operation = "connect",
                        attempt,
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        error_class = ?error.class(),
                        "BLE connection failed without retry"
                    );
                    return Err(error);
                }
                Err(_) => {
                    let _ = bounded("connect-timeout-cancel", self.op_timeout(), async {
                        device.disconnect().await.map_err(|e| from_bluer(&e))
                    })
                    .await;
                    tracing::debug!(
                        operation = "connect",
                        attempt,
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        "BLE connection deadline expired"
                    );
                    return Err(BleError::Timeout {
                        operation: "connect",
                    });
                }
            }
        }
    }
}

fn retryable_connect_kind(kind: &bluer::ErrorKind) -> bool {
    matches!(
        kind,
        bluer::ErrorKind::ConnectionAttemptFailed
            | bluer::ErrorKind::Failed
            | bluer::ErrorKind::InProgress
            | bluer::ErrorKind::NotReady
    )
}

#[async_trait(?Send)]
impl BleTransport for BluezTransport {
    async fn open(&mut self) -> Result<(), BleError> {
        if self.state.is_open() {
            return Ok(());
        }
        // Validate before any D-Bus action.
        self.config.validate()?;

        let session = bounded("session-open", self.op_timeout(), async {
            Session::new().await.map_err(|e| from_bluer(&e))
        })
        .await?;
        let adapter =
            adapter::resolve_adapter(&session, self.config.adapter.as_deref(), self.op_timeout())
                .await?;
        adapter::ensure_powered(&adapter, self.config.power_policy, self.op_timeout()).await?;

        let device = discovery::resolve_device(
            &adapter,
            &self.config.selector,
            self.config.discovery_timeout,
            self.config.require_advertisement_evidence,
            self.op_timeout(),
        )
        .await?;

        let already_connected = bounded("device-connected", self.op_timeout(), async {
            device.is_connected().await.map_err(|e| from_bluer(&e))
        })
        .await?;
        if already_connected {
            return Err(BleError::Contention {
                detail: "already-connected",
            });
        }

        tracing::debug!(operation = "connect", "connecting to configured device");
        self.connect_with_retry(&device).await?;

        // Transactional tail: locate + subscribe must all succeed before any
        // resource is committed to `self.state`. On failure the local
        // notification streams are dropped (BlueZ StopNotify runs on drop)
        // and the device is disconnected; `self.state` stays empty so the
        // transport remains closed and reusable.
        let outcome: Result<(VeSmartGatt, NotifyStream, NotifyStream, NotifyStream), BleError> =
            async {
                let gatt = gatt::locate(&device, self.op_timeout()).await?;
                let control_notifications = self.subscribe_one(&gatt.control).await?;
                let last_data_notifications = self.subscribe_one(&gatt.last_data).await?;
                let data_notifications = self.subscribe_one(&gatt.data).await?;
                Ok((
                    gatt,
                    control_notifications,
                    last_data_notifications,
                    data_notifications,
                ))
            }
            .await;

        match outcome {
            Ok((gatt, control_notifications, last_data_notifications, data_notifications)) => {
                self.state.commit(session, adapter, device, gatt);
                self.control_notifications = Some(control_notifications);
                self.last_data_notifications = Some(last_data_notifications);
                self.data_notifications = Some(data_notifications);
                tracing::debug!(operation = "transport-open", "BLE transport open");
                Ok(())
            }
            Err(err) => {
                tracing::debug!(
                    operation = "transport-open",
                    error_class = ?err.class(),
                    "open failed; rolling back BLE connection"
                );
                let _ = bounded("disconnect-rollback", self.op_timeout(), async {
                    device.disconnect().await.map_err(|e| from_bluer(&e))
                })
                .await;
                // `session`, `adapter`, `device`, `gatt` and the notification
                // streams were never committed; dropping them here releases
                // the bus resources. `self.state` remains empty and `open`
                // false, so the object is closed and reusable.
                debug_assert!(
                    !self.state.has_resources(),
                    "failed open must not leave stale session/adapter/device/gatt state"
                );
                Err(err)
            }
        }
    }

    async fn read_control(&mut self) -> Result<Vec<u8>, BleError> {
        self.require_open()?;
        let control = &self.state.gatt.as_ref().expect("open").control;
        bounded("control-read", self.op_timeout(), async {
            control.read().await.map_err(|e| from_bluer(&e))
        })
        .await
    }

    async fn write_control(&mut self, data: &[u8]) -> Result<(), BleError> {
        self.require_open()?;
        let gatt = self.state.gatt.as_ref().expect("open");
        let write = gatt::write_bounded(
            &gatt.control,
            gatt.control_write,
            data,
            self.config.write_chunk_size,
        );
        bounded("control-write", self.op_timeout(), write).await
    }

    async fn write_data(&mut self, data: &[u8]) -> Result<(), BleError> {
        self.require_open()?;
        let gatt = self.state.gatt.as_ref().expect("open");
        let write = gatt::write_bounded(
            &gatt.data,
            gatt.data_write,
            data,
            self.config.write_chunk_size,
        );
        bounded("data-write", self.op_timeout(), write).await
    }

    async fn write_last_data(&mut self, data: &[u8]) -> Result<(), BleError> {
        self.require_open()?;
        let gatt = self.state.gatt.as_ref().expect("open");
        let write = gatt::write_bounded(
            &gatt.last_data,
            gatt.last_data_write,
            data,
            self.config.write_chunk_size,
        );
        bounded("last-data-write", self.op_timeout(), write).await
    }

    async fn next_notification(&mut self) -> Result<Notification, BleError> {
        self.require_open()?;
        let control = self.control_notifications.as_mut().expect("open");
        let last_data = self.last_data_notifications.as_mut().expect("open");
        let data = self.data_notifications.as_mut().expect("open");

        let received = tokio::time::timeout(self.config.notification_timeout, async {
            tokio::select! {
                value = control.next() => value.map(|value| Notification { source: NotificationSource::Control, value }),
                value = last_data.next() => value.map(|value| Notification { source: NotificationSource::LastData, value }),
                value = data.next() => value.map(|value| Notification { source: NotificationSource::Data, value }),
            }
        })
        .await;

        match received {
            Ok(Some(notification)) => Ok(notification),
            Ok(None) => Err(BleError::NotificationStopped),
            Err(_) => Err(BleError::Timeout {
                operation: "notification",
            }),
        }
    }

    async fn rssi(&mut self) -> Result<Option<i16>, BleError> {
        self.require_open()?;
        let device = self.state.device.as_ref().expect("open");
        bounded("rssi", self.op_timeout(), async {
            device.rssi().await.map_err(|e| from_bluer(&e))
        })
        .await
    }

    async fn close(&mut self) {
        // Dropping the streams stops the notification sessions (BlueZ
        // StopNotify runs on drop of the bluer stream).
        self.control_notifications = None;
        self.last_data_notifications = None;
        self.data_notifications = None;
        let connected_by_us = self.state.connected_by_us;
        if let Some(device) = self.state.device.take().filter(|_| connected_by_us) {
            let result = bounded("disconnect", self.op_timeout(), async {
                device.disconnect().await.map_err(|e| from_bluer(&e))
            })
            .await;
            if let Err(err) = result {
                tracing::debug!(
                    operation = "disconnect",
                    error_class = ?err.class(),
                    "disconnect failed during close; continuing cleanup"
                );
            }
        }
        self.state.clear();
        tracing::debug!(operation = "transport-close", "BLE transport closed");
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
