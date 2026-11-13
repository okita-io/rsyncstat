#!/usr/bin/env bash
# Build RSyncStat.app, sign it with your Developer ID, and notarize it.
#
#   scripts/bundle-macos.sh
#
# Output: target/release/bundle/osx/RSyncStat.app, dist/RSyncStat-macos.zip and
# dist/RSyncStat.dmg (drag-to-Applications disk image, signed and notarized)
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
dmg="$dist/RSyncStat.dmg"
make_zip() { rm -f "$zip"; ditto -c -k --keepParent "$app" "$zip"; }

# make_dmg: a disk image holding the app and an Applications shortcut.
make_dmg() {
  local stage
  stage="$(mktemp -d)"
  cp -R "$app" "$stage/"
  ln -s /Applications "$stage/Applications"
  rm -f "$dmg"
  hdiutil create -quiet -volname "RSyncStat" -srcfolder "$stage" \
    -format UDZO -fs HFS+ "$dmg"
  rm -rf "$stage"
  codesign --force --timestamp --sign "$identity" "$dmg"
  codesign --verify --verbose=2 "$dmg"
}

make_zip

if [ -z "${NOTARY_PROFILE:-}" ]; then
  echo "==> Building the disk image"
  make_dmg
  echo "NOTARY_PROFILE not set: signed but NOT notarized."
  echo "Gatekeeper will still warn on other Macs. See the header of this script."
  echo "Signed: $zip and $dmg"
  exit 0
fi

echo "==> Notarizing the app (this can take a few minutes)"
xcrun notarytool submit "$zip" --keychain-profile "$NOTARY_PROFILE" --wait
xcrun stapler staple "$app"
xcrun stapler validate "$app"
make_zip   # rebuild so the zip holds the stapled app

echo "==> Building and notarizing the disk image"
make_dmg
xcrun notarytool submit "$dmg" --keychain-profile "$NOTARY_PROFILE" --wait
xcrun stapler staple "$dmg"
xcrun stapler validate "$dmg"

echo "==> Gatekeeper check"
spctl --assess --type execute --verbose=2 "$app"
spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"

echo "done: $zip and $dmg"
