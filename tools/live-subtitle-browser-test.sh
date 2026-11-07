#!/usr/bin/env bash
# Publishes a real-time VP9 + Opus WebM that carries a WebVTT subtitle track to
# `tpt-kinetix live` with ffmpeg, and plays the live HLS in headless Chrome with
# hls.js *while it is still being published*. Passes only when the subtitle
# group's cues reach the video's text track, which exercises the live subtitle
# playlist with its LL-HLS parts (`sub-{k}-{n}-{i}.vtt`).
# Needs: node >= 18, Chrome (CHROME=/path), curl + network (player library, once),
# ffmpeg with libvpx-vp9 and libopus, a built tpt-kinetix.
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

work="${TMPDIR:-/tmp}/tpt-live-subtitle-browser-test"
rm -rf "$work" && mkdir -p "$work"
# A cue every two seconds, so every live segment and part has subtitle text.
{
  echo "WEBVTT"
  for i in $(seq 0 14); do
    s=$((i * 2)); e=$((s + 1))
    printf '\n00:00:%02d.000 --> 00:00:%02d.800\nCue %d\n' "$s" "$e" "$i"
  done
} > "$work/subs.vtt"
ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=320x240:rate=25 -t 30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -t 30 -pix_fmt yuv420p \
  -c:v libvpx-vp9 -g 25 -b:v 300k -c:a libopus -ac 2 -b:a 64k "$work/av.webm"
ffmpeg -loglevel error -y -i "$work/av.webm" -i "$work/subs.vtt" \
  -map 0 -map 1 -c copy -c:s webvtt "$work/src.webm"

cargo build --release -q -p tpt-kinetix-cli
bin="${CARGO_TARGET_DIR:-target}/release/tpt-kinetix"
port=8872
"$bin" live --port "$port" --segment-seconds 2 >"$work/live.log" 2>&1 &
server=$!
publisher=
trap 'kill $server ${publisher:-} 2>/dev/null || true' EXIT
for _ in $(seq 1 100); do   # wait until the server answers before publishing
  curl -s -o /dev/null "http://127.0.0.1:$port/" && break
  sleep 0.2
done
ffmpeg -loglevel error -re -i "$work/src.webm" -c copy -f webm -method POST -chunked_post 1 \
  "http://127.0.0.1:$port/ingest/cam" &
publisher=$!
sleep 8   # let a few segments and parts accumulate; the publish keeps running (~30 s)
echo "--- live subtitle playlist (parts of the segment in progress) ---"
curl -s "http://127.0.0.1:$port/cam/subs-0.m3u8" | tee "$work/subs.m3u8" | tail -n 12
node tpt-kinetix-package/tests/browser/run.cjs "$chrome" "$PWD/$libs" "http://127.0.0.1:$port" 10 "hlssubs:cam/master.m3u8"
