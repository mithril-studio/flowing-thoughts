#!/usr/bin/env bash
# Called only after release.sh verifies signing, notarization and the updater feed.
set -euo pipefail
if [ "$#" -ne 6 ]; then
  echo 'usage: publish-release.sh TAG NOTES TAR SIGNATURE MANIFEST DMG' >&2
  exit 1
fi
REPO="mithril-studio/flowing-thoughts-releases"
TAG="$1"
NOTES="$2"
shift 2
# This app already has a stable release. Fail closed on lookup errors rather
# than accidentally repointing the stable feed to an older version.
CURRENT="$(gh release view --repo "$REPO" --json tagName --jq .tagName)"
python3 - "$TAG" "$CURRENT" <<'PY'
import re
import sys

def version(tag):
    if not re.fullmatch(r"v\d+\.\d+\.\d+", tag):
        sys.exit(f"error: expected a stable version tag, got {tag!r}")
    return tuple(map(int, tag[1:].split(".")))

if version(sys.argv[1]) <= version(sys.argv[2]):
    sys.exit("error: release must be newer than the current stable version")
PY
for ASSET in "$@"; do
  [ -s "$ASSET" ] || { echo "error: missing or empty asset: $ASSET" >&2; exit 1; }
done
CHECK_DIR="$(mktemp -d)"
trap 'rm -rf "$CHECK_DIR"' EXIT

# A failed upload/verification leaves a draft, never a broken public update.
# Existing releases are not overwritten: inspect/delete a failed draft manually.
gh release create "$TAG" --repo "$REPO" --draft \
  --title "FlowingThoughts ${TAG}" --notes "$NOTES" "$@"
gh release download "$TAG" --repo "$REPO" --dir "$CHECK_DIR"
for ASSET in "$@"; do
  cmp "$ASSET" "$CHECK_DIR/$(basename "$ASSET")" \
    || { echo "error: uploaded asset differs: $ASSET; release remains draft" >&2; exit 1; }
done
gh release edit "$TAG" --repo "$REPO" --draft=false --latest
