#!/usr/bin/env bash
# Validate GitHub's certificate/password and notarization key without building.
set -euo pipefail
set +x
umask 077
for VAR in APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY APPLE_API_KEY_BASE64 APPLE_API_KEY_ID APPLE_API_ISSUER_ID; do
  if [ -z "${!VAR:-}" ]; then
    echo "error: $VAR is required" >&2
    exit 1
  fi
done

WORK="$(mktemp -d)"
KEYCHAIN="$WORK/preflight.keychain-db"
cleanup() {
  security delete-keychain "$KEYCHAIN" >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT
printf '%s' "$APPLE_CERTIFICATE" | base64 -D > "$WORK/certificate.p12"
security create-keychain -p preflight "$KEYCHAIN"
security import "$WORK/certificate.p12" -k "$KEYCHAIN" -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign >/dev/null \
  || { echo 'error: certificate import failed; check APPLE_CERTIFICATE_BASE64 and APPLE_CERTIFICATE_PASSWORD' >&2; exit 1; }
IDENTITIES="$(security find-identity -v -p codesigning "$KEYCHAIN")"
grep -Fq "\"$APPLE_SIGNING_IDENTITY\"" <<< "$IDENTITIES" \
  || { echo 'error: certificate does not contain the configured valid Developer ID identity' >&2; exit 1; }
printf '%s' "$APPLE_API_KEY_BASE64" | base64 -D > "$WORK/AuthKey.p8"
xcrun notarytool history --key "$WORK/AuthKey.p8" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER_ID" >/dev/null \
  || { echo 'error: notarization authentication failed; check the Apple API key, key ID and issuer' >&2; exit 1; }
echo 'Apple certificate import, Developer ID identity, and notarization authentication passed.'
