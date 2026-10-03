//! Key-frame-aligned segment planning from an MP4 sample index.

use std::ops::Range;

use tpt_kinetix_core::codec::MediaType;
use tpt_kinetix_demux::Mp4Index;

/// One segment: for every packaged track, the sample range it contains.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    /// Sample range per packaged track (empty when a track has nothing in this
    /// time span, e.g. an audio track shorter than the video).
    pub samples: Vec<Range<usize>>,
    /// Duration of each track's span in seconds (its earliest presentation time
    /// to the next segment's, or the track's end for the last one).
    pub seconds: Vec<f64>,
    /// Earliest presentation time of each track in the segment, in track ticks
    /// (the DASH timeline `t`; the fragment's `tfdt` is the first sample's decode time).
    pub start_ticks: Vec<u64>,
    /// Duration of each track's span in track ticks (DASH timeline `d`).
    pub ticks: Vec<u64>,
}

/// The segment grid shared by all tracks.
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentPlan {
    /// Segments in presentation order (segment `n` of the URIs is `segments[n - 1]`).
    pub segments: Vec<Segment>,
    /// The packaged track that decides the boundaries (the first video track,
    /// else the first track).
    pub lead: usize,
}

fn us(dts: u64, timescale: u32) -> u128 {
    u128::from(dts) * 1_000_000 / u128::from(timescale.max(1))
}

/// Plans segments over the tracks `source_indices` (indices into `index`).
///
/// Boundaries are chosen on the lead track at key frames once `target_seconds`
/// have elapsed; every other track is cut at the first sample at or after the
/// same instant, so the segments of all renditions line up in time.
pub(crate) fn plan_segments(
    index: &Mp4Index,
    source_indices: &[usize],
    target_seconds: f64,
) -> SegmentPlan {
    let tracks = index.tracks();
    let lead = source_indices
        .iter()
        .position(|&i| tracks[i].media_type == MediaType::Video)
        .unwrap_or(0);
    let lead_src = source_indices[lead];
    let lead_samples = index.samples(lead_src);
    let lead_scale = tracks[lead_src].timescale.max(1);
    let target = target_seconds.max(0.1);

    // Boundary sample indices on the lead track.
    let mut lead_starts = vec![0usize];
    if let Some(first) = lead_samples.first() {
        let mut seg_start = first.dts;
        for (i, s) in lead_samples.iter().enumerate().skip(1) {
            if s.is_key && (s.dts - seg_start) as f64 / f64::from(lead_scale) >= target {
                lead_starts.push(i);
                seg_start = s.dts;
            }
        }
    }

    // Per-track start sample of each segment.
    let starts: Vec<Vec<usize>> = source_indices
        .iter()
        .enumerate()
        .map(|(p, &src)| {
            if p == lead {
                return lead_starts.clone();
            }
            let samples = index.samples(src);
            let scale = tracks[src].timescale;
            lead_starts
                .iter()
                .enumerate()
                .map(|(k, &ls)| {
                    if k == 0 {
                        0
                    } else {
                        let boundary = us(lead_samples[ls].dts, lead_scale);
                        samples.partition_point(|s| us(s.dts, scale) < boundary)
                    }
                })
                .collect()
        })
        .collect();

    // Per track and segment: the sample range and the earliest presentation
    // time (dts + composition offset) in it. A segment's timeline entry and its
    // duration are defined on presentation time, not decode time: for B-frame
    // streams the two differ by the composition delay.
    let nseg = lead_starts.len();
    let mut ranges: Vec<Vec<Range<usize>>> = Vec::new();
    let mut min_pts: Vec<Vec<Option<i64>>> = Vec::new();
    let mut end_pts: Vec<i64> = Vec::new();
    for (p, &src) in source_indices.iter().enumerate() {
        let samples = index.samples(src);
        let rs: Vec<Range<usize>> = (0..nseg)
            .map(|k| {
                let end = starts[p]
                    .get(k + 1)
                    .copied()
                    .unwrap_or(samples.len())
                    .min(samples.len());
                starts[p][k].min(end)..end
            })
            .collect();
        let pts = |s: &tpt_kinetix_demux::mp4::SampleRef| s.dts as i64 + i64::from(s.cts_offset);
        min_pts.push(
            rs.iter()
                .map(|r| samples[r.clone()].iter().map(pts).min())
                .collect(),
        );
        end_pts.push(
            samples
                .iter()
                .map(|s| pts(s) + i64::from(s.duration))
                .max()
                .unwrap_or(0),
        );
        ranges.push(rs);
    }

    let mut segments = Vec::with_capacity(nseg);
    for k in 0..nseg {
        let mut seg = Segment {
            samples: Vec::new(),
            seconds: Vec::new(),
            start_ticks: Vec::new(),
            ticks: Vec::new(),
        };
        for (p, &src) in source_indices.iter().enumerate() {
            let (start, ticks) = match min_pts[p][k] {
                Some(start) => {
                    // The span runs to the next non-empty segment's start, or to
                    // the end of the track's presentation.
                    let stop = min_pts[p][k + 1..]
                        .iter()
                        .flatten()
                        .next()
                        .copied()
                        .unwrap_or(end_pts[p]);
                    (start.max(0) as u64, (stop - start).max(0) as u64)
                }
                None => (0, 0),
            };
            seg.samples.push(ranges[p][k].clone());
            seg.start_ticks.push(start);
            seg.ticks.push(ticks);
            seg.seconds
                .push(ticks as f64 / f64::from(tracks[src].timescale.max(1)));
        }
        segments.push(seg);
    }
    SegmentPlan { segments, lead }
}
