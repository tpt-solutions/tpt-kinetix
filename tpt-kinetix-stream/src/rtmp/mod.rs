//! RTMP ingest server — handshake, chunk stream parsing, and live ingest.

pub mod amf;
pub mod chunk;
pub mod flv;
pub mod handshake;
pub mod server;

pub use amf::{Amf0Value, AmfError};
pub use chunk::{ChunkAssembler, ChunkParser, MessageTypeId, RtmpMessage};
pub use flv::{
    parse_audio_tag, parse_video_tag, AacPacketType, AvcPacketType, FlvAudioTag, FlvVideoTag,
};
pub use server::{RtmpConfig, RtmpMediaEvent, RtmpServer};
