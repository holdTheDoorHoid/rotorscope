#!/usr/bin/env bash
# Prepare, or check, the patched esp-hal checkout the P4 hub builds against.
#
# The Elecrow P4 board carries ESP32-P4 silicon revision v1.3. Upstream esp-hal
# supports only v3.0 and newer (different interrupt controller, clocks and ROM).
# firmware/patches/esp-hal-p4-pre-v3/ holds the community compatibility patch
# (naoNao89/esp32p4-pre-v3-kit 0.1.3, MIT/Apache-2.0; see PROVENANCE.md there)
# against one pinned esp-hal commit, and our own fixes on top
# (rotorscope-*.patch). We do not run the kit's own tool; this does the same
# steps in the open.
#
#   tools/p4-prepare.sh                 clone the pinned commit, apply the patches, then --verify
#   tools/p4-prepare.sh --verify [DIR]  check an existing checkout, offline (default DIR: upstream/)
#   tools/p4-prepare.sh --update-sums   after changing a rotorscope-*.patch on purpose: rewrite
#                                       its SHA-256 in manifest.json; review the diff and commit both
#
# manifest.json pins everything: the esp-hal commit (a full 40-hex commit id,
# never a branch or tag), the community patch's SHA-256, and the SHA-256 of
# each of our patches, in the order they are applied. Any mismatch stops the
# script (exit 1) and nothing is applied: a patch on disk that the manifest
# does not list, a listed patch that is missing, a wrong checksum, a different
# commit after the fetch. A failed preparation removes the half-made checkout.
#
# --verify reads only: it checks that DIR is at the pinned commit, that its Git
# objects are intact (git fsck), that no file is hidden from Git (assume-
# unchanged, skip-worktree), and that the files differ from that commit in
# exactly the way the patches say, byte for byte, with nothing else added or
# changed. Files Git ignores (build output) are not checked.
#
# Output: firmware/patches/esp-hal-p4-pre-v3/upstream/ (git-ignored).
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
dir="$here/firmware/patches/esp-hal-p4-pre-v3"
manifest="$dir/manifest.json"
community="$dir/esp32p4-pre-v3.patch"
# Fixed: the manifest must name exactly this repository.
REPO_URL=https://github.com/esp-rs/esp-hal.git

hint=""
die() { echo "p4-prepare: $*$hint" >&2; exit 1; }
# Clean up on any exit: temporary folders, and a half-made checkout.
made=0; up=""; tmpdirs=()
cleanup() {
    local d
    for d in "${tmpdirs[@]}"; do rm -rf "$d"; done
    if [ "$made" = 1 ] && [ -n "$up" ]; then rm -rf "$up"; echo "p4-prepare: failed; removed the half-made $up" >&2; fi
}
trap cleanup EXIT
# Git without the user's hooks, templates or prompts.
g() { GIT_TERMINAL_PROMPT=0 GIT_OPTIONAL_LOCKS=0 git -c core.hooksPath=/dev/null -c protocol.allow=never -c protocol.https.allow=always "$@"; }

json() {  # json <python expression on m>: one value from the manifest
    python3 -I -c 'import json,sys; m=json.load(open(sys.argv[1])); v='"$1"'; print(v if isinstance(v,str) else "\n".join(v))' "$manifest"
}

read_manifest() {
    [ -f "$manifest" ] || die "$manifest is missing"
    repo="$(json 'm["upstream"]["repository"]')"
    rev="$(json 'm["upstream"]["revision"]')"
    want_community="$(json 'm["patch"]["sha256"]')"
    [ "$repo" = "$REPO_URL" ] || die "manifest repository is '$repo', not $REPO_URL"
    [[ $rev =~ ^[0-9a-f]{40}$ ]] || die "manifest revision '$rev' is not a full 40-hex commit id (a branch or tag name is refused)"
    [[ $want_community =~ ^[0-9a-f]{64}$ ]] || die "manifest patch.sha256 is not a SHA-256"
    mapfile -t local_names < <(json '[p["path"] for p in m.get("local_patches", [])]' | sed '/^$/d')
    mapfile -t local_sums < <(json '[p["sha256"] for p in m.get("local_patches", [])]' | sed '/^$/d')
}

sha() { sha256sum "$1" | cut -d' ' -f1; }

# Every patch, checked, in the order it is applied (to the array `patches`).
check_patches() {
    local got i name
    [ -f "$community" ] || die "$community is missing"
    got="$(sha "$community")"
    [ "$got" = "$want_community" ] || die "esp32p4-pre-v3.patch SHA-256 $got != manifest $want_community"
    patches=("$community")
    for i in "${!local_names[@]}"; do
        name="${local_names[$i]}"
        [[ $name =~ ^rotorscope-[A-Za-z0-9._-]+\.patch$ ]] || die "manifest local patch '$name' is not a rotorscope-*.patch file name"
        [ -f "$dir/$name" ] || die "manifest lists $name, which is missing"
        got="$(sha "$dir/$name")"
        [ "$got" = "${local_sums[$i]}" ] || die "$name SHA-256 $got != manifest ${local_sums[$i]} (changed on purpose? tools/p4-prepare.sh --update-sums)"
        patches+=("$dir/$name")
    done
    for p in "$dir"/rotorscope-*.patch; do
        [ -e "$p" ] || continue
        [[ " ${local_names[*]} " == *" $(basename "$p") "* ]] || die "$(basename "$p") is not listed in manifest.json local_patches (tools/p4-prepare.sh --update-sums)"
    done
}

update_sums() {
    python3 -I - "$manifest" "$dir" <<'PY'
import glob, hashlib, json, os, sys
manifest, d = sys.argv[1], sys.argv[2]
m = json.load(open(manifest))
old = [p["path"] for p in m.get("local_patches", [])]
found = sorted(os.path.basename(p) for p in glob.glob(os.path.join(d, "rotorscope-*.patch")))
# Keep the existing order; new patches go last, in name order.
order = [n for n in old if n in found] + [n for n in found if n not in old]
m["local_patches"] = [{"path": n, "sha256": hashlib.sha256(open(os.path.join(d, n), "rb").read()).hexdigest()} for n in order]
with open(manifest, "w") as f:
    json.dump(m, f, indent=2)
    f.write("\n")
for p in m["local_patches"]:
    print(f'{p["sha256"]}  {p["path"]}')
PY
    echo "manifest.json updated: review 'git diff $manifest' and commit it with the patch" >&2
}

verify() {  # verify <checkout>: returns only if it is exactly the pinned commit plus the patches
    local up="$1" t head bad f e meta mode blob i
    [ -d "$up/.git" ] || die "$up is not a Git checkout"
    head="$(g -C "$up" rev-parse --verify HEAD)" || die "$up: no HEAD"
    [ "$head" = "$rev" ] || die "$up is at $head, not the pinned $rev"
    [ "$(g -C "$up" cat-file -t "$rev")" = commit ] || die "$up: $rev is not a commit object"
    g -C "$up" fsck --no-progress --no-dangling >/dev/null 2>&1 || die "$up: git fsck found damaged objects"
    # Index entries hidden from status (assume-unchanged: lower-case tag; skip-worktree: S).
    bad="$(g -C "$up" ls-files -v | grep -E '^([a-z]|S) ' || true)"
    [ -z "$bad" ] || die "$up: files hidden from Git (assume-unchanged or skip-worktree):"$'\n'"$bad"
    g -C "$up" diff-index --cached --quiet HEAD -- || die "$up: the index differs from $rev (staged changes)"

    t="$(mktemp -d)"
    tmpdirs+=("$t")
    # What the patches touch, and the result of applying them to the pinned files.
    mkdir -p "$t/want"
    g -C "$t/want" init -q --template=
    : >"$t/touched"
    for p in "${patches[@]}"; do
        g -C "$t/want" apply --numstat -z "$p" | tr '\0' '\n' | awk -F'\t' 'NF >= 3 { print $3 }' >>"$t/touched"
    done
    if grep -q '=>' "$t/touched"; then die "a patch renames a file; --verify does not handle renames"; fi
    sort -u -o "$t/touched" "$t/touched"
    while IFS= read -r f; do
        if g -C "$up" cat-file -e "$rev:$f" 2>/dev/null; then
            mkdir -p "$t/want/$(dirname "$f")"
            g -C "$up" cat-file blob "$rev:$f" >"$t/want/$f"
        fi
    done <"$t/touched"
    for p in "${patches[@]}"; do
        g -C "$t/want" apply "$p" || die "$(basename "$p") does not apply to the pinned files"
    done

    # What actually differs from the pinned commit: tracked files whose bytes
    # differ (hashed here, not taken from Git's stat cache) or are gone, plus
    # files Git neither tracks nor ignores.
    local -a files=() blobs=() hashes=()
    : >"$t/changed"
    while IFS= read -r -d '' e; do
        meta="${e%%$'\t'*}"; f="${e#*$'\t'}"
        read -r mode blob _ <<<"$meta"
        case "$mode" in 100644|100755) ;; *) die "$up: $f has mode $mode (only regular files are expected)" ;; esac
        if [ -f "$up/$f" ] && [ ! -L "$up/$f" ]; then files+=("$f"); blobs+=("$blob"); else echo "$f" >>"$t/changed"; fi
    done < <(g -C "$up" ls-files -s -z)
    mapfile -t hashes < <(printf '%s\n' "${files[@]}" | (cd "$up" && g hash-object --stdin-paths))
    [ "${#hashes[@]}" = "${#files[@]}" ] || die "$up: could not hash every tracked file"
    for i in "${!files[@]}"; do
        [ "${hashes[$i]}" = "${blobs[$i]}" ] || echo "${files[$i]}" >>"$t/changed"
    done
    g -C "$up" ls-files --others --exclude-standard >>"$t/changed"
    sort -u -o "$t/changed" "$t/changed"

    bad="$(comm -23 "$t/changed" "$t/touched")"
    [ -z "$bad" ] || die "$up: changed but not by any patch:"$'\n'"$bad"
    while IFS= read -r f; do
        if [ -e "$t/want/$f" ]; then
            [ -f "$up/$f" ] || die "$up: $f is missing (a patch creates or changes it)"
            cmp -s "$t/want/$f" "$up/$f" || die "$up: $f differs from the patched file"
        else
            [ ! -e "$up/$f" ] || die "$up: $f exists, but the patches delete it"
        fi
    done <"$t/touched"
    echo "verified $up: esp-hal $rev + $(basename "$community")$(printf ' + %s' "${local_names[@]}"), ${#files[@]} tracked files hashed, nothing else changed"
}

prepare() {
    up="$dir/upstream"
    if [ -e "$up" ]; then
        hint=" (remove $up and rerun to prepare it afresh)"
        verify "$up"
        echo "already prepared"
        return 0
    fi
    command -v git >/dev/null || die "git not found"
    made=1
    g init -q --template= "$up"
    g -C "$up" remote add origin "$REPO_URL"
    g -C "$up" fetch -q --depth 1 --no-tags --no-recurse-submodules origin "$rev"
    [ "$(g -C "$up" rev-parse --verify FETCH_HEAD)" = "$rev" ] || die "fetched $(g -C "$up" rev-parse FETCH_HEAD), not $rev"
    g -C "$up" checkout -q --detach "$rev"
    [ "$(g -C "$up" rev-parse --verify HEAD)" = "$rev" ] || die "checkout is at $(g -C "$up" rev-parse HEAD), not $rev"
    for p in "${patches[@]}"; do
        g -C "$up" apply "$p"
        echo "applied $(basename "$p")"
    done
    verify "$up"
    made=0
    echo "prepared esp-hal $rev + patches in $up"
}

case "${1:-}" in
    --update-sums) update_sums ;;
    --verify)
        read_manifest; check_patches
        verify "$(cd "${2:-$dir/upstream}" && pwd)" ;;
    "") read_manifest; check_patches; prepare ;;
    *) die "unknown option '$1' (none, --verify [DIR] or --update-sums)" ;;
esac
