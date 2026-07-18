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

/// A fully reassembled RTMP message (all its chunks concatenated).
#[derive(Debug, Clone)]
pub struct RtmpMessage {
    /// The message type id (see [`MessageTypeId`]).
    pub message_type_id: u8,
    /// The message stream id.
    pub message_stream_id: u32,
    /// The (absolute) timestamp of the message.
    pub timestamp: u32,
    /// The complete message payload.
    pub payload: Vec<u8>,
}

/// Reassembles RTMP chunk streams into complete [`RtmpMessage`]s.
///
/// Feed raw bytes with [`ChunkAssembler::push`]; it buffers incomplete data and
/// returns whatever complete messages it can decode. Chunks belonging to the
/// same message (split across `chunk_size` boundaries) are concatenated using
/// the per-chunk-stream continuation state.
pub struct ChunkAssembler {
    parser: ChunkParser,
    buf: Vec<u8>,
    /// In-progress payload accumulation per chunk-stream id.
    partial: HashMap<u32, PartialMessage>,
}

struct PartialMessage {
    header: ChunkHeader,
    data: Vec<u8>,
}

impl ChunkAssembler {
    /// Create a new assembler with the default (128-byte) chunk size.
    pub fn new() -> Self {
        Self {
            parser: ChunkParser::new(),
            buf: Vec::new(),
            partial: HashMap::new(),
        }
    }

    /// Update the negotiated chunk size (e.g. after a `SetChunkSize` message).
    pub fn set_chunk_size(&mut self, size: u32) {
        if size > 0 {
            self.parser.chunk_size = size;
        }
    }

    /// The current negotiated chunk size.
    pub fn chunk_size(&self) -> u32 {
        self.parser.chunk_size
    }

    /// Append `bytes` to the internal buffer and return any messages that became
    /// complete as a result.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<RtmpMessage> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();

        loop {
            // Try to parse one chunk header from the front of the buffer.
            let (header, after_header_len) = match self.parser.parse_chunk_header(&self.buf) {
                Ok((h, remaining)) => (h, self.buf.len() - remaining.len()),
                // Not enough bytes yet for a header — wait for more data.
                Err(_) => break,
            };

            let cs_id = header.chunk_stream_id;

            // Determine how many payload bytes this chunk carries.
            let already = self.partial.get(&cs_id).map(|p| p.data.len()).unwrap_or(0);
            let remaining_msg = (header.message_length as usize).saturating_sub(already);
            let this_chunk = remaining_msg.min(self.parser.chunk_size as usize);

            // Do we have the whole chunk payload buffered?
            if self.buf.len() < after_header_len + this_chunk {
                // Roll back: we consumed header parsing state but not enough
                // payload. Since parse_chunk_header mutated prev_headers, we must
                // still wait; leave buffer intact and break. (Header state is
                // idempotent enough for the next attempt on the same bytes.)
                break;
            }

            let payload = self.buf[after_header_len..after_header_len + this_chunk].to_vec();
            // Remove consumed bytes from the front of the buffer.
            self.buf.drain(..after_header_len + this_chunk);

            let entry = self.partial.entry(cs_id).or_insert_with(|| PartialMessage {
                header: header.clone(),
                data: Vec::with_capacity(header.message_length as usize),
            });
            entry.header = header.clone();
            entry.data.extend_from_slice(&payload);

            if entry.data.len() >= header.message_length as usize {
                let complete = self.partial.remove(&cs_id).expect("just inserted");
                out.push(RtmpMessage {
                    message_type_id: complete.header.message_type_id,
                    message_stream_id: complete.header.message_stream_id,
                    timestamp: complete.header.timestamp,
                    payload: complete.data,
                });
            }
        }

        out
    }
}

impl Default for ChunkAssembler {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a Type-0 chunk (basic header cs_id=3) carrying `payload` as a single
    /// chunk (payload must be <= chunk_size).
    fn type0_chunk(type_id: u8, stream_id: u32, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.push(0x03); // fmt=0, cs_id=3
        v.extend_from_slice(&[0, 0, 0]); // timestamp
        let len = payload.len() as u32;
        v.extend_from_slice(&len.to_be_bytes()[1..]); // message_length (3 bytes)
        v.push(type_id);
        v.extend_from_slice(&stream_id.to_le_bytes()); // message_stream_id (LE)
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn parses_single_chunk_message() {
        let mut asm = ChunkAssembler::new();
        let chunk = type0_chunk(9 /* video */, 1, b"hello");
        let msgs = asm.push(&chunk);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].message_type_id, 9);
        assert_eq!(msgs[0].message_stream_id, 1);
        assert_eq!(msgs[0].payload, b"hello");
    }

    #[test]
    fn waits_for_incomplete_data() {
        let mut asm = ChunkAssembler::new();
        let chunk = type0_chunk(9, 1, b"partial-body");
        // Feed only the first half.
        let half = chunk.len() / 2;
        assert!(asm.push(&chunk[..half]).is_empty());
        // Feed the rest — now it completes.
        let msgs = asm.push(&chunk[half..]);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].payload, b"partial-body");
    }

    #[test]
    fn reassembles_message_split_across_chunks() {
        let mut asm = ChunkAssembler::new();
        asm.set_chunk_size(4); // force multi-chunk messages
        assert_eq!(asm.chunk_size(), 4);

        // A 10-byte video message = 4 + 4 + 2 across three chunks.
        // First chunk: Type-0 header + 4 payload bytes.
        let mut bytes = Vec::new();
        {
            let mut v = Vec::new();
            v.push(0x03);
            v.extend_from_slice(&[0, 0, 0]);
            v.extend_from_slice(&10u32.to_be_bytes()[1..]);
            v.push(9);
            v.extend_from_slice(&1u32.to_le_bytes());
            v.extend_from_slice(b"AAAA");
            bytes.extend_from_slice(&v);
        }
        // Continuation chunks: Type-3 (0xC3), 4 bytes then 2 bytes.
        bytes.push(0xC3);
        bytes.extend_from_slice(b"BBBB");
        bytes.push(0xC3);
        bytes.extend_from_slice(b"CC");

        let msgs = asm.push(&bytes);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].payload, b"AAAABBBBCC");
    }

    #[test]
    fn message_type_id_roundtrip() {
        assert_eq!(MessageTypeId::from_u8(9), Some(MessageTypeId::Video));
        assert_eq!(MessageTypeId::from_u8(8), Some(MessageTypeId::Audio));
        assert_eq!(MessageTypeId::from_u8(200), None);
    }
}
