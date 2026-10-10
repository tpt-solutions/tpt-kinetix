# tpt-kinetix-package

Just-in-time HLS (fMP4) and DASH packaging of MP4 files.

Given an MP4 or WebM/Matroska file (a local file, an HTTP object, or any `AsyncReadAt`), `Packager` reads only the index,
plans key-frame-aligned segments, and builds playlists, init segments and media segments **on demand**
from ranged reads: nothing is pre-processed or stored. Pure Rust, no C, and it compiles to WebAssembly,
so the same code can run on a server, in a Cloudflare Worker, or in the browser.

Output names: `master.m3u8`, `track-{i}.m3u8`, `init-{i}.mp4`, `seg-{i}-{n}.m4s`, `manifest.mpd`.

`LivePackager` is the live counterpart: a sliding-window packager that turns pushed
packets into live fMP4 HLS (low-latency parts, blocking reload) and dynamic DASH. It is
what `tpt-kinetix-stream`'s `LiveServer` is built on.

The `wasm` feature adds JavaScript bindings (`wasm-pack build --features wasm`); see
`examples/edge-worker` for a Cloudflare Worker that packages MP4s in object storage on request.

See `todo-io.md` in the repository root.
