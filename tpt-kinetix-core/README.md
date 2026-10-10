# tpt-kinetix-core

Shared types that flow between all Kinetix crates. Pure Rust, no OS or threading
dependencies, so it also builds for `wasm32-unknown-unknown`.

- `VideoFrame`, `PointCloud` — decoded media (`frame`)
- `Packet`, `StreamInfo` — encoded data and per-track description (`packet`, `stream`)
- `Timestamp` — rational media timestamps
- `CodecId`, `MediaType`, `PixelFormat` — codec and format identifiers
- `EncodeConfig`, `RateControl`, `SpeedPreset` — codec-agnostic encoder settings
- `DecoderCapabilities` — what a decoder honestly supports (`pixel_exact`,
  `deterministic`, ...); `capabilities()` is the source of truth, not README prose
- `KinetixError` — the top-level error enum, including `NotPixelExact`

See the [workspace README](../README.md) for the full project overview, architecture diagram, and quickstart guide.
