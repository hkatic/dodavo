#!/bin/bash
# Builds Dodavo with every window open on made-up transfers, takes a picture of
# each into the given folder, and writes which controls VoiceOver would have no
# name for. Works with the screen locked. For looking at layouts, not behaviour.
#
#   macos/tests/ui-preview.sh <folder> [language]
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
out="$1"
mkdir -p "$out"
cd "$root"
DODAVO_UI_TEST=1 macos/build.sh --debug > "$out/build.log" 2>&1 || { tail -20 "$out/build.log"; exit 1; }
pkill -x Dodavo 2>/dev/null || true
sleep 1
DODAVO_LANG="${2:-en}" DODAVO_AUDIT_FILE="$out/audit.txt" \
    target/macos/Dodavo.app/Contents/MacOS/Dodavo --profile uipreview &
pid=$!
sleep 6
cat > "$out/windows.swift" <<'SWIFT'
import CoreGraphics
let all = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
for w in all where (w[kCGWindowOwnerName as String] as? String) == "Dodavo" {
    if (w[kCGWindowLayer as String] as? Int) == 0, let id = w[kCGWindowNumber as String] as? Int {
        let name = (w[kCGWindowName as String] as? String) ?? "window"
        print("\(id)\t\(name)")
    }
}
SWIFT
xcrun swift "$out/windows.swift" 2>/dev/null | while IFS=$'\t' read -r id name; do
    screencapture -x -o -l "$id" "$out/${name// /-}-$id.png" 2>/dev/null || true
done
kill "$pid" 2>/dev/null || true
cat "$out/audit.txt" 2>/dev/null || echo "no audit was written"
ls "$out"/*.png 2>/dev/null
