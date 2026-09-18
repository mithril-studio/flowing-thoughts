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
# - Developer ID signing and notarization (required by default):
#     APPLE_SIGNING_IDENTITY  "Developer ID Application: <name> (<TEAMID>)"
#     NOTARY_API_KEY_PATH, NOTARY_API_KEY_ID, NOTARY_API_ISSUER
#   REQUIRE_DEVELOPER_ID=0 skips both for a local test build. Never publish
#   such a build: a different signing identity makes macOS ask every user for
#   all permissions again.
#
# Usage:
#   scripts/release.sh                        # uses version from tauri.conf.json
#   scripts/release.sh "Notes for release"    # optional release notes

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

RELEASES_REPO="mithril-studio/flowing-thoughts-releases"
KEY_PATH="${TAURI_SIGNING_PRIVATE_KEY_PATH:-$HOME/.tauri/flowingthoughts_updater.key}"
NOTES="${1:-}"
REQUIRE_DEVELOPER_ID="${REQUIRE_DEVELOPER_ID:-1}"

# Checked before the build so a missing variable fails in seconds, not after
# a full release build.
if [ "$REQUIRE_DEVELOPER_ID" = "1" ]; then
  for VAR in APPLE_SIGNING_IDENTITY NOTARY_API_KEY_PATH NOTARY_API_KEY_ID NOTARY_API_ISSUER; do
    if [ -z "${!VAR:-}" ]; then
      echo "error: $VAR is not set; Developer ID signing and notarization are required" >&2
      echo "set REQUIRE_DEVELOPER_ID=0 only for a local test build that is never published" >&2
      exit 1
    fi
  done
  if [ ! -r "$NOTARY_API_KEY_PATH" ]; then
    echo "error: notarization API key not readable at $NOTARY_API_KEY_PATH" >&2
    exit 1
  fi
fi

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
#
# Code signing happens during the Tauri build. Do not re-sign the .app after
# this point: the tarball and its .sig already exist, and updates would then
# ship different code than the DMG. The assertions below check both.
npx tauri build --bundles app

BUNDLE_DIR_EARLY="src-tauri/target/release/bundle"
APP_PATH="$BUNDLE_DIR_EARLY/macos/FlowingThoughts.app"
UPDATER_TAR="$APP_PATH.tar.gz"

# Release assertions. macOS keys privacy permissions (Microphone,
# Accessibility, Input Monitoring, System Audio Recording) to the code
# signature, so a malformed signature or a missing usage description is a
# broken release even though the app builds and launches here.
fail() {
  echo "error: release check failed: $*" >&2
  exit 1
}

echo "==> Checking the signed bundle"
BUNDLE_ID="$(jq -r .identifier src-tauri/tauri.conf.json)"
[ -d "$APP_PATH" ] || fail "$APP_PATH was not built"
APP_PLIST="$APP_PATH/Contents/Info.plist"
APP_BIN="$APP_PATH/Contents/MacOS/$(plutil -extract CFBundleExecutable raw "$APP_PLIST")"
[ -f "$APP_BIN" ] || fail "main binary not found at $APP_BIN"

codesign --verify --deep --strict --verbose=2 "$APP_PATH" \
  || fail "codesign --verify --deep --strict does not pass (was APPLE_SIGNING_IDENTITY set for the build?)"

# Captured first, then matched: `codesign | grep -q` can die of SIGPIPE under
# pipefail. The same goes for nm below.
SIGN_INFO="$(codesign -dvvv "$APP_PATH" 2>&1)"
SIGN_ID="$(sed -n 's/^Identifier=//p' <<< "$SIGN_INFO")"
[ "$SIGN_ID" = "$BUNDLE_ID" ] \
  || fail "signature identifier is '$SIGN_ID', expected the bundle identifier '$BUNDLE_ID'"
grep -q '^Info.plist entries=' <<< "$SIGN_INFO" \
  || fail "Info.plist is not bound to the signature"

# Hardened runtime without the audio-input entitlement means macOS refuses
# the microphone without ever prompting. Notarization requires the hardened
# runtime, so a Developer ID build must have it.
if grep -q '^CodeDirectory.*runtime' <<< "$SIGN_INFO"; then
  ENTITLEMENTS="$(codesign -d --entitlements - --xml "$APP_PATH" 2>/dev/null || true)"
  grep -q 'com.apple.security.device.audio-input' <<< "$ENTITLEMENTS" \
    || fail "hardened runtime is on but the com.apple.security.device.audio-input entitlement is missing"
fi

if [ "$REQUIRE_DEVELOPER_ID" = "1" ]; then
  grep -q '^CodeDirectory.*runtime' <<< "$SIGN_INFO" \
    || fail "hardened runtime is off; notarization will reject the bundle"
  grep -Fqx "Authority=${APPLE_SIGNING_IDENTITY}" <<< "$SIGN_INFO" \
    || fail "bundle is not signed by the configured Developer ID identity"
fi

for KEY in NSMicrophoneUsageDescription NSAudioCaptureUsageDescription; do
  VALUE="$(plutil -extract "$KEY" raw "$APP_PLIST" 2>/dev/null || true)"
  [ -n "$VALUE" ] || fail "$KEY is missing from the bundled Info.plist"
done

# The process tap functions do not exist before macOS 14.4 and the app must
# still launch on 13.4, so they are resolved with dlsym at runtime. A hard
# link shows up as an undefined symbol here.
UNDEFINED_SYMBOLS="$(nm -um "$APP_BIN")" || fail "nm could not read $APP_BIN"
if grep -q 'ProcessTap' <<< "$UNDEFINED_SYMBOLS"; then
  grep 'ProcessTap' <<< "$UNDEFINED_SYMBOLS" >&2
  fail "the binary hard-links ProcessTap symbols and would not launch before macOS 14.4"
fi

# The updater tarball must hold the same signed code as the DMG.
[ -f "$UPDATER_TAR" ] || fail "updater tarball not found at $UPDATER_TAR"
UPDATER_CHECK_DIR="$(mktemp -d)"
tar -xzf "$UPDATER_TAR" -C "$UPDATER_CHECK_DIR"
codesign --verify --deep --strict "$UPDATER_CHECK_DIR/FlowingThoughts.app" \
  || fail "the app inside the updater tarball does not pass codesign --verify"
APP_CDHASH="$(sed -n 's/^CDHash=//p' <<< "$SIGN_INFO")"
UPDATER_CDHASH="$(codesign -dvvv "$UPDATER_CHECK_DIR/FlowingThoughts.app" 2>&1 | sed -n 's/^CDHash=//p')"
rm -rf "$UPDATER_CHECK_DIR"
[ -n "$APP_CDHASH" ] && [ "$APP_CDHASH" = "$UPDATER_CDHASH" ] \
  || fail "updater tarball has CDHash '$UPDATER_CDHASH' but the app has '$APP_CDHASH'"
echo "    signed as $SIGN_ID, CDHash $APP_CDHASH, checks passed"

mkdir -p "$BUNDLE_DIR_EARLY/dmg"
DMG_OUT="$BUNDLE_DIR_EARLY/dmg/FlowingThoughts_${VERSION}_$(uname -m).dmg"
STAGE="$(mktemp -d)"
cp -R "$APP_PATH" "$STAGE/"
codesign --verify --deep --strict "$STAGE/FlowingThoughts.app" || fail "the staged copy for the DMG lost its signature"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "FlowingThoughts" -srcfolder "$STAGE" -ov -format UDZO "$DMG_OUT" >/dev/null
rm -rf "$STAGE"

if [ "$REQUIRE_DEVELOPER_ID" = "1" ]; then
  echo "==> Notarizing and stapling the DMG"
  xcrun notarytool submit "$DMG_OUT" \
    --key "$NOTARY_API_KEY_PATH" \
    --key-id "$NOTARY_API_KEY_ID" \
    --issuer "$NOTARY_API_ISSUER" \
    --wait
  xcrun stapler staple "$DMG_OUT"
  xcrun stapler validate "$DMG_OUT"
fi

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

# No here-strings below: `<<<` appends a newline, and the updater rejects a
# signature that ends in one ("Invalid symbol 10, offset 416").
#
# The file must literally be named latest.json — `gh release upload file#label`
# only changes the display label, while the updater resolves assets by name.
LATEST_JSON_DIR="$(mktemp -d)"
LATEST_JSON="$LATEST_JSON_DIR/latest.json"
cat > "$LATEST_JSON" <<EOF
{
  "version": "${VERSION}",
  "notes": $(printf '%s' "${NOTES:-Release ${TAG}}" | jq -Rs .),
  "pub_date": "${PUB_DATE}",
  "platforms": {
    "darwin-${RUST_TARGET}": {
      "signature": $(printf '%s' "$SIGNATURE" | jq -Rs .),
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
  "$LATEST_JSON"
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
