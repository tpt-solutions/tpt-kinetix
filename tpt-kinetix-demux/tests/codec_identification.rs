//! End-to-end test: build a minimal but structurally-complete MP4 in memory and
//! verify that [`Mp4Demuxer`] discovers the track, classifies its media type,
//! and identifies the codec from the `stsd` sample entry.

use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_demux::mp4::Mp4Demuxer;

// ---------------------------------------------------------------------------
// Tiny box builder
// ---------------------------------------------------------------------------

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// Wraps `payload` in a box with four-character `box_type`, prepending the
/// 8-byte size+type header.
fn boxed(box_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = (payload.len() + 8) as u32;
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&be32(size));
    out.extend_from_slice(box_type);
    out.extend_from_slice(payload);
    out
}

/// Concatenate several byte buffers.
fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}

// ---------------------------------------------------------------------------
// Box payload builders (payloads only — `boxed` adds the header)
// ---------------------------------------------------------------------------

fn tkhd_payload(track_id: u32, width: u32, height: u32) -> Vec<u8> {
    let mut p = vec![0u8, 0, 0, 0]; // version 0 + flags
    p.extend_from_slice(&be32(0)); // creation_time
    p.extend_from_slice(&be32(0)); // modification_time
    p.extend_from_slice(&be32(track_id)); // track_id
    p.extend_from_slice(&be32(0)); // reserved
    p.extend_from_slice(&be32(0)); // duration
    p.extend_from_slice(&[0u8; 52]); // reserved[2] + layer + ... + matrix[9]
    p.extend_from_slice(&be32(width << 16)); // width 16.16
    p.extend_from_slice(&be32(height << 16)); // height 16.16
    p
}

fn mdhd_payload(timescale: u32, duration: u32) -> Vec<u8> {
    let mut p = vec![0u8, 0, 0, 0]; // version 0 + flags
    p.extend_from_slice(&be32(0)); // creation_time
    p.extend_from_slice(&be32(0)); // modification_time
    p.extend_from_slice(&be32(timescale));
    p.extend_from_slice(&be32(duration));
    p.extend_from_slice(&[0u8; 4]); // language + pre_defined
    p
}

fn hdlr_payload(handler: &[u8; 4]) -> Vec<u8> {
    let mut p = vec![0u8, 0, 0, 0]; // version + flags
    p.extend_from_slice(&be32(0)); // pre_defined
    p.extend_from_slice(handler); // handler_type
    p.extend_from_slice(&[0u8; 12]); // reserved[3]
    p.push(0); // empty name
    p
}

fn stsd_payload(fourcc: &[u8; 4]) -> Vec<u8> {
    let mut p = vec![0u8, 0, 0, 0]; // version + flags
    p.extend_from_slice(&be32(1)); // entry_count
                                   // one sample entry box containing a small placeholder payload
    p.extend_from_slice(&boxed(fourcc, &[0u8; 16]));
    p
}

fn stts_payload(sample_count: u32, delta: u32) -> Vec<u8> {
    let mut p = vec![0u8, 0, 0, 0];
    p.extend_from_slice(&be32(1)); // one entry
    p.extend_from_slice(&be32(sample_count));
    p.extend_from_slice(&be32(delta));
    p
}

fn stsc_payload(samples_per_chunk: u32) -> Vec<u8> {
    let mut p = vec![0u8, 0, 0, 0];
    p.extend_from_slice(&be32(1)); // one entry
    p.extend_from_slice(&be32(1)); // first_chunk
    p.extend_from_slice(&be32(samples_per_chunk)); // samples_per_chunk
    p.extend_from_slice(&be32(1)); // sample_description_index
    p
}

fn stsz_payload(sizes: &[u32]) -> Vec<u8> {
    let mut p = vec![0u8, 0, 0, 0];
    p.extend_from_slice(&be32(0)); // default_size = 0
    p.extend_from_slice(&be32(sizes.len() as u32));
    for s in sizes {
        p.extend_from_slice(&be32(*s));
    }
    p
}

fn stco_payload(offsets: &[u32]) -> Vec<u8> {
    let mut p = vec![0u8, 0, 0, 0];
    p.extend_from_slice(&be32(offsets.len() as u32));
    for o in offsets {
        p.extend_from_slice(&be32(*o));
    }
    p
}

/// Assemble a full single-track MP4 with a valid box tree.
fn build_mp4(handler: &[u8; 4], fourcc: &[u8; 4], sample_sizes: &[u32]) -> Vec<u8> {
    // Compute a chunk offset that points *somewhere* inside the mdat we append.
    // The demuxer only needs offsets to be within the file bounds when reading
    // packets, which this test does exercise.
    let stbl = boxed(
        b"stbl",
        &concat(&[
            boxed(b"stsd", &stsd_payload(fourcc)),
            boxed(b"stts", &stts_payload(sample_sizes.len() as u32, 1000)),
            boxed(b"stsc", &stsc_payload(sample_sizes.len() as u32)),
            boxed(b"stsz", &stsz_payload(sample_sizes)),
            // placeholder; patched below once we know the mdat location.
            boxed(b"stco", &stco_payload(&[0])),
        ]),
    );
    let minf = boxed(b"minf", &stbl);
    let mdia = boxed(
        b"mdia",
        &concat(&[
            boxed(b"mdhd", &mdhd_payload(30_000, 30_000)),
            boxed(b"hdlr", &hdlr_payload(handler)),
            minf,
        ]),
    );
    let trak = boxed(
        b"trak",
        &concat(&[boxed(b"tkhd", &tkhd_payload(1, 640, 480)), mdia]),
    );
    let mvhd = {
        let mut p = vec![0u8, 0, 0, 0];
        p.extend_from_slice(&be32(0)); // creation
        p.extend_from_slice(&be32(0)); // modification
        p.extend_from_slice(&be32(30_000)); // timescale
        p.extend_from_slice(&be32(30_000)); // duration
        p.extend_from_slice(&[0u8; 80]); // rate/volume/matrix/pre_defined/next_track_id
        p
    };
    let moov = boxed(b"moov", &concat(&[boxed(b"mvhd", &mvhd), trak]));

    let ftyp = boxed(
        b"ftyp",
        &concat(&[b"isom".to_vec(), be32(0).to_vec(), b"isom".to_vec()]),
    );

    // mdat holding the actual sample bytes.
    let total_sample_bytes: u32 = sample_sizes.iter().sum();
    let mdat_payload = vec![0xABu8; total_sample_bytes as usize];
    let mdat = boxed(b"mdat", &mdat_payload);

    // Layout: ftyp, moov, mdat. The first sample's byte offset into the file is
    // the start of mdat's payload = len(ftyp)+len(moov)+8.
    let mdat_data_offset = (ftyp.len() + moov.len() + 8) as u32;

    // Rebuild with a corrected stco offset now that layout is known.
    let stbl = boxed(
        b"stbl",
        &concat(&[
            boxed(b"stsd", &stsd_payload(fourcc)),
            boxed(b"stts", &stts_payload(sample_sizes.len() as u32, 1000)),
            boxed(b"stsc", &stsc_payload(sample_sizes.len() as u32)),
            boxed(b"stsz", &stsz_payload(sample_sizes)),
            boxed(b"stco", &stco_payload(&[mdat_data_offset])),
        ]),
    );
    let minf = boxed(b"minf", &stbl);
    let mdia = boxed(
        b"mdia",
        &concat(&[
            boxed(b"mdhd", &mdhd_payload(30_000, 30_000)),
            boxed(b"hdlr", &hdlr_payload(handler)),
            minf,
        ]),
    );
    let trak = boxed(
        b"trak",
        &concat(&[boxed(b"tkhd", &tkhd_payload(1, 640, 480)), mdia]),
    );
    let moov = boxed(b"moov", &concat(&[boxed(b"mvhd", &mvhd), trak]));

    concat(&[ftyp, moov, mdat])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn identifies_h264_video_track() {
    let mp4 = build_mp4(b"vide", b"avc1", &[10, 20, 30]);
    let demux = Mp4Demuxer::new(mp4).expect("MP4 should parse");

    let tracks = demux.tracks();
    assert_eq!(tracks.len(), 1);
    let t = &tracks[0];
    assert_eq!(t.handler_type, *b"vide");
    assert_eq!(t.media_type, MediaType::Video);
    assert_eq!(t.codec, Some(CodecId::H264));
    assert_eq!(t.width, 640);
    assert_eq!(t.height, 480);
}

#[test]
fn identifies_aac_audio_track() {
    let mp4 = build_mp4(b"soun", b"mp4a", &[5, 5]);
    let demux = Mp4Demuxer::new(mp4).expect("MP4 should parse");

    let t = &demux.tracks()[0];
    assert_eq!(t.media_type, MediaType::Audio);
    assert_eq!(t.codec, Some(CodecId::Aac));
}

#[test]
fn unknown_codec_preserved() {
    let mp4 = build_mp4(b"vide", b"zzzz", &[8]);
    let demux = Mp4Demuxer::new(mp4).expect("MP4 should parse");

    let t = &demux.tracks()[0];
    assert_eq!(t.codec, Some(CodecId::Unknown(*b"zzzz")));
    assert_eq!(t.media_type, MediaType::Video);
}

#[test]
fn reads_packets_from_built_mp4() {
    use tpt_kinetix_demux::Demuxer;

    let mp4 = build_mp4(b"vide", b"avc1", &[10, 20, 30]);
    let mut demux = Mp4Demuxer::new(mp4).expect("MP4 should parse");

    let mut sizes = Vec::new();
    while let Some(pkt) = demux.read_packet().expect("read_packet") {
        sizes.push(pkt.size());
    }
    assert_eq!(sizes, vec![10, 20, 30]);
}
