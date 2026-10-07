//! RTMP ingest server — handshake, chunk stream parsing, and live ingest.

pub mod amf;
pub mod chunk;
pub mod flv;
pub mod handshake;
pub mod server;

pub use amf::{Amf0Value, AmfError};
pub use chunk::{ChunkAssembler, ChunkParser, MessageTypeId, RtmpMessage};
pub use flv::{
    parse_audio_tag, parse_audio_tags, parse_video_tag, parse_video_tags, AacPacketType,
    AvcPacketType, ExVideoPacketType, FlvAudioTag, FlvVideoCodec, FlvVideoTag, HdrMetadata,
};
pub use server::{RtmpCapabilities, RtmpConfig, RtmpMediaEvent, RtmpServer, RtmpsIdentity};
