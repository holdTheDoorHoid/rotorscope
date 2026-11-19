#!/usr/bin/env bash
# Keys are not in firmware images (docs/LINK-KEYS-DESIGN.md section 9, "Key in images"):
# build the same commit from two independent throwaway checkouts, one of them with decoy key
# files and the retired key variable set, and compare the images' SHA-256.
#
#   tools/check-image-identity.sh [--plain] [--paths] [node-wifi|gateway]...
#                                 default: node-wifi gateway
#
# What is compared: the app image espflash writes to the board (`espflash save-image`),
# which is what "the same image for every kit" means. The ELFs are compared too and
# reported; they also carry debug sections, which are never flashed.
#
# What else an image depends on, besides the source:
#   - the build time and date in the app descriptor (esp-bootloader-esp-idf), unless
#     SOURCE_DATE_EPOCH is set: tools/repro-env.sh sets it to the commit's time;
#   - absolute paths of crates outside the firmware's own workspace (crates/*, the cargo and
#     rustup homes) in panic messages: tools/repro-env.sh remaps them (/rotorscope, /cargo,
#     /rustup), which also keeps the builder's home directory out of the image;
#   - the directory the checkout is in, through cargo itself: for a path dependency outside
#     the firmware's workspace (every crate under crates/), cargo hashes its absolute location
#     into the crate's symbol names (`-C metadata`), and the linker then lays the image out
#     differently. Nothing in rustflags undoes that on stable Rust. So images are identical
#     when built in the same directory path, on any machine; that is what the default run
#     checks (each checkout is moved to one path in turn). `--paths` builds them in two
#     different directories as well, to show the difference.
# `--plain` builds without tools/repro-env.sh, to show what it removes.
#
# Uses the committed tree (git archive HEAD): commit first. Needs about 2.5 GB free while it
# runs; everything is removed afterwards. Touches no board.
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
trap 'rm -rf "$work"' EXIT
commit=$(git -C "$root" rev-parse --short HEAD)
[[ -z $(git -C "$root" status --porcelain --untracked-files=no) ]] || echo "check-image-identity: NOTE: uncommitted changes are not part of this check (it builds HEAD $commit)"
commit_time=$(git -C "$root" log -1 --format=%ct)

# Two independent copies of the commit.
for d in one two; do
  mkdir -p "$work/$d"
  git -C "$root" archive HEAD | tar -x -C "$work/$d"
done
# Copy one gets decoy key files where the retired build looked for one, and the variable.
# Random bytes made here, never a real key; nothing may read them.
head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' >"$work/one/tools/link.key"
printf 'rotorscope kit v1\n%s\n' "$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')" >"$work/one/tools/kit.key"
chmod 600 "$work/one/tools/link.key" "$work/one/tools/kit.key"

# build <checkout dir> <image> <decoy 0|1> <out.bin>
build() {
  local co=$1 img=$2 decoy=$3 out=$4 dir feats=() tdir elf
  case $img in
    node-wifi) dir=$co/firmware/node-wifi/c5; feats=(--features esp32c5,band24,espnow)
               tdir=$co/firmware/node-wifi/target-identity ;;
    gateway)   dir=$co/firmware/gateway; tdir=$co/firmware/gateway/target-identity ;;
    *) echo "unknown image $img" >&2; return 2 ;;
  esac
  elf=$tdir/riscv32imac-unknown-none-elf/release/$img
  (
    cd "$dir"
    unset ROTORSCOPE_LINK_KEY_FILE ROTORSCOPE_BENCH CARGO_TARGET_DIR SOURCE_DATE_EPOCH
    (( decoy )) && export ROTORSCOPE_LINK_KEY_FILE=$co/tools/link.key
    extra=()
    if (( ! plain )); then
      export SOURCE_DATE_EPOCH=$commit_time  # the copies have no .git
      # shellcheck source=repro-env.sh
      . "$co/tools/repro-env.sh" riscv32imac-unknown-none-elf
      extra=("${REPRO_CARGO_ARGS[@]}")
    fi
    RUSTC_WRAPPER= cargo build --release "${feats[@]}" --target-dir "$tdir" "${extra[@]}" >"$work/build-$img.log" 2>&1
  ) || { echo "check-image-identity: $img failed to build in $co:" >&2; tail -20 "$work/build-$img.log" >&2; return 1; }
  espflash save-image --chip esp32c5 "$elf" "$out" >/dev/null 2>&1
  cp "$elf" "$out.elf"
  if grep -aq 'link-key:' "$elf"; then echo "FAIL $img: the image carries a link-key tag"; return 1; fi
  rm -rf "$tdir"
}

# compare <label> <a.bin> <b.bin>: 0 when the app images are identical
compare() {
  local label=$1 x=$2 y=$3 hx hy
  hx=$(sha256sum <"$x" | cut -d' ' -f1); hy=$(sha256sum <"$y" | cut -d' ' -f1)
  if [[ $hx == "$hy" ]]; then
    echo "ok   $label: app images identical, SHA-256 $hx"
  else
    echo "FAIL $label: app images differ ($hx vs $hy), $(cmp -l "$x" "$y" 2>/dev/null | wc -l) bytes"
    strings -n 8 "$x" | grep -m3 -E "$work|/home/|/\.cargo/" | sed 's/^/     path in image: /' || true
  fi
  if cmp -s "$x.elf" "$y.elf"; then echo "     $label: ELFs identical too"; else echo "     $label: ELFs differ (debug sections, never flashed)"; fi
  [[ $hx == "$hy" ]]
}

fails=0
same=$work/rotorscope
for img in "${images[@]}"; do
  echo "check-image-identity: $img at $commit, two checkouts built in turn at one path$( (( plain )) && echo ' (--plain)')..."
  mv "$work/one" "$same"; build "$same" "$img" 1 "$work/$img-one.bin"; mv "$same" "$work/one"
  mv "$work/two" "$same"; build "$same" "$img" 0 "$work/$img-two.bin"; mv "$same" "$work/two"
  compare "$img (decoy keys vs none, same path)" "$work/$img-one.bin" "$work/$img-two.bin" || fails=$((fails + 1))
  if (( paths )); then
    echo "check-image-identity: $img, the second checkout built where it is (another path), for comparison..."
    build "$work/two" "$img" 0 "$work/$img-two-elsewhere.bin"
    compare "$img (another path; differs by design, see the header)" "$work/$img-one.bin" "$work/$img-two-elsewhere.bin" || true
  fi
done
if (( fails )); then echo "check-image-identity: $fails FAILED"; exit 1; fi
echo "check-image-identity: all passed"
