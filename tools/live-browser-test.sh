#!/usr/bin/env bash
# Publishes a real-time AV1 or VP9 + Opus WebM to `tpt-kinetix live` with ffmpeg and
# plays it with hls.js in headless Chrome *while it is still being published*.
# Needs: node >= 18, Chrome (CHROME=/path), curl + network (player library, once),
# ffmpeg with libaom-av1 or libvpx-vp9 and libopus, a built tpt-kinetix.
#   VCODEC=vp9 (default) or av1
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
[ -s "$libs/dash.js" ] || curl -sL -o "$libs/dash.js" "https://cdn.jsdelivr.net/npm/dashjs@4/dist/dash.all.min.js"

work="${TMPDIR:-/tmp}/tpt-live-browser-test"
rm -rf "$work" && mkdir -p "$work"
if [ "${VCODEC:-vp9}" = av1 ]; then venc=(-c:v libaom-av1 -cpu-used 8 -g 25); else venc=(-c:v libvpx-vp9 -g 25 -b:v 300k); fi
ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=320x240:rate=25 -t 30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -t 30 -pix_fmt yuv420p \
  "${venc[@]}" -c:a libopus -ac 2 -b:a 64k "$work/src.webm"

cargo build --release -q -p tpt-kinetix-cli
port=8871
target/release/tpt-kinetix live --port "$port" --segment-seconds 2 >"$work/live.log" 2>&1 &
server=$!
ffmpeg -loglevel error -re -i "$work/src.webm" -c copy -f webm -method POST -chunked_post 1 \
  "http://127.0.0.1:$port/ingest/cam" &
publisher=$!
trap 'kill $server $publisher 2>/dev/null || true' EXIT
sleep 8   # let a few segments accumulate; the publish keeps running (~30 s)
node tpt-kinetix-package/tests/browser/run.cjs "$chrome" "$PWD/$libs" "http://127.0.0.1:$port" 5 "hls:cam/master.m3u8"
# Record the measured numbers: the packager's live-edge floor plus the hls.js
# playback latency, so the "no numbers recorded" gap stays closed.
echo "--- packager live edge (/_stats) ---"
curl -s "http://127.0.0.1:$port/cam/_stats" | tee "$work/stats.json"; echo
echo "--- live manifest (LL-HLS parts) ---"
curl -s "http://127.0.0.1:$port/cam/track-0.m3u8" | head -n 20 | tee "$work/playlist.txt"
echo "--- low-latency DASH manifest ---"
curl -s "http://127.0.0.1:$port/cam/manifest.mpd" | head -n 12 | tee "$work/manifest.txt"
