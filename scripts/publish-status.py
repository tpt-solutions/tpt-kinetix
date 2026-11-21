#!/usr/bin/env python3
"""Show which workspace crates need publishing to crates.io.

For every workspace member that is not `publish = false`, this reports:

- the local version (from `cargo metadata`);
- the newest stable version on crates.io (404 means the name is not
  published yet);
- how many commits touched the crate since the release tag `v<local version>`,
  plus any uncommitted changes under the crate directory;
- an action: publish, bump, or nothing.

It also prints a publish order: each crate after the crates it depends on
(path dependencies inside the publishable set only).

Only the Python standard library and `cargo` / `git` on PATH are needed.
Run from anywhere: `python scripts/publish-status.py`, or `just publish-status`.
Network access to https://crates.io is required. Requests are sequential and
spaced one second apart, as the crates.io crawler policy asks.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
USER_AGENT = "tpt-kinetix-publish-status (release tooling; contact via repository issues)"
CRATES_API = "https://crates.io/api/v1/crates/{}"
SEMVER = re.compile(r"^(\d+)\.(\d+)\.(\d+)")


def run(args: list[str]) -> str:
    """Run a command in the repo root and return its stdout."""
    proc = subprocess.run(
        args, cwd=REPO, capture_output=True, text=True, check=False
    )
    if proc.returncode != 0:
        sys.exit(f"command failed: {' '.join(args)}\n{proc.stderr}")
    return proc.stdout


def parse_semver(text: str) -> tuple[int, int, int] | None:
    match = SEMVER.match(text or "")
    if not match:
        return None
    return tuple(int(part) for part in match.groups())  # type: ignore[return-value]


def workspace_crates() -> dict[str, dict]:
    """Publishable workspace members, keyed by crate name."""
    meta = json.loads(run(["cargo", "metadata", "--no-deps", "--format-version", "1"]))
    members = set(meta["workspace_members"])
    crates = {}
    for pkg in meta["packages"]:
        if pkg["id"] not in members:
            continue
        # `publish = false` is serialised as an empty list; no key means "any registry".
        if pkg["publish"] == []:
            continue
        crates[pkg["name"]] = {
            "version": pkg["version"],
            "dir": Path(pkg["manifest_path"]).parent,
            "deps": [
                dep["name"]
                for dep in pkg["dependencies"]
                if dep.get("path") and dep.get("kind") in (None, "normal")
            ],
        }
    return crates


def crates_io_version(name: str) -> str | None | bool:
    """Newest stable version on crates.io. None: not published. False: lookup failed."""
    request = urllib.request.Request(CRATES_API.format(name), headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            data = json.load(response)
    except urllib.error.HTTPError as err:
        if err.code == 404:
            return None
        return False
    except (urllib.error.URLError, TimeoutError):
        return False
    return data["crate"].get("max_stable_version") or data["crate"]["max_version"]


def tag_exists(tag: str) -> bool:
    proc = subprocess.run(
        ["git", "rev-parse", "-q", "--verify", f"refs/tags/{tag}"],
        cwd=REPO, capture_output=True, text=True, check=False,
    )
    return proc.returncode == 0


def commits_since(tag: str | None, path: Path) -> int:
    rel = path.relative_to(REPO).as_posix()
    rev = f"{tag}..HEAD" if tag else "HEAD"
    out = run(["git", "rev-list", "--count", rev, "--", rel]).strip()
    return int(out or 0)


def uncommitted(path: Path) -> int:
    rel = path.relative_to(REPO).as_posix()
    return len([line for line in run(["git", "status", "--porcelain", "--", rel]).splitlines() if line])


def publish_order(crates: dict[str, dict]) -> list[str]:
    """Topological order: dependencies first, ties broken by name."""
    order: list[str] = []
    done: set[str] = set()

    def visit(name: str, stack: tuple[str, ...]) -> None:
        if name in done:
            return
        if name in stack:
            sys.exit(f"dependency cycle among publishable crates: {' -> '.join(stack + (name,))}")
        for dep in sorted(crates[name]["deps"]):
            if dep in crates:
                visit(dep, stack + (name,))
        done.add(name)
        order.append(name)

    for name in sorted(crates):
        visit(name, ())
    return order


def decide(local: str, remote: str | None | bool, changed: int, dirty: int, tag_found: bool) -> str:
    if remote is False:
        return "unknown (crates.io lookup failed)"
    if remote is None:
        return "PUBLISH (new crate)"
    local_v, remote_v = parse_semver(local), parse_semver(remote)  # type: ignore[arg-type]
    if local_v and remote_v and local_v < remote_v:
        return "CHECK (crates.io is ahead of local)"
    if local_v and remote_v and local_v > remote_v:
        return "PUBLISH (version behind local)"
    if changed or dirty:
        note = "" if tag_found else " (no release tag v" + local + ")"
        return f"BUMP+PUBLISH (changed since release){note}"
    return "up to date"


def main() -> None:
    crates = workspace_crates()
    if not crates:
        sys.exit("no publishable workspace crates found")

    rows = []
    for name in sorted(crates):
        info = crates[name]
        local = info["version"]
        tag = f"v{local}"
        tag_found = tag_exists(tag)
        since = commits_since(tag if tag_found else None, info["dir"])
        dirty = uncommitted(info["dir"])
        remote = crates_io_version(name)
        time.sleep(1)  # crates.io asks for at most one request per second
        rows.append((name, local, remote, since, dirty, decide(local, remote, since, dirty, tag_found)))

    width = max(len(row[0]) for row in rows)
    print(f"{'crate':<{width}}  {'local':<8}  {'crates.io':<10}  {'commits':<8}  {'uncommitted':<11}  action")
    for name, local, remote, since, dirty, action in rows:
        remote_text = "(none)" if remote is None else ("error" if remote is False else str(remote))
        print(f"{name:<{width}}  {local:<8}  {remote_text:<10}  {since:<8}  {dirty:<11}  {action}")

    print()
    print("Commits are counted since the release tag v<local version>, or since the start of history")
    print("when that tag is missing. Each crate is versioned together (shared workspace version),")
    print("so a release normally bumps every crate. See release-plz.toml.")
    print()
    print("Publish order (dependencies first, publishable crates only):")
    for position, name in enumerate(publish_order(crates), start=1):
        print(f"  {position:>2}. {name}")


if __name__ == "__main__":
    main()
