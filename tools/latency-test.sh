#!/usr/bin/env bash
# Glass-to-glass latency of the live HLS path, measured in headless Chrome.
#
# A page draws a wall-clock barcode, publishes it through MediaRecorder + WebSocket,
# plays the served HLS back with hls.js and decodes the barcode from the playing
# frame (see tpt-kinetix-stream/tests/browser/latency.html), so the number covers
# capture, encode, ingest, packaging, HTTP and decode. Prints one JSON line per
# configuration and a table.
#
# Needs: node >= 18, Chrome (CHROME=/path), curl + network once for hls.js.
#   SECONDS_MEASURED=20 (default), CONFIGS="seg:part:ll:mime ..." to override.
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
[ -s "$libs/hls.js" ] || curl -sL -o "$libs/hls.js" "https://cdn.jsdelivr.net/npm/hls.js@1/dist/hls.min.js"
cargo build -q -p tpt-kinetix-stream --example live_server
secs="${SECONDS_MEASURED:-20}"
# segment seconds : part seconds (0 = no parts) : hls.js lowLatencyMode : recorder mime
configs="${CONFIGS:-2:0:0:vp9 2:0.333:1:vp9 1:0.333:1:vp9 2:0.333:1:av1}"
port=8920
for cfg in $configs; do
  IFS=: read -r seg part ll codec <<<"$cfg"
  mime="video/webm;codecs=$codec,opus"
  port=$((port + 1))
  SEGMENT_SECONDS=$seg PART_SECONDS=$part target/debug/examples/live_server "$port" >/dev/null 2>&1 &
  server=$!
  sleep 1
  echo "--- segment ${seg}s, parts ${part}s, hls.js lowLatencyMode=${ll}, ${codec}"
  node tpt-kinetix-stream/tests/browser/latency.cjs "$chrome" "$PWD/$libs" "http://127.0.0.1:$port" "$ll" "$secs" "$mime" || echo "FAILED"
  kill $server 2>/dev/null || true
done
