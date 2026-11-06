#!/usr/bin/env bash
# Runs every task example against a release build of the CLI. CI runs this;
# run it locally after `cargo build --release -p tpt-kinetix-cli`.
set -euo pipefail
cd "$(dirname "$0")"
failed=0
for script in [0-9][0-9]-*.sh; do
  echo "== $script"
  if ! bash "$script"; then
    echo "FAILED: $script" >&2
    failed=1
  fi
done
exit $failed
