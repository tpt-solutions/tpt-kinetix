//! Malformed-MP4 regression corpus.
//!
//! This test maintains a small, hand-crafted corpus of malformed and malicious
//! MP4 byte buffers under `tests/corpus/mp4/`.  The corpus doubles as:
//!
//! * a **regression suite** — every sample is fed to `parse_mp4` and
//!   `Mp4Demuxer::new`, which must never panic (errors are fine); and
//! * a **fuzzer seed set** — copy the files into
//!   `fuzz/corpus/fuzz_mp4_box/` before running `cargo fuzz run fuzz_mp4_box`
//!   to give libFuzzer a head start.
//!
//! The samples are generated on first run (or when missing) so the repository
//! stays free of opaque binary blobs while remaining reproducible.

use std::path::{Path, PathBuf};

use tpt_kinetix_demux::mp4::container::parse_mp4;
use tpt_kinetix_demux::mp4::Mp4Demuxer;

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("corpus")
        .join("mp4")
}

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// The set of hand-crafted malformed samples: `(filename, bytes)`.
fn malformed_samples() -> Vec<(&'static str, Vec<u8>)> {
    let mut samples: Vec<(&'static str, Vec<u8>)> = Vec::new();

    // 1. Empty file.
    samples.push(("empty.bin", Vec::new()));

    // 2. Truncated box header (only 3 of 8 bytes).
    samples.push(("truncated_header.bin", vec![0x00, 0x00, 0x00]));

    // 3. Box claims a size far larger than the buffer.
    let mut oversized = Vec::new();
    oversized.extend_from_slice(&be32(0xFFFF_FFFF));
    oversized.extend_from_slice(b"moov");
    samples.push(("oversized_box.bin", oversized));

    // 4. Box with size smaller than the 8-byte header.
    let mut undersized = Vec::new();
    undersized.extend_from_slice(&be32(4)); // impossible: < header
    undersized.extend_from_slice(b"moov");
    samples.push(("undersized_box.bin", undersized));

    // 5. moov present but empty (no trak).
    let mut empty_moov = Vec::new();
    empty_moov.extend_from_slice(&be32(8));
    empty_moov.extend_from_slice(b"moov");
    samples.push(("empty_moov.bin", empty_moov));

    // 6. moov containing a trak that is truncated mid-box.
    let mut trak_trunc_inner = Vec::new();
    trak_trunc_inner.extend_from_slice(&be32(64)); // trak claims 64 bytes...
    trak_trunc_inner.extend_from_slice(b"trak");
    trak_trunc_inner.extend_from_slice(&[0u8; 8]); // ...but only 8 provided
    let mut moov_trunc_trak = Vec::new();
    moov_trunc_trak.extend_from_slice(&be32((8 + trak_trunc_inner.len()) as u32));
    moov_trunc_trak.extend_from_slice(b"moov");
    moov_trunc_trak.extend_from_slice(&trak_trunc_inner);
    samples.push(("moov_truncated_trak.bin", moov_trunc_trak));

    // 7. size == 0 box (means "to end of file") followed by nothing useful.
    let mut zero_size = Vec::new();
    zero_size.extend_from_slice(&be32(0));
    zero_size.extend_from_slice(b"mdat");
    samples.push(("zero_size_box.bin", zero_size));

    // 8. largesize (size==1) but the 8-byte largesize field is missing.
    let mut bad_largesize = Vec::new();
    bad_largesize.extend_from_slice(&be32(1));
    bad_largesize.extend_from_slice(b"mdat");
    bad_largesize.extend_from_slice(&[0x00, 0x00]); // only 2 of 8 bytes
    samples.push(("bad_largesize.bin", bad_largesize));

    // 9. Deeply nested empty containers (moov→trak→mdia→minf→stbl) with no
    //    leaf boxes — exercises the walker's recursion without data.
    fn wrap(kind: &[u8; 4], inner: Vec<u8>) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&be32((8 + inner.len()) as u32));
        out.extend_from_slice(kind);
        out.extend_from_slice(&inner);
        out
    }
    let nested = wrap(
        b"moov",
        wrap(
            b"trak",
            wrap(b"mdia", wrap(b"minf", wrap(b"stbl", Vec::new()))),
        ),
    );
    samples.push(("deeply_nested_empty.bin", nested));

    // 10. Non-MP4 garbage that happens to start with plausible ASCII.
    samples.push((
        "ascii_garbage.bin",
        b"this is definitely not an mp4 file at all".to_vec(),
    ));

    samples
}

/// Ensure every hand-crafted sample exists on disk.
fn ensure_corpus_written(dir: &Path) {
    std::fs::create_dir_all(dir).expect("create corpus dir");
    for (name, bytes) in malformed_samples() {
        let path = dir.join(name);
        if !path.exists() {
            std::fs::write(&path, &bytes).expect("write corpus sample");
        }
    }
}

#[test]
fn corpus_is_materialised() {
    let dir = corpus_dir();
    ensure_corpus_written(&dir);
    // Confirm the expected number of samples are present.
    let count = std::fs::read_dir(&dir)
        .expect("read corpus dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "bin").unwrap_or(false))
        .count();
    assert!(
        count >= malformed_samples().len(),
        "expected at least {} corpus files, found {count}",
        malformed_samples().len()
    );
}

#[test]
fn parse_mp4_never_panics_on_corpus() {
    let dir = corpus_dir();
    ensure_corpus_written(&dir);

    for entry in std::fs::read_dir(&dir).expect("read corpus dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().map(|x| x != "bin").unwrap_or(true) {
            continue;
        }
        let data = std::fs::read(&path).expect("read corpus sample");

        // Neither entry point may panic; both are allowed to return Err.
        let _ = parse_mp4(&data);
        let _ = Mp4Demuxer::new(data);
    }
}

#[test]
fn in_memory_malformed_inputs_never_panic() {
    // Belt-and-braces: exercise the generators directly (independent of disk).
    for (_, bytes) in malformed_samples() {
        let _ = parse_mp4(&bytes);
        let _ = Mp4Demuxer::new(bytes);
    }
}
