//! Minimal progressive MP4 / ISO-BMFF muxer for a single H.264 video track.
//!
//! The muxer buffers sample metadata (size, duration, keyframe flag) and sample
//! payloads, then emits a valid `ftyp` + `mdat` + `moov` file on [`Mp4Muxer::finish`].
//!
//! Samples must be provided in **AVCC** form (each NAL unit prefixed by a
//! 4-byte big-endian length), which is what MP4 stores in `mdat`.

use thiserror::Error;

/// Errors that can occur while muxing.
#[derive(Debug, Error)]
pub enum MuxError {
    /// A configuration value was invalid (e.g. zero timescale).
    #[error("invalid muxer configuration: {0}")]
    InvalidConfig(String),
}

/// Configuration for [`Mp4Muxer`].
#[derive(Debug, Clone)]
pub struct Mp4MuxerConfig {
    /// Coded picture width in pixels.
    pub width: u16,
    /// Coded picture height in pixels.
    pub height: u16,
    /// Media timescale (ticks per second), e.g. `30_000` for 30fps content.
    pub timescale: u32,
    /// Sequence Parameter Set NAL unit (RBSP with NAL header, *without* start code).
    pub sps: Vec<u8>,
    /// Picture Parameter Set NAL unit (RBSP with NAL header, *without* start code).
    pub pps: Vec<u8>,
}

struct SampleMeta {
    size: u32,
    duration: u32,
    is_key: bool,
}

/// A minimal MP4 muxer for a single H.264 video track.
///
/// See the [crate-level example](crate) for usage.
pub struct Mp4Muxer {
    config: Mp4MuxerConfig,
    mdat: Vec<u8>,
    samples: Vec<SampleMeta>,
}

impl Mp4Muxer {
    /// Create a new muxer from `config`.
    pub fn new(config: Mp4MuxerConfig) -> Self {
        Self {
            config,
            mdat: Vec::new(),
            samples: Vec::new(),
        }
    }

    /// Append one coded sample (access unit) in AVCC (length-prefixed) form.
    ///
    /// - `data` — the sample payload as stored in `mdat`.
    /// - `duration` — sample duration in `timescale` ticks.
    /// - `is_key` — whether this sample is a sync sample (IDR).
    pub fn write_sample(&mut self, data: &[u8], duration: u32, is_key: bool) {
        self.mdat.extend_from_slice(data);
        self.samples.push(SampleMeta {
            size: data.len() as u32,
            duration,
            is_key,
        });
    }

    /// Number of samples written so far.
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    /// Finish muxing and return the full MP4 file bytes.
    pub fn finish(self) -> Vec<u8> {
        let ftyp = self.build_ftyp();

        // mdat layout: [size(4)][type(4)][payload]
        let mdat_header_len = 8usize;
        let mdat_total = mdat_header_len + self.mdat.len();

        // The mdat payload begins at: ftyp.len() + 8 (mdat header).
        let mdat_payload_offset = (ftyp.len() + mdat_header_len) as u32;

        let moov = self.build_moov(mdat_payload_offset);

        let mut out = Vec::with_capacity(ftyp.len() + mdat_total + moov.len());
        out.extend_from_slice(&ftyp);
        out.extend_from_slice(&(mdat_total as u32).to_be_bytes());
        out.extend_from_slice(b"mdat");
        out.extend_from_slice(&self.mdat);
        out.extend_from_slice(&moov);
        out
    }

    fn build_ftyp(&self) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(b"isom"); // major brand
        body.extend_from_slice(&0u32.to_be_bytes()); // minor version
        body.extend_from_slice(b"isom"); // compatible brands
        body.extend_from_slice(b"avc1");
        body.extend_from_slice(b"mp41");
        box_with_type(b"ftyp", &body)
    }

    fn build_moov(&self, mdat_payload_offset: u32) -> Vec<u8> {
        let total_duration: u64 = self.samples.iter().map(|s| s.duration as u64).sum();

        let mvhd = self.build_mvhd(total_duration);
        let trak = self.build_trak(total_duration, mdat_payload_offset);

        let mut body = Vec::new();
        body.extend_from_slice(&mvhd);
        body.extend_from_slice(&trak);
        box_with_type(b"moov", &body)
    }

    fn build_mvhd(&self, duration: u64) -> Vec<u8> {
        let mut b = Vec::new();
        b.push(0); // version 0
        b.extend_from_slice(&[0, 0, 0]); // flags
        b.extend_from_slice(&0u32.to_be_bytes()); // creation_time
        b.extend_from_slice(&0u32.to_be_bytes()); // modification_time
        b.extend_from_slice(&self.config.timescale.to_be_bytes());
        b.extend_from_slice(&(duration as u32).to_be_bytes());
        b.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
        b.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
        b.extend_from_slice(&0u16.to_be_bytes()); // reserved
        b.extend_from_slice(&[0u8; 8]); // reserved
        b.extend_from_slice(&IDENTITY_MATRIX);
        b.extend_from_slice(&[0u8; 24]); // pre_defined
        b.extend_from_slice(&2u32.to_be_bytes()); // next_track_id
        box_with_type(b"mvhd", &b)
    }

    fn build_trak(&self, duration: u64, mdat_payload_offset: u32) -> Vec<u8> {
        let tkhd = self.build_tkhd(duration);
        let mdia = self.build_mdia(duration, mdat_payload_offset);

        let mut body = Vec::new();
        body.extend_from_slice(&tkhd);
        body.extend_from_slice(&mdia);
        box_with_type(b"trak", &body)
    }

    fn build_tkhd(&self, duration: u64) -> Vec<u8> {
        let mut b = Vec::new();
        b.push(0); // version 0
        b.extend_from_slice(&[0, 0, 7]); // flags: enabled|in_movie|in_preview
        b.extend_from_slice(&0u32.to_be_bytes()); // creation_time
        b.extend_from_slice(&0u32.to_be_bytes()); // modification_time
        b.extend_from_slice(&1u32.to_be_bytes()); // track_id
        b.extend_from_slice(&0u32.to_be_bytes()); // reserved
        b.extend_from_slice(&(duration as u32).to_be_bytes());
        b.extend_from_slice(&[0u8; 8]); // reserved
        b.extend_from_slice(&0u16.to_be_bytes()); // layer
        b.extend_from_slice(&0u16.to_be_bytes()); // alternate_group
        b.extend_from_slice(&0u16.to_be_bytes()); // volume (0 for video)
        b.extend_from_slice(&0u16.to_be_bytes()); // reserved
        b.extend_from_slice(&IDENTITY_MATRIX);
        // width/height are 16.16 fixed point
        b.extend_from_slice(&((self.config.width as u32) << 16).to_be_bytes());
        b.extend_from_slice(&((self.config.height as u32) << 16).to_be_bytes());
        box_with_type(b"tkhd", &b)
    }

    fn build_mdia(&self, duration: u64, mdat_payload_offset: u32) -> Vec<u8> {
        let mdhd = self.build_mdhd(duration);
        let hdlr = build_hdlr();
        let minf = self.build_minf(mdat_payload_offset);

        let mut body = Vec::new();
        body.extend_from_slice(&mdhd);
        body.extend_from_slice(&hdlr);
        body.extend_from_slice(&minf);
        box_with_type(b"mdia", &body)
    }

    fn build_mdhd(&self, duration: u64) -> Vec<u8> {
        let mut b = Vec::new();
        b.push(0); // version 0
        b.extend_from_slice(&[0, 0, 0]); // flags
        b.extend_from_slice(&0u32.to_be_bytes()); // creation_time
        b.extend_from_slice(&0u32.to_be_bytes()); // modification_time
        b.extend_from_slice(&self.config.timescale.to_be_bytes());
        b.extend_from_slice(&(duration as u32).to_be_bytes());
        b.extend_from_slice(&0x55c4u16.to_be_bytes()); // language 'und'
        b.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
        box_with_type(b"mdhd", &b)
    }

    fn build_minf(&self, mdat_payload_offset: u32) -> Vec<u8> {
        let vmhd = build_vmhd();
        let dinf = build_dinf();
        let stbl = self.build_stbl(mdat_payload_offset);

        let mut body = Vec::new();
        body.extend_from_slice(&vmhd);
        body.extend_from_slice(&dinf);
        body.extend_from_slice(&stbl);
        box_with_type(b"minf", &body)
    }

    fn build_stbl(&self, mdat_payload_offset: u32) -> Vec<u8> {
        let stsd = self.build_stsd();
        let stts = self.build_stts();
        let stss = self.build_stss();
        let stsc = build_stsc(self.samples.len() as u32);
        let stsz = self.build_stsz();
        let stco = build_stco(mdat_payload_offset);

        let mut body = Vec::new();
        body.extend_from_slice(&stsd);
        body.extend_from_slice(&stts);
        if let Some(stss) = stss {
            body.extend_from_slice(&stss);
        }
        body.extend_from_slice(&stsc);
        body.extend_from_slice(&stsz);
        body.extend_from_slice(&stco);
        box_with_type(b"stbl", &body)
    }

    fn build_stsd(&self) -> Vec<u8> {
        let avc1 = self.build_avc1();
        let mut b = Vec::new();
        b.extend_from_slice(&[0, 0, 0, 0]); // version + flags
        b.extend_from_slice(&1u32.to_be_bytes()); // entry_count
        b.extend_from_slice(&avc1);
        box_with_type(b"stsd", &b)
    }

    fn build_avc1(&self) -> Vec<u8> {
        let avcc = self.build_avcc();

        let mut b = Vec::new();
        b.extend_from_slice(&[0u8; 6]); // reserved
        b.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        b.extend_from_slice(&[0u8; 16]); // pre_defined + reserved
        b.extend_from_slice(&self.config.width.to_be_bytes());
        b.extend_from_slice(&self.config.height.to_be_bytes());
        b.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // horizresolution 72dpi
        b.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // vertresolution 72dpi
        b.extend_from_slice(&0u32.to_be_bytes()); // reserved
        b.extend_from_slice(&1u16.to_be_bytes()); // frame_count
        b.extend_from_slice(&[0u8; 32]); // compressorname
        b.extend_from_slice(&0x0018u16.to_be_bytes()); // depth
        b.extend_from_slice(&0xffffu16.to_be_bytes()); // pre_defined = -1
        b.extend_from_slice(&avcc);
        box_with_type(b"avc1", &b)
    }

    fn build_avcc(&self) -> Vec<u8> {
        let sps = &self.config.sps;
        let pps = &self.config.pps;

        // AVCDecoderConfigurationRecord
        let mut b = Vec::new();
        b.push(1); // configurationVersion
        // profile/compat/level taken from the SPS payload (bytes after NAL header)
        let (profile, compat, level) = if sps.len() >= 4 {
            (sps[1], sps[2], sps[3])
        } else {
            (0x42, 0x00, 0x1e) // baseline 3.0 fallback
        };
        b.push(profile);
        b.push(compat);
        b.push(level);
        b.push(0xff); // 6 bits reserved + 2 bits lengthSizeMinusOne = 3 (4-byte)
        b.push(0xe1); // 3 bits reserved + 5 bits numOfSPS = 1
        b.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        b.extend_from_slice(sps);
        b.push(1); // numOfPPS
        b.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        b.extend_from_slice(pps);
        box_with_type(b"avcC", &b)
    }

    fn build_stts(&self) -> Vec<u8> {
        // Run-length encode consecutive equal durations.
        let mut entries: Vec<(u32, u32)> = Vec::new(); // (count, delta)
        for s in &self.samples {
            match entries.last_mut() {
                Some((count, delta)) if *delta == s.duration => *count += 1,
                _ => entries.push((1, s.duration)),
            }
        }

        let mut b = Vec::new();
        b.extend_from_slice(&[0, 0, 0, 0]); // version + flags
        b.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (count, delta) in entries {
            b.extend_from_slice(&count.to_be_bytes());
            b.extend_from_slice(&delta.to_be_bytes());
        }
        box_with_type(b"stts", &b)
    }

    fn build_stss(&self) -> Option<Vec<u8>> {
        let sync: Vec<u32> = self
            .samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.is_key)
            .map(|(i, _)| (i as u32) + 1) // 1-based
            .collect();

        // If every sample is a keyframe, stss can be omitted (all are sync).
        if sync.is_empty() || sync.len() == self.samples.len() {
            return None;
        }

        let mut b = Vec::new();
        b.extend_from_slice(&[0, 0, 0, 0]);
        b.extend_from_slice(&(sync.len() as u32).to_be_bytes());
        for n in sync {
            b.extend_from_slice(&n.to_be_bytes());
        }
        Some(box_with_type(b"stss", &b))
    }

    fn build_stsz(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&[0, 0, 0, 0]); // version + flags
        b.extend_from_slice(&0u32.to_be_bytes()); // sample_size = 0 → per-sample table
        b.extend_from_slice(&(self.samples.len() as u32).to_be_bytes());
        for s in &self.samples {
            b.extend_from_slice(&s.size.to_be_bytes());
        }
        box_with_type(b"stsz", &b)
    }
}

// ---------------------------------------------------------------------------
// Free box builders that don't depend on muxer state
// ---------------------------------------------------------------------------

fn build_hdlr() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&[0, 0, 0, 0]); // version + flags
    b.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
    b.extend_from_slice(b"vide"); // handler_type
    b.extend_from_slice(&[0u8; 12]); // reserved
    b.extend_from_slice(b"VideoHandler\0"); // name (null-terminated)
    box_with_type(b"hdlr", &b)
}

fn build_vmhd() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&[0, 0, 0, 1]); // version + flags (flags=1 required)
    b.extend_from_slice(&0u16.to_be_bytes()); // graphicsmode
    b.extend_from_slice(&[0u8; 6]); // opcolor
    box_with_type(b"vmhd", &b)
}

fn build_dinf() -> Vec<u8> {
    // dref with a single "self-contained" url entry (flags=1).
    let mut url = Vec::new();
    url.extend_from_slice(&[0, 0, 0, 1]); // version + flags (self-contained)
    let url_box = box_with_type(b"url ", &url);

    let mut dref = Vec::new();
    dref.extend_from_slice(&[0, 0, 0, 0]); // version + flags
    dref.extend_from_slice(&1u32.to_be_bytes()); // entry_count
    dref.extend_from_slice(&url_box);
    let dref_box = box_with_type(b"dref", &dref);

    box_with_type(b"dinf", &dref_box)
}

fn build_stsc(sample_count: u32) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&[0, 0, 0, 0]); // version + flags
    if sample_count == 0 {
        b.extend_from_slice(&0u32.to_be_bytes()); // entry_count
    } else {
        b.extend_from_slice(&1u32.to_be_bytes()); // entry_count
        b.extend_from_slice(&1u32.to_be_bytes()); // first_chunk
        b.extend_from_slice(&sample_count.to_be_bytes()); // samples_per_chunk
        b.extend_from_slice(&1u32.to_be_bytes()); // sample_description_index
    }
    box_with_type(b"stsc", &b)
}

fn build_stco(chunk_offset: u32) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&[0, 0, 0, 0]); // version + flags
    b.extend_from_slice(&1u32.to_be_bytes()); // entry_count (single chunk)
    b.extend_from_slice(&chunk_offset.to_be_bytes());
    box_with_type(b"stco", &b)
}

/// Wrap `body` in a box of `box_type`, prefixing the 8-byte size+type header.
fn box_with_type(box_type: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let size = (body.len() + 8) as u32;
    let mut out = Vec::with_capacity(size as usize);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(box_type);
    out.extend_from_slice(body);
    out
}

/// The ISO-BMFF identity transformation matrix (9 x 32-bit fixed point).
const IDENTITY_MATRIX: [u8; 36] = [
    0x00, 0x01, 0x00, 0x00, // a = 1.0
    0x00, 0x00, 0x00, 0x00, // b
    0x00, 0x00, 0x00, 0x00, // u
    0x00, 0x00, 0x00, 0x00, // c
    0x00, 0x01, 0x00, 0x00, // d = 1.0
    0x00, 0x00, 0x00, 0x00, // v
    0x00, 0x00, 0x00, 0x00, // x
    0x00, 0x00, 0x00, 0x00, // y
    0x40, 0x00, 0x00, 0x00, // w = 1.0 (2.30 fixed)
];

#[cfg(test)]
mod tests {
    use super::*;

    fn read_u32(data: &[u8], off: usize) -> u32 {
        u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
    }

    fn find_box(data: &[u8], ty: &[u8; 4]) -> Option<usize> {
        // naive top-level + shallow scan for a 4-byte type marker
        data.windows(4).position(|w| w == ty)
    }

    fn sample_muxer() -> Mp4Muxer {
        let mut m = Mp4Muxer::new(Mp4MuxerConfig {
            width: 320,
            height: 240,
            timescale: 30_000,
            sps: vec![0x67, 0x42, 0x00, 0x1e, 0xaa],
            pps: vec![0x68, 0xce, 0x3c, 0x80],
        });
        m.write_sample(&[0, 0, 0, 3, 0x65, 0x11, 0x22], 1000, true);
        m.write_sample(&[0, 0, 0, 2, 0x41, 0x33], 1000, false);
        m
    }

    #[test]
    fn produces_ftyp_mdat_moov_in_order() {
        let bytes = sample_muxer().finish();
        assert_eq!(&bytes[4..8], b"ftyp");
        // ftyp size
        let ftyp_size = read_u32(&bytes, 0) as usize;
        assert_eq!(&bytes[ftyp_size + 4..ftyp_size + 8], b"mdat");
    }

    #[test]
    fn mdat_size_matches_payload() {
        let m = sample_muxer();
        let payload_len = m.mdat.len();
        let bytes = m.finish();
        let ftyp_size = read_u32(&bytes, 0) as usize;
        let mdat_size = read_u32(&bytes, ftyp_size) as usize;
        assert_eq!(mdat_size, payload_len + 8);
    }

    #[test]
    fn moov_contains_expected_boxes() {
        let bytes = sample_muxer().finish();
        for ty in [b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsd", b"avc1", b"avcC"] {
            assert!(find_box(&bytes, ty).is_some(), "missing box {:?}", std::str::from_utf8(ty));
        }
    }

    #[test]
    fn stco_offset_points_at_mdat_payload() {
        let bytes = sample_muxer().finish();
        let ftyp_size = read_u32(&bytes, 0) as usize;
        let mdat_payload_off = (ftyp_size + 8) as u32;
        let stco_pos = find_box(&bytes, b"stco").unwrap();
        // stco: type(4) then version/flags(4), entry_count(4), offset(4)
        let offset = read_u32(&bytes, stco_pos + 4 + 4 + 4);
        assert_eq!(offset, mdat_payload_off);
        // The byte at that offset should be the first byte of the first sample.
        assert_eq!(bytes[offset as usize], 0x00);
    }

    #[test]
    fn stsz_records_each_sample_size() {
        let bytes = sample_muxer().finish();
        let stsz_pos = find_box(&bytes, b"stsz").unwrap();
        // stsz: type(4), version/flags(4), sample_size(4), sample_count(4), sizes...
        let count = read_u32(&bytes, stsz_pos + 4 + 4 + 4);
        assert_eq!(count, 2);
        let s0 = read_u32(&bytes, stsz_pos + 4 + 12);
        let s1 = read_u32(&bytes, stsz_pos + 4 + 16);
        assert_eq!(s0, 7);
        assert_eq!(s1, 6);
    }

    #[test]
    fn stss_lists_only_keyframes() {
        let bytes = sample_muxer().finish();
        let stss_pos = find_box(&bytes, b"stss").unwrap();
        let count = read_u32(&bytes, stss_pos + 4 + 4);
        assert_eq!(count, 1);
        let n = read_u32(&bytes, stss_pos + 4 + 8);
        assert_eq!(n, 1); // first sample is the only keyframe
    }
}
