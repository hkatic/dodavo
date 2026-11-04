#!/bin/bash
# Fails when a program would not start on the oldest macOS it is meant for, the
# counterpart of installer/check-imports.ps1. The compiler rejects Swift APIs
# that are too new; this checks what it cannot: what the program says of itself,
# and which libraries it needs at the start.
#
#   macos/check-floor.sh <program> [oldest macOS, default 11.0]
set -euo pipefail

program="$1"
floor="${2:-11.0}"

minos=$(vtool -show-build "$program" | awk '/minos/ {print $2; exit}')
if [ "$minos" != "$floor" ]; then
    echo "$program: made for macOS $minos, not $floor" >&2
    exit 1
fi

architectures=$(lipo -archs "$program")
if [ "$architectures" != "arm64" ]; then
    echo "$program: architectures $architectures, expected arm64" >&2
    exit 1
fi

# Whatever is not weak must be there on the oldest macOS. libswift_Concurrency is
# not, before macOS 12, nor is anything with a version newer than the floor.
bad=0
while read -r library rest; do
    case "$library" in
    *libswift_Concurrency*)
        case "$rest" in *weak*) ;; *) echo "$program: needs $library" >&2; bad=1 ;; esac
        ;;
    /System/Library/Frameworks/* | /System/Library/PrivateFrameworks/* | /usr/lib/*) ;;
    *)
        echo "$program: needs $library, which is not part of macOS" >&2
        bad=1
        ;;
    esac
done < <(otool -L "$program" | tail -n +2 | sed 's/^[[:space:]]*//')
[ "$bad" = 0 ] || exit 1
echo "$program: starts on macOS $floor and later, as far as can be told without it"
