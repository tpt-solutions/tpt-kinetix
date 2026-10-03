//! Multi-track, passthrough MP4 writer.
//!
//! [`Mp4Writer`] writes already-encoded packets (from any demuxer) into a
//! progressive MP4 **without holding the media in memory**: samples are
//! buffered per track only until a chunk (about half a second, or 4 MiB) is
//! complete, then written to the output; the sample tables are small and are
//! turned into the `moov` box when [`Mp4Writer::finish`] is called.
//!
//! The output is `ftyp`, a 64-bit-size `mdat`, then `moov` ("moov at end"),
//! which is why the output must be seekable (the `mdat` size is patched at the
//! end). Use [`crate::faststart::faststart`] to move `moov` to the front for
//! progressive HTTP playback.
//!
//! Streams are described by [`StreamInfo`]; any codec with a sample-entry
//! writer in [`crate::entries`] can be carried, including audio Kinetix has no
//! decoder for.

use std::io::{Seek, SeekFrom, Write};

use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::stream::StreamInfo;

use crate::entries::{boxed, full_box, handler_for, sample_entry};
use crate::MuxError;

/// Movie timescale written to `mvhd` (ticks per second).
const MOVIE_TIMESCALE: u64 = 1000;

/// Tunables for [`Mp4Writer`].
#[derive(Debug, Clone)]
pub struct WriterOptions {
    /// Target duration of one interleaved chunk, in milliseconds.
    pub chunk_duration_ms: u32,
    /// Upper bound on the buffered bytes of one chunk.
    pub max_chunk_bytes: usize,
    /// Always write 64-bit chunk offsets (`co64`), even for small files.
    pub force_co64: bool,
}

impl Default for WriterOptions {
    fn default() -> Self {
        Self {
            chunk_duration_ms: 500,
            max_chunk_bytes: 4 << 20,
            force_co64: false,
        }
    }
}

struct Track {
    info: StreamInfo,
    entry: Vec<u8>,
    pending: Vec<u8>,
    pending_samples: u32,
    pending_start_dts: i64,
    sizes: Vec<u32>,
    stts: Vec<(u32, u32)>,
    ctts: Vec<(u32, i32)>,
    any_ctts: bool,
    any_negative_ctts: bool,
    sync: Vec<u32>,
    all_key: bool,
    chunk_offsets: Vec<u64>,
    stsc: Vec<(u32, u32)>,
    samples: u32,
    first_dts: Option<i64>,
    first_cts: i32,
    last_dts: i64,
    last_delta: u32,
    /// Highest `dts + cts` seen: the end of presentation is this plus a duration.
    max_pts: i64,
    /// Caller-supplied duration of the most recent sample (see
    /// [`Mp4Writer::write_packet_with_duration`]).
    last_duration_hint: Option<u32>,
}

fn push_run<T: PartialEq + Copy>(runs: &mut Vec<(u32, T)>, v: T) {
    match runs.last_mut() {
        Some((n, last)) if *last == v => *n += 1,
        _ => runs.push((1, v)),
    }
}

/// `value` in `from` (num/den seconds per tick) expressed in `1/to_scale` ticks.
fn rescale(value: i64, from: (u32, u32), to_scale: u32) -> Result<i64, MuxError> {
    let (num, den) = (i128::from(from.0), i128::from(from.1));
    if den == 0 {
        return Err(MuxError::InvalidTimestamps(
            "time base with a zero denominator".into(),
        ));
    }
    // value * (num/den) seconds * to_scale ticks/second, rounded to nearest.
    let n = i128::from(value) * num * i128::from(to_scale);
    let r = if n >= 0 {
        (n + den / 2) / den
    } else {
        -((-n + den / 2) / den)
    };
    i64::try_from(r).map_err(|_| MuxError::InvalidTimestamps("timestamp overflow".into()))
}

/// A progressive MP4 writer over a seekable output.
pub struct Mp4Writer<W: Write + Seek> {
    w: W,
    opts: WriterOptions,
    tracks: Vec<Track>,
    mdat_start: u64,
    pos: u64,
}

impl<W: Write + Seek> Mp4Writer<W> {
    /// Starts a file with one track per entry of `streams` (the track at index
    /// `i` receives packets whose `stream_index` is `i`).
    pub fn new(mut w: W, streams: &[StreamInfo], opts: WriterOptions) -> Result<Self, MuxError> {
        if streams.is_empty() {
            return Err(MuxError::InvalidConfig(
                "at least one stream is required".into(),
            ));
        }
        let mut tracks = Vec::with_capacity(streams.len());
        for (i, s) in streams.iter().enumerate() {
            handler_for(s.media_type)?;
            if s.timescale == 0 {
                return Err(MuxError::InvalidConfig(format!(
                    "stream {i} has a zero timescale"
                )));
            }
            let mut info = s.clone();
            info.index = i as u32;
            tracks.push(Track {
                entry: sample_entry(&info)?,
                info,
                pending: Vec::new(),
                pending_samples: 0,
                pending_start_dts: 0,
                sizes: Vec::new(),
                stts: Vec::new(),
                ctts: Vec::new(),
                any_ctts: false,
                any_negative_ctts: false,
                sync: Vec::new(),
                all_key: true,
                chunk_offsets: Vec::new(),
                stsc: Vec::new(),
                samples: 0,
                first_dts: None,
                first_cts: 0,
                last_dts: 0,
                last_delta: 0,
                max_pts: i64::MIN,
                last_duration_hint: None,
            });
        }
        let ftyp = build_ftyp(streams);
        w.write_all(&ftyp)?;
        let mdat_start = ftyp.len() as u64;
        // 64-bit-size mdat header; the size is patched in `finish`.
        w.write_all(&1u32.to_be_bytes())?;
        w.write_all(b"mdat")?;
        w.write_all(&0u64.to_be_bytes())?;
        Ok(Self {
            w,
            opts,
            tracks,
            mdat_start,
            pos: mdat_start + 16,
        })
    }

    /// Appends one packet. Packets of one stream must arrive in decode order;
    /// across streams they should arrive roughly in time order (the chunking is
    /// per stream, so a stream that runs far ahead only costs interleave quality).
    pub fn write_packet(&mut self, p: &Packet) -> Result<(), MuxError> {
        self.write_packet_with_duration(p, None)
    }

    /// Like [`Self::write_packet`], with the packet's duration in its stream's
    /// timescale ticks when the source knows it. Durations are otherwise
    /// inferred from the next packet's DTS; the hint only matters for the last
    /// sample of a stream, whose duration has no successor to infer it from.
    pub fn write_packet_with_duration(
        &mut self,
        p: &Packet,
        duration: Option<u32>,
    ) -> Result<(), MuxError> {
        let chunk_ticks_ms = i64::from(self.opts.chunk_duration_ms);
        let max_bytes = self.opts.max_chunk_bytes;
        let ti = p.stream_index as usize;
        let t = self
            .tracks
            .get_mut(ti)
            .ok_or_else(|| MuxError::InvalidConfig(format!("packet for unknown stream {ti}")))?;
        let scale = t.info.timescale;
        let dts = rescale(p.dts.value, p.dts.time_base, scale)?;
        let pts = if p.pts.is_none() {
            dts
        } else {
            rescale(p.pts.value, p.pts.time_base, scale)?
        };
        let cts = i32::try_from(pts - dts)
            .map_err(|_| MuxError::InvalidTimestamps("composition offset out of range".into()))?;
        if p.data.len() > u32::MAX as usize {
            return Err(MuxError::InvalidConfig("sample larger than 4 GiB".into()));
        }

        match t.first_dts {
            None => {
                t.first_dts = Some(dts);
                t.first_cts = cts;
                t.pending_start_dts = dts;
            }
            Some(_) => {
                let delta = dts - t.last_dts;
                let delta = u32::try_from(delta).map_err(|_| {
                    MuxError::InvalidTimestamps(format!(
                        "stream {ti}: decode timestamps must not decrease (dts {dts} after {})",
                        t.last_dts
                    ))
                })?;
                push_run(&mut t.stts, delta);
                t.last_delta = delta;
            }
        }
        t.last_dts = dts;
        t.last_duration_hint = duration;
        t.max_pts = t.max_pts.max(pts);

        push_run(&mut t.ctts, cts);
        t.any_ctts |= cts != 0;
        t.any_negative_ctts |= cts < 0;
        t.samples += 1;
        if p.is_key_frame {
            t.sync.push(t.samples);
        } else {
            t.all_key = false;
        }
        t.sizes.push(p.data.len() as u32);
        t.pending.extend_from_slice(&p.data);
        t.pending_samples += 1;

        let elapsed_ms = (dts - t.pending_start_dts) * 1000 / i64::from(scale);
        if elapsed_ms >= chunk_ticks_ms || t.pending.len() >= max_bytes {
            self.flush_chunk(ti)?;
        }
        Ok(())
    }

    fn flush_chunk(&mut self, ti: usize) -> Result<(), MuxError> {
        let t = &mut self.tracks[ti];
        if t.pending_samples == 0 {
            return Ok(());
        }
        self.w.write_all(&t.pending)?;
        t.chunk_offsets.push(self.pos);
        self.pos += t.pending.len() as u64;
        let chunk_no = t.chunk_offsets.len() as u32;
        if t.stsc
            .last()
            .is_none_or(|&(_, spc)| spc != t.pending_samples)
        {
            t.stsc.push((chunk_no, t.pending_samples));
        }
        t.pending.clear();
        t.pending_samples = 0;
        t.pending_start_dts = t.last_dts;
        Ok(())
    }

    /// Flushes the remaining media, writes `moov` and returns the output.
    pub fn finish(mut self) -> Result<W, MuxError> {
        for ti in 0..self.tracks.len() {
            self.flush_chunk(ti)?;
        }
        for t in &mut self.tracks {
            if t.samples > 0 {
                // The last sample has no successor: use the source's duration
                // when given, else repeat the previous one (or a nominal 1/25 s
                // for a one-sample track).
                let d = t.last_duration_hint.unwrap_or(if t.samples > 1 {
                    t.last_delta
                } else {
                    t.info.timescale / 25
                });
                push_run(&mut t.stts, d);
                t.last_delta = d;
            }
        }
        let mdat_end = self.pos;
        self.w.seek(SeekFrom::Start(self.mdat_start + 8))?;
        self.w
            .write_all(&(mdat_end - self.mdat_start).to_be_bytes())?;
        self.w.seek(SeekFrom::Start(mdat_end))?;
        let moov = self.build_moov(mdat_end)?;
        self.w.write_all(&moov)?;
        self.w.flush()?;
        Ok(self.w)
    }

    fn build_moov(&self, max_offset: u64) -> Result<Vec<u8>, MuxError> {
        let co64 = self.opts.force_co64 || max_offset > u64::from(u32::MAX);
        // Global time origin: the earliest first-DTS across tracks, so a track
        // that starts later gets an empty edit instead of silently shifting.
        let us =
            |t: &Track| t.first_dts.unwrap_or(0) as i128 * 1_000_000 / i128::from(t.info.timescale);
        let origin_us = self
            .tracks
            .iter()
            .filter(|t| t.samples > 0)
            .map(us)
            .min()
            .unwrap_or(0);

        let mut movie_duration = 0u64;
        let mut traks = Vec::new();
        for (i, t) in self.tracks.iter().enumerate() {
            let (trak, dur) =
                build_trak(t, i as u32 + 1, co64, ((us(t) - origin_us) / 1000) as u64)?;
            movie_duration = movie_duration.max(dur);
            traks.push(trak);
        }

        let mut mvhd = Vec::new();
        mvhd.extend_from_slice(&[0u8; 8]); // creation, modification
        mvhd.extend_from_slice(&(MOVIE_TIMESCALE as u32).to_be_bytes());
        mvhd.extend_from_slice(&(movie_duration.min(u64::from(u32::MAX)) as u32).to_be_bytes());
        mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
        mvhd.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
        mvhd.extend_from_slice(&[0u8; 10]); // reserved
        mvhd.extend_from_slice(&MATRIX);
        mvhd.extend_from_slice(&[0u8; 24]); // pre_defined
        mvhd.extend_from_slice(&(self.tracks.len() as u32 + 1).to_be_bytes());

        let mut body = full_box(b"mvhd", 0, 0, &mvhd);
        for t in traks {
            body.extend(t);
        }
        Ok(boxed(b"moov", &body))
    }
}

const MATRIX: [u8; 36] = [
    0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0, 0, 0,
];

fn build_ftyp(streams: &[StreamInfo]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(b"isom");
    body.extend_from_slice(&512u32.to_be_bytes());
    let mut brands: Vec<&[u8; 4]> = vec![b"isom", b"iso2"];
    if streams.iter().any(|s| s.codec == CodecId::H264) {
        brands.push(b"avc1");
    }
    if streams.iter().any(|s| s.codec == CodecId::Av1) {
        brands.push(b"av01");
    }
    brands.push(b"mp41");
    for b in brands {
        body.extend_from_slice(b);
    }
    boxed(b"ftyp", &body)
}

fn be32s(v: impl IntoIterator<Item = u32>) -> Vec<u8> {
    v.into_iter().flat_map(u32::to_be_bytes).collect()
}

/// Builds one `trak`; returns it with the track's presentation duration in
/// movie-timescale ticks (including any leading empty edit).
fn build_trak(
    t: &Track,
    track_id: u32,
    co64: bool,
    start_delay_ms: u64,
) -> Result<(Vec<u8>, u64), MuxError> {
    let scale = u64::from(t.info.timescale);
    let media_dur: u64 = t
        .stts
        .iter()
        .map(|&(n, d)| u64::from(n) * u64::from(d))
        .sum();
    let media_time = t
        .info
        .edit_media_time
        .unwrap_or(i64::from(t.first_cts))
        .max(0) as u64;

    // Edit list: an optional empty edit (late start) then the media edit that
    // skips `media_time` ticks (codec priming / B-frame composition delay).
    // Presentation runs to the end of the latest-presented sample, which for
    // B-frame streams lies past the last decode time by the composition delay.
    let present_end = (t.max_pts.max(0) as u64 + u64::from(t.last_delta)).max(media_dur);
    let seg_ms = present_end.saturating_sub(media_time) * MOVIE_TIMESCALE / scale;
    let need_edits = start_delay_ms > 0 || media_time > 0;
    let track_dur_ms = if need_edits {
        start_delay_ms + seg_ms
    } else {
        media_dur * MOVIE_TIMESCALE / scale
    };
    let edts = if need_edits {
        let mut e = Vec::new();
        let mut count = 1u32;
        if start_delay_ms > 0 {
            count += 1;
            e.extend_from_slice(&(start_delay_ms as u32).to_be_bytes());
            e.extend_from_slice(&(-1i32).to_be_bytes());
            e.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        }
        e.extend_from_slice(&(seg_ms as u32).to_be_bytes());
        e.extend_from_slice(&(media_time.min(i32::MAX as u64) as i32).to_be_bytes());
        e.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        let mut body = count.to_be_bytes().to_vec();
        body.extend(e);
        boxed(b"edts", &full_box(b"elst", 0, 0, &body))
    } else {
        Vec::new()
    };

    let is_video = t.info.media_type == MediaType::Video;
    let mut tkhd = vec![0u8; 8];
    tkhd.extend_from_slice(&track_id.to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 4]);
    tkhd.extend_from_slice(&(track_dur_ms.min(u64::from(u32::MAX)) as u32).to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 8]); // reserved
    tkhd.extend_from_slice(&[0u8; 4]); // layer, alternate group
    tkhd.extend_from_slice(&(if is_video { 0u16 } else { 0x0100 }).to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 2]);
    tkhd.extend_from_slice(&MATRIX);
    tkhd.extend_from_slice(&(t.info.width << 16).to_be_bytes());
    tkhd.extend_from_slice(&(t.info.height << 16).to_be_bytes());

    let mut mdhd = vec![0u8; 8];
    mdhd.extend_from_slice(&t.info.timescale.to_be_bytes());
    mdhd.extend_from_slice(&(media_dur.min(u64::from(u32::MAX)) as u32).to_be_bytes());
    mdhd.extend_from_slice(&0x55C4u16.to_be_bytes()); // "und"
    mdhd.extend_from_slice(&[0u8; 2]);

    let (handler, name) = handler_for(t.info.media_type)?;
    let mut hdlr = vec![0u8; 4];
    hdlr.extend_from_slice(&handler);
    hdlr.extend_from_slice(&[0u8; 12]);
    hdlr.extend_from_slice(name.as_bytes());
    hdlr.push(0);

    let media_header = if is_video {
        full_box(b"vmhd", 0, 1, &[0u8; 8])
    } else {
        full_box(b"smhd", 0, 0, &[0u8; 4])
    };
    let dinf = boxed(
        b"dinf",
        &full_box(
            b"dref",
            0,
            0,
            &[&1u32.to_be_bytes()[..], &full_box(b"url ", 0, 1, &[])].concat(),
        ),
    );

    // Sample tables.
    let mut stbl = Vec::new();
    stbl.extend(full_box(
        b"stsd",
        0,
        0,
        &[&1u32.to_be_bytes()[..], &t.entry].concat(),
    ));
    let mut stts = (t.stts.len() as u32).to_be_bytes().to_vec();
    for &(n, d) in &t.stts {
        stts.extend(n.to_be_bytes());
        stts.extend(d.to_be_bytes());
    }
    stbl.extend(full_box(b"stts", 0, 0, &stts));
    if t.any_ctts {
        let mut c = (t.ctts.len() as u32).to_be_bytes().to_vec();
        for &(n, o) in &t.ctts {
            c.extend(n.to_be_bytes());
            c.extend((o as u32).to_be_bytes());
        }
        stbl.extend(full_box(b"ctts", u8::from(t.any_negative_ctts), 0, &c));
    }
    if !t.all_key {
        let mut s = (t.sync.len() as u32).to_be_bytes().to_vec();
        s.extend(be32s(t.sync.iter().copied()));
        stbl.extend(full_box(b"stss", 0, 0, &s));
    }
    let mut stsc = (t.stsc.len() as u32).to_be_bytes().to_vec();
    for &(first, spc) in &t.stsc {
        stsc.extend(first.to_be_bytes());
        stsc.extend(spc.to_be_bytes());
        stsc.extend(1u32.to_be_bytes());
    }
    stbl.extend(full_box(b"stsc", 0, 0, &stsc));
    let uniform = t
        .sizes
        .first()
        .is_some_and(|&f| t.sizes.iter().all(|&s| s == f));
    let mut stsz = if uniform {
        t.sizes[0].to_be_bytes().to_vec()
    } else {
        0u32.to_be_bytes().to_vec()
    };
    stsz.extend((t.sizes.len() as u32).to_be_bytes());
    if !uniform {
        stsz.extend(be32s(t.sizes.iter().copied()));
    }
    stbl.extend(full_box(b"stsz", 0, 0, &stsz));
    if co64 {
        let mut c = (t.chunk_offsets.len() as u32).to_be_bytes().to_vec();
        for &o in &t.chunk_offsets {
            c.extend(o.to_be_bytes());
        }
        stbl.extend(full_box(b"co64", 0, 0, &c));
    } else {
        let mut c = (t.chunk_offsets.len() as u32).to_be_bytes().to_vec();
        for &o in &t.chunk_offsets {
            c.extend((o as u32).to_be_bytes());
        }
        stbl.extend(full_box(b"stco", 0, 0, &c));
    }

    let minf = boxed(
        b"minf",
        &[media_header, dinf, boxed(b"stbl", &stbl)].concat(),
    );
    let mdia = boxed(
        b"mdia",
        &[
            full_box(b"mdhd", 0, 0, &mdhd),
            full_box(b"hdlr", 0, 0, &hdlr),
            minf,
        ]
        .concat(),
    );
    let trak = boxed(
        b"trak",
        &[full_box(b"tkhd", 0, 3, &tkhd), edts, mdia].concat(),
    );
    Ok((trak, track_dur_ms))
}
