# tpt-kinetix-mux

Container **muxers** for the [TPT Kinetix](https://github.com/tpt-solutions/tpt-kinetix)
media processing engine. This is the write-side counterpart to `tpt-kinetix-demux`.

## Status

- ✅ `Mp4Writer` — multi-track passthrough MP4 writer. Streams are described by
  `StreamInfo`, so any codec with a sample-entry writer is carried (AV1, VP9,
  Opus, AAC, H.264/H.265, FLAC, AC-3, ...), including audio Kinetix cannot
  decode. Memory is bounded to roughly one chunk per track; output is `moov`-at-end
  and must be seekable.
- ✅ `faststart` — move `moov` to the front for progressive HTTP playback.
- ✅ `FragmentWriter` — fragmented MP4 / CMAF (init segment plus `moof`/`mdat`
  fragments) for HLS-fMP4, DASH and live. Writes to any `std::io::Write`; no seeking.
  With `cenc` (below), its fragments are encrypted.
- ✅ `WebmWriter` — WebM/Matroska for AV1, VP9 and Opus, in a live mode (unknown
  sizes, no seeking) or a finite mode (patched sizes plus a `Cues` index). WebVTT
  subtitle tracks, Matroska tags and chapters are written too.
- ✅ `IvfWriter` — IVF for one AV1 or VP9 track.
- ✅ `OggWriter` — Ogg for one Opus track.
- ✅ CENC (`cenc` module) — `cenc` (AES-CTR) and `cbcs` (AES-CBC, whole samples)
  protection for fragmented MP4 with a subsample map, ClearKey `pssh` in the init
  segment, for VP9, AV1 and Opus samples. Keys are passed in; nothing is fetched.
- ✅ WebVTT subtitle tracks in MP4 (`wvtt`/`vttc`) output, with the track language
  written from `mdhd`.
- ✅ `Mp4Muxer` — the older single-track H.264 (`avc1`) writer, kept for compatibility.

## Example

```rust
use tpt_kinetix_mux::{Mp4Muxer, Mp4MuxerConfig};

let mut muxer = Mp4Muxer::new(Mp4MuxerConfig {
    width: 320,
    height: 240,
    timescale: 30_000,
    sps: sps_bytes,
    pps: pps_bytes,
});

for au in access_units {
    muxer.write_sample(&au.avcc_bytes, au.duration_ticks, au.is_keyframe);
}

std::fs::write("out.mp4", muxer.finish())?;
```

For multi-track or non-H.264 output use `Mp4Writer` / `FragmentWriter` / `WebmWriter`
(see their rustdoc), or `tpt-kinetix remux` from the CLI.

## License

MIT OR Apache-2.0
