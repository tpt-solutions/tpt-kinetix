#!/usr/bin/env bash
# Runs examples/edge-worker/handler.mjs in Node against a local range-capable
# origin and a fake R2 bucket; every response must equal the native
# `tpt-kinetix package` output byte for byte.
# Needs: wasm-pack, node >= 18, ffmpeg (to make a sample), the wasm32 target.
set -euo pipefail
cd "$(dirname "$0")/.."
work="${TMPDIR:-/tmp}/tpt-edge-worker-test"
rm -rf "$work" && mkdir -p "$work"
ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=320x240:rate=25 -t 12 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -t 12 \
  -c:v libx264 -bf 2 -g 25 -pix_fmt yuv420p -c:a aac -ac 2 "$work/clip.mp4"
wasm-pack build tpt-kinetix-package --target web --release --no-opt \
  --out-dir "$PWD/target/wasm-pkg-web" -- --features wasm
# The handler packages with 6 s segments; use the same for the reference output.
cargo run --release -q -p tpt-kinetix-cli -- package "$work/clip.mp4" "$work/native" --segment-seconds 6
node examples/edge-worker/test.mjs target/wasm-pkg-web "$work/clip.mp4" "$work/native"
