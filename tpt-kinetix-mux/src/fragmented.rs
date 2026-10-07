//! Fragmented MP4 (fMP4 / CMAF-style) writer.
//!
//! [`FragmentWriter`] produces what live streaming, HLS-fMP4 and DASH need: one
//! **initialization segment** (`ftyp` + a `moov` that carries only codec
//! configuration and an `mvex`) and a sequence of **media fragments**
//! (`moof` + `mdat`), each independently addressable. Nothing requires seeking
//! or the whole file, so the output can be any [`std::io::Write`] (a socket, a
//! pipe, an HTTP response) and memory is bounded by one fragment.
//!
//! The caller decides where fragments end (typically at video key frames, about
//! every 2-6 s): push packets with [`FragmentWriter::push`] and call
//! [`FragmentWriter::flush`] at each boundary.
//!
//! Timestamps are preserved: each track fragment's `tfdt` is its first sample's
//! decode time, and composition offsets are written as signed `trun` values.

use tpt_kinetix_core::codec::CodecId;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::stream::StreamInfo;

use crate::entries::{boxed, full_box, handler_for, sample_entry};
use crate::MuxError;

const MOVIE_TIMESCALE: u32 = 1000;
const SYNC_FLAGS: u32 = 0x0200_0000;
const NON_SYNC_FLAGS: u32 = 0x0101_0000;

struct Sample {
    dts: i64,
    cts: i32,
    duration: Option<u32>,
    key: bool,
    data: Vec<u8>,
}

struct Track {
    info: StreamInfo,
    entry: Vec<u8>,
    pending: Vec<Sample>,
    last_dts: Option<i64>,
}

/// Writes fMP4 initialization and media segments.
pub struct FragmentWriter {
    tracks: Vec<Track>,
    sequence: u32,
    segment_index: bool,
}

fn rescale(value: i64, from: (u32, u32), to_scale: u32) -> Result<i64, MuxError> {
    let (num, den) = (i128::from(from.0), i128::from(from.1));
    if den == 0 {
        return Err(MuxError::InvalidTimestamps(
            "time base with a zero denominator".into(),
        ));
    }
    let n = i128::from(value) * num * i128::from(to_scale);
    let r = if n >= 0 {
        (n + den / 2) / den
    } else {
        -((-n + den / 2) / den)
    };
    i64::try_from(r).map_err(|_| MuxError::InvalidTimestamps("timestamp overflow".into()))
}

impl FragmentWriter {
    /// One track per `streams` entry (packets reference them by `stream_index`).
    pub fn new(streams: &[StreamInfo]) -> Result<Self, MuxError> {
        if streams.is_empty() {
            return Err(MuxError::InvalidConfig(
                "at least one stream is required".into(),
            ));
        }
        let mut tracks = Vec::new();
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
                last_dts: None,
            });
        }
        Ok(Self {
            tracks,
            sequence: 1,
            segment_index: false,
        })
    }

    /// Makes every flushed fragment a self-contained DASH-IF / CMAF *media
    /// segment*: a `styp` box, then (for a single-track writer) a `sidx` box
    /// describing the fragment (earliest presentation time, duration, SAP), then
    /// `moof` + `mdat`. Off by default (live parts and plain fragments are bare
    /// `moof` + `mdat`).
    pub fn with_segment_index(mut self, on: bool) -> Self {
        self.segment_index = on;
        self
    }

    /// Starts fragment sequence numbers at `n` instead of 1 (so independently
    /// generated segments of one presentation carry their own numbers).
    pub fn starting_sequence(mut self, n: u32) -> Self {
        self.sequence = n.max(1);
        self
    }

    /// The initialization segment (`ftyp` + `moov`).
    pub fn init_segment(&self) -> Vec<u8> {
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"iso6");
        ftyp.extend_from_slice(&1u32.to_be_bytes());
        let mut brands: Vec<&[u8; 4]> = vec![b"iso6", b"cmfc", b"isom", b"mp41"];
        if self.tracks.iter().any(|t| t.info.codec == CodecId::H264) {
            brands.push(b"avc1");
        }
        if self.tracks.iter().any(|t| t.info.codec == CodecId::Av1) {
            brands.push(b"av01");
        }
        for b in brands {
            ftyp.extend_from_slice(b);
        }

        let mut mvhd = vec![0u8; 8];
        mvhd.extend_from_slice(&MOVIE_TIMESCALE.to_be_bytes());
        mvhd.extend_from_slice(&0u32.to_be_bytes()); // duration unknown
        mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        mvhd.extend_from_slice(&0x0100u16.to_be_bytes());
        mvhd.extend_from_slice(&[0u8; 10]);
        mvhd.extend_from_slice(&crate::writer::MATRIX);
        mvhd.extend_from_slice(&[0u8; 24]);
        mvhd.extend_from_slice(&(self.tracks.len() as u32 + 1).to_be_bytes());

        let mut moov = full_box(b"mvhd", 0, 0, &mvhd);
        let mut mvex = Vec::new();
        for (i, t) in self.tracks.iter().enumerate() {
            let id = i as u32 + 1;
            moov.extend(init_trak(t, id));
            let mut trex = id.to_be_bytes().to_vec();
            trex.extend_from_slice(&1u32.to_be_bytes()); // default sample description
            trex.extend_from_slice(&[0u8; 12]); // default duration, size, flags
            mvex.extend(full_box(b"trex", 0, 0, &trex));
        }
        moov.extend(boxed(b"mvex", &mvex));
        [boxed(b"ftyp", &ftyp), boxed(b"moov", &moov)].concat()
    }

    /// Buffers one packet for the next fragment. `duration` is the sample's
    /// duration in its stream's timescale when the source knows it; otherwise it
    /// is inferred from the next packet's decode time.
    pub fn push(&mut self, p: &Packet, duration: Option<u32>) -> Result<(), MuxError> {
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
        if let Some(prev) = t.last_dts {
            let delta = dts - prev;
            let delta = u32::try_from(delta).map_err(|_| {
                MuxError::InvalidTimestamps(format!(
                    "stream {ti}: decode timestamps must not decrease"
                ))
            })?;
            if let Some(last) = t.pending.last_mut() {
                // The previous sample's duration is its distance to this one.
                last.duration = Some(delta);
            }
        }
        t.last_dts = Some(dts);
        t.pending.push(Sample {
            dts,
            cts,
            duration,
            key: p.is_key_frame,
            data: p.data.clone(),
        });
        Ok(())
    }

    /// Number of samples buffered for stream `stream`.
    pub fn buffered(&self, stream: usize) -> usize {
        self.tracks.get(stream).map_or(0, |t| t.pending.len())
    }

    /// Buffered duration of stream `stream` in milliseconds (decode-time span
    /// from its first buffered sample to its latest).
    pub fn buffered_ms(&self, stream: usize) -> i64 {
        let Some(t) = self.tracks.get(stream) else {
            return 0;
        };
        match (t.pending.first(), t.pending.last()) {
            (Some(a), Some(b)) => (b.dts - a.dts) * 1000 / i64::from(t.info.timescale),
            _ => 0,
        }
    }

    /// Emits the buffered samples as one `moof` + `mdat` fragment, or `None` if
    /// there is nothing to write. Unless `finalize` is set, each track's newest
    /// sample is held back when its duration is still unknown (it joins the next
    /// fragment); with `finalize`, it takes the previous duration.
    pub fn flush(&mut self, finalize: bool) -> Result<Option<Vec<u8>>, MuxError> {
        let mut taken: Vec<Vec<Sample>> = Vec::with_capacity(self.tracks.len());
        for t in &mut self.tracks {
            let mut out: Vec<Sample> = std::mem::take(&mut t.pending);
            if let Some(last) = out.last() {
                if last.duration.is_none() {
                    if finalize {
                        let prev = out.len().checked_sub(2).and_then(|i| out[i].duration);
                        let d = prev.unwrap_or(t.info.timescale / 25);
                        out.last_mut().unwrap().duration = Some(d);
                    } else {
                        t.pending.push(out.pop().unwrap());
                    }
                }
            }
            taken.push(out);
        }
        if taken.iter().all(Vec::is_empty) {
            return Ok(None);
        }

        let seq = self.sequence;
        self.sequence += 1;
        // Pass 1 with zero data offsets to learn the moof size, pass 2 for real.
        let zeros = vec![0i32; taken.len()];
        let moof_len = build_moof(seq, &taken, &zeros).len();
        let mut offsets = Vec::with_capacity(taken.len());
        let mut running = moof_len as u64 + 8;
        for samples in &taken {
            offsets.push(running as i32);
            running += samples.iter().map(|s| s.data.len() as u64).sum::<u64>();
        }
        let moof = build_moof(seq, &taken, &offsets);
        debug_assert_eq!(moof.len(), moof_len);

        let data_len: usize = taken.iter().flatten().map(|s| s.data.len()).sum();
        let mut out = if self.segment_index {
            let mut head = boxed(b"styp", &styp_payload());
            if taken.len() == 1 && !taken[0].is_empty() {
                let ref_len = (moof_len + 8 + data_len) as u32;
                head.extend(build_sidx(&self.tracks[0], &taken[0], ref_len));
            }
            head.extend(moof);
            head
        } else {
            moof
        };
        out.reserve(data_len + 16);
        if data_len + 8 > u32::MAX as usize {
            return Err(MuxError::Unsupported("fragment larger than 4 GiB".into()));
        }
        out.extend_from_slice(&(data_len as u32 + 8).to_be_bytes());
        out.extend_from_slice(b"mdat");
        for s in taken.iter().flatten() {
            out.extend_from_slice(&s.data);
        }
        Ok(Some(out))
    }
}

/// `styp` payload: major brand `msdh` (CMAF media segment), compatible `msdh`
/// and `msix` (segment index present).
fn styp_payload() -> Vec<u8> {
    let mut v = b"msdh".to_vec();
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(b"msdh");
    v.extend_from_slice(b"msix");
    v
}

/// A version-1 `sidx` with one reference: the fragment (`moof` + `mdat`,
/// `ref_len` bytes) that follows it.
fn build_sidx(track: &Track, samples: &[Sample], ref_len: u32) -> Vec<u8> {
    let earliest = samples
        .iter()
        .map(|s| s.dts + i64::from(s.cts))
        .min()
        .unwrap_or(0)
        .max(0) as u64;
    let duration: u64 = samples
        .iter()
        .map(|s| u64::from(s.duration.unwrap_or(0)))
        .sum();
    let sap = samples[0].key;
    let mut p = Vec::new();
    p.extend_from_slice(&1u32.to_be_bytes()); // reference_ID (track id)
    p.extend_from_slice(&track.info.timescale.to_be_bytes());
    p.extend_from_slice(&earliest.to_be_bytes());
    p.extend_from_slice(&0u64.to_be_bytes()); // first_offset
    p.extend_from_slice(&0u16.to_be_bytes()); // reserved
    p.extend_from_slice(&1u16.to_be_bytes()); // reference_count
    p.extend_from_slice(&(ref_len & 0x7FFF_FFFF).to_be_bytes()); // type 0 = media
    p.extend_from_slice(&(duration.min(u64::from(u32::MAX)) as u32).to_be_bytes());
    let sap_word: u32 = if sap { 0x9000_0000 } else { 0 }; // starts_with_SAP, SAP_type 1
    p.extend_from_slice(&sap_word.to_be_bytes());
    full_box(b"sidx", 1, 0, &p)
}

fn init_trak(t: &Track, track_id: u32) -> Vec<u8> {
    let is_video = t.info.media_type == tpt_kinetix_core::codec::MediaType::Video;
    let mut tkhd = vec![0u8; 8];
    tkhd.extend_from_slice(&track_id.to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 4]);
    tkhd.extend_from_slice(&0u32.to_be_bytes()); // duration unknown
    tkhd.extend_from_slice(&[0u8; 8]);
    tkhd.extend_from_slice(&[0u8; 4]);
    tkhd.extend_from_slice(&(if is_video { 0u16 } else { 0x0100 }).to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 2]);
    tkhd.extend_from_slice(&crate::writer::MATRIX);
    tkhd.extend_from_slice(&(t.info.width << 16).to_be_bytes());
    tkhd.extend_from_slice(&(t.info.height << 16).to_be_bytes());

    let mut mdhd = vec![0u8; 8];
    mdhd.extend_from_slice(&t.info.timescale.to_be_bytes());
    mdhd.extend_from_slice(&0u32.to_be_bytes());
    mdhd.extend_from_slice(&0x55C4u16.to_be_bytes());
    mdhd.extend_from_slice(&[0u8; 2]);

    let (handler, name) = handler_for(t.info.media_type).expect("validated in new()");
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
    // Empty sample tables: the samples are in the fragments.
    let empty = 0u32.to_be_bytes();
    let stbl = [
        full_box(b"stsd", 0, 0, &[&1u32.to_be_bytes()[..], &t.entry].concat()),
        full_box(b"stts", 0, 0, &empty),
        full_box(b"stsc", 0, 0, &empty),
        full_box(b"stsz", 0, 0, &[0u8; 8]),
        full_box(b"stco", 0, 0, &empty),
    ]
    .concat();
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
    // Keep the source's media edit (AAC priming, a muxer's composition delay).
    let edts = match t.info.edit_media_time {
        Some(mt) if mt > 0 => {
            let mut e = 1u32.to_be_bytes().to_vec();
            e.extend_from_slice(&0u32.to_be_bytes()); // segment duration: to the end
            e.extend_from_slice(&(mt.min(i64::from(i32::MAX)) as i32).to_be_bytes());
            e.extend_from_slice(&0x0001_0000u32.to_be_bytes());
            boxed(b"edts", &full_box(b"elst", 0, 0, &e))
        }
        _ => Vec::new(),
    };
    boxed(
        b"trak",
        &[full_box(b"tkhd", 0, 3, &tkhd), edts, mdia].concat(),
    )
}

fn build_moof(sequence: u32, tracks: &[Vec<Sample>], data_offsets: &[i32]) -> Vec<u8> {
    let mut moof = full_box(b"mfhd", 0, 0, &sequence.to_be_bytes());
    for (i, samples) in tracks.iter().enumerate() {
        if samples.is_empty() {
            continue;
        }
        let id = i as u32 + 1;
        // default-base-is-moof: data offsets are relative to the moof start.
        let tfhd = full_box(b"tfhd", 0, 0x02_0000, &id.to_be_bytes());
        let tfdt = full_box(b"tfdt", 1, 0, &(samples[0].dts.max(0) as u64).to_be_bytes());
        // trun v1: data offset, duration, size, flags, signed composition offset.
        let mut trun = (samples.len() as u32).to_be_bytes().to_vec();
        trun.extend_from_slice(&data_offsets.get(i).copied().unwrap_or(0).to_be_bytes());
        for s in samples {
            trun.extend_from_slice(&s.duration.unwrap_or(0).to_be_bytes());
            trun.extend_from_slice(&(s.data.len() as u32).to_be_bytes());
            trun.extend_from_slice(
                &(if s.key { SYNC_FLAGS } else { NON_SYNC_FLAGS }).to_be_bytes(),
            );
            trun.extend_from_slice(&(s.cts as u32).to_be_bytes());
        }
        let trun = full_box(b"trun", 1, 0x000F01, &trun);
        moof.extend(boxed(b"traf", &[tfhd, tfdt, trun].concat()));
    }
    boxed(b"moof", &moof)
}
