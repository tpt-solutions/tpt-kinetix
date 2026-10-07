#!/usr/bin/env bash
# Publishes from a real browser: headless Chrome (fake camera + microphone) opens the live
# server's /publish page, MediaRecorder streams AV1/VP9 + Opus over a WebSocket, and the
# served HLS must appear live and decode in ffmpeg.
# TRANSPORT=whip publishes over WebRTC (WHIP, VP9 + Opus) instead of a WebSocket.
# Needs: node >= 18, Chrome (CHROME=/path), ffmpeg (FFMPEG=/path if not on node's PATH).
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
export FFMPEG="${FFMPEG:-$(command -v ffmpeg)}"
transport="${TRANSPORT:-ws}"   # ws (WebSocket + MediaRecorder) or whip (WebRTC)
features=()
[ "$transport" = whip ] && features=(--features whip)
cargo build -q -p tpt-kinetix-stream ${features[@]+"${features[@]}"} --example live_server
port="${PORT:-8899}"
target/debug/examples/live_server "$port" >"${TMPDIR:-/tmp}/tpt-publish-server.log" 2>&1 &
server=$!
trap 'kill $server 2>/dev/null || true' EXIT
sleep 1
node tpt-kinetix-stream/tests/browser/publish.cjs "$chrome" "http://127.0.0.1:$port"
