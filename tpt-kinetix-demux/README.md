# tpt-kinetix-demux

Container demuxers exposing a uniform packet-extraction API via the `Demuxer`
trait.

- **MP4 / ISO-BMFF** (`mp4`) — full box parser, track/codec identification,
  sample-table timing, packet extraction.
- **Matroska / WebM** (`mkv`, `mkv_reader`, `mkv_stream`) — `MkvDemuxer` is a basic
  whole-buffer EBML reader (track enumeration, SimpleBlock/Block extraction, no advanced
  lacing). `MkvReader` is indexed and works over a `ReadAt` source, with the `Cues`
  index parsed. `MkvStream` is an incremental (sans-IO) parser for live AV1 / VP9 / Opus
  WebM: unknown-size segments and clusters, any chunk boundaries, and `av1C` / `vpcC` /
  `dOps` records built for MP4.
- **IVF** (`ivf`) — raw AV1/VP9 elementary streams (`DKIF`): fourcc, geometry,
  per-frame ranges, seeking by key frame.
- **MPEG-TS** (`ts`) — 188-byte packet sync/re-sync, PAT/PMT parsing
  (including multi-packet PSI sections), PES depacketization with PTS/DTS
  recovery, PCR tracking, and adaptation-field random-access indicators for
  key-frame detection. Covers broadcast and HLS `.ts` segment input; H.264
  packets come out Annex-B framed. This crate only frames and carries codec bytes; it contains no
  H.264 decoder (see PATENTS.md).

`Mp4Reader` / `MkvReader` read through the `ReadAt` / `AsyncReadAt` traits (files,
memory, HTTP range requests with the `http` feature), so large inputs are never
slurped whole.

## WebAssembly

`tpt-kinetix-demux` and `tpt-kinetix-core` are pure-Rust with no OS/threading
dependencies, so they build for `wasm32-unknown-unknown` for in-browser
container/codec inspection:

```sh
rustup target add wasm32-unknown-unknown
cargo build -p tpt-kinetix-core -p tpt-kinetix-demux --target wasm32-unknown-unknown
```

This is verified on CI (the `WASM (demux + core)` job).

See the [workspace README](../README.md) for the full project overview,
architecture diagram, and quickstart guide.
