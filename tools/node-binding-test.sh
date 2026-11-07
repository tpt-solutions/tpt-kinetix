#!/usr/bin/env bash
# Builds the Node addon (tpt-kinetix-node), stages the npm package with the native
# library in it, packs it, installs the tarball into a clean project, and runs the
# package tests against ffmpeg-made media.
# Needs: node and npm, ffmpeg and ffprobe (with libx264, libvpx, libopus).
set -euo pipefail
cd "$(dirname "$0")/.."
root="$PWD"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cargo build --release -q -p tpt-kinetix-node

# The addon is the cdylib under its platform's name, loaded as a .node file.
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) lib="${CARGO_TARGET_DIR:-target}/release/tpt_kinetix_node.dll" ;;
  Darwin) lib="${CARGO_TARGET_DIR:-target}/release/libtpt_kinetix_node.dylib" ;;
  *) lib="${CARGO_TARGET_DIR:-target}/release/libtpt_kinetix_node.so" ;;
esac

stage="$work/stage"
mkdir -p "$stage/native"
cp tpt-kinetix-node/index.js tpt-kinetix-node/index.d.ts tpt-kinetix-node/package.json "$stage/"
cp "$lib" "$stage/native/tpt_kinetix_node.node"

# The package is private (see package.json); packing still works for the test.
tarball="$(cd "$work" && npm pack --silent "$stage" --pack-destination "$work" | tail -n 1)"
app="$work/app"
mkdir -p "$app"
(cd "$app" && npm init -y >/dev/null && npm install --silent "$work/$tarball")

ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=320x240:rate=25 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -t 4 \
  -c:v libx264 -pix_fmt yuv420p -c:a aac -ac 2 "$work/av.mp4"
ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=320x240:rate=25 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -t 4 \
  -c:v libvpx-vp9 -b:v 300k -pix_fmt yuv420p -c:a libopus -ac 2 "$work/av.webm"

mkdir "$work/scratch"
cp tpt-kinetix-node/test/test.cjs "$app/test.cjs"
ffprobe_bin="$(command -v ffprobe)"
if command -v cygpath >/dev/null 2>&1; then ffprobe_bin="$(cygpath -m "$ffprobe_bin")"; fi
(cd "$app" && FFPROBE="$ffprobe_bin" node test.cjs "$work/av.mp4" "$work/av.webm" "$work/scratch")
