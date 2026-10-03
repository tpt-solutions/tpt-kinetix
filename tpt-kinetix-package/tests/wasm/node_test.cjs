// Drives the WASM packager from Node with genuinely asynchronous range reads and
// requires its output to be byte-identical to the native CLI's `package` output.
//
//   wasm-pack build tpt-kinetix-package --target nodejs --release --no-opt \
//       --out-dir target/wasm-pkg -- --features wasm
//   tpt-kinetix package in.mp4 out --segment-seconds 3
//   node tpt-kinetix-package/tests/wasm/node_test.cjs target/wasm-pkg in.mp4 out 3
const fs = require("fs");
const assert = require("assert");
const path = require("path");

const [pkgDir, src, nativeDir, seconds] = process.argv.slice(2);
const { WasmPackager } = require(path.resolve(pkgDir, "tpt_kinetix_package.js"));

(async () => {
  const fh = await fs.promises.open(src, "r");
  const { size } = await fh.stat();
  let reads = 0;
  let bytes = 0;
  const read = async (offset, length) => {
    reads++;
    bytes += length;
    await new Promise((r) => setImmediate(r)); // really yield to the event loop
    const buf = Buffer.alloc(length);
    const { bytesRead } = await fh.read(buf, 0, length, offset);
    if (bytesRead !== length) throw new Error(`short read at ${offset}`);
    return new Uint8Array(buf);
  };

  const pk = await WasmPackager.open(size, read, Number(seconds));
  const openReads = reads;
  const same = (name, got) => {
    const want = fs.readFileSync(path.join(nativeDir, name));
    const g = typeof got === "string" ? Buffer.from(got) : Buffer.from(got);
    assert.ok(want.equals(g), `${name} differs from the native output`);
  };

  same("master.m3u8", pk.hlsMaster());
  same("manifest.mpd", pk.dashMpd());
  let files = 2;
  for (let t = 0; t < pk.trackCount; t++) {
    same(`track-${t}.m3u8`, pk.hlsMedia(t));
    same(`init-${t}.mp4`, pk.initSegment(t));
    files += 2;
    for (let n = 1; n <= pk.segmentCount; n++) {
      same(`seg-${t}-${n}.m4s`, await pk.mediaSegment(t, n));
      files++;
    }
  }
  assert.strictEqual(pk.requests, reads);
  await assert.rejects(pk.mediaSegment(0, 9999));
  assert.throws(() => pk.hlsMedia(99));
  console.log(
    `OK: ${files} files byte-identical to native output; ` +
      `index loaded with ${openReads} range read(s); ${reads} reads / ${(bytes / 1024).toFixed(0)} KiB of a ${(size / 1024).toFixed(0)} KiB file; ` +
      `${pk.trackCount} tracks x ${pk.segmentCount} segments; codecs ${pk.codec(0)} ${pk.codec(1)}`
  );
  await fh.close();
})().catch((e) => {
  console.error("FAIL:", e);
  process.exit(1);
});
