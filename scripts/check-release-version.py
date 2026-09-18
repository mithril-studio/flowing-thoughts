#!/usr/bin/env python3
"""Reject version drift before spending time on a signed release build."""
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parent.parent


def main():
    if len(sys.argv) != 2 or not re.fullmatch(r"v\d+\.\d+\.\d+", sys.argv[1]):
        sys.exit("Expected a stable release tag: vMAJOR.MINOR.PATCH")
    expected = sys.argv[1][1:]
    for name in ("package.json", "package-lock.json", "src-tauri/tauri.conf.json"):
        data = json.loads((ROOT / name).read_text())
        if data["version"] != expected:
            sys.exit(f"{name}: version {data['version']} does not match {expected}")
        if name == "package-lock.json" and data["packages"][""]["version"] != expected:
            sys.exit("package-lock.json: root package version does not match tag")
    for name in ("src-tauri/Cargo.toml", "src-tauri/Cargo.lock"):
        # Match only our named package, not a dependency's version.
        match = re.search(r'^name = "flowing-thoughts"\nversion = "([^"]+)"',
                          (ROOT / name).read_text(), re.MULTILINE)
        if not match or match[1] != expected:
            sys.exit(f"{name}: flowing-thoughts version does not match {expected}")
    print(f"All package versions match v{expected}")


if __name__ == "__main__":
    main()
