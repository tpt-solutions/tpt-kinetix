#!/usr/bin/env python3
"""Fail if a publishable workspace crate can pull in a patent-encumbered `out-*` crate.

Policy (see PATENTS.md): `out-*` crates are patent-encumbered, must be `publish = false`
(in Cargo.toml and release-plz.toml), and no publishable crate may depend on one through a
normal or build dependency. Dev-dependencies are not shipped to users and are allowed.
"""
import json
import re
import subprocess
import sys

meta = json.loads(
    subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"], text=True
    )
)
pkgs = {p["name"]: p for p in meta["packages"]}
encumbered = {n for n in pkgs if n.startswith("out-")}


def publishable(pkg):
    # cargo metadata: `publish` is null when publishing is unrestricted, [] when `publish = false`.
    return pkg["publish"] is None or len(pkg["publish"]) > 0


errors = []
for name in sorted(encumbered):
    if publishable(pkgs[name]):
        errors.append(f"{name}: must set `publish = false` in its Cargo.toml")

try:
    rp = open("release-plz.toml", encoding="utf-8").read()
except OSError:
    rp = ""
for name in sorted(encumbered):
    block = re.search(
        r'\[\[package\]\]\s*name\s*=\s*"%s"(.*?)(?=\n\[|\Z)' % re.escape(name), rp, re.S
    )
    if not block or not re.search(r"publish\s*=\s*false", block.group(1)):
        errors.append(f"{name}: needs a [[package]] block with `publish = false` in release-plz.toml")

for name, pkg in sorted(pkgs.items()):
    if name in encumbered or not publishable(pkg):
        continue
    for dep in pkg["dependencies"]:
        if dep["name"] in encumbered and dep["kind"] != "dev":
            errors.append(f"{name} (publishable) depends on encumbered crate {dep['name']}")

if errors:
    print("Encumbered-crate guard FAILED (see PATENTS.md):")
    for e in errors:
        print("  -", e)
    sys.exit(1)
print(f"OK: {len(encumbered)} encumbered crate(s) ({', '.join(sorted(encumbered))}) are unpublished and unreachable from published crates.")
