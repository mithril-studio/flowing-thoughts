#!/usr/bin/env bash
set -euo pipefail

# FlowingThoughts release script — builds, signs, and publishes an update to
# mithril-studio/flowing-thoughts-releases. Run from the repo root.
#
# Prereqs:
# - TAURI_SIGNING_PRIVATE_KEY_PATH env var (default: ~/.tauri/flowingthoughts_updater.key)
# - gh CLI authenticated (gh auth status)
# - jq installed (brew install jq)
# - Version bumped in src-tauri/tauri.conf.json BEFORE running this script
#
# Usage:
#   scripts/release.sh                        # uses version from tauri.conf.json
#   scripts/release.sh "Notes for release"    # optional release notes

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

RELEASES_REPO="mithril-studio/flowing-thoughts-releases"
KEY_PATH="${TAURI_SIGNING_PRIVATE_KEY_PATH:-$HOME/.tauri/flowingthoughts_updater.key}"
NOTES="${1:-}"

if [ ! -f "$KEY_PATH" ]; then
  echo "error: signing key not found at $KEY_PATH" >&2
  echo "set TAURI_SIGNING_PRIVATE_KEY_PATH or regenerate with:" >&2
  echo "  npx @tauri-apps/cli signer generate --password '' --write-keys $KEY_PATH" >&2
  exit 1
fi

VERSION="$(jq -r .version src-tauri/tauri.conf.json)"
if [ -z "$VERSION" ] || [ "$VERSION" = "null" ]; then
  echo "error: could not read version from src-tauri/tauri.conf.json" >&2
  exit 1
fi

TAG="v${VERSION}"
echo "==> Building FlowingThoughts ${TAG}"

if gh release view "$TAG" --repo "$RELEASES_REPO" >/dev/null 2>&1; then
  echo "error: release ${TAG} already exists on ${RELEASES_REPO}" >&2
  echo "bump the version in src-tauri/tauri.conf.json and try again" >&2
  exit 1
fi

# Build signed update artifacts. Tauri v2 reads the key CONTENT from
# TAURI_SIGNING_PRIVATE_KEY (the _PATH variant is not recognised). The key
# password comes from the env or from a sibling ".password" file. Note: the
# key MUST have a non-empty password — empty-password keys fail to decode
# ("Wrong password") in current tauri CLI versions.
export TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY_PATH")"
if [ -z "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" ] && [ -f "${KEY_PATH}.password" ]; then
  TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(cat "${KEY_PATH}.password")"
fi
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD

# The DMG bundler (Finder AppleScript) is flaky in non-interactive shells and
# a failure there would otherwise abort the whole release. Build the app +
# updater artifacts first, then create the DMG ourselves with hdiutil.
npx tauri build --bundles app

BUNDLE_DIR_EARLY="src-tauri/target/release/bundle"
mkdir -p "$BUNDLE_DIR_EARLY/dmg"
DMG_OUT="$BUNDLE_DIR_EARLY/dmg/FlowingThoughts_${VERSION}_$(uname -m).dmg"
STAGE="$(mktemp -d)"
cp -R "$BUNDLE_DIR_EARLY/macos/FlowingThoughts.app" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "FlowingThoughts" -srcfolder "$STAGE" -ov -format UDZO "$DMG_OUT" >/dev/null
rm -rf "$STAGE"

BUNDLE_DIR="src-tauri/target/release/bundle/macos"
ARCH="$(uname -m)"
if [ "$ARCH" = "arm64" ]; then
  RUST_TARGET="aarch64"
else
  RUST_TARGET="x86_64"
fi

APP_TAR="${BUNDLE_DIR}/FlowingThoughts.app.tar.gz"
APP_SIG="${APP_TAR}.sig"

if [ ! -f "$APP_TAR" ] || [ ! -f "$APP_SIG" ]; then
  echo "error: expected updater artifacts not found:" >&2
  echo "  $APP_TAR" >&2
  echo "  $APP_SIG" >&2
  echo "confirm bundle.createUpdaterArtifacts is true in tauri.conf.json" >&2
  exit 1
fi

SIGNATURE="$(cat "$APP_SIG")"
PUB_DATE="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

RELEASE_TAR="FlowingThoughts_${VERSION}_${RUST_TARGET}.app.tar.gz"
RELEASE_SIG="${RELEASE_TAR}.sig"
cp "$APP_TAR" "$BUNDLE_DIR/$RELEASE_TAR"
cp "$APP_SIG" "$BUNDLE_DIR/$RELEASE_SIG"

LATEST_JSON="$(mktemp -t latest-XXXXXX.json)"
cat > "$LATEST_JSON" <<EOF
{
  "version": "${VERSION}",
  "notes": $(jq -Rs . <<< "${NOTES:-Release ${TAG}}"),
  "pub_date": "${PUB_DATE}",
  "platforms": {
    "darwin-${RUST_TARGET}": {
      "signature": $(jq -Rs . <<< "$SIGNATURE"),
      "url": "https://github.com/${RELEASES_REPO}/releases/download/${TAG}/${RELEASE_TAR}"
    }
  }
}
EOF

echo "==> latest.json preview"
cat "$LATEST_JSON"

DMG_FILE="$(ls "$BUNDLE_DIR"/../dmg/*.dmg 2>/dev/null | head -n 1 || true)"

echo "==> Creating GitHub release ${TAG} on ${RELEASES_REPO}"
ASSETS=(
  "$BUNDLE_DIR/$RELEASE_TAR"
  "$BUNDLE_DIR/$RELEASE_SIG"
  "$LATEST_JSON#latest.json"
)
if [ -n "$DMG_FILE" ]; then
  ASSETS+=("$DMG_FILE")
fi

gh release create "$TAG" \
  --repo "$RELEASES_REPO" \
  --title "FlowingThoughts ${TAG}" \
  --notes "${NOTES:-Release ${TAG}}" \
  "${ASSETS[@]}"

rm -f "$LATEST_JSON"

echo
echo "==> Done. Release URL:"
gh release view "$TAG" --repo "$RELEASES_REPO" --json url --jq .url
echo
echo "The app will pick up this update at next launch via the updater plugin."
