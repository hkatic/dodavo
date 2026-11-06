#!/bin/bash
# Runs the core's tests on the connected Android device (or emulator), with
# the folders the core would get from the app: builds the test binary of the
# library for arm64-v8a, pushes it, runs it in /data/local/tmp/dodavo-test and
# passes on its exit code. The arguments go to the test binary:
#
#   android/test-on-device.sh --test-threads=1 [--skip net::tests::] [name]
#
# cargo-ndk puts a runner of its own in place of any other, so this builds
# with --no-run and runs the binary itself. ANDROID_SERIAL picks the device
# when several are connected. DODAVO_DEVICE_TMP runs the tests in another
# folder, /sdcard/Download/dodavo-test say, for the shared storage (where adb's
# own user sees it, not as an app would).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$HOME/Library/Android/sdk/ndk/30.0.16248370}"

cargo ndk -t arm64-v8a --platform 26 test --no-run --quiet -p dodavo --lib
# The newest test binary of the library (whose crate is dodavo_core).
binary=$(ls -t target/aarch64-linux-android/debug/deps/dodavo_core-* |
    grep -v '\.[a-z]*$' | head -1)

dir=/data/local/tmp/dodavo-test
tmp="${DODAVO_DEVICE_TMP:-$dir/tmp}"
adb shell "mkdir -p $dir '$tmp'" >/dev/null
adb push "$binary" "$dir/dodavo-tests" >/dev/null
args=""
for arg in "$@"; do
    args="$args '${arg//\'/\'\\\'\'}'"
done
set +e
adb shell "cd $dir && chmod 755 dodavo-tests && \
    TMPDIR='$tmp' HOME=$dir DODAVO_DATA_DIR=$dir/data DODAVO_TEMP_DIR=$dir/temp \
    DODAVO_DOWNLOADS_DIR=$dir/downloads RUST_BACKTRACE=1 \
    ./dodavo-tests $args; echo \"exit:\$?\"" | tee /dev/stderr | tail -1 | grep -q '^exit:0'
status=$?
exit $status
