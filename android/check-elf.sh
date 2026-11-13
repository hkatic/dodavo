#!/bin/bash
# Checks the native libraries of the Android app, as installer/check-imports.ps1
# does for Windows and macos/check-floor.sh for the Mac: each one's segments are
# aligned to 16 KB pages (which Google Play requires, and Android 15 and later
# devices with such pages need), it needs only the system's own libraries,
# both ABIs are there, and an app holds none of the libraries that cargo
# builds beside libdodavo.so. Fails when something is wrong.
#
#   android/check-elf.sh <app.aab | app.apk | libdodavo.so>...
#
# Needs the NDK (ANDROID_NDK_HOME, or the version the app uses in the SDK).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
ndk="${ANDROID_NDK_HOME:-${ANDROID_HOME:-$HOME/Library/Android/sdk}/ndk/30.0.16248370}"
readelf=$(ls "$ndk"/toolchains/llvm/prebuilt/*/bin/llvm-readelf 2>/dev/null | head -1)
[ -x "$readelf" ] || { echo "No llvm-readelf in $ndk" >&2; exit 1; }

# What Android has on every device the app runs on (API 26 and later).
allowed="libc.so libm.so libdl.so liblog.so libandroid.so"
abis="arm64-v8a x86_64"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failed=0
fail() {
    echo "FAIL: $*" >&2
    failed=1
}

check_so() {
    local so="$1" name="$2"
    local segments=0
    while read -r align; do
        segments=$((segments + 1))
        if (( align < 16384 )); then
            fail "$name: a LOAD segment is aligned to $((align)) bytes, not 16 KB"
        fi
    done < <("$readelf" -lW "$so" | awk '$1 == "LOAD" { print $NF }')
    (( segments > 0 )) || fail "$name: no LOAD segments found"
    while read -r needed; do
        [[ " $allowed " == *" $needed "* ]] || fail "$name needs $needed, which is not one of the system libraries it may need"
    done < <("$readelf" -d "$so" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')
}

for input in "$@"; do
    case "$input" in
        *.so)
            check_so "$input" "$input"
            ;;
        *.apk | *.aab)
            dir="$work/$(basename "$input")"
            mkdir -p "$dir"
            unzip -q -o "$input" '*.so' -d "$dir" 2>/dev/null || true
            for abi in $abis; do
                found=$(find "$dir" -path "*lib/$abi/*.so" | head -1)
                [ -n "$found" ] || fail "$input has no native library for $abi"
            done
            while IFS= read -r so; do
                check_so "$so" "$(basename "$input"): ${so#"$dir"/}"
                # A library named as cargo names what it builds for a
                # dependency (libiroh-<hash>.so) is one that the app would
                # carry and never load: libdodavo.so has all of Rust's code
                # (see buildRust in android/app/build.gradle.kts).
                if [[ "$(basename "$so")" =~ -[0-9a-f]{16}\.so$ ]]; then
                    fail "$input has ${so#"$dir"/}, which the app does not use"
                fi
            done < <(find "$dir" -name '*.so')
            ;;
        *)
            fail "$input is not an app bundle, an APK or a library"
            ;;
    esac
done

if (( failed )); then
    exit 1
fi
echo "The native libraries are aligned to 16 KB and need only Android's own libraries."
