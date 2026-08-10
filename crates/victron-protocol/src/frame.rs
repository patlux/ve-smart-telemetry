//! Bounded reassembly of Data/LastData notifications into complete CBOR
//! payloads, mirroring the receive logic of
//! `scripts/read-victron-history.py`:
//!
//! * notifications on the **Data** characteristic append bytes to the buffer;
//! * a notification on the **LastData** characteristic appends its bytes and
//!   then finalizes: the accumulated buffer is returned and cleared.
//!
//! In the observed captures the device sent complete streams directly on
//! LastData, so a caller that only handles complete single-frame streams may
//! call [`Reassembler::push_last_data`] for every LastData notification and
//! feed [`crate::response::Response::parse_stream`] the returned payload.

/// Default maximum reassembly buffer size (64 KiB).
pub const DEFAULT_MAX_BUFFER: usize = 64 * 1024;

/// Reassembly failure: a chunk would push the buffer past its capacity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReassemblyError {
    /// Configured capacity in bytes.
    pub capacity: usize,
    /// Total bytes that were needed (buffer + chunk).
    pub needed: usize,
}

impl core::fmt::Display for ReassemblyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "reassembly buffer limit: {} bytes needed, {} bytes capacity",
            self.needed, self.capacity
        )
    }
}

impl std::error::Error for ReassemblyError {}

/// Bounded incoming Data/LastData chunk buffer.
#[derive(Debug, Clone)]
pub struct Reassembler {
    buffer: Vec<u8>,
    max: usize,
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::new()
    }
}

impl Reassembler {
    /// Create a reassembler with [`DEFAULT_MAX_BUFFER`] capacity.
    pub fn new() -> Self {
        Self::with_max(DEFAULT_MAX_BUFFER)
    }

    /// Create a reassembler with an explicit capacity (must be ≥ 1).
    pub fn with_max(max: usize) -> Self {
        Reassembler {
            buffer: Vec::new(),
            max: max.max(1),
        }
    }

    /// Append a chunk received on the Data characteristic.
    pub fn push_data(&mut self, chunk: &[u8]) -> Result<(), ReassemblyError> {
        self.append(chunk)
    }

    /// Append a chunk received on the LastData characteristic and finalize.
    ///
    /// Returns the complete accumulated payload (buffer + `chunk`) and
    /// clears the reassembler. An empty `chunk` with a non-empty buffer
    /// still finalizes (matches the Python behavior).
    pub fn push_last_data(&mut self, chunk: &[u8]) -> Result<Option<Vec<u8>>, ReassemblyError> {
        self.append(chunk)?;
        Ok(Some(self.take()))
    }

    /// Take the accumulated payload and clear the buffer, regardless of
    /// whether a LastData has been seen.
    pub fn take(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.buffer)
    }

    /// Drop the accumulated payload without returning it.
    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    /// Bytes currently buffered between Data and LastData.
    pub fn pending_len(&self) -> usize {
        self.buffer.len()
    }

    /// Whether the buffer holds no bytes.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Configured capacity.
    pub fn capacity(&self) -> usize {
        self.max
    }

    fn append(&mut self, chunk: &[u8]) -> Result<(), ReassemblyError> {
        let needed = self.buffer.len().saturating_add(chunk.len());
        if needed > self.max {
            // Drop everything: a corrupted/oversized stream is not
            // recoverable, and keeping it would only waste memory.
            self.buffer.clear();
            return Err(ReassemblyError {
                capacity: self.max,
                needed,
            });
        }
        self.buffer.extend_from_slice(chunk);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_chunks_then_last_data_finalizes() {
        let mut r = Reassembler::new();
        r.push_data(&[0x01, 0x02]).unwrap();
        r.push_data(&[0x03]).unwrap();
        assert_eq!(r.pending_len(), 3);
        let payload = r.push_last_data(&[0x04, 0x05]).unwrap();
        assert_eq!(
            payload.as_deref(),
            Some(&[0x01, 0x02, 0x03, 0x04, 0x05][..])
        );
        assert!(r.is_empty());
    }

    #[test]
    fn single_last_data_complete_stream() {
        // The observed case: complete streams arrive directly on LastData.
        let mut r = Reassembler::new();
        let payload = r.push_last_data(&[0x08, 0x03]).unwrap();
        assert_eq!(payload.as_deref(), Some(&[0x08, 0x03][..]));
        assert!(r.is_empty());
    }

    #[test]
    fn last_data_empty_finalizes_pending() {
        let mut r = Reassembler::new();
        r.push_data(&[0xaa]).unwrap();
        let payload = r.push_last_data(&[]).unwrap();
        assert_eq!(payload.as_deref(), Some(&[0xaa][..]));
    }

    #[test]
    fn buffer_limit_enforced_and_resets() {
        let mut r = Reassembler::with_max(4);
        r.push_data(&[1, 2]).unwrap();
        let err = r.push_data(&[3, 4, 5]).unwrap_err();
        assert_eq!(
            err,
            ReassemblyError {
                capacity: 4,
                needed: 5
            }
        );
        assert!(r.is_empty(), "oversized stream must be dropped");
        // reassembler still usable afterwards
        r.push_data(&[9]).unwrap();
        assert_eq!(r.pending_len(), 1);
    }

    #[test]
    fn take_and_clear() {
        let mut r = Reassembler::new();
        r.push_data(&[1]).unwrap();
        assert_eq!(r.take(), vec![1]);
        r.push_data(&[2]).unwrap();
        r.clear();
        assert!(r.is_empty());
    }
}
