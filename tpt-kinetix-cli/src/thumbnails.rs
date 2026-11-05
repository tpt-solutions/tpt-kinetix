//! `tpt-kinetix thumbnails`: stills from the keyframes of an AV1 or VP9 video.
//!
//! The file is read twice. The first pass finds the keyframes of the video
//! track without decoding anything; the second decodes only the chosen ones.
//! Each keyframe is decoded by a fresh decoder, which needs no reference
//! frames, so memory stays at one frame however long the video is.
//!
//! Only 8-bit 4:2:0 video is written. Other formats are refused with a message
//! rather than converted badly.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{bail, Context, Result};
use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_core::frame::VideoFrame;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::pixel_format::PixelFormat;
use tpt_kinetix_core::stream::StreamInfo;
use tpt_kinetix_demux::{Demuxer, IvfDemuxer, MkvReader, Mp4Reader};

use crate::png;

/// A contact sheet: the stills scaled to `tile_width` and laid out in `columns`.
#[derive(Debug, Clone, Copy)]
pub struct Sprite {
    pub columns: usize,
    pub tile_width: u32,
}

/// Writes `count` PNG stills, spread evenly over the video's keyframes, to
/// `output`, and a `sprite.png` contact sheet when `sprite` is given.
pub fn run(input: &Path, output: &Path, count: usize, sprite: Option<Sprite>) -> Result<()> {
    if count == 0 {
        bail!("--count must be at least 1");
    }
    if let Some(s) = sprite {
        if s.columns == 0 || s.tile_width == 0 {
            bail!("--sprite-columns and --sprite-width must be at least 1");
        }
    }
    let (mut reader, streams) = open(input)?;
    let (video, codec) = video_track(&streams)?;

    // Pass 1: where the keyframes are, as indices among the video packets.
    let mut keyframes = Vec::new();
    let mut ordinal = 0u64;
    while let Some(p) = reader.read_packet()? {
        if p.stream_index as usize == video {
            if p.is_key_frame {
                keyframes.push(ordinal);
            }
            ordinal += 1;
        }
    }
    if keyframes.is_empty() {
        bail!("no keyframes found in {}", input.display());
    }
    let wanted = pick(&keyframes, count);

    // Pass 2: decode the chosen keyframes.
    std::fs::create_dir_all(output)
        .with_context(|| format!("failed to create {}", output.display()))?;
    let (mut reader, _) = open(input)?;
    let mut ordinal = 0u64;
    let mut next = wanted.iter().copied().peekable();
    let mut written = Vec::new();
    // Scaled copies for the contact sheet: small, so kept in memory.
    let mut tiles: Vec<(u32, u32, Vec<u8>)> = Vec::new();
    while let Some(p) = reader.read_packet()? {
        if p.stream_index as usize != video {
            continue;
        }
        let here = ordinal;
        ordinal += 1;
        if next.peek() != Some(&here) {
            continue;
        }
        next.next();
        let Some(frame) = decode_keyframe(codec, &p)? else {
            eprintln!("skipping keyframe {here}: the decoder produced no picture");
            continue;
        };
        let path = output.join(format!("thumb-{:04}.png", written.len() + 1));
        let rgb = to_rgb(&frame)?;
        std::fs::write(&path, png::encode_rgb(frame.width, frame.height, &rgb))
            .with_context(|| format!("failed to write {}", path.display()))?;
        if let Some(s) = sprite {
            let th = tile_height(frame.width, frame.height, s.tile_width);
            tiles.push((
                s.tile_width,
                th,
                scale_nearest(&rgb, frame.width, frame.height, s.tile_width, th),
            ));
        }
        written.push((path, seconds(&p)));
    }

    if let Some(s) = sprite {
        if let Some(&(tw, th, _)) = tiles.first() {
            let (w, h, rgb) = compose(
                &tiles.iter().map(|t| t.2.clone()).collect::<Vec<_>>(),
                tw,
                th,
                s.columns,
            );
            let path = output.join("sprite.png");
            std::fs::write(&path, png::encode_rgb(w, h, &rgb))
                .with_context(|| format!("failed to write {}", path.display()))?;
            println!("{}  {}x{} ({} per row)", path.display(), w, h, s.columns);
        }
    }

    for (path, at) in &written {
        println!("{}  t={at:.3}s", path.display());
    }
    println!(
        "wrote {} thumbnail(s) from {} keyframe(s) -> {}",
        written.len(),
        keyframes.len(),
        output.display()
    );
    Ok(())
}

/// Opens a WebM, IVF or MP4 file, returning its demuxer and track list. A fresh
/// call starts from the beginning, which is how the second pass rewinds.
fn open(path: &Path) -> Result<(Box<dyn Demuxer>, Vec<StreamInfo>)> {
    let mut head = [0u8; 4];
    let mut f = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let n = f.read(&mut head)?;
    let is_webm = n == 4 && head == [0x1A, 0x45, 0xDF, 0xA3];
    let is_ivf = n == 4 && &head == b"DKIF";
    if is_webm {
        let reader = MkvReader::open(File::open(path)?)
            .with_context(|| format!("failed to parse WebM: {}", path.display()))?;
        let streams = reader.streams().to_vec();
        Ok((Box::new(reader), streams))
    } else if is_ivf {
        let demuxer = IvfDemuxer::new(std::fs::read(path)?)
            .with_context(|| format!("failed to parse IVF: {}", path.display()))?;
        let streams = vec![demuxer.stream_info().clone()];
        Ok((Box::new(demuxer), streams))
    } else {
        let reader = Mp4Reader::open(File::open(path)?)
            .with_context(|| format!("failed to parse MP4: {}", path.display()))?;
        let streams = reader.streams();
        Ok((Box::new(reader), streams))
    }
}

/// The first AV1 or VP9 video track: its packet index and codec.
fn video_track(streams: &[StreamInfo]) -> Result<(usize, CodecId)> {
    streams
        .iter()
        .enumerate()
        .find(|(_, s)| {
            s.media_type == MediaType::Video && matches!(s.codec, CodecId::Av1 | CodecId::Vp9)
        })
        .map(|(i, s)| (i, s.codec))
        .context("thumbnails need an AV1 or VP9 video track")
}

/// `count` keyframes spread evenly from the first to the last, or all of them
/// when there are no more than `count`.
fn pick(keyframes: &[u64], count: usize) -> Vec<u64> {
    let n = keyframes.len();
    if count >= n {
        return keyframes.to_vec();
    }
    if count == 1 {
        return vec![keyframes[n / 2]];
    }
    // The step is more than one, so the indices are distinct and increasing.
    (0..count)
        .map(|i| keyframes[i * (n - 1) / (count - 1)])
        .collect()
}

fn decode_keyframe(codec: CodecId, p: &Packet) -> Result<Option<VideoFrame>> {
    let frame = match codec {
        CodecId::Vp9 => tpt_kinetix_vp9::Vp9Decoder::new().decode(p)?,
        CodecId::Av1 => tpt_kinetix_av1::Av1Decoder::new().decode(p)?,
        other => bail!("no decoder for {}", other.name()),
    };
    Ok(frame)
}

/// The packet's presentation time in seconds.
fn seconds(p: &Packet) -> f64 {
    let (num, den) = p.pts.time_base;
    if den == 0 {
        return 0.0;
    }
    p.pts.value as f64 * f64::from(num) / f64::from(den)
}

/// The height of a tile that keeps the frame's aspect ratio.
fn tile_height(width: u32, height: u32, tile_width: u32) -> u32 {
    let h = u64::from(height) * u64::from(tile_width) / u64::from(width.max(1));
    u32::try_from(h.max(1)).unwrap_or(u32::MAX)
}

/// Nearest-neighbour resize of packed RGB.
fn scale_nearest(rgb: &[u8], w: u32, h: u32, nw: u32, nh: u32) -> Vec<u8> {
    let (w, h, nw, nh) = (w as usize, h as usize, nw as usize, nh as usize);
    let mut out = Vec::with_capacity(nw * nh * 3);
    for y in 0..nh {
        let sy = (y * h / nh).min(h - 1);
        for x in 0..nw {
            let sx = (x * w / nw).min(w - 1);
            let at = (sy * w + sx) * 3;
            out.extend_from_slice(&rgb[at..at + 3]);
        }
    }
    out
}

/// Lays equal-sized RGB tiles out in rows of `columns`; the last row is padded black.
fn compose(tiles: &[Vec<u8>], tw: u32, th: u32, columns: usize) -> (u32, u32, Vec<u8>) {
    let (tw, th) = (tw as usize, th as usize);
    let rows = tiles.len().div_ceil(columns);
    let (cw, ch) = (tw * columns, th * rows);
    let mut out = vec![0u8; cw * ch * 3];
    for (i, tile) in tiles.iter().enumerate() {
        let (col, row) = (i % columns, i / columns);
        for y in 0..th {
            let dst = ((row * th + y) * cw + col * tw) * 3;
            let src = y * tw * 3;
            out[dst..dst + tw * 3].copy_from_slice(&tile[src..src + tw * 3]);
        }
    }
    (cw as u32, ch as u32, out)
}

/// Converts an 8-bit 4:2:0 frame to packed RGB (BT.601, limited range).
fn to_rgb(frame: &VideoFrame) -> Result<Vec<u8>> {
    if frame.pixel_format != PixelFormat::Yuv420p {
        bail!(
            "thumbnails support 8-bit 4:2:0 video only, not {:?}",
            frame.pixel_format
        );
    }
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    if frame.data.len() != w * h + 2 * cw * ch {
        bail!(
            "unexpected plane layout ({} bytes): is the video higher than 8-bit?",
            frame.data.len()
        );
    }
    let (y, chroma) = frame.data.split_at(w * h);
    let (u, v) = chroma.split_at(cw * ch);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for row in 0..h {
        for col in 0..w {
            let luma = 1.164 * (f32::from(y[row * w + col]) - 16.0);
            let ci = (row / 2) * cw + col / 2;
            let cb = f32::from(u[ci]) - 128.0;
            let cr = f32::from(v[ci]) - 128.0;
            rgb.push(clamp(luma + 1.596 * cr));
            rgb.push(clamp(luma - 0.392 * cb - 0.813 * cr));
            rgb.push(clamp(luma + 2.017 * cb));
        }
    }
    Ok(rgb)
}

fn clamp(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_are_even_distinct_and_within_range() {
        let keys: Vec<u64> = (0..10).map(|i| i * 25).collect();
        // 10 keyframes, 3 picks: indices 0, 4 and 9.
        assert_eq!(pick(&keys, 3), vec![keys[0], keys[4], keys[9]]);
        assert_eq!(pick(&keys, 1), vec![keys[5]]);
        assert_eq!(pick(&keys, 50), keys);
        let many = pick(&keys, 4);
        assert_eq!(many.len(), 4);
        assert!(many.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn scaling_and_composing_place_tiles_in_a_grid() {
        // A 2x2 image of distinct pixels, scaled to 4x4 by nearest neighbour.
        let rgb = vec![1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4];
        let big = scale_nearest(&rgb, 2, 2, 4, 4);
        assert_eq!(&big[..6], &[1, 1, 1, 1, 1, 1]);
        assert_eq!(&big[(4 * 3)..(4 * 3) + 3], &[1, 1, 1]);
        // Three one-pixel tiles in two columns: a 2x2 sheet with a black gap.
        let (w, h, out) = compose(
            &[vec![10, 10, 10], vec![20, 20, 20], vec![30, 30, 30]],
            1,
            1,
            2,
        );
        assert_eq!((w, h), (2, 2));
        assert_eq!(&out[..6], &[10, 10, 10, 20, 20, 20]);
        assert_eq!(&out[6..], &[30, 30, 30, 0, 0, 0]);
    }

    #[test]
    fn grey_frame_converts_to_grey_rgb() {
        // Y = 128, U = V = 128 is mid grey: 1.164 * 112 = 130.4 in every channel.
        let frame = VideoFrame {
            pts: tpt_kinetix_core::timestamp::Timestamp::NONE,
            dts: tpt_kinetix_core::timestamp::Timestamp::NONE,
            data: vec![128; 4 * 4 + 2 * 2 * 2],
            width: 4,
            height: 4,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: true,
        };
        let rgb = to_rgb(&frame).unwrap();
        assert_eq!(rgb.len(), 4 * 4 * 3);
        assert!(rgb.iter().all(|&c| c == 130));
    }

    #[test]
    fn high_bit_depth_frames_are_refused() {
        let frame = VideoFrame {
            pts: tpt_kinetix_core::timestamp::Timestamp::NONE,
            dts: tpt_kinetix_core::timestamp::Timestamp::NONE,
            data: vec![0; 2 * (4 * 4 + 2 * 2 * 2)],
            width: 4,
            height: 4,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: true,
        };
        assert!(to_rgb(&frame).is_err());
    }
}
