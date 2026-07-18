# tpt-kinetix-mux

Container **muxers** for the [TPT Kinetix](https://github.com/tpt-solutions/tpt-kinetix)
media processing engine.

This is the write-side counterpart to `tpt-kinetix-demux`.

## Status

- ✅ Progressive (non-fragmented) MP4 / ISO-BMFF output
- ✅ Single H.264 (`avc1`) video track with a full `stbl` sample table
- ⛔ Audio tracks / multiple tracks — not yet implemented
- ⛔ Fragmented MP4 (`moof`/`mfra`) — not yet implemented

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

## License

MIT OR Apache-2.0
