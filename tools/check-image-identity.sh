#!/usr/bin/env bash
# Firmware images are the same for every kit and every builder: build the same commit from two
# independent throwaway checkouts in two different directories, one of them with decoy key
# files and the retired key variable set, and compare the images byte for byte. Then look in
# them for the builder's home directory, user name and host name.
#
#   tools/check-image-identity.sh [--plain] [--paths] [node-wifi|gateway]...
#                                 default: node-wifi gateway
#
# What is compared: the app image espflash writes to the board (`espflash save-image` of the
# ELF, as tools/release-build.sh makes it), and the ELF itself. espflash copies only the
# ELF's loadable segments, but it also writes the SHA-256 of the whole ELF file into the app
# descriptor, so a difference anywhere in the ELF (its debug info included) shows in the image
# as 65 bytes: those 32 plus the image's own SHA-256 digest (32) and checksum (1).
#
# What an image depends on, besides the source, and how each is taken out
# (tools/repro-env.sh does all three):
#   - the build time and date in the app descriptor (esp-bootloader-esp-idf): SOURCE_DATE_EPOCH
#     is the commit's time;
#   - absolute host paths in panic messages and debug info (the checkout, the cargo registry,
#     the rustup home, so the builder's home directory): --remap-path-prefix;
#   - the checkout's directory itself: cargo hashes the location of every path dependency
#     outside the firmware's workspace (every crate under crates/) into symbol names, and the
#     linker lays the image out differently. Nothing in rustflags undoes that on stable Rust,
#     so both checkouts are built through one fixed path, /tmp/rotorscope-repro (repro_link),
#     which cargo does not resolve to the real directory. `--paths` also builds the second
#     checkout where it is, without the fixed path, to show the difference that removes.
# `--plain` builds without tools/repro-env.sh at all, to show what it removes.
#
# Uses the committed tree (git archive HEAD): commit first. Needs about 2.5 GB free while it
# runs; everything is removed afterwards. Touches no board. Exit 0 when every image matches
# and holds no builder details.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(dirname "$here")
plain=0 paths=0
while [[ ${1:-} == --* ]]; do
  case $1 in
    --plain) plain=1 ;;
    --paths) paths=1 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
  shift
done
images=("$@"); (( ${#images[@]} )) || images=(node-wifi gateway)

work=$(mktemp -d "${TMPDIR:-/tmp}/rs-identity.XXXXXX")
cleanup() {
  [[ -L /tmp/rotorscope-repro && -O /tmp/rotorscope-repro && $(readlink /tmp/rotorscope-repro) == "$work"/* ]] && rm -f /tmp/rotorscope-repro
  rm -rf "$work"
}
trap cleanup EXIT
commit=$(git -C "$root" rev-parse --short HEAD)
[[ -z $(git -C "$root" status --porcelain --untracked-files=no) ]] || echo "check-image-identity: NOTE: uncommitted changes are not part of this check (it builds HEAD $commit)"
commit_time=$(git -C "$root" log -1 --format=%ct)

# Two independent copies of the commit, in different directories (and at different depths).
one=$work/one two=$work/elsewhere/another-name
for d in "$one" "$two"; do
  mkdir -p "$d"
  git -C "$root" archive HEAD | tar -x -C "$d"
done
# Copy one gets decoy key files where the retired build looked for one, and the variable.
# Random bytes made here, never a real key; nothing may read them.
head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' >"$one/tools/link.key"
printf 'rotorscope kit v1\n%s\n' "$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')" >"$one/tools/kit.key"
chmod 600 "$one/tools/link.key" "$one/tools/kit.key"

# build <checkout dir> <image> <decoy 0|1> <linked 0|1> <out.bin>
build() {
  local co=$1 img=$2 decoy=$3 linked=$4 out=$5 dir feats=() tdir elf
  case $img in
    node-wifi) dir=$co/firmware/node-wifi/c5; feats=(--features esp32c5,band24,espnow) ;;
    gateway)   dir=$co/firmware/gateway ;;
    *) echo "unknown image $img" >&2; return 2 ;;
  esac
  tdir=$co/target-identity
  elf=$tdir/riscv32imac-unknown-none-elf/release/$img
  (
    cd "$dir"
    unset ROTORSCOPE_LINK_KEY_FILE ROTORSCOPE_BENCH CARGO_TARGET_DIR SOURCE_DATE_EPOCH
    (( decoy )) && export ROTORSCOPE_LINK_KEY_FILE=$co/tools/link.key
    extra=() targ=$tdir
    if (( ! plain )); then
      export SOURCE_DATE_EPOCH=$commit_time  # the copies have no .git
      # shellcheck source=repro-env.sh
      REPRO_ROOT=$co . "$here/repro-env.sh" riscv32imac-unknown-none-elf
      if (( linked )); then repro_link "$co"; targ=$REPRO_LINK/target-identity; fi
      extra=("${REPRO_CARGO_ARGS[@]}")
    fi
    RUSTC_WRAPPER= cargo build --release --locked --offline "${feats[@]}" --target-dir "$targ" "${extra[@]}" >"$work/build-$img.log" 2>&1
  ) || { echo "check-image-identity: $img failed to build in $co:" >&2; tail -20 "$work/build-$img.log" >&2; return 1; }
  espflash save-image --chip esp32c5 "$elf" "$out" >/dev/null 2>&1
  cp "$elf" "$out.elf"
  if grep -aq 'link-key:' "$elf"; then echo "FAIL $img: the image carries a link-key tag"; return 1; fi
  rm -rf "$tdir"
}

# compare <label> <a.bin> <b.bin>: 0 when the app images and the ELFs are identical
compare() {
  local label=$1 x=$2 y=$3 hx hy ok=0
  hx=$(sha256sum <"$x" | cut -d' ' -f1); hy=$(sha256sum <"$y" | cut -d' ' -f1)
  if [[ $hx == "$hy" ]]; then
    echo "ok   $label: app images identical, SHA-256 $hx"
  else
    echo "FAIL $label: app images differ ($hx vs $hy), $(cmp -l "$x" "$y" 2>/dev/null | wc -l) bytes"
    ok=1
  fi
  if cmp -s "$x.elf" "$y.elf"; then echo "     $label: ELFs identical too"; else echo "     $label: ELFs differ"; ok=1; fi
  return $ok
}

# leaks <label> <file>...: 0 when no builder details are in the files
leaks() {
  local label=$1 found; shift
  REPRO_LEAK_PATHS=("$work")
  if found=$(repro_leaks "$@"); then
    echo "ok   $label: no home directory, user name, host name or build folder in the image or ELF"
  else
    echo "FAIL $label:"; sed 's/^/     /' <<<"$found"; return 1
  fi
}

# shellcheck source=repro-env.sh
. "$here/repro-env.sh" riscv32imac-unknown-none-elf  # for repro_leaks
fails=0
for img in "${images[@]}"; do
  how=$( (( plain )) && echo ' (--plain: no tools/repro-env.sh)' || echo ', each through the fixed path')
  echo "check-image-identity: $img at $commit, two checkouts in different directories$how..."
  build "$one" "$img" 1 1 "$work/$img-one.bin"
  build "$two" "$img" 0 1 "$work/$img-two.bin"
  compare "$img (decoy keys vs none, two directories)" "$work/$img-one.bin" "$work/$img-two.bin" || fails=$((fails + 1))
  leaks "$img" "$work/$img-one.bin" "$work/$img-one.bin.elf" || fails=$((fails + 1))
  if (( paths && ! plain )); then
    echo "check-image-identity: $img, the second checkout built where it is (no fixed path), for comparison..."
    build "$two" "$img" 0 0 "$work/$img-two-unlinked.bin"
    compare "$img (without the fixed path; differs, see the header)" "$work/$img-one.bin" "$work/$img-two-unlinked.bin" || true
  fi
done
if (( fails )); then echo "check-image-identity: $fails FAILED"; exit 1; fi
echo "check-image-identity: all passed"
