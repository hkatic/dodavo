#!/bin/bash
# Writes res/third-party-licenses-android.txt: the licenses of every crate
# compiled into the Android app's native library, which they require to go
# with every copy, then those of the Rust standard library. The Kotlin and
# AndroidX libraries of the app follow (see android/app/build.gradle.kts).
# Run it after changing dependencies or toolchains, and commit the file
# together with Cargo.lock. Needs cargo-about
# (cargo install cargo-about --locked --features cli).
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

# The same configuration, for what is compiled for Android.
sed -e 's/^targets = .*/targets = ["aarch64-linux-android", "x86_64-linux-android"]/' \
    installer/about.toml > "$work/about.toml"

manifest=crates/dodavo-android/Cargo.toml
[ -f "$manifest" ] || manifest=crates/dodavo-ffi/Cargo.toml
cargo about generate --locked --fail -c "$work/about.toml" \
    -m "$manifest" installer/about.hbs | tr -d '\r' > "$work/crates.txt"

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
    if [ -f android/app/build/dodavo/libraries.txt ]; then
        echo
        cat android/app/build/dodavo/libraries.txt
    fi
} > "$work/all.txt"

if ! cmp -s "$work/all.txt" "$dest"; then
    cp "$work/all.txt" "$dest"
fi
wc -c "$dest"
