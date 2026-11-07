# tpt-kinetix-package

Just-in-time HLS (fMP4) and DASH packaging of MP4 and WebM/Matroska files.

Given a file (a local file, an HTTP object, or any `AsyncReadAt`), `Packager` reads only the index,
plans key-frame-aligned segments, and builds playlists, init segments and media segments **on demand**
from ranged reads: nothing is pre-processed or stored. Pure Rust, no C, and it compiles to WebAssembly,
so the same code can run on a server, in a Cloudflare Worker, or in the browser.

## What it produces

| Output | Content |
| --- | --- |
| `master.m3u8` | HLS master playlist, listing the tracks as renditions; audio and subtitle renditions carry `LANGUAGE` |
| `track-{i}.m3u8`, `init-{i}.mp4`, `seg-{i}-{n}.m4s` | HLS media playlist, init segment and media segments for track `i` |
| `iframes-{v}.m3u8` | I-frame (trick-play) playlist for video track `v`, one rendition per video track |
| `subs-{k}.m3u8` | WebVTT subtitle rendition `k`, one per subtitle track, referenced from the master and DASH |
| `manifest.mpd` | DASH MPD, with a set per track: video, audio and text (`lang` on each set) |
| `key.json` | CENC ClearKey (when encrypted): the key line the playlists point at |

Encryption is CENC with the `cenc` or `cbcs` scheme, ClearKey only, for VP9, AV1 and Opus samples.
The keys are passed in by the caller; the packager fetches none.

`LivePackager` is the live counterpart: a sliding-window packager that turns pushed
packets into live fMP4 HLS (low-latency parts, blocking reload, LL-HLS subtitle parts) and dynamic
DASH, with the same CENC options. It is what `tpt-kinetix-stream`'s `LiveServer` is built on.

The `wasm` feature adds JavaScript bindings (`wasm-pack build --features wasm`). They cover HLS, DASH,
subtitles, trick-play and init/media segments, but not WebM input, encryption or remux. The npm
package for them is described in [npm/README.md](npm/README.md). `examples/edge-worker` is a
Cloudflare Worker that packages MP4s in object storage on request.

Changes are listed in [CHANGELOG.md](CHANGELOG.md). Open work is in [todo-io.md](../todo-io.md).
