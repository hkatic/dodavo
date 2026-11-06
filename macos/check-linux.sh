#!/bin/bash
# Type-checks the Linux build from a Mac, tests included, as check-windows.sh
# does for Windows: Linux runs the core's tests with the POSIX code that Android
# uses, in CI. Nothing is linked or run: the C parts that dependencies compile
# get empty stand-ins.
#
#   macos/check-linux.sh [cargo check options]
#
# Once: rustup target add x86_64-unknown-linux-gnu
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

stubs="$root/target/linux-check-stubs"
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
echo '#include <stddef.h>' > "$stubs/stdlib.h"
cat > "$stubs/fake-ar" <<'SH'
#!/bin/sh
for a in "$@"; do case "$a" in *.a) printf '!<arch>\n' > "$a"; exit 0;; esac; done
exit 0
SH
chmod +x "$stubs/fake-ar"

CARGO_FEATURE_PURE=1 CC_x86_64_unknown_linux_gnu=clang AR_x86_64_unknown_linux_gnu="$stubs/fake-ar" \
    CFLAGS_x86_64_unknown_linux_gnu="--target=x86_64-unknown-linux-gnu -I$stubs -Wno-everything" \
    cargo check --target x86_64-unknown-linux-gnu --workspace --all-targets --message-format=short "$@" 2>&1 \
    | grep -vE '^warning: (ring|blake3)@|^[[:space:]]+(cargo:|[A-Z_]+ = )'
