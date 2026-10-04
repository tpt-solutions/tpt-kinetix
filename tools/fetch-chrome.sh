#!/usr/bin/env bash
# Downloads a Chrome-for-Testing build into ~/.cache/tpt-chrome and prints its
# path, for the browser playback tests. The two test scripts pick it up from
# $CHROME, so CI just runs this and then the test.
#
# Uses the Chrome-for-Testing "known-good-versions" JSON endpoint, which needs
# no API key. If a system Chrome/Chromium is already on PATH, that is used
# instead and nothing is downloaded.
set -euo pipefail

cache="${TPT_CHROME_CACHE:-$HOME/.cache/tpt-chrome}"

# A system browser is fine and saves the download.
if [ -z "${CHROME:-}" ]; then
  for c in /usr/bin/google-chrome /usr/bin/google-chrome-stable \
           /usr/bin/chromium /usr/bin/chromium-browser; do
    if [ -x "$c" ]; then
      echo "$c"
      exit 0
    fi
  done
fi

mkdir -p "$cache"
marker="$cache/CHROME_PATH"
if [ -f "$marker" ] && [ -x "$(cat "$marker")" ]; then
  cat "$marker"
  exit 0
fi

# The Chrome-for-Testing platform key for this machine.
chrome_platform() {
  case "$(uname -s)" in
    Darwin) echo "mac-arm64" ;;
    Linux)
      case "$(uname -m)" in
        aarch64 | arm64) echo "linux-arm64" ;;
        *) echo "linux64" ;;
      esac
      ;;
    *) echo "unsupported-$(uname -s)" ;;
  esac
}

channel="${CHROME_CHANNEL:-Stable}"
base="https://googlechromelabs.github.io/chrome-for-testing"
latest="$base/latest-patch-versions-per-build-with-downloads.json"

# Pick the version and download URL for this platform out of the JSON. Node is
# already a prerequisite of both browser tests, so use it rather than jq.
read -r version url < <(curl -fsSL "$latest" | CHANNEL="$channel" PLATFORM="$(chrome_platform)" node -e '
const fs = require("fs");
const d = JSON.parse(fs.readFileSync(0, "utf8"));
const ch = d.channels[process.env.CHANNEL];
if (!ch) { console.error(`unknown channel ${process.env.CHANNEL}`); process.exit(2); }
const hit = (ch.downloads.chrome || []).find(x => x.platform === process.env.PLATFORM);
if (!hit) { console.error(`no chrome build for ${process.env.PLATFORM}`); process.exit(2); }
console.log(ch.version, hit.url);
')
if [ -z "${url:-}" ]; then
  echo "fetch-chrome: could not resolve a download URL" >&2
  exit 2
fi

archive="$cache/chrome.zip"
echo "fetching Chrome for Testing $version" >&2
curl -fsSL -o "$archive" "$url"
# The zip always contains one top-level directory named after the platform.
if command -v unzip >/dev/null 2>&1; then
  unzip -q -o "$archive" -d "$cache"
else
  python3 -c "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])" \
    "$archive" "$cache"
fi
rm -f "$archive"

# Find the binary inside the extracted tree rather than guessing its path.
exe=$(find "$cache" -maxdepth 3 -type f \( -name chrome -o -name 'Google Chrome' \) \
      -perm -u+x 2>/dev/null | head -n 1)
if [ -z "$exe" ]; then
  echo "fetch-chrome: no Chrome binary found under $cache" >&2
  exit 2
fi
chmod +x "$exe"
echo "$exe" > "$marker"
echo "$exe"
