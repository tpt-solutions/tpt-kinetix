#!/usr/bin/env bash
# Plays CENC-protected (ClearKey) DASH output in headless Chrome with dash.js:
# the browser's EME must decrypt the VP9 and Opus fragments with the key, and
# playback must advance with video decoded. Uses VP9 + Opus, the royalty-free
# codecs that whole-sample CENC is valid for.
# CENC_SCHEME=cbcs selects the CBC scheme (default cenc).
# Needs: node >= 18, Chrome (set CHROME=/path/to/chrome), curl + network (to fetch
# dash.js once), ffmpeg with libvpx-vp9 and libopus, a built tpt-kinetix.
set -euo pipefail
cd "$(dirname "$0")/.."
chrome="${CHROME:-}"
if [ -z "$chrome" ]; then
  for c in "/c/Program Files/Google/Chrome/Application/chrome.exe" \
           "/usr/bin/google-chrome" "/usr/bin/chromium" \
           "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"; do
    [ -x "$c" ] && chrome="$c" && break
  done
fi
[ -n "$chrome" ] || { echo "set CHROME to a Chrome/Chromium binary" >&2; exit 2; }

libs=target/browser-libs
mkdir -p "$libs"
[ -s "$libs/dash.js" ] || curl -sL -o "$libs/dash.js" "https://cdn.jsdelivr.net/npm/dashjs@4/dist/dash.all.min.js"

work="${TMPDIR:-/tmp}/tpt-browser-cenc-test"
rm -rf "$work" && mkdir -p "$work"
ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=320x240:rate=25 -t 12 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -t 12 -pix_fmt yuv420p \
  -c:v libvpx-vp9 -g 25 -b:v 300k -c:a libopus -ac 2 -b:a 64k "$work/av.webm"
cargo build --release -q -p tpt-kinetix-cli
target/release/tpt-kinetix remux "$work/av.webm" "$work/av.mp4" --faststart

# A fixed test key. The key ID and key are public test values, never real content keys.
export CENC_KID=00112233445566778899aabbccddeeff
export CENC_KEY=0f0e0d0c0b0a09080706050403020100
port=8863
target/release/tpt-kinetix serve "$work/av.mp4" --port "$port" --segment-seconds 3 \
  --cenc-key-id "$CENC_KID" --cenc-key "$CENC_KEY" --cenc-scheme "${CENC_SCHEME:-cenc}" >"$work/serve.log" 2>&1 &
server=$!
trap 'kill $server 2>/dev/null || true' EXIT
sleep 2
node tpt-kinetix-package/tests/browser/run.cjs "$chrome" "$PWD/$libs" "http://127.0.0.1:$port" 5 "dash:manifest.mpd"
