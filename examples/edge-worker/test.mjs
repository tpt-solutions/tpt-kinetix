// Exercises handler.mjs in Node: a local origin that honours Range requests (and
// counts them), plus a fake R2 bucket. Every response must equal the native
// `tpt-kinetix package` output byte for byte.
//
//   node test.mjs <wasm-pkg-web-dir> <source.mp4> <native-package-dir>
import assert from "node:assert/strict";
import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { createHandler } from "./handler.mjs";

const [pkgDir, src, nativeDir] = process.argv.slice(2);
const glue = await import(pathToFileURL(path.resolve(pkgDir, "tpt_kinetix_package.js")).href);
const wasmModule = new WebAssembly.Module(
  fs.readFileSync(path.resolve(pkgDir, "tpt_kinetix_package_bg.wasm")),
);
const file = fs.readFileSync(src);
const etag = '"abc123"';

// An origin serving `file` at /clip.mp4 with Range support.
let originRequests = 0;
let originBytes = 0;
const origin = http.createServer((req, res) => {
  if (req.url !== "/clip.mp4") {
    res.statusCode = 404;
    return res.end("nope");
  }
  originRequests++;
  const m = /^bytes=(\d+)-(\d*)$/.exec(req.headers.range || "");
  if (!m) {
    res.statusCode = 200; // ignores Range: a non-compliant origin
    return res.end(file);
  }
  const a = Number(m[1]);
  const b = Math.min(m[2] === "" ? file.length - 1 : Number(m[2]), file.length - 1);
  const body = file.subarray(a, b + 1);
  originBytes += body.length;
  res.writeHead(206, {
    "Content-Range": `bytes ${a}-${b}/${file.length}`,
    "Content-Length": body.length,
    ETag: etag,
  });
  res.end(body);
});
await new Promise((r) => origin.listen(0, "127.0.0.1", r));
const ORIGIN_BASE = `http://127.0.0.1:${origin.address().port}`;

const names = fs.readdirSync(nativeDir).sort();
const get = (handle, env, name, init) => handle(new Request(`http://worker.test/clip.mp4/${name}`, init), env);
const bytes = async (res) => Buffer.from(await res.arrayBuffer());

// 1. Every native output file is reproduced exactly through HTTP-range reads.
const handle = createHandler({ ...glue, wasmModule });
const env = { ORIGIN_BASE };
for (const name of names) {
  const res = await get(handle, env, name);
  assert.equal(res.status, 200, `${name}: ${res.status} ${await res.clone().text()}`);
  assert.ok((await bytes(res)).equals(fs.readFileSync(path.join(nativeDir, name))), `${name} differs`);
  const cc = res.headers.get("cache-control");
  const immutable = /^(init-|seg-)/.test(name);
  assert.equal(/immutable/.test(cc), immutable, `${name}: cache-control ${cc}`);
  assert.equal(res.headers.get("access-control-allow-origin"), "*");
}
console.log(`ok  ${names.length} files byte-identical to the native output (origin: ${originRequests} range requests, ${(originBytes / 1024).toFixed(0)} KiB of ${(file.length / 1024).toFixed(0)} KiB)`);

// 2. A fresh isolate serving one segment reads the index plus that segment only.
{
  originRequests = 0;
  originBytes = 0;
  const fresh = createHandler({ ...glue, wasmModule });
  const seg = names.find((n) => n.startsWith("seg-0-2"));
  assert.equal((await get(fresh, env, seg)).status, 200);
  const first = originRequests;
  assert.equal((await get(fresh, env, seg)).status, 200);
  assert.ok(originRequests - first <= 3, "a repeat segment costs only its own range reads");
  assert.ok(first <= 8, `${first} origin requests for one cold segment`);
  // On a large file one segment is a small fraction of the bytes; the sample used
  // by `just edge-worker-test` is tiny, so only assert this for big inputs.
  if (file.length > 8_000_000) {
    assert.ok(originBytes < file.length / 2, `read ${originBytes} of ${file.length} bytes for one segment`);
  }
  console.log(`ok  one cold segment: ${first} origin requests, ${(originBytes / 1024).toFixed(0)} KiB`);
}

// 3. Validators, HEAD, errors.
{
  const res = await get(handle, env, "master.m3u8");
  const tag = res.headers.get("etag");
  assert.ok(tag && tag.startsWith('W/"'), `etag ${tag}`);
  const cached = await get(handle, env, "master.m3u8", { headers: { "If-None-Match": tag } });
  assert.equal(cached.status, 304);
  const head = await get(handle, env, "master.m3u8", { method: "HEAD" });
  assert.equal(head.status, 200);
  assert.equal((await bytes(head)).length, 0);

  assert.equal((await get(handle, env, "seg-0-9999.m4s")).status, 404);
  assert.equal((await get(handle, env, "seg-9-1.m4s")).status, 404);
  assert.equal((await get(handle, env, "nonsense.txt")).status, 404);
  assert.equal((await handle(new Request("http://worker.test/missing.mp4/master.m3u8"), env)).status, 404);
  assert.equal((await handle(new Request("http://worker.test/clip.mp4/master.m3u8", { method: "POST" }), env)).status, 405);
  assert.equal((await handle(new Request("http://worker.test/master.m3u8"), env)).status, 404);
  // A non-compliant origin (ignores Range) is a 502, not a silent full download.
  const noRange = http.createServer((q, r) => r.end(file));
  await new Promise((ok) => noRange.listen(0, "127.0.0.1", ok));
  const bad = await createHandler({ ...glue, wasmModule })(
    new Request("http://worker.test/clip.mp4/master.m3u8"),
    { ORIGIN_BASE: `http://127.0.0.1:${noRange.address().port}` },
  );
  assert.equal(bad.status, 502);
  noRange.close();
  console.log("ok  validators, HEAD, 404/405/502 paths");
}

// 4. The R2-style binding path yields the same bytes.
{
  const bucket = {
    async head(key) {
      return key === "clip.mp4" ? { size: file.length, httpEtag: '"r2tag"' } : null;
    },
    async get(key, opts) {
      if (key !== "clip.mp4") return null;
      const { offset, length } = opts.range;
      const body = file.subarray(offset, offset + length);
      return { arrayBuffer: async () => body.buffer.slice(body.byteOffset, body.byteOffset + body.length) };
    },
  };
  const r2 = createHandler({ ...glue, wasmModule });
  for (const name of names.filter((n) => /^(master|init-0|seg-1-3)/.test(n))) {
    const res = await get(r2, { BUCKET: bucket }, name);
    assert.equal(res.status, 200);
    assert.ok((await bytes(res)).equals(fs.readFileSync(path.join(nativeDir, name))), `R2 ${name} differs`);
  }
  assert.equal((await r2(new Request("http://worker.test/nope.mp4/master.m3u8"), { BUCKET: bucket })).status, 404);
  console.log("ok  R2-style bucket binding");
}

origin.close();
