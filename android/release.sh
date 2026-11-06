#!/bin/bash
# The final release of the Android version, in one command. Checks that
# everything it needs is there, runs the tests, builds the app bundle signed
# with the upload key, and checks it.
#
#   android/release.sh [--skip-tests] [--allow-changes]
#
#   --skip-tests     leaves out the tests, when they have just run.
#   --allow-changes  for a trial: goes on with uncommitted changes, changed
#                    license texts, or a version that was released already.
#
# Writes target/android/Dodavo-<version>.aab and its .sha256, and what each step
# printed to target/android/release.log. Publishes nothing: the bundle is
# uploaded in Play Console (see android/play/checklist.md).
#
# It needs:
# - the upload key, in a keystore that is never committed:
#     DODAVO_UPLOAD_KEYSTORE        the keystore file,
#     DODAVO_UPLOAD_STORE_PASSWORD  its password,
#     DODAVO_UPLOAD_KEY_ALIAS       the key's alias,
#     DODAVO_UPLOAD_KEY_PASSWORD    the key's password,
#   made once with
#     keytool -genkeypair -v -keystore upload.jks -keyalg RSA -keysize 4096 \
#         -validity 10000 -alias upload
#   and registered in Play Console when the app is created (Play App Signing
#   keeps the key that signs the app on devices),
# - what android/build.sh needs, and cargo-about.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
studio_jdk="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
if [ -z "${JAVA_HOME:-}" ] && [ -d "$studio_jdk" ]; then
    export JAVA_HOME="$studio_jdk"
fi

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

# True when something differs from the last commit (line ends do not count).
has_changes() {
    ! git diff --ignore-cr-at-eol --quiet HEAD || [ -n "$(git ls-files --others --exclude-standard)" ]
}

# What the release needs is looked at first, so that it stops at once.
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
for crate in crates/dodavo-ffi crates/dodavo-android; do
    other=$(sed -n 's/^version = "\(.*\)"/\1/p' "$crate/Cargo.toml" | head -1)
    [ "$version" = "$other" ] || stop "Cargo.toml says version $version, and $crate/Cargo.toml says $other. Make them the same."
done

if [ "$allow_changes" = 0 ]; then
    if has_changes; then
        stop "there are uncommitted changes. A release is built from what is committed. Commit them, or use --allow-changes for a trial."
    fi
    if tagged=$(git rev-parse -q --verify "refs/tags/v$version^{commit}") && [ "$tagged" != "$(git rev-parse HEAD)" ]; then
        stop "v$version is already tagged on another commit, so this version has been released. Set a new version in the Cargo.toml files."
    fi
fi

for name in DODAVO_UPLOAD_KEYSTORE DODAVO_UPLOAD_STORE_PASSWORD DODAVO_UPLOAD_KEY_ALIAS DODAVO_UPLOAD_KEY_PASSWORD; do
    [ -n "${!name:-}" ] || stop "$name is not set. The bundle is signed with the upload key (see the top of android/release.sh)."
done
[ -f "$DODAVO_UPLOAD_KEYSTORE" ] || stop "the keystore $DODAVO_UPLOAD_KEYSTORE does not exist."
command -v cargo-ndk >/dev/null || stop "cargo-ndk is missing. Install it: cargo install cargo-ndk --locked"
cargo about --version >/dev/null 2>&1 \
    || stop "cargo-about is missing. Install it: cargo install cargo-about --locked --features cli"

mkdir -p target/android
log="$root/target/android/release.log"
: >"$log"
bundle="target/android/Dodavo-$version.aab"

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
        stop "that step failed. All that it printed is in target/android/release.log."
    fi
}

check_licenses() {
    android/licenses.sh || return 1
    if [ "$allow_changes" = 0 ] && ! git diff --ignore-cr-at-eol --quiet -- res/third-party-licenses-android.txt; then
        echo "res/third-party-licenses-android.txt has changed: dependencies or Rust were updated. Commit it first."
        return 1
    fi
}

test_app() {
    (cd android && ./gradlew --quiet :app:lintRelease :app:testReleaseUnitTest)
}

# Signed throughout. Not with -strict, which refuses a self-signed
# certificate, as an upload key's is.
verify_signature() {
    local said
    said=$("$JAVA_HOME/bin/jarsigner" -verify "$bundle")
    echo "$said"
    grep -q "^jar verified" <<<"$said"
}

step "Writing the licenses of the components" check_licenses
if [ "$run_tests" = 1 ]; then
    step "Running the tests (a few minutes, with the network tests)" cargo test --locked --workspace -- --test-threads=1
    step "Checking the app (lint and unit tests)" test_app
fi
step "Building the app bundle" android/build.sh
step "Checking the upload signature" verify_signature
(cd target/android && shasum -a 256 "Dodavo-$version.aab" >"Dodavo-$version.aab.sha256")

echo
echo "Done: $bundle"
echo "Upload it in Play Console (android/play/checklist.md)."
