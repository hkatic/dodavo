#!/bin/bash
# Writes and verifies target/macos/update-macos.json, the signed update manifest
# of the Mac version, from the disk image, release-notes/<version>/<lang>.txt and
# the secret key in $DODAVO_UPDATE_KEY_FILE or --key. With --publish, adds it and
# the disk image to the GitHub release v<version>, which is a draft by then
# (installer/publish.ps1 makes it) or is made as one here.
#
#   macos/publish.sh [--key <file>] [--publish]
#
# Installed copies look for the manifests of the latest published release, each
# system for its own: a release without one of them breaks that system's check.
# So a release goes public only when it has both, and one for one system only
# still carries the other's manifest, of the version that system is on.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
key="${DODAVO_UPDATE_KEY_FILE:-}"
publish=0
while [ $# -gt 0 ]; do
    case "$1" in
    --key) key="$2"; shift 2 ;;
    --publish) publish=1; shift ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done
[ -n "$key" ] || { echo "give the secret key: --key or \$DODAVO_UPDATE_KEY_FILE" >&2; exit 2; }

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
dmg="target/macos/Dodavo-$version-macos.dmg"
sha="$dmg.sha256"
manifest="target/macos/update-macos.json"
[ -f "$dmg" ] || { echo "$dmg is missing: run macos/make-dmg.sh" >&2; exit 1; }

cargo build --release --features release-tool --bin dodavo-release
tool=target/release/dodavo-release
"$tool" manifest --platform macos --installer "$dmg" --key "$key" \
    --notes "release-notes/$version" --out "$manifest"
"$tool" verify --platform macos --manifest "$manifest" --installer "$dmg"

[ "$publish" = 1 ] || exit 0
tag="v$version"
if ! gh release view "$tag" >/dev/null 2>&1; then
    gh release create "$tag" --draft --title "Dodavo $version" \
        --notes-file "release-notes/$version/en.txt"
fi
gh release upload "$tag" "$dmg" "$sha" "$manifest" --clobber
state=$(gh release view "$tag" --json isDraft --jq .isDraft)
echo "Added to $tag (draft: $state). It goes public with: gh release edit $tag --draft=false"
echo "Check first that it has update.json as well."
