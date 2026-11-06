#!/bin/bash
# Writes res/third-party-licenses-android.txt: the licenses of every crate
# compiled into the Android app's native library, which they require to go
# with every copy, then those of the Rust standard library, and then those
# of the app's Kotlin and AndroidX libraries (the task libraryLicenses of
# android/app/build.gradle.kts, which fails on a license that is not
# allowed there). Run it after changing dependencies or toolchains, and
# commit the file together with Cargo.lock. Needs cargo-about
# (cargo install cargo-about --locked --features cli) and a JDK (Android
# Studio's, if JAVA_HOME is not set).
#
# What is accepted is in installer/about.toml, which lists the licenses for
# Windows. Written only when the text changes: cargo rebuilds on its
# modification time.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

dest=res/third-party-licenses-android.txt
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The same configuration, for what is compiled for Android, and the same
# layout, which says that the app's libraries come last.
sed -e 's/^targets = .*/targets = ["aarch64-linux-android", "x86_64-linux-android"]/' \
    installer/about.toml > "$work/about.toml"
sed -e 's/^library, which every Rust program contains, follows at the end\.$/library, which every Rust program contains, follows them, and then the\
libraries of the app./' installer/about.hbs > "$work/about.hbs"
grep -q '^libraries of the app\.$' "$work/about.hbs"

# The app's libraries.
if [ -z "${JAVA_HOME:-}" ] && [ -d "/Applications/Android Studio.app/Contents/jbr/Contents/Home" ]; then
    export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
fi
(cd android && ./gradlew -q :app:libraryLicenses)
libraries=android/app/build/dodavo/libraries.txt

manifest=crates/dodavo-android/Cargo.toml
[ -f "$manifest" ] || manifest=crates/dodavo-ffi/Cargo.toml
cargo about generate --locked --fail -c "$work/about.toml" \
    -m "$manifest" "$work/about.hbs" | tr -d '\r' > "$work/crates.txt"

sysroot=$(rustc --print sysroot)
library="$sysroot/share/doc/rust/COPYRIGHT-library.html"
rust_version=$(rustc --version | cut -d' ' -f2)
{
    cat "$work/crates.txt"
    echo
    echo "================================================================================"
    echo "The Rust standard library ($rust_version)"
    echo "================================================================================"
    echo
    if [ -f "$library" ]; then
        if command -v textutil >/dev/null; then
            textutil -convert txt -stdout "$library" | tr -d '\r'
        else
            sed -e 's/<[^>]*>//g' "$library"
        fi
    else
        echo "Rust is dual licensed under the MIT license and the Apache License, version 2.0."
        echo "See https://github.com/rust-lang/rust/blob/master/COPYRIGHT"
    fi
    echo
    echo "================================================================================"
    echo
    cat "$libraries"
} > "$work/all.txt"

if ! cmp -s "$work/all.txt" "$dest"; then
    cp "$work/all.txt" "$dest"
fi
wc -c "$dest"
