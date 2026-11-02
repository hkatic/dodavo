#!/bin/bash
# Type-checks the Windows build from a Mac, tests included, which catches most of
# what a change to shared code can break, in seconds instead of a CI run. Nothing
# is linked or run: the C parts that dependencies compile get empty stand-ins.
#
#   macos/check-windows.sh [cargo check options]
#
# Once: rustup target add x86_64-pc-windows-msvc
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

stubs="$root/target/windows-check-stubs"
mkdir -p "$stubs"
cat > "$stubs/assert.h" <<'H'
#define assert(x) ((void)0)
#ifndef static_assert
#define static_assert _Static_assert
#endif
H
cat > "$stubs/string.h" <<'H'
#include <stddef.h>
void *memcpy(void *, const void *, size_t);
void *memset(void *, int, size_t);
void *memmove(void *, const void *, size_t);
int memcmp(const void *, const void *, size_t);
H
for header in stdlib.h intrin.h immintrin.h windows.h; do
    echo '#include <stddef.h>' > "$stubs/$header"
done
cat > "$stubs/fake-ar" <<'SH'
#!/bin/sh
for a in "$@"; do case "$a" in -out:*) printf '!<arch>\n' > "${a#-out:}";; esac; done
exit 0
SH
cat > "$stubs/llvm-rc" <<'SH'
#!/bin/sh
prev=
for a in "$@"; do
  case "$a" in /fo?*) : > "${a#/fo}";; esac
  case "$prev" in /fo|-fo) : > "$a";; esac
  prev="$a"
done
exit 0
SH
chmod +x "$stubs/fake-ar" "$stubs/llvm-rc"

PATH="$stubs:$PATH" CARGO_FEATURE_PURE=1 TARGET_CC=clang TARGET_AR="$stubs/fake-ar" \
    CFLAGS_x86_64_pc_windows_msvc="--target=x86_64-pc-windows-msvc -I$stubs -Wno-everything" \
    cargo check --target x86_64-pc-windows-msvc --all-targets --message-format=short "$@" 2>&1 \
    | grep -vE '^warning: (ring|blake3)@|^[[:space:]]+(cargo:|[A-Z_]+ = )'
