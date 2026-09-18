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
