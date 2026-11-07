#!/usr/bin/env bash
# Builds the Python wheel (tpt-kinetix-py), installs it into a fresh virtualenv, and
# runs its tests against ffmpeg-made media. The tests run outside the source tree so
# they import the installed package, not the checkout.
# Needs: maturin, Python 3 with venv, ffmpeg and ffprobe (with libx264, libvpx, libopus).
set -euo pipefail
cd "$(dirname "$0")/.."
root="$PWD"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

py="${PYTHON:-python3}"
maturin build --release -q -m tpt-kinetix-py/Cargo.toml -i "$py" -o "$work/wheels"
"$py" -m venv "$work/venv"
venv_py="$work/venv/bin/python"
[ -x "$venv_py" ] || venv_py="$work/venv/Scripts/python.exe"
"$venv_py" -m pip install -q "$work"/wheels/*.whl
(cd "$work" && "$venv_py" "$root/tpt-kinetix-py/tests/test_bindings.py")
echo "python binding test passed"
