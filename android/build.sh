#!/bin/bash
# Builds the Android app with its native library and checks that library (see
# android/check-elf.sh): the app bundle for Google Play, or with --debug an APK
# to install on a device (adb install -r target/android/Dodavo-debug.apk).
#
#   android/build.sh [--debug]
#
# Needs JDK 21 (Android Studio's, if JAVA_HOME is not set), the Android SDK with
# NDK r30 (30.0.16248370), cargo-ndk and the Rust targets aarch64-linux-android
# and x86_64-linux-android. A release bundle is signed with the upload key when
# DODAVO_UPLOAD_KEYSTORE is set (see android/release.sh), otherwise unsigned.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
studio_jdk="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
if [ -z "${JAVA_HOME:-}" ] && [ -d "$studio_jdk" ]; then
    export JAVA_HOME="$studio_jdk"
fi

debug=0
for option in "$@"; do
    case "$option" in
    --debug) debug=1 ;;
    *)
        echo "Unknown option $option. Use --debug." >&2
        exit 2
        ;;
    esac
done

mkdir -p target/android
if [ "$debug" = 1 ]; then
    (cd android && ./gradlew --quiet :app:assembleDebug)
    cp android/app/build/outputs/apk/debug/app-debug.apk target/android/Dodavo-debug.apk
    android/check-elf.sh target/android/Dodavo-debug.apk
    echo "target/android/Dodavo-debug.apk"
else
    (cd android && ./gradlew --quiet :app:bundleRelease)
    version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
    cp android/app/build/outputs/bundle/release/app-release.aab "target/android/Dodavo-$version.aab"
    android/check-elf.sh "target/android/Dodavo-$version.aab"
    echo "target/android/Dodavo-$version.aab"
fi
