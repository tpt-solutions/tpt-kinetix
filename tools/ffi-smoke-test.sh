#!/usr/bin/env bash
# Builds the C ABI (tpt-kinetix-ffi) as a shared library, links the C smoke test
# against it, and runs the smoke test on generated media: an H.264 + AAC MP4 (probe,
# faststart, remux, package) and a VP9 + Opus WebM (CENC package; CENC only covers
# those codecs).
# Needs: ffmpeg with libx264, libvpx and libopus; cbindgen (to check the committed
# header is current); a C compiler (cc, gcc or clang on PATH).
set -euo pipefail
cd "$(dirname "$0")/.."
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# The committed header must match what cbindgen generates from the Rust source.
cbindgen --lang c --crate tpt-kinetix-ffi --output "$work/tpt_kinetix.h"
if ! diff -u --strip-trailing-cr tpt-kinetix-ffi/include/tpt_kinetix.h "$work/tpt_kinetix.h" >/dev/null; then
  echo "tpt-kinetix-ffi/include/tpt_kinetix.h is stale: run 'just ffi-header'" >&2
  exit 1
fi

cargo build --release -q -p tpt-kinetix-ffi

# Copy the library out of the tree: a path with spaces breaks the -L argument when
# the linker is a Windows binary reached through MSYS.
lib_dir="$work/lib"
mkdir -p "$lib_dir"
for f in libtpt_kinetix_ffi.so libtpt_kinetix_ffi.dylib tpt_kinetix_ffi.dll \
         tpt_kinetix_ffi.dll.lib libtpt_kinetix_ffi.a; do
  if [ -f "${CARGO_TARGET_DIR:-target}/release/$f" ]; then cp "${CARGO_TARGET_DIR:-target}/release/$f" "$lib_dir/"; fi
done
if [ -f "$lib_dir/tpt_kinetix_ffi.dll.lib" ]; then
  cp "$lib_dir/tpt_kinetix_ffi.dll.lib" "$lib_dir/tpt_kinetix_ffi.lib"
fi

cc_bin="${CC:-$(command -v cc || command -v gcc || command -v clang)}"
exe="$work/smoke"
libs=(-L"$lib_dir" -ltpt_kinetix_ffi)
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) exe="$exe.exe" ;;
  Darwin) ;;
  *) libs+=(-lpthread -ldl -lm) ;;
esac
"$cc_bin" -Wall -Wextra -Werror -Itpt-kinetix-ffi/include \
  tpt-kinetix-ffi/tests/c/smoke.c -o "$exe" "${libs[@]}"

ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=320x240:rate=25 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -t 4 \
  -c:v libx264 -pix_fmt yuv420p -c:a aac -ac 2 "$work/av.mp4"
ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=320x240:rate=25 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -t 4 \
  -c:v libvpx-vp9 -b:v 300k -pix_fmt yuv420p -c:a libopus -ac 2 "$work/av.webm"

mkdir "$work/out"
# Windows finds the DLL through PATH, but the ELF and Mach-O loaders do not: they
# search LD_LIBRARY_PATH and DYLD_LIBRARY_PATH.
PATH="$lib_dir:$PATH" LD_LIBRARY_PATH="$lib_dir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
  DYLD_LIBRARY_PATH="$lib_dir${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}" \
  "$exe" "$work/av.mp4" "$work/av.webm" "$work/out"
echo "ffi smoke test passed"
