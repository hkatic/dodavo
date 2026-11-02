#!/bin/bash
# Makes the disk image of a release from target/macos/Dodavo.app (macos/build.sh
# with $DODAVO_SIGN_IDENTITY set to a "Developer ID Application" certificate),
# and has Apple notarize both. Writes target/macos/Dodavo-<version>-macos.dmg and
# its .sha256.
#
#   macos/make-dmg.sh [--no-notarize]
#
# Notarizing needs credentials stored once with
#   xcrun notarytool store-credentials dodavo --apple-id … --team-id … --password …
# or another name in $DODAVO_NOTARY_PROFILE.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
app="target/macos/Dodavo.app"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
dmg="target/macos/Dodavo-$version-macos.dmg"
profile="${DODAVO_NOTARY_PROFILE:-dodavo}"
notarize=1
[ "${1:-}" = "--no-notarize" ] && notarize=0

macos/check-floor.sh "$app/Contents/MacOS/Dodavo"
codesign --verify --deep --strict "$app"

submit() {
    xcrun notarytool submit "$1" --keychain-profile "$profile" --wait
}

if [ "$notarize" = 1 ]; then
    # The app first, so that it is stapled on the image and opens without
    # asking Apple, also offline.
    zip="target/macos/Dodavo-notarize.zip"
    ditto -c -k --keepParent "$app" "$zip"
    submit "$zip"
    rm -f "$zip"
    xcrun stapler staple "$app"
fi

# The app, and a link to the folder to drag it to.
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
ditto "$app" "$stage/Dodavo.app"
ln -s /Applications "$stage/Applications"
rm -f "$dmg"
hdiutil create -quiet -volname "Dodavo $version" -srcfolder "$stage" -fs APFS \
    -format UDZO -ov "$dmg"

if [ -n "${DODAVO_SIGN_IDENTITY:-}" ]; then
    codesign --force --timestamp --sign "$DODAVO_SIGN_IDENTITY" "$dmg"
fi
if [ "$notarize" = 1 ]; then
    submit "$dmg"
    xcrun stapler staple "$dmg"
    spctl -a -t open --context context:primary-signature -vv "$dmg"
    spctl -a -vvv "$app"
fi
(cd target/macos && shasum -a 256 "$(basename "$dmg")" > "$(basename "$dmg").sha256")
echo "$dmg"
