#!/usr/bin/env bash
# Task: "Take a live stream from an encoder and play it as HLS while it arrives."
# ffmpeg publishes a WebM over HTTP to `tpt-kinetix live`, and the playlist is
# read back while the publish is still running.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
make_sample

port=8941
"$TPT" live --port "$port" --segment-seconds 2 >"$work/live.log" 2>&1 &
pids+=($!)
ffmpeg -loglevel error -re -i "$work/src.webm" -c copy -f webm \
  -method POST -chunked_post 1 "http://127.0.0.1:$port/ingest/cam" &
pids+=($!)

# Wait for the first playlist, then check it has segments.
for _ in $(seq 1 60); do
  curl -sf "http://127.0.0.1:$port/cam/master.m3u8" -o "$work/master.m3u8" && break
  sleep 0.5
done
require_text "$work/master.m3u8" "#EXTM3U"
curl -sf "http://127.0.0.1:$port/cam/track-0.m3u8" -o "$work/track-0.m3u8"
require_text "$work/track-0.m3u8" "#EXTINF"
echo "OK: live playlist served while the publish was still running"
