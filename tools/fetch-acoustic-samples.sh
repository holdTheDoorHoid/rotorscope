#!/usr/bin/env bash
# Fetch drone and background recordings for tuning and testing the rotor-sound
# detector (crates/acoustic, tools/acoustic-file). Nothing fetched here is ever
# committed: testdata/ is git-ignored, and each set has its own licence.
#
#   batear    batear-io/batear-datasets  MIT. Mavic Pro, Mini 4 Pro, 5-inch FPV,
#             backgrounds. Stored in Git LFS; fetched file by file (about 400 MB).
#   volanti   agamrossen/VolAnti test/audio. Real rotor rig and wind.
#   alemadi   saraalemadi/DroneAudioDataset. Licence: non-commercial academic
#             use, and it excludes security and surveillance use. The owner
#             has said we may use it (2026-11-17). Used only to measure the
#             detector here; nothing derived from it is shipped.
#
# Usage: tools/fetch-acoustic-samples.sh [batear] [volanti] [alemadi]   (default: all)
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
out="$here/testdata/acoustic"
mkdir -p "$out"
sets=("$@"); [ ${#sets[@]} -gt 0 ] || sets=(batear volanti alemadi)

clone() { # url dir
    if [ -d "$2/.git" ]; then echo "have $2"; else GIT_LFS_SKIP_SMUDGE=1 git clone -q --depth 1 "$1" "$2"; fi
}

for s in "${sets[@]}"; do
    case "$s" in
    batear)
        clone https://github.com/batear-io/batear-datasets "$out/batear"
        # LFS pointers -> real files, six at a time, without needing git-lfs.
        # The mavic-bat-overlap set is skipped: four 46 MB bat-survey
        # recordings, slow to fetch and not needed for tuning.
        (cd "$out/batear" && git ls-files '*.wav' '*.WAV' | grep -v mavic-bat-overlap | while read -r f; do
            if head -c 40 "$f" | grep -q '^version https://git-lfs'; then echo "$f"; fi
        done | xargs -r -P 6 -I{} sh -c 'curl -sfL --retry 3 -o "$1.part" "https://media.githubusercontent.com/media/batear-io/batear-datasets/HEAD/$1" && mv "$1.part" "$1" || { rm -f "$1.part"; echo "failed: $1" >&2; }' _ {})
        ;;
    volanti) clone https://github.com/agamrossen/VolAnti "$out/volanti" ;;
    alemadi) clone https://github.com/saraalemadi/DroneAudioDataset "$out/alemadi" ;;
    *) echo "unknown set: $s" >&2; exit 2 ;;
    esac
done
du -sh "$out"/* 2>/dev/null
