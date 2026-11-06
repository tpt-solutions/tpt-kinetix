#!/usr/bin/env bash
# Task: "Inspect a file I can only reach over HTTP without downloading it."
# `probe` reads an MP4 over HTTP range requests, so only the index is fetched.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
make_sample
ffmpeg -loglevel error -y -i "$work/src.webm" -c copy -movflags +faststart "$work/remote.mp4"

# Pick a Python that actually runs (on Windows, `python3` can be a Store stub).
python=""
for candidate in "${PYTHON:-}" python3 python; do
  if [ -n "$candidate" ] && "$candidate" -c "import sys" >/dev/null 2>&1; then
    python="$candidate"
    break
  fi
done
[ -n "$python" ] || { echo "SKIP: no Python 3 found; set PYTHON=" >&2; exit 1; }

port=8931
"$python" "$(dirname "$0")/range_server.py" "$work" "$port" &
pids+=($!)
for _ in $(seq 1 50); do
  curl -s -o /dev/null "http://127.0.0.1:$port/remote.mp4" && break
  sleep 0.2
done

"$TPT" probe "http://127.0.0.1:$port/remote.mp4" | tee "$work/remote.txt"
require_text "$work/remote.txt" "codec=Vp9"
echo "OK: remote MP4 probed over HTTP range requests"
