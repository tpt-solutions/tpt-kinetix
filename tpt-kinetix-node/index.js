'use strict';

// Public API over the native addon. The addon is built by tools/node-binding-test.sh
// (or `cargo build -p tpt-kinetix-node --release` plus a copy, see README.md).
const native = require('./native/tpt_kinetix_node.node');

/**
 * Lists the tracks of a WebM, Matroska, IVF or MP4 file.
 * @param {string} path
 * @returns {{format: string, streams: Array<object>}}
 */
function probe(path) {
  return JSON.parse(native.probeJson(path));
}

/** Writes a copy of `input` with the MP4 index moved to the front. */
function faststart(input, output) {
  return native.faststart(input, output);
}

/**
 * Copies the streams of `input` into `output` without decoding. The container follows
 * the extension of `output`.
 * @param {string} input
 * @param {string} output
 * @param {{faststart?: boolean, fragmentMs?: number}} [options]
 */
function remux(input, output, options = {}) {
  return native.remux(input, output, Boolean(options.faststart), options.fragmentMs);
}

/**
 * Writes an HLS + DASH package of `input` into `outDir`.
 * @param {string} input
 * @param {string} outDir
 * @param {{segmentSeconds?: number, keyId?: string, key?: string, scheme?: 'cenc'|'cbcs'}} [options]
 * @returns {Promise<{tracks: number, segments: number, files: number, bytes: number}>}
 */
function pkg(input, outDir, options = {}) {
  return native.package(
    input,
    outDir,
    options.segmentSeconds ?? 6,
    options.keyId,
    options.key,
    options.scheme ?? 'cenc',
  );
}

module.exports = {
  version: native.version,
  probe,
  faststart,
  remux,
  package: pkg,
};
