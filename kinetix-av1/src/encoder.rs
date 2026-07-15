//! AV1 encoder backed by `rav1e`.

use anyhow::Context as _;
use kinetix_core::{
    frame::VideoFrame, packet::Packet, pixel_format::PixelFormat, timestamp::Timestamp,
};
use rav1e::prelude::*;

/// Configuration for the AV1 encoder.
#[derive(Debug, Clone)]
pub struct Av1EncoderConfig {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Target bitrate in bits per second.  0 = CQP mode.
    pub bitrate: u32,
    /// Constant quality parameter (0 = lossless, 255 = worst quality).
    pub quantizer: u8,
    /// Speed preset (0 = slowest/best quality, 10 = fastest).
    pub speed: u8,
    /// Maximum interval between keyframes.
    pub keyframe_interval: u64,
}

impl Default for Av1EncoderConfig {
    fn default() -> Self {
        Self {
            width: 640,
            height: 480,
            bitrate: 0,
            quantizer: 100,
            speed: 6,
            keyframe_interval: 240,
        }
    }
}

/// Stateful AV1 encoder wrapping `rav1e`.
pub struct Av1Encoder {
    context: Context<u8>,
}

impl Av1Encoder {
    /// Build a new encoder from `config`.
    pub fn new(config: &Av1EncoderConfig) -> anyhow::Result<Self> {
        let mut enc = EncoderConfig::with_speed_preset(config.speed);
        enc.width = config.width as usize;
        enc.height = config.height as usize;
        enc.quantizer = config.quantizer as usize;
        enc.bitrate = config.bitrate as i32;
        enc.max_key_frame_interval = config.keyframe_interval;
        enc.min_key_frame_interval = config.keyframe_interval / 2;
        enc.chroma_sampling = ChromaSampling::Cs420;
        enc.bit_depth = 8;

        let rav1e_cfg = Config::new().with_encoder_config(enc);
        let context: Context<u8> = rav1e_cfg
            .new_context()
            .with_context(|| "rav1e Config::new_context failed")?;

        Ok(Self { context })
    }

    /// Encode one [`VideoFrame`] (yuv420p) and return a packet if one is ready.
    ///
    /// Converts the frame to a `rav1e::Frame<u8>`, sends it to the encoder,
    /// then tries to receive a packet.  Because rav1e buffers frames internally
    /// the caller may receive `None` until enough frames have been submitted.
    pub fn encode_frame(&mut self, frame: &VideoFrame) -> anyhow::Result<Option<Packet>> {
        anyhow::ensure!(
            frame.pixel_format == PixelFormat::Yuv420p,
            "Av1Encoder only supports Yuv420p input, got {:?}",
            frame.pixel_format
        );

        let w = frame.width as usize;
        let h = frame.height as usize;
        let y_size = w * h;
        let uv_size = y_size / 4;
        let expected = y_size + uv_size * 2;
        anyhow::ensure!(
            frame.data.len() >= expected,
            "frame data too short: expected {expected}, got {}",
            frame.data.len()
        );

        // Fill rav1e frame planes.
        let mut rav1e_frame = self.context.new_frame();
        let y_data = &frame.data[..y_size];
        let cb_data = &frame.data[y_size..y_size + uv_size];
        let cr_data = &frame.data[y_size + uv_size..y_size + uv_size * 2];

        rav1e_frame.planes[0].copy_from_raw_u8(y_data, w, 1);
        rav1e_frame.planes[1].copy_from_raw_u8(cb_data, w / 2, 1);
        rav1e_frame.planes[2].copy_from_raw_u8(cr_data, w / 2, 1);

        self.context
            .send_frame(rav1e_frame)
            .map_err(|e| anyhow::anyhow!("rav1e send_frame: {e:?}"))?;

        self.try_receive_one()
    }

    /// Flush buffered frames and return all remaining packets.
    pub fn flush(&mut self) -> anyhow::Result<Vec<Packet>> {
        // Signal end-of-stream.
        self.context
            .send_frame(None)
            .map_err(|e| anyhow::anyhow!("rav1e flush send_frame: {e:?}"))?;

        let mut packets = Vec::new();
        loop {
            match self.context.receive_packet() {
                Ok(pkt) => packets.push(rav1e_packet_to_core(pkt)),
                Err(EncoderStatus::LimitReached) => break,
                Err(EncoderStatus::Encoded) => continue,
                Err(e) => return Err(anyhow::anyhow!("rav1e receive_packet during flush: {e:?}")),
            }
        }
        Ok(packets)
    }

    /// Attempt to pull one packet from rav1e without blocking.
    fn try_receive_one(&mut self) -> anyhow::Result<Option<Packet>> {
        loop {
            match self.context.receive_packet() {
                Ok(pkt) => return Ok(Some(rav1e_packet_to_core(pkt))),
                Err(EncoderStatus::NeedMoreData) | Err(EncoderStatus::LimitReached) => {
                    return Ok(None)
                }
                Err(EncoderStatus::Encoded) => continue,
                Err(e) => return Err(anyhow::anyhow!("rav1e receive_packet: {e:?}")),
            }
        }
    }
}

/// Convert a `rav1e::Packet<u8>` into a `kinetix_core::packet::Packet`.
fn rav1e_packet_to_core(pkt: rav1e::prelude::Packet<u8>) -> Packet {
    let pts = Timestamp::new(pkt.input_frameno as i64, (1, 90_000));
    let is_key = pkt.frame_type == FrameType::KEY;
    Packet {
        pts,
        dts: pts,
        data: pkt.data,
        stream_index: 0,
        is_key_frame: is_key,
    }
}
