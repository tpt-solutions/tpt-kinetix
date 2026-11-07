# tpt-kinetix-package (WebAssembly)

Just-in-time HLS and DASH packaging of MP4 files, in the browser or Node.js. The
packager reads only the bytes it needs, through a callback you provide, so a large
file never has to be loaded whole.

```js
const { WasmPackager } = require("tpt-kinetix-package");

// read(offset, length) must resolve to a Uint8Array of exactly `length` bytes.
const pk = await WasmPackager.open(fileSize, read, /* segment seconds */ 6);
pk.hlsMaster();          // master.m3u8
pk.hlsMedia(0);          // track-0.m3u8
pk.dashMpd();            // manifest.mpd
const init = pk.initSegment(0);            // init-0.mp4
const seg = await pk.mediaSegment(0, 1);   // seg-0-1.m4s
```

Subtitles and trick-play are included: `pk.subtitleCount`, `pk.subtitleMediaPlaylist(k)` and
`await pk.subtitleSegment(k, n)` for WebVTT tracks, and `await pk.iframePlaylist(v)` and
`await pk.iframeFile(v)` for the I-frame playlist and file of video track `v`.

`probe_mp4(bytes)` returns the track list of an in-memory MP4 as a JSON string.

Not included: WebM input (use the native packager), encryption, and remux. Remux is
file based and has no WebAssembly build yet.

The package is built with `wasm-pack --target nodejs`. Its output matches the native
`tpt-kinetix package` command byte for byte (`tools/wasm-package-test.sh`).
