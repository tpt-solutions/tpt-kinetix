'use strict';

// Exercises the Node package on ffmpeg-made media. Usage:
//   node test.cjs <h264-aac.mp4> <vp9-opus.webm> <scratch dir>
// Run by tools/node-binding-test.sh against the packed tarball, installed into a
// clean project, so the test sees what npm users get.
const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const tk = require('tpt-kinetix-node');

const [mp4, webm, scratch] = process.argv.slice(2);
const KEY_ID = '00112233445566778899aabbccddeeff';
const KEY = 'ffeeddccbbaa99887766554433221100';

// The script passes the ffprobe path in FFPROBE, since a shell PATH is not always
// visible to a Windows child process.
const ffprobe = process.env.FFPROBE || 'ffprobe';

function codecs(file) {
  const out = execFileSync(ffprobe, ['-v', 'error', '-show_entries', 'stream=codec_name',
    '-of', 'json', file], { encoding: 'utf8' });
  return JSON.parse(out).streams.map((s) => s.codec_name);
}

const tests = [];
const test = (name, fn) => tests.push([name, fn]);

test('version is a semver string', () => {
  assert.match(tk.version(), /^\d+\.\d+\.\d+$/);
});

test('probe returns the tracks as an object', () => {
  const info = tk.probe(mp4);
  assert.ok(info.format.startsWith('mov'));
  assert.deepEqual(info.streams.map((s) => s.codec), ['h264', 'aac']);
  assert.equal(info.streams[0].width, 320);
  assert.equal(info.streams[1].channels, 2);
});

test('probe of a missing file throws', () => {
  assert.throws(() => tk.probe(path.join(scratch, 'nope.mp4')));
});

test('faststart and remux keep the tracks', async () => {
  const fast = path.join(scratch, 'fast.mp4');
  await tk.faststart(mp4, fast);
  assert.deepEqual(codecs(fast), ['h264', 'aac']);

  const webmOut = path.join(scratch, 'remux.webm');
  await tk.remux(webm, webmOut);
  assert.deepEqual(codecs(webmOut), ['vp9', 'opus']);

  const frag = path.join(scratch, 'frag.mp4');
  await tk.remux(mp4, frag, { fragmentMs: 1000 });
  assert.deepEqual(codecs(frag), ['h264', 'aac']);
});

test('package writes HLS and DASH and reports counts', async () => {
  const out = path.join(scratch, 'pkg');
  const counts = await tk.package(mp4, out, { segmentSeconds: 2 });
  assert.equal(counts.tracks, 2);
  assert.ok(counts.segments > 0);
  for (const name of ['master.m3u8', 'manifest.mpd', 'track-0.m3u8', 'init-0.mp4']) {
    assert.ok(fs.existsSync(path.join(out, name)), name);
  }
  assert.equal(fs.existsSync(path.join(out, 'key.json')), false);
});

test('CENC package writes a key file', async () => {
  const out = path.join(scratch, 'pkg-cenc');
  await tk.package(webm, out, { segmentSeconds: 2, keyId: KEY_ID, key: KEY });
  const license = JSON.parse(fs.readFileSync(path.join(out, 'key.json'), 'utf8'));
  assert.ok(license.keys);
});

test('half a CENC key rejects', async () => {
  await assert.rejects(tk.package(mp4, path.join(scratch, 'x'), { keyId: KEY_ID }));
});

let failed = 0;
(async () => {
  for (const [name, fn] of tests) {
    try {
      await fn();
      console.log(`ok - ${name}`);
    } catch (err) {
      failed += 1;
      console.log(`not ok - ${name}\n  ${err.message}`);
    }
  }
  if (failed > 0) {
    process.exitCode = 1;
  } else {
    console.log(`all ${tests.length} node binding tests passed`);
  }
})();
