# Just-in-time HLS/DASH in a Worker

Packages MP4 files in object storage into HLS (fMP4) and DASH **on request**, with no pre-processing and
nothing stored: the Worker fetches the MP4's index (a couple of range reads) and, per segment, only that
segment's bytes. Segment responses are immutable, so the CDN caches them.

```
wasm-pack build tpt-kinetix-package --target web --release \
    --out-dir ../examples/edge-worker/pkg -- --features wasm
cd examples/edge-worker && npx wrangler deploy
# then:  https://<worker>/<object-key>/master.m3u8
```

`handler.mjs` is runtime-neutral (Workers, Deno, Fastly, Node 18+); `index.mjs` is the Workers entry.
`test.mjs` runs the handler in Node against a local range-capable origin and checks every response against
the native `tpt-kinetix package` output byte for byte (`just edge-worker-test`).

Untested here: a real Cloudflare deployment (no account), real R2 (the bucket code path is exercised in
`test.mjs` with a fake bucket), and Workers' CPU/memory limits under load.
