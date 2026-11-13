#!/usr/bin/env bash
# Build RSyncStat.app, sign it with your Developer ID, and notarize it.
#
#   scripts/bundle-macos.sh
#
# Output: target/release/bundle/osx/RSyncStat.app and dist/RSyncStat-macos.zip
#
# Environment:
#   SIGN_IDENTITY   Name or SHA-1 of a "Developer ID Application" identity.
#                   Default: the first one found in your keychains.
#   NOTARY_PROFILE  Name of a notarytool keychain profile. When unset the app
#                   is signed but not notarized. Create one once with:
#                     xcrun notarytool store-credentials rsyncstat-notary \
#                       --apple-id you@example.com --team-id TEAMID
#                   (it prompts for an app-specific password and stores it in
#                   your keychain; nothing is written to this repo)
#   SKIP_SIGN=1     Build the bundle only, no signing or notarizing.
set -euo pipefail

[ "$(uname -s)" = "Darwin" ] || { echo "error: this script needs macOS" >&2; exit 1; }

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
app="target/release/bundle/osx/RSyncStat.app"
dist="$root/dist"

command -v cargo-bundle >/dev/null || {
  echo "error: cargo-bundle not found. Run: cargo install cargo-bundle --locked" >&2
  exit 1
}

echo "==> Building the app bundle"
cargo bundle --release -p rsyncstat-app >/dev/null
[ -d "$app" ] || { echo "error: $app was not created" >&2; exit 1; }

if [ "${SKIP_SIGN:-}" = "1" ]; then
  echo "SKIP_SIGN=1: bundle left unsigned at $app"
  exit 0
fi

identity="${SIGN_IDENTITY:-}"
if [ -z "$identity" ]; then
  # Use the hash, not the name: the same certificate imported twice makes the
  # name ambiguous.
  identity="$(security find-identity -v -p codesigning \
    | awk '/Developer ID Application/ { print $2; exit }')"
fi
[ -n "$identity" ] || {
  echo "error: no Developer ID Application identity found in your keychain" >&2
  exit 1
}

echo "==> Signing with hardened runtime"
codesign --force --deep --timestamp --options runtime \
  --sign "$identity" "$app"
codesign --verify --deep --strict --verbose=2 "$app"

mkdir -p "$dist"
zip="$dist/RSyncStat-macos.zip"
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"

if [ -z "${NOTARY_PROFILE:-}" ]; then
  echo "NOTARY_PROFILE not set: signed but NOT notarized."
  echo "Gatekeeper will still warn on other Macs. See the header of this script."
  echo "Signed zip: $zip"
  exit 0
fi

echo "==> Notarizing (this can take a few minutes)"
xcrun notarytool submit "$zip" --keychain-profile "$NOTARY_PROFILE" --wait

echo "==> Stapling the ticket"
xcrun stapler staple "$app"
xcrun stapler validate "$app"

# Rebuild the zip so it contains the stapled app.
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"

echo "==> Gatekeeper check"
spctl --assess --type execute --verbose=2 "$app"

echo "done: $zip"
