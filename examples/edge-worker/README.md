# Just-in-time HLS/DASH in a Worker

Packages MP4 files in object storage into HLS (fMP4) and DASH **on request**, with no pre-processing
and nothing stored: the Worker fetches the file's index (a couple of range reads) and, per segment,
only that segment's bytes. The WebAssembly build reads MP4 only, so WebM input is not served here.

## Routes

For an object `<key>` the Worker answers:

| Request | Response |
| --- | --- |
| `/<key>/master.m3u8` | HLS master playlist |
| `/<key>/track-<i>.m3u8`, `/<key>/init-<i>.mp4`, `/<key>/seg-<i>-<n>.m4s` | HLS media playlist, init segment and media segments |
| `/<key>/iframes-<v>.m3u8`, `/<key>/iframes-<v>.m4s` | Trick-play I-frame playlist and file for video track `v` |
| `/<key>/subs-<k>.m3u8`, `/<key>/sub-<k>-<n>.vtt` | WebVTT subtitle playlist and segments for subtitle track `k` |
| `/<key>/manifest.mpd` | DASH MPD |

Init, segment, I-frame and subtitle responses are immutable for a given object key. They are stored
in the Cache API (`caches.default`) where the runtime has one, and answered from there before the
origin is read. Failures are not cached. Segments honour a single `Range` header (206, or 416 when
unsatisfiable), so a player can fetch part of a segment.

## Deploying

```
wasm-pack build tpt-kinetix-package --target web --release \
    --out-dir ../examples/edge-worker/pkg -- --features wasm
cd examples/edge-worker && npx wrangler deploy
# then:  https://<worker>/<object-key>/master.m3u8
```

`handler.mjs` is runtime-neutral (Workers, Deno, Fastly, Node 18+); `index.mjs` is the Workers entry.
`test.mjs` runs the handler in Node against a local range-capable origin and checks every response
against the native `tpt-kinetix package` output byte for byte (`just edge-worker-test`).

Untested here: a real Cloudflare deployment (no account), real R2 (the bucket code path is exercised in
`test.mjs` with a fake bucket), and Workers' CPU/memory limits under load.
