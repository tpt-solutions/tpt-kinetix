//! Fragmented MP4 (`moof` / `traf` / `trun`) sample indexing.
//!
//! A fragmented file keeps only codec configuration in `moov` (plus an `mvex`
//! with per-track defaults, the `trex` boxes); samples live in a sequence of
//! `moof` + `mdat` pairs. [`parse_moof`] turns one `moof` into [`SampleRef`]s
//! with absolute file offsets, so the same reader serves progressive and
//! fragmented files.
//!
//! All counts and sizes come from the file, so everything is bounds-checked and
//! capped (samples per track, `trun` entries per box).

use anyhow::{bail, Result};

use super::container::{walk_boxes, Mp4Track};
use super::reader::{SampleRef, MAX_SAMPLES_PER_TRACK};

/// Per-track defaults from `mvex/trex`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Trex {
    pub track_id: u32,
    pub default_duration: u32,
    pub default_size: u32,
    pub default_flags: u32,
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// Whether a `moov` payload declares fragments (`mvex`).
pub fn is_fragmented(moov_payload: &[u8]) -> bool {
    walk_boxes(moov_payload).any(|(k, _)| &k == b"mvex")
}

/// The `trex` defaults in a `moov` payload.
pub fn parse_trex_list(moov_payload: &[u8]) -> Vec<Trex> {
    let mut out = Vec::new();
    for (kind, mvex) in walk_boxes(moov_payload) {
        if &kind != b"mvex" {
            continue;
        }
        for (k, p) in walk_boxes(mvex) {
            if &k == b"trex" {
                // version/flags, track_ID, desc_index, duration, size, flags
                if let (Some(id), Some(dur), Some(size), Some(flags)) =
                    (be32(p, 4), be32(p, 12), be32(p, 16), be32(p, 20))
                {
                    out.push(Trex {
                        track_id: id,
                        default_duration: dur,
                        default_size: size,
                        default_flags: flags,
                    });
                }
            }
        }
    }
    out
}

/// The fields of a `tfhd` this parser uses.
struct Tfhd {
    flags: u32,
    track_id: u32,
    base_data_offset: Option<u64>,
    default_duration: Option<u32>,
    default_size: Option<u32>,
    default_flags: Option<u32>,
}

/// `sample_is_non_sync_sample` (ISO/IEC 14496-12 §8.8.3.1).
const NON_SYNC: u32 = 0x0001_0000;

/// Appends the samples of one `moof` to `out` (indexed like `tracks`).
///
/// `moof_start` is the file offset of the `moof` box; `payload` is its bytes
/// after the box header. `next_dts[i]` is the running decode time of track `i`
/// and is advanced past this fragment.
pub fn parse_moof(
    moof_start: u64,
    payload: &[u8],
    trex: &[Trex],
    tracks: &[Mp4Track],
    next_dts: &mut [u64],
    out: &mut [Vec<SampleRef>],
) -> Result<()> {
    // Where the next track fragment's data starts when no explicit base exists:
    // the end of the previous one (ISO/IEC 14496-12 §8.8.7).
    let mut prev_data_end: Option<u64> = None;
    for (kind, traf) in walk_boxes(payload) {
        if &kind != b"traf" {
            continue;
        }
        let mut tfhd: Option<Tfhd> = None;
        let mut tfdt: Option<u64> = None;
        let mut truns: Vec<&[u8]> = Vec::new();
        for (k, p) in walk_boxes(traf) {
            match &k {
                b"tfhd" => {
                    let flags = be32(p, 0).unwrap_or(0) & 0x00FF_FFFF;
                    let Some(id) = be32(p, 4) else { continue };
                    let mut at = 8usize;
                    let take32 = |at: &mut usize, cond: bool| -> Option<u32> {
                        if !cond {
                            return None;
                        }
                        let v = be32(p, *at)?;
                        *at += 4;
                        Some(v)
                    };
                    let base = if flags & 0x1 != 0 {
                        let v = be64(p, at);
                        at += 8;
                        v
                    } else {
                        None
                    };
                    let _desc = take32(&mut at, flags & 0x2 != 0);
                    let dur = take32(&mut at, flags & 0x8 != 0);
                    let size = take32(&mut at, flags & 0x10 != 0);
                    let fl = take32(&mut at, flags & 0x20 != 0);
                    tfhd = Some(Tfhd {
                        flags,
                        track_id: id,
                        base_data_offset: base,
                        default_duration: dur,
                        default_size: size,
                        default_flags: fl,
                    });
                }
                b"tfdt" => {
                    let version = p.first().copied().unwrap_or(0);
                    tfdt = if version == 1 {
                        be64(p, 4)
                    } else {
                        be32(p, 4).map(u64::from)
                    };
                }
                b"trun" => truns.push(p),
                _ => {}
            }
        }
        let Some(Tfhd {
            flags,
            track_id: id,
            base_data_offset: explicit_base,
            default_duration: def_dur,
            default_size: def_size,
            default_flags: def_flags,
        }) = tfhd
        else {
            continue;
        };
        let Some(ti) = tracks.iter().position(|t| t.track_id == id) else {
            continue; // a track we did not parse (e.g. non-audio/video)
        };
        let tx = trex
            .iter()
            .find(|t| t.track_id == id)
            .copied()
            .unwrap_or_default();
        let (def_dur, def_size, def_flags) = (
            def_dur.unwrap_or(tx.default_duration),
            def_size.unwrap_or(tx.default_size),
            def_flags.unwrap_or(tx.default_flags),
        );
        let base = if let Some(b) = explicit_base {
            b
        } else if flags & 0x2_0000 != 0 {
            moof_start
        } else {
            prev_data_end.unwrap_or(moof_start)
        };
        if let Some(t) = tfdt {
            next_dts[ti] = t;
        }
        let mut cursor = base;
        for trun in truns {
            let tflags = be32(trun, 0).unwrap_or(0) & 0x00FF_FFFF;
            let version = trun.first().copied().unwrap_or(0);
            let Some(count) = be32(trun, 4) else { continue };
            let mut at = 8usize;
            if tflags & 0x1 != 0 {
                let off = be32(trun, at).map(|v| v as i32);
                at += 4;
                let Some(off) = off else {
                    bail!("truncated trun")
                };
                cursor = (base as i64)
                    .checked_add(i64::from(off))
                    .and_then(|v| u64::try_from(v).ok())
                    .ok_or_else(|| anyhow::anyhow!("trun data offset out of range"))?;
            }
            let first_flags = if tflags & 0x4 != 0 {
                let v = be32(trun, at);
                at += 4;
                v
            } else {
                None
            };
            let per_sample = 4 * usize::from(tflags & 0x100 != 0)
                + 4 * usize::from(tflags & 0x200 != 0)
                + 4 * usize::from(tflags & 0x400 != 0)
                + 4 * usize::from(tflags & 0x800 != 0);
            // `count` is untrusted: it must be backed by real bytes.
            if per_sample > 0 && (count as usize) > trun.len().saturating_sub(at) / per_sample {
                bail!(
                    "trun claims {count} samples but has {} bytes",
                    trun.len().saturating_sub(at)
                );
            }
            for i in 0..count as usize {
                if out[ti].len() >= MAX_SAMPLES_PER_TRACK {
                    bail!("track {id} exceeds {MAX_SAMPLES_PER_TRACK} samples");
                }
                let mut field = |present: bool, default: u32| -> u32 {
                    if present {
                        let v = be32(trun, at).unwrap_or(default);
                        at += 4;
                        v
                    } else {
                        default
                    }
                };
                let duration = field(tflags & 0x100 != 0, def_dur);
                let size = field(tflags & 0x200 != 0, def_size);
                let sflags = field(tflags & 0x400 != 0, def_flags);
                let cts_raw = field(tflags & 0x800 != 0, 0);
                let sflags = if i == 0 {
                    first_flags.unwrap_or(sflags)
                } else {
                    sflags
                };
                let cts_offset = if version == 1 {
                    cts_raw as i32
                } else {
                    cts_raw.min(i32::MAX as u32) as i32
                };
                out[ti].push(SampleRef {
                    offset: cursor,
                    size,
                    dts: next_dts[ti],
                    cts_offset,
                    duration,
                    is_key: sflags & NON_SYNC == 0,
                });
                next_dts[ti] = next_dts[ti].saturating_add(u64::from(duration));
                cursor = cursor.saturating_add(u64::from(size));
            }
        }
        prev_data_end = Some(cursor);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(kind: &[u8; 4], p: &[u8]) -> Vec<u8> {
        let mut v = (p.len() as u32 + 8).to_be_bytes().to_vec();
        v.extend(kind);
        v.extend(p);
        v
    }

    #[test]
    fn untrusted_trun_counts_are_rejected_not_allocated() {
        // trun: flags = duration|size present, count = 4 billion, no sample bytes.
        let mut trun = vec![0, 0, 0x03, 0x00];
        trun.extend(0xFFFF_FFFFu32.to_be_bytes());
        let tfhd = [vec![0u8, 0x02, 0, 0], 1u32.to_be_bytes().to_vec()].concat();
        let traf = [boxed(b"tfhd", &tfhd), boxed(b"trun", &trun)].concat();
        let moof = boxed(b"traf", &traf);
        let mut t = Mp4TrackStub::track(1);
        let mut dts = vec![0u64];
        let mut out = vec![Vec::new()];
        let tracks = [t.take()];
        assert!(parse_moof(0, &moof, &[], &tracks, &mut dts, &mut out).is_err());
        assert!(out[0].is_empty());
    }

    struct Mp4TrackStub(Option<Mp4Track>);
    impl Mp4TrackStub {
        fn track(id: u32) -> Self {
            use super::super::boxes::{CttsBox, StscBox, StszBox, SttsBox};
            use tpt_kinetix_core::codec::MediaType;
            Self(Some(Mp4Track {
                track_id: id,
                timescale: 1000,
                duration: 0,
                handler_type: *b"vide",
                media_type: MediaType::Video,
                codec: None,
                width: 0,
                height: 0,
                stts: SttsBox { entries: vec![] },
                stss: None,
                stsz: StszBox {
                    default_size: 0,
                    sample_sizes: vec![],
                },
                chunk_offsets: vec![],
                stsc: StscBox { entries: vec![] },
                ctts: CttsBox::default(),
                edit_media_time: None,
                extradata: vec![],
                channels: 0,
                sample_rate: 0,
                bits_per_sample: 0,
            }))
        }
        fn take(&mut self) -> Mp4Track {
            self.0.take().unwrap()
        }
    }
}
