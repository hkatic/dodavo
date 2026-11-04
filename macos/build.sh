#!/bin/bash
# Builds Dodavo.app for Apple Silicon: the Rust library, then the Swift program,
# then the bundle with what goes in it. Signs it with $DODAVO_SIGN_IDENTITY, or
# ad hoc without one (it then runs only on this Mac).
#
#   macos/build.sh [--debug]
#
# Writes target/macos/Dodavo.app. macos/make-dmg.sh makes the disk image of it.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

config=release
[ "${1:-}" = "--debug" ] && config=debug

# The oldest macOS that the program runs on. Xcode's own build system would
# refuse this one by now, but the compiler and the linker still take it.
export MACOSX_DEPLOYMENT_TARGET=11.0
target=aarch64-apple-darwin
rust_flag=()
[ "$config" = release ] && rust_flag=(--release)

cargo build -p dodavo-ffi --target "$target" ${rust_flag[@]+"${rust_flag[@]}"}
library="$root/target/$target/$config"

# The Swift enum of the text keys follows the Rust tables.
keys="macos/Sources/Dodavo/Keys.generated.swift"
cargo run -q -p dodavo-ffi --example gen_keys > "$keys.new"
if cmp -s "$keys.new" "$keys"; then rm "$keys.new"; else mv "$keys.new" "$keys"; fi

# What the Rust library needs of the system, as rustc says.
native=$(cargo rustc -q -p dodavo-ffi --target "$target" ${rust_flag[@]+"${rust_flag[@]}"} --crate-type staticlib -- --print native-static-libs 2>&1 \
    | sed -n 's/.*native-static-libs: //p' | tail -1)

# The compiler itself, not a build system: both of Xcode's and SwiftPM's either
# refuse a target older than Xcode's oldest supported macOS or quietly raise it,
# while the compiler and the linker still take it (check-floor.sh makes sure).
# Language mode 5 and no `async`: Swift's concurrency would need a library of its
# own in the app on macOS 11.
program="$root/target/swift/$config/Dodavo"
mkdir -p "$(dirname "$program")"
swift_flags=(-Onone -g)
[ "$config" = release ] && swift_flags=(-O -whole-module-optimization)
# macos/tests/ui-preview.sh: a build that opens every window with made-up data.
[ -n "${DODAVO_UI_TEST:-}" ] && swift_flags+=(-D DODAVO_UI_TEST)
# shellcheck disable=SC2086
xcrun swiftc -target arm64-apple-macos11.0 -swift-version 5 "${swift_flags[@]}" \
    -module-name Dodavo -I macos/Sources/CDodavo/include \
    -suppress-warnings \
    macos/Sources/Dodavo/*.swift \
    -L "$library" -ldodavo_ffi $native \
    -o "$program"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
years=$(sed -n 's/^copyright-years = "\(.*\)"/\1/p' Cargo.toml | head -1)

app="$root/target/macos/Dodavo.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$program" "$app/Contents/MacOS/Dodavo"
[ "$config" = release ] && strip -x "$app/Contents/MacOS/Dodavo"
sed -e "s/@VERSION@/$version/g" -e "s/@YEARS@/$years/g" macos/Resources/Info.plist > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"
cp macos/Resources/Dodavo.icns "$app/Contents/Resources/"
# The licenses go with every copy.
cp LICENSE "$app/Contents/Resources/LICENSE.txt"
cp res/third-party-licenses-macos.txt "$app/Contents/Resources/THIRD-PARTY-LICENSES.txt"

# Help pages, and what macOS shows in each language.
mkdir -p "$app/Contents/Resources/help"
cp res/help/macos/* "$app/Contents/Resources/help/"
cargo run -q -p dodavo-ffi --example gen_lproj -- "$app/Contents/Resources"

identity="${DODAVO_SIGN_IDENTITY:--}"
timestamp=--timestamp=none
[ "$identity" = "-" ] || timestamp=--timestamp
codesign --force --options runtime "$timestamp" --sign "$identity" \
    --entitlements macos/Resources/Dodavo.entitlements "$app"

"$root/macos/check-floor.sh" "$app/Contents/MacOS/Dodavo"
echo "$app"
