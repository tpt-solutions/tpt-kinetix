#!/usr/bin/env bash
# Task: "Change the container without re-encoding." WebM to MP4 (moov first for
# progressive playback), MP4 to fragmented MP4 on stdout, and one video track to IVF.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
make_sample

"$TPT" remux "$work/src.webm" "$work/out.mp4" --faststart
require_file "$work/out.mp4"
"$TPT" doctor "$work/out.mp4"

"$TPT" remux "$work/out.mp4" - --fragmented > "$work/frag.mp4"
require_file "$work/frag.mp4"

"$TPT" remux "$work/src.webm" "$work/video.ivf"
require_file "$work/video.ivf"

# Packets must survive: the MP4 keeps every video frame of the WebM (200 in the sample).
ffprobe -v error -count_packets -select_streams v:0 \
  -show_entries stream=nb_read_packets -of csv=p=0 "$work/out.mp4" > "$work/count.txt"
require_text "$work/count.txt" "200"
echo "OK: remux to MP4, fragmented MP4 and IVF"
