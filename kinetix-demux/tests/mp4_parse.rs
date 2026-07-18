//! Integration tests for the kinetix-demux MP4 parser using hand-crafted byte
//! fixtures.

use kinetix_demux::mp4::{
    boxes::{parse_box_header, parse_ftyp, parse_stco, parse_stsd, parse_stsz, parse_stts},
    Mp4Demuxer,
};

// ---------------------------------------------------------------------------
// Helper: write a big-endian u32 into a byte buffer.
// ---------------------------------------------------------------------------
fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

fn be64(v: u64) -> [u8; 8] {
    v.to_be_bytes()
}

// ---------------------------------------------------------------------------
// 1. ftyp
// ---------------------------------------------------------------------------

#[test]
fn test_parse_ftyp() {
    // Payload (header already stripped):
    //   major_brand      = b"isom"  (4 bytes)
    //   minor_version    = 0        (4 bytes)
    //   compatible_brand = b"isom"  (4 bytes)
    //   compatible_brand = b"mp41"  (4 bytes)
    let payload: Vec<u8> = vec![
        b'i', b's', b'o', b'm', // major_brand
        0, 0, 0, 0, // minor_version
        b'i', b's', b'o', b'm', // compat
        b'm', b'p', b'4', b'1', // compat
    ];

    let (rest, ftyp) = parse_ftyp(&payload).expect("parse_ftyp should succeed");
    assert!(rest.is_empty());
    assert_eq!(&ftyp.major_brand, b"isom");
    assert_eq!(ftyp.minor_version, 0);
    assert_eq!(ftyp.compatible_brands.len(), 2);
    assert_eq!(&ftyp.compatible_brands[0], b"isom");
    assert_eq!(&ftyp.compatible_brands[1], b"mp41");
}

// ---------------------------------------------------------------------------
// 2. stts
// ---------------------------------------------------------------------------

#[test]
fn test_parse_stts() {
    // Payload: version(1) + flags(3) + entry_count(4) + entries(n*8)
    //   2 entries: (10 samples, delta 512) and (5 samples, delta 1024)
    let mut payload = vec![
        0u8, 0, 0, 0, // version + flags
    ];
    payload.extend_from_slice(&be32(2)); // entry_count
    payload.extend_from_slice(&be32(10));
    payload.extend_from_slice(&be32(512));
    payload.extend_from_slice(&be32(5));
    payload.extend_from_slice(&be32(1024));

    let (rest, stts) = parse_stts(&payload).expect("parse_stts should succeed");
    assert!(rest.is_empty());
    assert_eq!(stts.entries.len(), 2);
    assert_eq!(stts.entries[0].sample_count, 10);
    assert_eq!(stts.entries[0].sample_delta, 512);
    assert_eq!(stts.entries[1].sample_count, 5);
    assert_eq!(stts.entries[1].sample_delta, 1024);
}

// ---------------------------------------------------------------------------
// 3. stsz
// ---------------------------------------------------------------------------

#[test]
fn test_parse_stsz() {
    // default_size = 0 → variable sizes follow
    let mut payload = vec![0u8, 0, 0, 0]; // version + flags
    payload.extend_from_slice(&be32(0)); // default_size = 0
    payload.extend_from_slice(&be32(3)); // sample_count = 3
    payload.extend_from_slice(&be32(100));
    payload.extend_from_slice(&be32(200));
    payload.extend_from_slice(&be32(300));

    let (rest, stsz) = parse_stsz(&payload).expect("parse_stsz should succeed");
    assert!(rest.is_empty());
    assert_eq!(stsz.default_size, 0);
    assert_eq!(stsz.sample_sizes, vec![100, 200, 300]);
}

// ---------------------------------------------------------------------------
// 4. stco
// ---------------------------------------------------------------------------

#[test]
fn test_parse_stco() {
    let mut payload = vec![0u8, 0, 0, 0]; // version + flags
    payload.extend_from_slice(&be32(3)); // entry_count
    payload.extend_from_slice(&be32(0x0000_1000));
    payload.extend_from_slice(&be32(0x0000_2000));
    payload.extend_from_slice(&be32(0x0000_3000));

    let (rest, stco) = parse_stco(&payload).expect("parse_stco should succeed");
    assert!(rest.is_empty());
    assert_eq!(stco.offsets, vec![0x1000, 0x2000, 0x3000]);
}

// ---------------------------------------------------------------------------
// 5. box header largesize
// ---------------------------------------------------------------------------

#[test]
fn test_box_header_largesize() {
    // size field = 1  →  largesize follows as u64
    let largesize: u64 = 0x0000_0001_0000_0000; // 4 GiB
    let mut buf = vec![];
    buf.extend_from_slice(&be32(1)); // size = 1 (largesize indicator)
    buf.extend_from_slice(b"mdat"); // box_type
    buf.extend_from_slice(&be64(largesize)); // actual 64-bit size

    let (rest, hdr) = parse_box_header(&buf).expect("parse_box_header should succeed");
    assert!(rest.is_empty());
    assert_eq!(hdr.box_type, *b"mdat");
    assert_eq!(hdr.size, largesize);
}

// ---------------------------------------------------------------------------
// 6. empty / invalid demuxer returns error gracefully
// ---------------------------------------------------------------------------

#[test]
fn test_empty_demuxer_returns_err() {
    let result = Mp4Demuxer::new(vec![]);
    assert!(
        result.is_err(),
        "Mp4Demuxer::new on empty data should return Err"
    );
}

#[test]
fn test_truncated_demuxer_does_not_panic() {
    // Random garbage — should fail gracefully, not panic.
    let garbage = vec![0x00u8, 0xFF, 0xAB, 0xCD, 0x01, 0x02];
    let _ = Mp4Demuxer::new(garbage); // we only care that it doesn't panic
}

// ---------------------------------------------------------------------------
// 7. stsd — sample descriptions / codec identification
// ---------------------------------------------------------------------------

/// Builds a minimal `stsd` payload containing a single sample entry whose box
/// type is `fourcc` with `entry_extra` bytes of payload.
fn build_stsd(fourcc: &[u8; 4], entry_extra: &[u8]) -> Vec<u8> {
    let mut payload = vec![0u8, 0, 0, 0]; // version + flags
    payload.extend_from_slice(&be32(1)); // entry_count = 1

    // Sample entry box: size(4) + type(4) + extra.
    let entry_size = 8 + entry_extra.len() as u32;
    payload.extend_from_slice(&be32(entry_size));
    payload.extend_from_slice(fourcc);
    payload.extend_from_slice(entry_extra);
    payload
}

#[test]
fn test_parse_stsd_avc1() {
    // A realistic-ish avc1 entry has a fixed 78-byte VisualSampleEntry header
    // before the avcC box; we only need enough bytes to exercise the parser.
    let extra = vec![0u8; 78];
    let payload = build_stsd(b"avc1", &extra);

    let (rest, stsd) = parse_stsd(&payload).expect("parse_stsd should succeed");
    assert!(rest.is_empty());
    assert_eq!(stsd.entries.len(), 1);
    assert_eq!(&stsd.entries[0].format, b"avc1");
    assert_eq!(stsd.codec_fourcc(), Some(*b"avc1"));
    assert_eq!(stsd.entries[0].extra.len(), 78);
}

#[test]
fn test_parse_stsd_mp4a() {
    let payload = build_stsd(b"mp4a", &[0u8; 28]);
    let (_, stsd) = parse_stsd(&payload).expect("parse_stsd should succeed");
    assert_eq!(stsd.codec_fourcc(), Some(*b"mp4a"));
}

#[test]
fn test_parse_stsd_empty_entries() {
    // entry_count = 0 → no sample entries, codec_fourcc is None.
    let payload = {
        let mut p = vec![0u8, 0, 0, 0];
        p.extend_from_slice(&be32(0));
        p
    };
    let (rest, stsd) = parse_stsd(&payload).expect("parse_stsd should succeed");
    assert!(rest.is_empty());
    assert!(stsd.entries.is_empty());
    assert_eq!(stsd.codec_fourcc(), None);
}

#[test]
fn test_parse_stsd_truncated_entry_does_not_panic() {
    // Claims one entry with a huge size but no payload bytes — must not panic.
    let mut payload = vec![0u8, 0, 0, 0];
    payload.extend_from_slice(&be32(1)); // entry_count = 1
    payload.extend_from_slice(&be32(9999)); // absurd entry size
    payload.extend_from_slice(b"avc1");
    // (no further bytes)
    let (_, stsd) = parse_stsd(&payload).expect("parse_stsd should not error");
    assert_eq!(stsd.codec_fourcc(), Some(*b"avc1"));
}
