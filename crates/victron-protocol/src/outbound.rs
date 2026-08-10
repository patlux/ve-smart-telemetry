//! Outbound chunking: splitting an encoded request into typed Data/LastData
//! chunks for the VE.Smart characteristics.
//!
//! Evidence (see `analysis/victronconnect-protocol-reference.md` §6.3 and
//! the captured live-reader fixtures now exposed through `victron-cli read-once`):
//!
//! * the proven live reader writes **single-frame** read requests directly
//!   to the LastData characteristic (`...0003`, `DATA1_UUID` in the script);
//! * `writeChunkToStack()` in the app splits queued outbound data by the
//!   negotiated chunk size and writes through the two data characteristics;
//!   the native diagnostics distinguish `Writing to data:` from
//!   `Writing to lastData:`, and the inbound side treats LastData as the
//!   finalizing chunk.
//!
//! The provably safe outbound contract implemented here: a payload that fits
//! in one chunk goes to **LastData** (the observed single-frame pattern);
//! a payload that needs several chunks sends every chunk except the last to
//! **Data** and the final chunk to **LastData**. The exact alternation rule
//! for multi-chunk writes is still pending live confirmation (see the
//! unresolved-items note in the crate docs); this API only commits to the
//! final-chunk rule, which is the part the inbound reassembler depends on.
//!
//! The API is pure and transport-agnostic: it returns typed chunks that a
//! BlueZ/service layer maps onto `ServiceVariant::data_uuid()` /
//! `last_data_uuid()`.

use crate::ProtocolError;

/// Which VE.Smart characteristic an outbound chunk is written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundTarget {
    /// The Data characteristic (`...0004`): a non-final chunk.
    Data,
    /// The LastData characteristic (`...0003`): the final chunk (and the
    /// only chunk of a single-frame request).
    LastData,
}

/// One outbound chunk with its target characteristic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundChunk {
    /// Target characteristic for this chunk.
    pub target: OutboundTarget,
    /// Chunk bytes (never empty, never longer than the negotiated size).
    pub bytes: Vec<u8>,
}

/// Split an encoded request payload into typed outbound chunks.
///
/// * `payload` must be non-empty (an empty request is rejected).
/// * `chunk_size` must be ≥ 1 (zero is rejected). Callers normally pass the
///   negotiated chunk size clamped to at least
///   [`crate::control::MIN_ATT_CHUNK_SIZE`] (20 bytes).
/// * A payload of `chunk_size` bytes or fewer yields exactly one chunk
///   targeting [`OutboundTarget::LastData`] (the observed single-frame
///   write pattern).
/// * A larger payload yields `ceil(len / chunk_size)` chunks: every chunk
///   except the last targets [`OutboundTarget::Data`], the last targets
///   [`OutboundTarget::LastData`].
///
/// Returns [`ProtocolError::InvalidOutbound`] for an empty payload or a zero
/// chunk size.
pub fn split_request(
    payload: &[u8],
    chunk_size: usize,
) -> Result<Vec<OutboundChunk>, ProtocolError> {
    if payload.is_empty() {
        return Err(ProtocolError::InvalidOutbound("empty request payload"));
    }
    if chunk_size == 0 {
        return Err(ProtocolError::InvalidOutbound("chunk size must be >= 1"));
    }
    let mut chunks = Vec::new();
    let mut rest = payload;
    loop {
        let take = rest.len().min(chunk_size);
        let (head, tail) = rest.split_at(take);
        let target = if tail.is_empty() {
            OutboundTarget::LastData
        } else {
            OutboundTarget::Data
        };
        chunks.push(OutboundChunk {
            target,
            bytes: head.to_vec(),
        });
        if tail.is_empty() {
            break;
        }
        rest = tail;
    }
    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_frame_goes_to_last_data() {
        // The observed live pattern: one-frame requests write to ...0003.
        let chunks = split_request(&[0x05, 0x03, 0x81, 0x19, 0xed, 0xbb], 20).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].target, OutboundTarget::LastData);
        assert_eq!(chunks[0].bytes, vec![0x05, 0x03, 0x81, 0x19, 0xed, 0xbb]);
    }

    #[test]
    fn payload_exactly_one_chunk_size_goes_to_last_data() {
        let payload = vec![0xaa; 20];
        let chunks = split_request(&payload, 20).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].target, OutboundTarget::LastData);
        assert_eq!(chunks[0].bytes, payload);
    }

    #[test]
    fn multi_chunk_data_then_last_data() {
        // 25 bytes at chunk size 10 → 3 chunks: Data, Data, LastData.
        let payload: Vec<u8> = (0..25u8).collect();
        let chunks = split_request(&payload, 10).unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].target, OutboundTarget::Data);
        assert_eq!(chunks[0].bytes, (0..10).collect::<Vec<u8>>());
        assert_eq!(chunks[1].target, OutboundTarget::Data);
        assert_eq!(chunks[1].bytes, (10..20).collect::<Vec<u8>>());
        assert_eq!(chunks[2].target, OutboundTarget::LastData);
        assert_eq!(chunks[2].bytes, (20..25).collect::<Vec<u8>>());
    }

    #[test]
    fn exact_multiples_still_end_on_last_data() {
        // 20 bytes at chunk size 10 → Data + LastData (not Data + Data).
        let payload = vec![0xbb; 20];
        let chunks = split_request(&payload, 10).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].target, OutboundTarget::Data);
        assert_eq!(chunks[1].target, OutboundTarget::LastData);
        assert_eq!(chunks[1].bytes.len(), 10);
    }

    #[test]
    fn chunk_size_one() {
        let payload = vec![1, 2, 3];
        let chunks = split_request(&payload, 1).unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].target, OutboundTarget::Data);
        assert_eq!(chunks[1].target, OutboundTarget::Data);
        assert_eq!(chunks[2].target, OutboundTarget::LastData);
        assert_eq!(chunks[2].bytes, vec![3]);
    }

    #[test]
    fn empty_payload_rejected() {
        assert_eq!(
            split_request(&[], 20),
            Err(ProtocolError::InvalidOutbound("empty request payload"))
        );
    }

    #[test]
    fn zero_chunk_size_rejected() {
        assert_eq!(
            split_request(&[0x01], 0),
            Err(ProtocolError::InvalidOutbound("chunk size must be >= 1"))
        );
    }

    #[test]
    fn chunks_reassemble_to_original() {
        // Round-trip property: concatenating chunk bytes in order yields the
        // original payload, for several sizes.
        let payload: Vec<u8> = (0..100u8).collect();
        for size in [1usize, 2, 7, 20, 99, 100, 101, 1000] {
            let chunks = split_request(&payload, size).unwrap();
            let joined: Vec<u8> = chunks
                .iter()
                .flat_map(|c| c.bytes.iter().copied())
                .collect();
            assert_eq!(joined, payload, "chunk size {size}");
            assert_eq!(chunks.last().unwrap().target, OutboundTarget::LastData);
        }
    }
}
