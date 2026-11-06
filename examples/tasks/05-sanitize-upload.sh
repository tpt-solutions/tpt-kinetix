#!/usr/bin/env bash
# Task: "Accept user uploads safely." `sanitize` re-muxes a WebM or MP4, keeps
# only AV1, VP9 and Opus, and refuses inputs over its limits (removing any
# partial output). Both paths are checked.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
make_sample

"$TPT" sanitize "$work/src.webm" "$work/clean.webm"
require_file "$work/clean.webm"

if "$TPT" sanitize "$work/src.webm" "$work/refused.webm" --max-packets 10; then
  echo "FAIL: sanitize accepted an input over --max-packets" >&2
  exit 1
fi
if [ -e "$work/refused.webm" ]; then
  echo "FAIL: a refused upload left an output file behind" >&2
  exit 1
fi
echo "OK: clean output written; over-limit upload refused with no output"
