#!/usr/bin/env bash
# Builds the WASM packager, runs it from Node over asynchronous range reads, and
# requires its output to be byte-identical to the native `package` command's.
# Needs: wasm-pack, node, ffmpeg (to make a sample), and the wasm32 target.
set -euo pipefail
cd "$(dirname "$0")/.."
work="${TMPDIR:-/tmp}/tpt-wasm-package-test"
rm -rf "$work" && mkdir -p "$work"
ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=320x240:rate=25 -t 12 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -t 12 \
  -c:v libx264 -bf 2 -g 25 -pix_fmt yuv420p -c:a aac -ac 2 "$work/src.mp4"
wasm-pack build tpt-kinetix-package --target nodejs --release --no-opt \
  --out-dir "$PWD/target/wasm-pkg" -- --features wasm
cargo run --release -q -p tpt-kinetix-cli -- package "$work/src.mp4" "$work/native" --segment-seconds 3
node tpt-kinetix-package/tests/wasm/node_test.cjs target/wasm-pkg "$work/src.mp4" "$work/native" 3
