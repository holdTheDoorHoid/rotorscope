#!/usr/bin/env bash
# Keys are not in firmware images (docs/LINK-KEYS-DESIGN.md section 9, "Key in images"):
# build the same commit in two throwaway checkouts at different paths, one of them with
# decoy key files and the retired key variable set, and compare the images' SHA-256.
#
#   tools/check-image-identity.sh [--plain] [node-wifi|gateway]...   default: node-wifi gateway
#
# What is compared: the app image espflash writes to the board (`espflash save-image`),
# which is what "the same image for every kit" means. The ELFs are compared too and
# reported; they also carry debug sections, which are never flashed.
#
# Two things in an image depend on where and when it was built, not on the source. The
# script removes both, the same way tools/flash-node.sh does (tools/repro-env.sh):
#   - the build time and date in the app descriptor (esp-bootloader-esp-idf), unless
#     SOURCE_DATE_EPOCH is set: set to the commit's time;
#   - absolute paths of crates outside the firmware's own workspace (crates/*, the cargo
#     registry) in panic messages: remapped to /rotorscope and /cargo.
# `--plain` builds without either, to show what differs then.
#
# Uses the committed tree (git archive HEAD): commit first. Needs about 2.5 GB free while it
# runs; everything is removed afterwards. Touches no board.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(dirname "$here")
plain=0
if [[ ${1:-} == --plain ]]; then plain=1; shift; fi
images=("$@"); (( ${#images[@]} )) || images=(node-wifi gateway)

work=$(mktemp -d "${TMPDIR:-/tmp}/rs-identity.XXXXXX")
trap 'rm -rf "$work"' EXIT
commit=$(git -C "$root" rev-parse --short HEAD)
[[ -z $(git -C "$root" status --porcelain --untracked-files=no) ]] || echo "check-image-identity: NOTE: uncommitted changes are not part of this check (it builds HEAD $commit)"

# Two checkouts whose paths differ in length as well as name.
a=$work/a
b=$work/checkout-number-two
for d in "$a" "$b"; do
  mkdir -p "$d"
  git -C "$root" archive HEAD | tar -x -C "$d"
done
# Copy A gets decoy key files where the retired build looked for one, and the variable.
# Random bytes made here, never a real key; nothing may read them.
head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' >"$a/tools/link.key"
printf 'rotorscope kit v1\n%s\n' "$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')" >"$a/tools/kit.key"
chmod 600 "$a/tools/link.key" "$a/tools/kit.key"

build() {  # build <checkout> <image> <decoy 0|1>: prints the ELF path
  local co=$1 img=$2 decoy=$3 dir feats elf
  case $img in
    node-wifi) dir=$co/firmware/node-wifi/c5; feats=(--features esp32c5,band24,espnow)
               elf=$co/firmware/node-wifi/target-espnow/riscv32imac-unknown-none-elf/release/node-wifi ;;
    gateway)   dir=$co/firmware/gateway; feats=()
               elf=$co/firmware/gateway/target-espnow/riscv32imac-unknown-none-elf/release/gateway ;;
    *) echo "unknown image $img" >&2; return 2 ;;
  esac
  (
    cd "$dir"
    unset ROTORSCOPE_LINK_KEY_FILE ROTORSCOPE_BENCH CARGO_TARGET_DIR
    (( decoy )) && export ROTORSCOPE_LINK_KEY_FILE=$co/tools/link.key
    extra=()
    if (( plain )); then
      unset SOURCE_DATE_EPOCH
    else
      # The copies have no .git: take the commit time from the real checkout.
      SOURCE_DATE_EPOCH=$(git -C "$root" log -1 --format=%ct)
      export SOURCE_DATE_EPOCH
      # shellcheck source=repro-env.sh
      . "$co/tools/repro-env.sh" riscv32imac-unknown-none-elf
      extra=("${REPRO_CARGO_ARGS[@]}")
    fi
    RUSTC_WRAPPER= cargo build --release "${feats[@]}" --target-dir "$(dirname "$(dirname "$(dirname "$elf")")")" "${extra[@]}" >"$co/build-$img.log" 2>&1
  ) || { echo "check-image-identity: $img failed to build in $co (log: build-$img.log):" >&2; tail -20 "$co/build-$img.log" >&2; return 1; }
  echo "$elf"
}

fails=0
for img in "${images[@]}"; do
  echo "check-image-identity: building $img at $commit in two checkouts$( (( plain )) && echo ' (--plain)')..."
  ea=$(build "$a" "$img" 1)
  eb=$(build "$b" "$img" 0)
  espflash save-image --chip esp32c5 "$ea" "$work/$img-a.bin" >/dev/null
  espflash save-image --chip esp32c5 "$eb" "$work/$img-b.bin" >/dev/null
  ia=$(sha256sum <"$work/$img-a.bin" | cut -d' ' -f1); ib=$(sha256sum <"$work/$img-b.bin" | cut -d' ' -f1)
  xa=$(sha256sum <"$ea" | cut -d' ' -f1); xb=$(sha256sum <"$eb" | cut -d' ' -f1)
  if [[ $ia == "$ib" ]]; then
    echo "ok   $img: app images identical, SHA-256 $ia"
  else
    echo "FAIL $img: app images differ ($ia vs $ib); $(cmp -l "$work/$img-a.bin" "$work/$img-b.bin" 2>/dev/null | wc -l) bytes differ"
    diff <(strings -n 12 "$work/$img-a.bin" | sed "s#$a#<checkout>#g") <(strings -n 12 "$work/$img-b.bin" | sed "s#$b#<checkout>#g") | head -5 || true
    strings -n 8 "$work/$img-a.bin" | grep -m3 -E "$a|/home/|registry/src" | sed 's/^/     path in image: /' || true
    fails=$((fails + 1))
  fi
  if [[ $xa == "$xb" ]]; then echo "     $img: ELFs identical too"; else echo "     $img: ELFs differ (debug sections, never flashed)"; fi
  # The retired build tagged every image with its key's id.
  if grep -aq 'link-key:' "$ea" "$work/$img-a.bin"; then
    echo "FAIL $img: an image carries a link-key tag"; fails=$((fails + 1))
  fi
  rm -rf "$a/firmware/$img/target-espnow" "$b/firmware/$img/target-espnow" \
         "$a/firmware/node-wifi/target-espnow" "$b/firmware/node-wifi/target-espnow"
done
if (( fails )); then echo "check-image-identity: $fails FAILED"; exit 1; fi
echo "check-image-identity: all passed"
