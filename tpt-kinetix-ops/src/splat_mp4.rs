//! Splat sequences in MP4: each `SPLS` time slab is one sample of a `spls`
//! track.
//!
//! The track's configuration (`splc`) is the splat header record from
//! `tpt-kinetix-splat`, which holds the slab duration, SH degree, quantisation
//! and start time. Converting back rebuilds the same `SPLS` bytes.

use std::io::Cursor;

use anyhow::{bail, Context, Result};
use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::stream::StreamInfo;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_demux::{Demuxer, Mp4Reader};
use tpt_kinetix_mux::writer::{Mp4Writer, WriterOptions};
use tpt_kinetix_splat::{read_sequence, sequence_config, sequence_from_config};

/// Track timescale: microseconds, so slab durations keep their precision.
const TIMESCALE: u32 = 1_000_000;

/// Packs an `SPLS` file into an MP4 with one `spls` sample per time slab.
///
/// # Errors
///
/// Fails if the input is not a valid `SPLS` file, or if the slab duration does
/// not fit the track timescale.
pub fn spls_to_mp4(spls: &[u8]) -> Result<Vec<u8>> {
    let seq = read_sequence(spls).context("reading SPLS input")?;
    let config = sequence_config(&seq).context("building SPLS track configuration")?;
    let slab_ticks = (f64::from(seq.slab_duration()) * f64::from(TIMESCALE)).round();
    if !(slab_ticks >= 1.0 && slab_ticks <= f64::from(u32::MAX)) {
        bail!("slab duration does not fit the track timescale");
    }
    let slab_ticks = slab_ticks as u32;

    let mut info = StreamInfo::new(0, CodecId::Spls, TIMESCALE);
    info.media_type = MediaType::Other;
    info.extradata = config;
    info.duration = u64::from(slab_ticks) * seq.entries().len() as u64;

    let mut writer = Mp4Writer::new(Cursor::new(Vec::new()), &[info], WriterOptions::default())
        .context("starting MP4 writer")?;
    for k in 0..seq.entries().len() {
        let pts = Timestamp::new(i64::from(slab_ticks) * k as i64, (1, TIMESCALE));
        let packet = Packet {
            pts,
            dts: pts,
            data: seq.slab_payload(k)?.to_vec(),
            stream_index: 0,
            is_key_frame: true,
        };
        writer
            .write_packet_with_duration(&packet, Some(slab_ticks))
            .with_context(|| format!("writing slab {k}"))?;
    }
    let out = writer.finish().context("finishing MP4")?;
    Ok(out.into_inner())
}

/// Reads the `spls` track of an MP4 back into `SPLS` bytes.
///
/// # Errors
///
/// Fails if the file has no `spls` track, or if its configuration or samples
/// are malformed.
pub fn mp4_to_spls(mp4: &[u8]) -> Result<Vec<u8>> {
    let mut reader = Mp4Reader::open(mp4.to_vec()).context("reading MP4")?;
    let info = reader
        .streams()
        .into_iter()
        .find(|s| s.codec == CodecId::Spls)
        .context("no spls track in this MP4")?;
    let mut payloads = Vec::new();
    while let Some(packet) = reader.read_packet().context("reading spls sample")? {
        payloads.push(packet.data);
    }
    let refs: Vec<&[u8]> = payloads.iter().map(Vec::as_slice).collect();
    Ok(sequence_from_config(&info.extradata, &refs)?)
}
