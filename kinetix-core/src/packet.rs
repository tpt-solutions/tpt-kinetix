use serde::{Deserialize, Serialize};

use crate::timestamp::Timestamp;

/// A compressed bitstream packet as produced by a demuxer.
///
/// A `Packet` carries the raw encoded bytes for a single access unit together
/// with the timing metadata needed to schedule decoding and presentation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Packet {
    /// Presentation timestamp.
    pub pts: Timestamp,
    /// Decode timestamp.
    pub dts: Timestamp,
    /// Compressed bitstream bytes.
    pub data: Vec<u8>,
    /// Index of the stream within the container this packet belongs to.
    pub stream_index: u32,
    /// Whether this packet starts a random-access point (IDR / key frame).
    pub is_key_frame: bool,
}

impl Packet {
    /// Returns the size of the packet payload in bytes.
    #[inline]
    pub fn size(&self) -> usize {
        self.data.len()
    }
}
