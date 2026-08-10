//! The narrow, wire-agnostic transport seam.
//!
//! Consumers (e.g. `victron-service`) depend on [`BleTransport`] and receive
//! raw bytes tagged with characteristic identity only. Protocol parsing
//! (CBOR, VREGs) happens above this layer.

use async_trait::async_trait;

use crate::error::BleError;

/// Which VE.Smart characteristic produced a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotificationSource {
    /// `...0002` control/negotiation characteristic.
    Control,
    /// `...0003` last-data characteristic (final chunk of a frame).
    LastData,
    /// `...0004` data characteristic (chunk stream).
    Data,
}

/// One raw notification value, tagged with its source characteristic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// Source characteristic.
    pub source: NotificationSource,
    /// Raw payload bytes, unparsed.
    pub value: Vec<u8>,
}

/// Narrow BLE transport used by the collector state machine.
///
/// Implementations are Linux/BlueZ-specific; fakes implement this trait for
/// tests. The trait is `?Send`-friendly: a single-threaded collector does not
/// require the transport (or its notification streams) to be `Send`.
#[async_trait(?Send)]
pub trait BleTransport {
    /// Open the transport end to end: validate configuration, select adapter,
    /// ensure powered state, resolve the bonded device, connect, locate the
    /// VE.Smart GATT (validating notify/indicate on all three characteristics,
    /// read on Control, and write capability on the outbound Control/Data
    /// characteristics), and subscribe to notifications.
    ///
    /// Transactional: if connect succeeds but GATT locate or any subscription
    /// fails, already-started notifications are stopped and the local device
    /// is disconnected before returning. A failed open leaves the object
    /// closed and reusable with no stale session/adapter/device/GATT state.
    ///
    /// Idempotent: returns `Ok(())` when already open.
    async fn open(&mut self) -> Result<(), BleError>;

    /// Read the `Control` characteristic value.
    async fn read_control(&mut self) -> Result<Vec<u8>, BleError>;

    /// Write to the `Control` characteristic, bounded by the configured chunk
    /// size and using the write procedure validated during `open`.
    async fn write_control(&mut self, data: &[u8]) -> Result<(), BleError>;

    /// Write a non-final chunk to the `Data` characteristic, bounded by the
    /// configured chunk size and using the write procedure validated during
    /// `open`.
    async fn write_data(&mut self, data: &[u8]) -> Result<(), BleError>;

    /// Write the final (or only) chunk to the `LastData` characteristic.
    async fn write_last_data(&mut self, data: &[u8]) -> Result<(), BleError>;

    /// Wait for the next notification from any VE.Smart characteristic.
    ///
    /// Subject to the configured `notification_timeout`; on expiry returns
    /// [`BleError::Timeout`]. Returns [`BleError::NotificationStopped`] when
    /// the session ends.
    async fn next_notification(&mut self) -> Result<Notification, BleError>;

    /// Last known RSSI in dBm, when BlueZ has a fresh value.
    async fn rssi(&mut self) -> Result<Option<i16>, BleError>;

    /// Close cleanly: stop notifications, disconnect the device.
    async fn close(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Structural test: a fake transport can be constructed and driven
    /// without any BlueZ involvement (test seam contract).
    struct FakeTransport {
        control_read: Vec<u8>,
        written: Vec<(NotificationSource, Vec<u8>)>,
        notifications: Vec<Notification>,
        open: bool,
    }

    #[async_trait(?Send)]
    impl BleTransport for FakeTransport {
        async fn open(&mut self) -> Result<(), BleError> {
            self.open = true;
            Ok(())
        }
        async fn read_control(&mut self) -> Result<Vec<u8>, BleError> {
            Ok(self.control_read.clone())
        }
        async fn write_control(&mut self, data: &[u8]) -> Result<(), BleError> {
            self.written
                .push((NotificationSource::Control, data.to_vec()));
            Ok(())
        }
        async fn write_data(&mut self, data: &[u8]) -> Result<(), BleError> {
            self.written.push((NotificationSource::Data, data.to_vec()));
            Ok(())
        }
        async fn write_last_data(&mut self, data: &[u8]) -> Result<(), BleError> {
            self.written
                .push((NotificationSource::LastData, data.to_vec()));
            Ok(())
        }
        async fn next_notification(&mut self) -> Result<Notification, BleError> {
            if let Some(n) = self.notifications.pop() {
                Ok(n)
            } else {
                Err(BleError::NotificationStopped)
            }
        }
        async fn rssi(&mut self) -> Result<Option<i16>, BleError> {
            Ok(Some(-61))
        }
        async fn close(&mut self) {
            self.open = false;
        }
    }

    #[tokio::test]
    async fn fake_transport_drives_the_trait() {
        let mut t = FakeTransport {
            control_read: vec![0x00, 0x04],
            written: Vec::new(),
            notifications: vec![Notification {
                source: NotificationSource::LastData,
                value: vec![0x02, 0x9f],
            }],
            open: false,
        };
        assert!(!t.open);
        t.open().await.unwrap();
        assert!(t.open);
        assert_eq!(t.read_control().await.unwrap(), vec![0x00, 0x04]);
        t.write_control(&[0xfa, 0x80, 0xff]).await.unwrap();
        t.write_data(&[0x03, 0x03]).await.unwrap();
        t.write_last_data(&[0x05, 0x03]).await.unwrap();
        assert_eq!(t.written.len(), 3);
        assert_eq!(t.written[2].0, NotificationSource::LastData);
        let n = t.next_notification().await.unwrap();
        assert_eq!(n.source, NotificationSource::LastData);
        assert_eq!(n.value, vec![0x02, 0x9f]);
        t.close().await;
        assert!(!t.open);
    }
}
