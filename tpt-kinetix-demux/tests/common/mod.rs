//! Shared MP4 test-file writer (multi-track, ctts, stss, chunking).
#![allow(dead_code)]

pub fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

pub fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&be32(payload.len() as u32 + 8));
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

pub fn full(version: u8, body: &[u8]) -> Vec<u8> {
    let mut p = vec![version, 0, 0, 0];
    p.extend_from_slice(body);
    p
}

#[derive(Clone)]
pub struct Sample {
    pub data: Vec<u8>,
    pub delta: u32,
    pub cts: i32,
    pub key: bool,
}

#[derive(Clone)]
pub struct Track {
    pub handler: [u8; 4],
    pub fourcc: [u8; 4],
    pub timescale: u32,
    pub samples_per_chunk: u32,
    pub samples: Vec<Sample>,
}

impl Track {
    pub fn video(timescale: u32, samples: Vec<Sample>) -> Self {
        Self {
            handler: *b"vide",
            fourcc: *b"avc1",
            timescale,
            samples_per_chunk: 3,
            samples,
        }
    }
    pub fn audio(timescale: u32, samples: Vec<Sample>) -> Self {
        Self {
            handler: *b"soun",
            fourcc: *b"mp4a",
            timescale,
            samples_per_chunk: 2,
            samples,
        }
    }
}

fn runs<T: PartialEq + Copy>(vals: impl Iterator<Item = T>) -> Vec<(u32, T)> {
    let mut out: Vec<(u32, T)> = Vec::new();
    for v in vals {
        match out.last_mut() {
            Some((n, last)) if *last == v => *n += 1,
            _ => out.push((1, v)),
        }
    }
    out
}

fn build_moov(tracks: &[Track], chunk_offsets: &[Vec<u32>]) -> Vec<u8> {
    let mut moov = Vec::new();
    moov.extend(boxed(b"mvhd", &full(0, &[0u8; 96])));
    for (ti, t) in tracks.iter().enumerate() {
        let n = t.samples.len() as u32;
        let stts = runs(t.samples.iter().map(|s| s.delta));
        let mut stts_p = be32(stts.len() as u32).to_vec();
        for (c, d) in &stts {
            stts_p.extend(be32(*c));
            stts_p.extend(be32(*d));
        }
        let ctts = runs(t.samples.iter().map(|s| s.cts));
        let mut ctts_p = be32(ctts.len() as u32).to_vec();
        for (c, o) in &ctts {
            ctts_p.extend(be32(*c));
            ctts_p.extend(be32(*o as u32));
        }
        let keys: Vec<u32> = t
            .samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.key)
            .map(|(i, _)| i as u32 + 1)
            .collect();
        let mut stss_p = be32(keys.len() as u32).to_vec();
        for k in &keys {
            stss_p.extend(be32(*k));
        }
        let mut stsc_p = be32(1).to_vec();
        stsc_p.extend(be32(1));
        stsc_p.extend(be32(t.samples_per_chunk));
        stsc_p.extend(be32(1));
        let mut stsz_p = be32(0).to_vec();
        stsz_p.extend(be32(n));
        for s in &t.samples {
            stsz_p.extend(be32(s.data.len() as u32));
        }
        let mut stco_p = be32(chunk_offsets[ti].len() as u32).to_vec();
        for o in &chunk_offsets[ti] {
            stco_p.extend(be32(*o));
        }
        let mut stsd_p = be32(1).to_vec();
        stsd_p.extend(boxed(&t.fourcc, &[0u8; 16]));

        let mut stbl = Vec::new();
        stbl.extend(boxed(b"stsd", &full(0, &stsd_p)));
        stbl.extend(boxed(b"stts", &full(0, &stts_p)));
        if t.samples.iter().any(|s| s.cts != 0) {
            stbl.extend(boxed(b"ctts", &full(1, &ctts_p)));
        }
        if t.samples.iter().any(|s| !s.key) {
            stbl.extend(boxed(b"stss", &full(0, &stss_p)));
        }
        stbl.extend(boxed(b"stsc", &full(0, &stsc_p)));
        stbl.extend(boxed(b"stsz", &full(0, &stsz_p)));
        stbl.extend(boxed(b"stco", &full(0, &stco_p)));

        // tkhd body: creation, modification, track_id, reserved, duration, 52 bytes
        // of layer/matrix/etc, then 16.16 width and height.
        let mut tkhd = vec![0u8; 8];
        tkhd.extend(be32(ti as u32 + 1));
        tkhd.extend([0u8; 8]);
        tkhd.extend([0u8; 52]);
        tkhd.extend(be32(1920 << 16));
        tkhd.extend(be32(1080 << 16));
        // mdhd body: creation, modification, timescale, duration, language.
        let mut mdhd = vec![0u8; 8];
        mdhd.extend(be32(t.timescale));
        mdhd.extend(be32(0));
        mdhd.extend([0u8; 4]);
        // hdlr body: pre_defined, handler_type, 12 reserved, empty name.
        let mut hdlr = vec![0u8; 4];
        hdlr.extend(t.handler);
        hdlr.extend([0u8; 12 + 1]);
        let mdia = [
            boxed(b"mdhd", &full(0, &mdhd)),
            boxed(b"hdlr", &full(0, &hdlr)),
            boxed(b"minf", &boxed(b"stbl", &stbl)),
        ]
        .concat();
        let trak = [boxed(b"tkhd", &full(0, &tkhd)), boxed(b"mdia", &mdia)].concat();
        moov.extend(boxed(b"trak", &trak));
    }
    boxed(b"moov", &moov)
}

/// Builds a file. `moov_first` puts the index before `mdat` ("faststart").
pub fn build_mp4(tracks: &[Track], moov_first: bool) -> Vec<u8> {
    let ftyp = boxed(b"ftyp", b"isom\0\0\0\0isomavc1");
    // mdat payload: each track's chunks back to back.
    let mut mdat = Vec::new();
    let mut rel_offsets: Vec<Vec<u32>> = Vec::new();
    for t in tracks {
        let mut offs = Vec::new();
        for chunk in t.samples.chunks(t.samples_per_chunk as usize) {
            offs.push(mdat.len() as u32);
            for s in chunk {
                mdat.extend_from_slice(&s.data);
            }
        }
        rel_offsets.push(offs);
    }
    let moov_len = build_moov(tracks, &rel_offsets).len();
    let base = ftyp.len() + if moov_first { moov_len } else { 0 } + 8;
    let abs: Vec<Vec<u32>> = rel_offsets
        .iter()
        .map(|v| v.iter().map(|o| o + base as u32).collect())
        .collect();
    let moov = build_moov(tracks, &abs);
    assert_eq!(moov.len(), moov_len);
    let mdat_box = boxed(b"mdat", &mdat);
    if moov_first {
        [ftyp, moov, mdat_box].concat()
    } else {
        [ftyp, mdat_box, moov].concat()
    }
}

pub fn video_samples(n: usize, size: usize) -> Vec<Sample> {
    (0..n)
        .map(|i| Sample {
            data: vec![(i % 251) as u8; size],
            delta: 40,
            // B-frame-ish: display order differs from decode order.
            cts: if i % 3 == 0 { 80 } else { 0 },
            key: i % 12 == 0,
        })
        .collect()
}

pub fn audio_samples(n: usize) -> Vec<Sample> {
    (0..n)
        .map(|i| Sample {
            data: vec![0xA0 | (i % 16) as u8; 17],
            delta: 1024,
            cts: 0,
            key: true,
        })
        .collect()
}
