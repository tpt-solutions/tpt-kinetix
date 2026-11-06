#!/usr/bin/env bash
# Task: "Make HLS and DASH for a player, from an MP4." Writes fMP4 segments,
# an HLS master and media playlists, and a DASH manifest into one folder.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
make_sample
"$TPT" remux "$work/src.webm" "$work/in.mp4" --faststart

"$TPT" package "$work/in.mp4" "$work/pkg" --segment-seconds 4
for f in master.m3u8 manifest.mpd track-0.m3u8 init-0.mp4 seg-0-1.m4s; do
  require_file "$work/pkg/$f"
done
require_text "$work/pkg/master.m3u8" "#EXTM3U"
require_text "$work/pkg/manifest.mpd" "<MPD"
echo "OK: HLS (master.m3u8) and DASH (manifest.mpd) written to a folder"
