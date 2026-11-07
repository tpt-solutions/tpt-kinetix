# tpt-kinetix-node (Node.js)

Node.js bindings for the TPT Kinetix container tools: probe, faststart, remux and package
media files. Nothing is decoded; the work is done on the container, in-process, by a native
addon built with [napi-rs](https://napi.rs).

```js
const kinetix = require("tpt-kinetix-node");

const info = kinetix.probe("clip.webm");              // { format, streams: [...] }
kinetix.faststart("in.mp4", "fast.mp4");              // index moved to the front
kinetix.remux("in.mp4", "out.webm");                  // stream copy, container from the extension
kinetix.remux("in.mp4", "frag.mp4", { fragmentMs: 2000 });

const counts = await kinetix.package("in.mp4", "out/", { segmentSeconds: 6 });
// CENC (VP9, AV1 and Opus samples only): keyId and key, 32 hex digits each
await kinetix.package("in.webm", "out-cenc/", {
  keyId: "00112233445566778899aabbccddeeff",
  key: "ffeeddccbbaa99887766554433221100",
  scheme: "cenc",   // or "cbcs"
});
```

`probe`, `faststart` and `remux` are synchronous or return promises as their types say;
`package` returns a promise of `{ tracks, segments, files, bytes }`. Failures throw an `Error`.
Types are in [index.d.ts](index.d.ts).

## Building and testing

The addon is a Cargo cdylib, built from the repository root with
`cargo build --release -p tpt-kinetix-node`. `tools/node-binding-test.sh` does the rest: it stages
`index.js`, `index.d.ts`, `package.json` and the addon (as `native/tpt_kinetix_node.node`), packs the
npm tarball, installs it into a clean project, and runs `test/test.cjs` against ffmpeg-made media.

The npm name is approved, but the package stays `private` in `package.json` until prebuilt addons are
built in CI for each platform. A build from one machine would break installs on the others. Once that
exists, remove the `private` field and publish.
