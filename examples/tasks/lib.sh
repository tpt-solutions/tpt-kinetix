# Shared helpers for the task examples. Sourced, not run.
# Each task script sets `set -euo pipefail`, then sources this file.

# Repo root, whatever directory the script was started from.
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# The CLI binary: TPT overrides it, else the release build (.exe on Windows).
if [ -z "${TPT:-}" ]; then
  if [ -x "$repo/target/release/tpt-kinetix" ]; then
    TPT="$repo/target/release/tpt-kinetix"
  else
    TPT="$repo/target/release/tpt-kinetix.exe"
  fi
fi

# Scratch directory, removed when the script exits.
work="$(mktemp -d)"

# Background processes to stop on exit: add a PID with `pids+=($!)`.
pids=()
cleanup() {
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done
  rm -rf "$work"
}
trap cleanup EXIT

# Writes an 8-second VP9 + Opus WebM sample to $work/src.webm.
make_sample() {
  ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=320x240:rate=25 -t 8 \
    -f lavfi -i sine=frequency=440:sample_rate=48000 -t 8 -pix_fmt yuv420p \
    -c:v libvpx-vp9 -g 25 -b:v 300k -c:a libopus -ac 2 -b:a 64k "$work/src.webm"
}

# Fails the script unless the file exists and is non-empty.
require_file() {
  [ -s "$1" ] || { echo "FAIL: expected output $1" >&2; exit 1; }
}

# Fails the script unless the text is found in the file.
require_text() {
  grep -q -- "$2" "$1" || { echo "FAIL: '$2' not found in $1" >&2; exit 1; }
}
