#!/bin/bash
# Runs a test binary built for Android on the connected device, as cargo's
# runner for the Android targets (.cargo/config.toml): pushes it, runs it in
# /data/local/tmp/dodavo-test with the folders the core would get from the app,
# and passes on its exit code.
#
#   cargo ndk -t arm64-v8a --platform 26 test -p dodavo --lib -- --test-threads=1
#
# ANDROID_SERIAL picks the device when several are connected. DODAVO_DEVICE_TMP
# runs the tests in another folder, /sdcard/Download/dodavo-test say, for the
# shared storage (where adb's own user sees it, not as an app would).
set -euo pipefail

binary="$1"
shift
dir=/data/local/tmp/dodavo-test
tmp="${DODAVO_DEVICE_TMP:-$dir/tmp}"
adb shell "mkdir -p $dir '$tmp'" >/dev/null
adb push "$binary" "$dir/$(basename "$binary")" >/dev/null
args=""
for arg in "$@"; do
    args="$args '${arg//\'/\'\\\'\'}'"
done
set +e
adb shell "cd $dir && chmod 755 '$(basename "$binary")' && \
    TMPDIR='$tmp' HOME=$dir DODAVO_DATA_DIR=$dir/data DODAVO_TEMP_DIR=$dir/temp \
    DODAVO_DOWNLOADS_DIR=$dir/downloads RUST_BACKTRACE=1 \
    ./'$(basename "$binary")' $args; echo \"exit:\$?\"" | tee /dev/stderr | tail -1 | grep -q '^exit:0'
status=$?
exit $status
