#!/usr/bin/env bash
# Build the JM reference H.264 decoder (ldecod) as a bit-exact conformance
# oracle, patched to dump per-MB pre/post-deblock luma and per-edge boundary
# strength. Used to root-cause the last non-bit-exact H.264 clips (freh*, HCHP*)
# where the ITU package ships no usable pixel/MV/bS trace.
#
# JM ldecod is the normative reference: its output is byte-identical to the ITU
# *_dec.yuv files. FFmpeg's public API cannot expose pre-deblock pixels, so a
# linked libavcodec harness is not an option; JM is small, pure C, and trivial
# to instrument.
#
# Requirements: git, a C toolchain. On Windows use mingw-w64 gcc (tested with
# 16.2.0); the patch makes JM's win32.{c,h} take the POSIX branch under mingw
# and forces O_BINARY on file IO. Needs -lws2_32 for ntohs/ntohl (rtp.c) and
# mingw's binmode.o so fopen/open default to binary.
#
#   ./tools/build-jm-oracle.sh <out-dir>       # clones + patches + builds
#   <out-dir>/ldecod.exe -p InputFile=clip.264 -p OutputFile=out.yuv
#
# NOTE: JM's config parser mishandles paths containing spaces — copy the .264
# into the working dir first (e.g. `cp Freh1_B.264 in.264`).
#
# Dump hooks (env vars, read by the patched ldecod):
#   JM_DUMP_DIR=<dir> JM_DUMP_POC=<poc>   -> <dir>/jm_poc<poc>_{pre,post}deblock.gray
#                                           (8-bit luma, size_x*size_y bytes)
#   JM_TRACE_POC=<poc> JM_TRACE_MB=<addr[,addr...]>
#       -> stderr: per-edge "JM VER/HOR edge=.. A=.. B=.. bs=[..]" + p/q pixels
#          before and "OUT row/col .." after, for the listed macroblock addresses
#
# Map display frame -> POC from ldecod's own per-frame table (POC column).
set -euo pipefail
OUT="${1:?usage: build-jm-oracle.sh <out-dir>}"
JM_URL="${JM_URL:-https://vcgit.hhi.fraunhofer.de/jvet/JM.git}"
HERE="$(cd "$(dirname "$0")" && pwd)"

mkdir -p "$OUT"
if [ ! -d "$OUT/jm/.git" ]; then
  git clone --depth 1 "$JM_URL" "$OUT/jm"
fi
cd "$OUT/jm"
git checkout -- . 2>/dev/null || true
git apply "$HERE/jm-ldecod-oracle.patch"

: "${CC:=gcc}"
BINMODE="$("$CC" -print-file-name=binmode.o 2>/dev/null || true)"
EXTRA=()
[ -n "$BINMODE" ] && [ -f "$BINMODE" ] && EXTRA+=("$BINMODE")
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) EXTRA+=(-lws2_32);; esac

cd source
"$CC" -O2 -w -DTRACE=0 -D_FILE_OFFSET_BITS=64 \
  -I app/ldecod -I lib/lcommon \
  app/ldecod/*.c lib/lcommon/*.c "${EXTRA[@]}" \
  -o "../ldecod.exe" -lm

echo "built: $OUT/jm/ldecod.exe"
