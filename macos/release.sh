#!/bin/bash
# The final release of the Mac version, in one command. Checks that everything it
# needs is there, runs the tests, builds and signs the app, has Apple notarize the
# app and its disk image, and checks the result as macOS would check a download.
#
#   macos/release.sh [--skip-tests] [--allow-changes]
#
#   --skip-tests     leaves out the tests, when they have just run.
#   --allow-changes  for a trial: goes on with uncommitted changes, changed license
#                    texts, or a version that was released already.
#
# Writes target/macos/Dodavo-<version>-macos.dmg and its .sha256, and what each step
# printed to target/macos/release.log. Publishes nothing: macos/publish.sh does that.
#
# It needs, once on each Mac:
# - a "Developer ID Application" certificate in the keychain (Xcode > Settings >
#   Apple Accounts > Manage Certificates); $DODAVO_SIGN_IDENTITY chooses among several,
# - the notary credentials, stored with
#     xcrun notarytool store-credentials dodavo --apple-id … --team-id …
#   or under another name in $DODAVO_NOTARY_PROFILE,
# - cargo-about (cargo install cargo-about --locked --features cli).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

run_tests=1
allow_changes=0
for option in "$@"; do
    case "$option" in
    --skip-tests) run_tests=0 ;;
    --allow-changes) allow_changes=1 ;;
    *)
        echo "Unknown option $option. Use --skip-tests or --allow-changes." >&2
        exit 2
        ;;
    esac
done

stop() {
    echo >&2
    echo "Stopped: $1" >&2
    exit 1
}

# True when something differs from the last commit. Line ends do not count: they
# depend on each machine's settings (see TODO.md).
has_changes() {
    ! git diff --ignore-cr-at-eol --quiet HEAD || [ -n "$(git ls-files --others --exclude-standard)" ]
}

# What the release needs is looked at first, so that it stops at once, and not after
# the tests and the build.
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
library_version=$(sed -n 's/^version = "\(.*\)"/\1/p' crates/dodavo-ffi/Cargo.toml | head -1)
[ "$version" = "$library_version" ] || stop "Cargo.toml says version $version, and crates/dodavo-ffi/Cargo.toml says $library_version. Make them the same."

if [ "$allow_changes" = 0 ]; then
    if has_changes; then
        stop "there are uncommitted changes. A release is built from what is committed. Commit them, or use --allow-changes for a trial."
    fi
    if tagged=$(git rev-parse -q --verify "refs/tags/v$version^{commit}") && [ "$tagged" != "$(git rev-parse HEAD)" ]; then
        stop "v$version is already tagged on another commit, so this version has been released. Set a new version in both Cargo.toml files."
    fi
fi

found=$(security find-identity -v -p codesigning | sed -n 's/.*"\(Developer ID Application: .*\)"$/\1/p' | sort -u)
identity="${DODAVO_SIGN_IDENTITY:-}"
if [ -n "$identity" ]; then
    printf '%s\n' "$found" | grep -qxF "$identity" || stop "the certificate \"$identity\" is not in the keychain."
else
    count=$(printf '%s\n' "$found" | grep -c . || true)
    case "$count" in
    0) stop "there is no Developer ID Application certificate in the keychain. Create one in Xcode > Settings > Apple Accounts > Manage Certificates." ;;
    1) identity="$found" ;;
    *) stop "there are several Developer ID Application certificates. Choose one with \$DODAVO_SIGN_IDENTITY." ;;
    esac
fi

profile="${DODAVO_NOTARY_PROFILE:-dodavo}"
xcrun notarytool history --keychain-profile "$profile" >/dev/null 2>&1 \
    || stop "Apple cannot be asked with the notary profile \"$profile\". Check the internet connection. If it is the first time on this Mac, store the credentials: xcrun notarytool store-credentials $profile --apple-id <your Apple ID> --team-id <your team ID>"

cargo about --version >/dev/null 2>&1 \
    || stop "cargo-about is missing. Install it: cargo install cargo-about --locked --features cli"

mkdir -p target/macos
log="$root/target/macos/release.log"
: >"$log"
app="target/macos/Dodavo.app"
dmg="target/macos/Dodavo-$version-macos.dmg"

# Says what is going on, and keeps what the step prints for when it fails.
step() {
    local what="$1"
    shift
    echo "$what"
    {
        echo
        echo "=== $what"
    } >>"$log"
    if ! "$@" >>"$log" 2>&1; then
        echo >&2
        tail -n 25 "$log" >&2
        stop "that step failed. All that it printed is in target/macos/release.log."
    fi
}

check_licenses() {
    macos/licenses.sh || return 1
    if [ "$allow_changes" = 0 ] && ! git diff --ignore-cr-at-eol --quiet -- res/third-party-licenses-macos.txt; then
        echo "res/third-party-licenses-macos.txt has changed: dependencies or Rust were updated. Commit it first."
        return 1
    fi
}

# What is checked on the image itself.
check_image_contents() {
    local mount="$1"
    local inside="$mount/Dodavo.app"
    local bundle_id
    bundle_id=$(plutil -extract CFBundleIdentifier raw macos/Resources/Info.plist) || return 1
    [ -L "$mount/Applications" ] || { echo "The image has no link to Applications."; return 1; }
    [ "$(plutil -extract CFBundleShortVersionString raw "$inside/Contents/Info.plist")" = "$version" ] \
        || { echo "The app on the image is not version $version."; return 1; }
    [ "$(plutil -extract CFBundleIdentifier raw "$inside/Contents/Info.plist")" = "$bundle_id" ] \
        || { echo "The app on the image does not have the bundle ID $bundle_id."; return 1; }
    cmp "$inside/Contents/MacOS/Dodavo" "$app/Contents/MacOS/Dodavo" || return 1
    xcrun stapler validate "$inside" || return 1
    spctl -a -t exec -vvv "$inside" 2>&1 | tee /dev/stderr | grep -q "source=Notarized Developer ID" \
        || { echo "macOS does not take the app on the image as notarized."; return 1; }
}

# As macOS would check a download. A function under `step` runs without `set -e`, so
# each check says for itself that it stops.
check_result() {
    codesign --verify --deep --strict "$app" || return 1
    xcrun stapler validate "$app" || return 1
    xcrun stapler validate "$dmg" || return 1
    spctl -a -t open --context context:primary-signature -vv "$dmg" 2>&1 | tee /dev/stderr | grep -q "source=Notarized Developer ID" \
        || { echo "macOS does not take the disk image as notarized."; return 1; }
    hdiutil verify -quiet "$dmg" || return 1
    (cd target/macos && shasum -a 256 -c "$(basename "$dmg").sha256") || return 1
    local mount
    mount=$(mktemp -d)
    hdiutil attach "$dmg" -nobrowse -readonly -noautoopen -quiet -mountpoint "$mount" || { rmdir "$mount"; return 1; }
    local result=0
    check_image_contents "$mount" || result=1
    hdiutil detach "$mount" -quiet || result=1
    rmdir "$mount" 2>/dev/null || true
    return "$result"
}

echo "Dodavo $version, from commit $(git rev-parse --short HEAD), signed by $identity."
export DODAVO_SIGN_IDENTITY="$identity"
export DODAVO_NOTARY_PROFILE="$profile"

step "Checking the third-party licenses…" check_licenses
if [ "$run_tests" = 1 ]; then
    step "Running the tests…" cargo test --workspace -- --test-threads=4
fi
step "Building and signing the app…" macos/build.sh
step "Having Apple notarize the app and the disk image, which takes a few minutes…" macos/make-dmg.sh
step "Checking the result as macOS checks a download…" check_result

if [ "$allow_changes" = 0 ] && has_changes; then
    stop "the build changed files that are committed, so the release does not match its commit. See git status."
fi

echo
echo "Done. Dodavo $version is signed, notarized and checked."
echo "Disk image: $dmg ($(stat -f %z "$dmg" | awk '{printf "%.1f MB", $1 / 1000000}'))"
echo "SHA-256: $(cut -d' ' -f1 "$dmg.sha256")"
echo
echo "Nothing has been published."
[ -f "release-notes/$version/en.txt" ] || echo "Write its release notes first, in release-notes/$version/."
echo "To publish it: macos/publish.sh --key <secret key file> --publish"
