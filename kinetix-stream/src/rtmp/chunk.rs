//! RTMP chunk stream parser.

use std::collections::HashMap;

/// The four RTMP chunk header formats (type 0–3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkHeaderFormat {
    /// Type 0 — full 11-byte message header.
    Type0,
    /// Type 1 — 7-byte delta header (no stream id).
    Type1,
    /// Type 2 — 3-byte timestamp-delta only.
    Type2,
    /// Type 3 — no message header (inherit everything from previous chunk).
    Type3,
}

/// Decoded RTMP chunk header.
#[derive(Debug, Clone)]
pub struct ChunkHeader {
    pub format: ChunkHeaderFormat,
    pub chunk_stream_id: u32,
    pub timestamp: u32,
    pub message_length: u32,
    pub message_type_id: u8,
    pub message_stream_id: u32,
}

/// Stateful parser that tracks per-chunk-stream context so that abbreviated
/// header formats (type 1/2/3) can be decoded against the previous header.
pub struct ChunkParser {
    /// Current negotiated chunk size (default 128 bytes as per RTMP spec).
    pub chunk_size: u32,
    prev_headers: HashMap<u32, ChunkHeader>,
}

impl ChunkParser {
    /// Create a new parser with the default chunk size of 128.
    pub fn new() -> Self {
        Self {
            chunk_size: 128,
            prev_headers: HashMap::new(),
        }
    }

    /// Parse one RTMP chunk basic header + message header from `data`.
    ///
    /// Returns `(ChunkHeader, remaining_bytes_after_header)`.
    ///
    /// This does **not** consume the chunk payload — the caller must read
    /// `min(header.message_length, self.chunk_size)` bytes of payload
    /// separately.
    pub fn parse_chunk_header<'a>(
        &mut self,
        data: &'a [u8],
    ) -> anyhow::Result<(ChunkHeader, &'a [u8])> {
        anyhow::ensure!(!data.is_empty(), "empty data for chunk header");

        // ── Basic header ────────────────────────────────────────────────────
        let first_byte = data[0];
        let fmt = match first_byte >> 6 {
            0 => ChunkHeaderFormat::Type0,
            1 => ChunkHeaderFormat::Type1,
            2 => ChunkHeaderFormat::Type2,
            3 => ChunkHeaderFormat::Type3,
            _ => unreachable!(),
        };

        let (chunk_stream_id, basic_header_len) = {
            let cs_id_raw = (first_byte & 0x3F) as u32;
            match cs_id_raw {
                0 => {
                    // 2-byte form: CS ID = second_byte + 64
                    anyhow::ensure!(data.len() >= 2, "truncated 2-byte basic header");
                    (data[1] as u32 + 64, 2usize)
                }
                1 => {
                    // 3-byte form: CS ID = third_byte * 256 + second_byte + 64
                    anyhow::ensure!(data.len() >= 3, "truncated 3-byte basic header");
                    (data[2] as u32 * 256 + data[1] as u32 + 64, 3usize)
                }
                _ => (cs_id_raw, 1usize),
            }
        };

        let rest = &data[basic_header_len..];

        // ── Message header ───────────────────────────────────────────────────
        // Retrieve previous header for this chunk stream (required for types 1–3).
        let prev = self.prev_headers.get(&chunk_stream_id).cloned();

        let (header, msg_header_len) = match fmt {
            ChunkHeaderFormat::Type0 => {
                // 11 bytes: timestamp(3) + length(3) + type_id(1) + stream_id(4 LE)
                anyhow::ensure!(rest.len() >= 11, "truncated type-0 message header");
                let ts_raw = u32::from_be_bytes([0, rest[0], rest[1], rest[2]]);
                let msg_len = u32::from_be_bytes([0, rest[3], rest[4], rest[5]]);
                let type_id = rest[6];
                let stream_id = u32::from_le_bytes([rest[7], rest[8], rest[9], rest[10]]);

                let timestamp = if ts_raw == 0x00FF_FFFF {
                    // extended timestamp: next 4 bytes
                    anyhow::ensure!(rest.len() >= 15, "truncated extended timestamp");
                    u32::from_be_bytes([rest[11], rest[12], rest[13], rest[14]])
                } else {
                    ts_raw
                };
                let consumed = if ts_raw == 0x00FF_FFFF { 15 } else { 11 };

                (
                    ChunkHeader {
                        format: fmt,
                        chunk_stream_id,
                        timestamp,
                        message_length: msg_len,
                        message_type_id: type_id,
                        message_stream_id: stream_id,
                    },
                    consumed,
                )
            }

            ChunkHeaderFormat::Type1 => {
                // 7 bytes: ts_delta(3) + length(3) + type_id(1)
                anyhow::ensure!(rest.len() >= 7, "truncated type-1 message header");
                let ts_delta = u32::from_be_bytes([0, rest[0], rest[1], rest[2]]);
                let msg_len = u32::from_be_bytes([0, rest[3], rest[4], rest[5]]);
                let type_id = rest[6];

                let base = prev.as_ref().map(|h| h.timestamp).unwrap_or(0);
                let (timestamp, consumed) = if ts_delta == 0x00FF_FFFF {
                    anyhow::ensure!(rest.len() >= 11, "truncated extended timestamp (type 1)");
                    let ext = u32::from_be_bytes([rest[7], rest[8], rest[9], rest[10]]);
                    (base.wrapping_add(ext), 11)
                } else {
                    (base.wrapping_add(ts_delta), 7)
                };

                let stream_id = prev.as_ref().map(|h| h.message_stream_id).unwrap_or(0);

                (
                    ChunkHeader {
                        format: fmt,
                        chunk_stream_id,
                        timestamp,
                        message_length: msg_len,
                        message_type_id: type_id,
                        message_stream_id: stream_id,
                    },
                    consumed,
                )
            }

            ChunkHeaderFormat::Type2 => {
                // 3 bytes: ts_delta(3)
                anyhow::ensure!(rest.len() >= 3, "truncated type-2 message header");
                let ts_delta = u32::from_be_bytes([0, rest[0], rest[1], rest[2]]);

                let (timestamp, consumed) = if ts_delta == 0x00FF_FFFF {
                    anyhow::ensure!(rest.len() >= 7, "truncated extended timestamp (type 2)");
                    let ext = u32::from_be_bytes([rest[3], rest[4], rest[5], rest[6]]);
                    let base = prev.as_ref().map(|h| h.timestamp).unwrap_or(0);
                    (base.wrapping_add(ext), 7)
                } else {
                    let base = prev.as_ref().map(|h| h.timestamp).unwrap_or(0);
                    (base.wrapping_add(ts_delta), 3)
                };

                let (msg_len, type_id, stream_id) = prev
                    .as_ref()
                    .map(|h| (h.message_length, h.message_type_id, h.message_stream_id))
                    .unwrap_or((0, 0, 0));

                (
                    ChunkHeader {
                        format: fmt,
                        chunk_stream_id,
                        timestamp,
                        message_length: msg_len,
                        message_type_id: type_id,
                        message_stream_id: stream_id,
                    },
                    consumed,
                )
            }

            ChunkHeaderFormat::Type3 => {
                // No message header — inherit everything from previous.
                let base = prev.unwrap_or(ChunkHeader {
                    format: ChunkHeaderFormat::Type3,
                    chunk_stream_id,
                    timestamp: 0,
                    message_length: 0,
                    message_type_id: 0,
                    message_stream_id: 0,
                });
                (
                    ChunkHeader {
                        format: fmt,
                        chunk_stream_id,
                        ..base
                    },
                    0,
                )
            }
        };

        // Store as the most-recent header for this CS ID.
        self.prev_headers.insert(chunk_stream_id, header.clone());

        Ok((&rest[msg_header_len..], header)).map(|(remaining, h)| (h, remaining))
    }
}

impl Default for ChunkParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Known RTMP message type IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageTypeId {
    SetChunkSize = 1,
    Abort = 2,
    Ack = 3,
    UserControl = 4,
    WindowAckSize = 5,
    SetPeerBandwidth = 6,
    Audio = 8,
    Video = 9,
    DataAmf3 = 15,
    SharedObjectAmf3 = 16,
    CommandAmf3 = 17,
    DataAmf0 = 18,
    SharedObjectAmf0 = 19,
    CommandAmf0 = 20,
    Aggregate = 22,
}

impl MessageTypeId {
    /// Convert a raw byte to a `MessageTypeId`, returning `None` if unknown.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::SetChunkSize),
            2 => Some(Self::Abort),
            3 => Some(Self::Ack),
            4 => Some(Self::UserControl),
            5 => Some(Self::WindowAckSize),
            6 => Some(Self::SetPeerBandwidth),
            8 => Some(Self::Audio),
            9 => Some(Self::Video),
            15 => Some(Self::DataAmf3),
            16 => Some(Self::SharedObjectAmf3),
            17 => Some(Self::CommandAmf3),
            18 => Some(Self::DataAmf0),
            19 => Some(Self::SharedObjectAmf0),
            20 => Some(Self::CommandAmf0),
            22 => Some(Self::Aggregate),
            _ => None,
        }
    }
}
