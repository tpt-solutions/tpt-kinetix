#!/usr/bin/env bash
# Builds the WASM packager, runs it from Node over asynchronous range reads, and
# requires its output to be byte-identical to the native `package` command's. The
# npm package is then packed and installed into a clean project, and that installed
# copy is the one tested.
# Needs: wasm-pack, node and npm, ffmpeg (to make a sample), and the wasm32 target.
set -euo pipefail
cd "$(dirname "$0")/.."
work="${TMPDIR:-/tmp}/tpt-wasm-package-test"
rm -rf "$work" && mkdir -p "$work"
ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=320x240:rate=25 -t 12 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -t 12 \
  -c:v libx264 -bf 2 -g 25 -pix_fmt yuv420p -c:a aac -ac 2 "$work/av.mp4"
# Add a WebVTT track, so subtitle and trick-play renditions are compared too.
cargo run --release -q -p tpt-kinetix-cli --example make_webvtt_sample -- "$work/av.mp4" "$work/src.mp4"
wasm-pack build tpt-kinetix-package --target nodejs --release --no-opt \
  --out-dir "$PWD/target/wasm-pkg" -- --features wasm
cargo run --release -q -p tpt-kinetix-cli -- package "$work/src.mp4" "$work/native" --segment-seconds 3

# The package stays private until the maintainer approves an npm scope and name.
node -e '
  const fs = require("fs");
  const p = "target/wasm-pkg/package.json";
  const j = JSON.parse(fs.readFileSync(p, "utf8"));
  j.private = true;
  fs.writeFileSync(p, JSON.stringify(j, null, 2) + "\n");
'
cp tpt-kinetix-package/npm/README.md target/wasm-pkg/README.md

tarball="$(npm pack --silent "$PWD/target/wasm-pkg" --pack-destination "$work" | tail -n 1)"
app="$work/npm-app"
mkdir -p "$app"
(cd "$app" && npm init -y >/dev/null && npm install --silent "$work/$tarball")
node tpt-kinetix-package/tests/wasm/node_test.cjs "$app/node_modules/tpt-kinetix-package" "$work/src.mp4" "$work/native" 3
