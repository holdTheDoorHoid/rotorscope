#!/usr/bin/env bash
# Prepare the patched esp-hal checkout the P4 hub builds against.
#
# The Elecrow P4 board carries ESP32-P4 silicon revision v1.3. Upstream esp-hal
# supports only v3.0 and newer (different interrupt controller, clocks and ROM).
# firmware/patches/esp-hal-p4-pre-v3/ holds the community compatibility patch
# (naoNao89/esp32p4-pre-v3-kit 0.1.3, MIT/Apache-2.0; see PROVENANCE.md there)
# against one pinned esp-hal commit. This script clones that commit and applies
# the patch, after checking the patch's SHA-256 against manifest.json. We do not
# run the kit's own tool; this does the same two steps in the open.
#
# Output: firmware/patches/esp-hal-p4-pre-v3/upstream/ (git-ignored).
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
dir="$here/firmware/patches/esp-hal-p4-pre-v3"
rev="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["upstream"]["revision"])' "$dir/manifest.json")"
want="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["patch"]["sha256"])' "$dir/manifest.json")"
got="$(sha256sum "$dir/esp32p4-pre-v3.patch" | cut -d' ' -f1)"
[ "$got" = "$want" ] || { echo "patch SHA-256 $got != manifest $want" >&2; exit 1; }
up="$dir/upstream"
if [ -d "$up/.git" ]; then
    if [ "$(git -C "$up" rev-parse HEAD)" = "$rev" ] && git -C "$up" apply --reverse --check "$dir/esp32p4-pre-v3.patch" 2>/dev/null; then
        echo "already prepared at $rev"; exit 0
    fi
    echo "$up exists but is not the prepared tree; remove it and rerun" >&2; exit 1
fi
git init -q "$up"
git -C "$up" remote add origin https://github.com/esp-rs/esp-hal.git
git -C "$up" fetch -q --depth 1 origin "$rev"
git -C "$up" checkout -q FETCH_HEAD
git -C "$up" apply "$dir/esp32p4-pre-v3.patch"
echo "prepared esp-hal $rev + pre-v3 patch in $up"
