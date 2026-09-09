#!/usr/bin/env bash
#
# Fetch the MPEG-4 / ISO-IEC 14496-26 AAC audio conformance bitstreams (the
# "al*" / "am*" streams) from the FFmpeg FATE sample suite into
# tpt-kinetix-aac/tests/fixtures/iso/ for the `iso_conformance` integration test.
#
# Source: https://fate-suite.ffmpeg.org/aac/  (public, no auth)
#   <name>.mp4  — the conformance bitstream (MP4-contained AAC)
#   <name>.s16  — ISO normative reference decoded PCM (kept for reference only)
#
# For each stream this script produces, using `ffmpeg`:
#   <name>.adts     — the raw AAC elementary stream (mp4 -> adts, stream copy)
#   <name>.ref.f32  — ffmpeg's f32le decode of that SAME elementary stream
#
# The test decodes `<name>.adts` with tpt-kinetix-aac and compares against
# `<name>.ref.f32` — a decoder-vs-decoder check on byte-identical input, which
# sidesteps container edit-list / encoder-delay trimming. `<name>.s16` (the ISO
# normative output) is downloaded too for manual cross-checks.
#
# Fixtures are git-ignored. Re-running skips streams already present.
# Requires: curl, ffmpeg on PATH.
#
# Usage:  tools/fetch-aac-conformance.sh
#         NAMES="al05_44 al18_44" tools/fetch-aac-conformance.sh

set -euo pipefail

BASE="https://fate-suite.ffmpeg.org/aac"
DEST="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/tpt-kinetix-aac/tests/fixtures/iso"

# stream name -> ISO reference .s16 basename (some are channel-reordered)
declare -A REF=(
  [al04_44]=al04_44               # LC mono
  [al05_44]=al05_44               # LC stereo
  [al06_44]=al06_44_reorder       # LC 5.1
  [al07_96]=al07_96_reorder       # LC 5.1 @ 96 kHz
  [al15_44]=al15_44_reorder       # SSR (unsupported — negative case)
  [al17_44]=al17_44               # LC 2x SCE, mismatched PCE (robustness)
  [al18_44]=al18_44               # LC mono, long
  [am00_88]=am00_88               # LC
  [am05_44]=am05_44_reorder       # LC multichannel
  [al22_chCfg0PCE_44]=al22_chCfg0PCE_44  # LC 7.1, channel_configuration 0 (PCE)
)

NAMES="${NAMES:-${!REF[*]}}"

if ! command -v ffmpeg >/dev/null; then
  echo "ffmpeg not found on PATH — required to build .adts / .ref.f32" >&2
  exit 1
fi

mkdir -p "$DEST"
for name in $NAMES; do
  ref="${REF[$name]:-$name}"
  out_dir="$DEST"
  adts="$out_dir/$name.adts"
  reff="$out_dir/$name.ref.f32"
  if [[ -f "$adts" && -f "$reff" ]]; then
    echo "skip $name (already present)"
    continue
  fi
  echo "fetch $name"
  curl -sSL --fail --max-time 120 -o "$out_dir/$name.mp4" "$BASE/$name.mp4"
  curl -sSL --fail --max-time 120 -o "$out_dir/$ref.s16"  "$BASE/$ref.s16" || true
  ffmpeg -hide_banner -loglevel error -y -i "$out_dir/$name.mp4" -c:a copy -f adts "$adts"
  ffmpeg -hide_banner -loglevel error -y -i "$adts" -f f32le -c:a pcm_f32le "$reff"
  rm -f "$out_dir/$name.mp4"
done
echo "done -> $DEST"
