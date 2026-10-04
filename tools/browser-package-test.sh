#!/usr/bin/env bash
# Plays the just-in-time HLS and DASH output in headless Chrome with hls.js and
# dash.js (real MSE players) and requires playback to cross a segment boundary
# with video decoded and no errors.
# Needs: node >= 18, Chrome (set CHROME=/path/to/chrome), curl + network (to fetch
# the player libraries once), ffmpeg (to make a sample), a built tpt-kinetix.
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
[ -s "$libs/hls.js" ]  || curl -sL -o "$libs/hls.js"  "https://cdn.jsdelivr.net/npm/hls.js@1/dist/hls.min.js"
[ -s "$libs/dash.js" ] || curl -sL -o "$libs/dash.js" "https://cdn.jsdelivr.net/npm/dashjs@4/dist/dash.all.min.js"

work="${TMPDIR:-/tmp}/tpt-browser-test"
rm -rf "$work" && mkdir -p "$work"
ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=320x240:rate=25 -t 12 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -t 12 \
  -c:v libx264 -bf 2 -g 25 -pix_fmt yuv420p -c:a aac -ac 2 "$work/clip.mp4"
cargo build --release -q -p tpt-kinetix-cli
port=8861
target/release/tpt-kinetix serve "$work/clip.mp4" --port "$port" --segment-seconds 3 >"$work/serve.log" 2>&1 &
server=$!
trap 'kill $server 2>/dev/null || true' EXIT
sleep 2
node tpt-kinetix-package/tests/browser/run.cjs "$chrome" "$PWD/$libs" "http://127.0.0.1:$port" 5
