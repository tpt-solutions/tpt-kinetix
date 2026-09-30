#!/usr/bin/env bash
#
# Fetch the FFmpeg FATE AV1 samples used by the AV1 conformance scorer
# (`tpt-kinetix-av1/examples/av1_fate_score.rs`) from
# https://fate-suite.ffmpeg.org/av1/ into fixtures/av1-fate/ (git-ignored).
#
# Usage:
#   tools/fetch-av1-fate.sh
#   KINETIX_AV1_FATE_DIR=$PWD/fixtures/av1-fate cargo run --release \
#       -p tpt-kinetix-av1 --example av1_fate_score
#
# Re-running skips files already present. Set DEST to override the location.

set -euo pipefail

BASE="https://fate-suite.ffmpeg.org/av1"
DEST="${DEST:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/fixtures/av1-fate}"
FILES=(
  decode_model.ivf
  film_grain.ivf
  frames_refs_short_signaling.ivf
  non_uniform_tiling.ivf
  seq_hdr_op_param_info.ivf
  switch_frame.ivf
  annexb.obu
)

mkdir -p "$DEST"
for f in "${FILES[@]}"; do
  if [[ -s "$DEST/$f" ]]; then
    echo "have $f"
  else
    echo "fetch $f"
    curl -fsSL --retry 3 -o "$DEST/$f" "$BASE/$f"
  fi
done
echo "KINETIX_AV1_FATE_DIR=$DEST"
