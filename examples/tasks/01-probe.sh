#!/usr/bin/env bash
# Task: "What is in this file?" Lists the tracks of a local file, then prints
# the same facts as ffprobe-shaped JSON for scripts.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
make_sample

"$TPT" probe "$work/src.webm"

"$TPT" probe --json "$work/src.webm" > "$work/probe.json"
require_text "$work/probe.json" '"codec_name":"vp9"'
require_text "$work/probe.json" '"codec_name":"opus"'
echo "OK: probe reports vp9 + opus"
