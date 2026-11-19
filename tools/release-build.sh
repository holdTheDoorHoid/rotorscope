#!/usr/bin/env bash
# Build the shipped firmware images of one commit the reproducible way, with checksums, a
# record of the build inputs and a parts list per image. Offline; touches no board.
#
#   tools/release-build.sh [--commit REV] [--out DIR] [--only NAME,...] [--toolchain TC]
#                          [--p4-upstream DIR]
#   tools/release-build.sh --self-test      checks of this script's decisions; builds nothing
#
#   --commit REV      the commit to build (default HEAD). Built from a fresh local clone, so
#                     nothing outside the commit (keys, local files, old target folders) can
#                     reach an image.
#   --out DIR         where the release goes (default release-out/rotorscope-<commit>, kept out
#                     of git); must not exist yet, or be empty.
#   --only NAMES      build only these images (comma-separated names from the table below).
#   --toolchain TC    the rustup toolchain for every image (default stable). To reproduce a
#                     release, use the exact version its BUILD-INPUTS.txt names, e.g. 1.99.1.
#   --p4-upstream DIR the git checkout tools/p4-prepare.sh made (default
#                     firmware/patches/esp-hal-p4-pre-v3/upstream). Only its pinned commit is
#                     used: the two patches are applied afresh from the commit being built.
#
# Refuses: a working tree with changes or untracked files; an image carrying a bench-build
# tag other than "none" (docs/BENCH.md "Bench builds"), or no tag where one is expected; an
# image with a link-key tag; an image or ELF holding the builder's home directory, user name
# or host name; cargo settings outside the repository that change what is built (rustflags,
# profiles, [env], [patch], [unstable] in the cargo home's config, or any .cargo folder above
# the build folder); less than 2 GB free.
#
# How the images are made the same on every machine (docs/dev/release.md has the evidence):
#   - built through one fixed path, /tmp/rotorscope-repro (tools/repro-env.sh, repro_link),
#     because cargo mixes the location of crates/* into symbol names;
#   - every host path remapped (/home, /rustup, /cargo, /rotorscope), SOURCE_DATE_EPOCH =
#     the commit's time, an empty environment apart from PATH and the cargo and rustup homes,
#     no compiler wrapper, --locked --offline;
#   - the image is `espflash save-image` of the ELF. espflash copies only the loadable segments,
#     but writes the SHA-256 of the whole ELF (debug info included) into the app descriptor, so
#     the ELF must be reproducible too, and it is published beside the image.
#
# Output (DIR):
#   <name>.bin              the app image espflash writes to the board (no debug info, no paths)
#   elf/<name>.elf          the ELF it was made from, with debug info but no host paths; its
#                           SHA-256 is inside <name>.bin. `espflash flash` of it writes <name>.bin
#   parts/<name>.txt        parts list; parts/<name>.cdx.json the same as CycloneDX 1.5
#   BUILD-INPUTS.txt        commit, toolchain and tool versions, lockfile hashes, environment
#   SHA256SUMS              of every file above
# Then the SIGNING STEP (printed at the end): this script never signs anything.
#
# Images (name: what, from where):
#   gateway                  firmware/gateway, `cargo fw`
#   node-wifi                firmware/node-wifi/c5, `cargo fw24-now` (2.4 GHz boot role,
#                            ESP-NOW; the one C5 image does both bands)
#   node-ble                 firmware/node-ble, C5 with ESP-NOW (as tools/flash-node.sh builds it)
#   node-sdr                 firmware/node-sdr/c5, `cargo fw-now`
#   rid-beacon-BENCH-ONLY    firmware/rid-beacon, C5: the bench test transmitter, NOT for the field
#   hub-p4                   firmware/hub-p4, `cargo fw` (needs the patched esp-hal checkout)
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(dirname "$here")
# shellcheck source=repro-env.sh
. "$here/repro-env.sh" riscv32imac-unknown-none-elf

# name | build folder | triple | chip | ELF name | bench kind | cargo arguments
#   bench kind: fenced = must carry exactly bench-build:none; tool = a bench tool with no fence
#   (must carry no bench tag at all).
IMAGES=(
  "gateway|firmware/gateway|riscv32imac-unknown-none-elf|esp32c5|gateway|fenced|"
  "node-wifi|firmware/node-wifi/c5|riscv32imac-unknown-none-elf|esp32c5|node-wifi|fenced|--features esp32c5,band24,espnow"
  "node-ble|firmware/node-ble|riscv32imac-unknown-none-elf|esp32c5|node-ble|fenced|--target riscv32imac-unknown-none-elf --no-default-features --features esp32c5,espnow"
  "node-sdr|firmware/node-sdr/c5|riscv32imac-unknown-none-elf|esp32c5|node-sdr|fenced|--features espnow"
  "rid-beacon-BENCH-ONLY|firmware/rid-beacon|riscv32imac-unknown-none-elf|esp32c5|rid-beacon|tool|--target riscv32imac-unknown-none-elf --no-default-features --features esp32c5"
  "hub-p4|firmware/hub-p4|riscv32imafc-unknown-none-elf|esp32p4|hub-p4|fenced|"
)
P4_PATCHES=firmware/patches/esp-hal-p4-pre-v3
MIN_FREE_KB=$((2 * 1024 * 1024))

die() { echo "release-build: $*" >&2; exit 1; }

field() {  # field <image line> <n>: the nth (1-based) |-separated field
  local IFS='|' f
  read -r -a f <<<"$1|"
  echo "${f[$(($2 - 1))]}"
}

image_line() {  # image_line <name>: its table line, or fail
  local l
  for l in "${IMAGES[@]}"; do [[ $(field "$l" 1) == "$1" ]] && { echo "$l"; return 0; }; done
  return 1
}

# tree_clean <repo>: 0 when nothing is changed or untracked (ignored files do not count).
tree_clean() { [[ -z $(git -C "$1" status --porcelain 2>/dev/null) ]]; }

# bench_verdict <elf> <fenced|tool>: empty output and 0 when the image may ship; else the
# reason and 1.
bench_verdict() {
  local tags
  tags=$(grep -aoE 'bench-build:[A-Za-z0-9_,:-]+' "$1" 2>/dev/null | LC_ALL=C sort -u | tr '\n' ' ' || true)
  tags=${tags% }
  if grep -aq 'link-key:' "$1"; then echo "carries a link-key tag (a key in an image)"; return 1; fi
  case $2 in
    fenced) [[ $tags == "bench-build:none" ]] && return 0
            if [[ -z $tags ]]; then echo "has no bench-build tag, so nothing shows it is a field build"
            else echo "is a BENCH build ($tags)"; fi
            return 1 ;;
    tool)   [[ -z $tags || $tags == "bench-build:none" ]] && return 0
            echo "is a BENCH build ($tags)"; return 1 ;;
  esac
  echo "unknown bench kind $2"; return 1
}


# cargo_config_verdict <file>: 0 when a cargo config outside the repository cannot change what
# is built (a compiler wrapper or incremental setting is fine: the build overrides both).
cargo_config_verdict() {
  [[ -f $1 ]] || return 0
  if grep -qE '^[[:space:]]*\[(profile|env|patch|unstable|target|source)([].]|$)|^[[:space:]]*(rustflags|rustdocflags|linker|target|rustc)[[:space:]]*=' "$1"; then
    echo "$1 sets something that changes what is built (rustflags, a profile, [env], [patch], [source], [unstable], a target or the compiler)"
    return 1
  fi
}

# stray_cargo_dirs <dir>: .cargo folders in <dir>'s ancestors (cargo reads their config).
stray_cargo_dirs() {
  local d=$1
  while [[ $d != / ]]; do
    d=$(dirname "$d")
    [[ -e $d/.cargo/config.toml || -e $d/.cargo/config ]] && echo "$d/.cargo"
  done
  return 0
}

# alias_features <config.toml> <alias>: the features an alias in a firmware's config builds
alias_features() {
  awk -v a="$2" '$1 == a && $2 == "=" { if (match($0, /--features [^ "]+/)) print substr($0, RSTART + 11, RLENGTH - 11); else print ""; found = 1 } END { exit !found }' "$1"
}

# sums_write <dir>: SHA256SUMS of every file below <dir> (sorted, relative paths)
sums_write() {
  (cd "$1" && find . -type f ! -name SHA256SUMS ! -name 'SHA256SUMS.*' -printf '%P\n' | LC_ALL=C sort | while IFS= read -r f; do sha256sum -- "$f"; done >SHA256SUMS)
}

# p4_prepare <src clone> <upstream checkout>: put the pinned esp-hal commit plus the two
# patches (from the clone) where hub-p4 expects them.
p4_prepare() {
  local src=$1 up=$2 rev want got p dest
  rev=$(python3 -I -c 'import json,sys;print(json.load(open(sys.argv[1]))["upstream"]["revision"])' "$src/$P4_PATCHES/manifest.json")
  want=$(python3 -I -c 'import json,sys;print(json.load(open(sys.argv[1]))["patch"]["sha256"])' "$src/$P4_PATCHES/manifest.json")
  got=$(sha256sum <"$src/$P4_PATCHES/esp32p4-pre-v3.patch" | cut -d' ' -f1)
  [[ $got == "$want" ]] || { echo "hub-p4: patch SHA-256 $got is not the manifest's $want"; return 1; }
  if [[ ! -d $up ]] || ! git -C "$up" cat-file -e "$rev^{commit}" 2>/dev/null; then
    echo "hub-p4: no esp-hal checkout holding commit $rev at $up. Run tools/p4-prepare.sh once (it downloads that commit), pass --p4-upstream, or leave hub-p4 out with --only"
    return 1
  fi
  dest=$src/$P4_PATCHES/upstream
  rm -rf "$dest"; mkdir -p "$dest"
  git -C "$up" archive "$rev" | tar -x -C "$dest"
  for p in "$src/$P4_PATCHES/esp32p4-pre-v3.patch" "$src/$P4_PATCHES"/rotorscope-*.patch; do
    [[ -e $p ]] || continue
    git -C "$src" apply --directory="$P4_PATCHES/upstream" "$p" || { echo "hub-p4: $(basename "$p") does not apply to esp-hal $rev"; return 1; }
  done
  echo "$rev"
}

free_kb() { df -Pk "$1" | awk 'NR == 2 { print $4 }'; }

self_test() {
  local t fails=0 out l r f
  set +e
  t=$(mktemp -d "${TMPDIR:-/tmp}/rs-release-test.XXXXXX")
  trap 'rm -rf "$t"' RETURN
  check() { if [[ $2 == 0 ]]; then echo "ok   $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi; }

  # Dirty trees.
  git init -q "$t/repo"; printf 'x\n' >"$t/repo/a"; printf 'ignored\n' >"$t/repo/.gitignore"
  git -C "$t/repo" add a .gitignore; git -C "$t/repo" -c user.name=t -c user.email=t@example.invalid commit -qm one
  tree_clean "$t/repo"; check "a committed tree is clean" $?
  printf 'secret\n' >"$t/repo/ignored"; tree_clean "$t/repo"; check "an ignored file (a key, say) does not make it dirty" $?
  printf 'y\n' >"$t/repo/a"; tree_clean "$t/repo" && r=1 || r=0; check "a changed file is refused" $r
  git -C "$t/repo" checkout -q a; printf 'z\n' >"$t/repo/new"; tree_clean "$t/repo" && r=1 || r=0; check "an untracked file is refused" $r

  # Bench tags and keys.
  printf 'xx bench-build:none\0 xx' >"$t/none.elf"; printf 'xx bench-build:faulttest\0' >"$t/bench.elf"
  printf 'xx bench-build:none\0 bench-build:fakedrone,faulttest\0' >"$t/both.elf"; printf 'xx' >"$t/notag.elf"
  printf 'xx bench-build:none\0 link-key:00112233\0' >"$t/key.elf"
  bench_verdict "$t/none.elf" fenced >/dev/null; check "a field build (bench-build:none) ships" $?
  bench_verdict "$t/bench.elf" fenced >/dev/null && r=1 || r=0; check "a bench build is refused" $r
  bench_verdict "$t/both.elf" fenced >/dev/null && r=1 || r=0; check "a bench tag beside bench-build:none is refused" $r
  bench_verdict "$t/notag.elf" fenced >/dev/null && r=1 || r=0; check "a fenced image with no tag at all is refused" $r
  bench_verdict "$t/notag.elf" tool >/dev/null; check "the unfenced bench tool with no tag ships (marked BENCH-ONLY)" $?
  bench_verdict "$t/bench.elf" tool >/dev/null && r=1 || r=0; check "the bench tool with a bench tag is refused" $r
  bench_verdict "$t/key.elf" fenced >/dev/null && r=1 || r=0; check "an image with a link-key tag is refused" $r

  # Builder details in an image.
  printf 'abc' >"$t/clean.bin"; printf 'abc %s/.cargo/x' "$HOME" >"$t/home.bin"
  printf 'abc-%s-def' "$(id -un)" >"$t/user.bin"; printf 'abc %s def' "$(hostname 2>/dev/null || uname -n)" >"$t/host.bin"
  printf 'abc %s/x' "$t/build" >"$t/folder.bin"
  REPRO_LEAK_PATHS=("$t/build")
  repro_leaks "$t/clean.bin" >/dev/null; check "an image without builder details passes" $?
  out=$(repro_leaks "$t/home.bin") && r=1 || r=0; check "the home directory path is found" $r
  [[ $out != *"$HOME"* ]]; check "the report names what was found, not its value" $?
  if (( $(id -un | wc -c) > 3 )); then repro_leaks "$t/user.bin" >/dev/null && r=1 || r=0; check "the user name is found" $r; fi
  if (( $( (hostname 2>/dev/null || uname -n) | wc -c) > 3 )); then repro_leaks "$t/host.bin" >/dev/null && r=1 || r=0; check "the host name is found" $r; fi
  repro_leaks "$t/folder.bin" >/dev/null && r=1 || r=0; check "the build folder path is found" $r
  REPRO_LEAK_PATHS=()

  # Cargo settings outside the repository.
  printf '[build]\nrustc-wrapper = "/x/sccache"\nincremental = false\n' >"$t/ok.toml"
  printf '[build]\nrustflags = ["-C", "opt-level=0"]\n' >"$t/flags.toml"
  printf '[profile.release]\ndebug = 0\n' >"$t/profile.toml"
  printf '[env]\nESP_LOG = "debug"\n' >"$t/env.toml"
  printf '[target.riscv32imac-unknown-none-elf]\nrunner = "x"\n' >"$t/target.toml"
  cargo_config_verdict "$t/ok.toml" >/dev/null; check "a cargo home config with only a wrapper is accepted" $?
  for f in flags profile env target; do
    cargo_config_verdict "$t/$f.toml" >/dev/null && r=1 || r=0; check "a cargo home config with $f settings is refused" $r
  done
  cargo_config_verdict "$t/missing.toml" >/dev/null; check "no cargo home config is fine" $?
  mkdir -p "$t/up/.cargo" "$t/up/a/b"; : >"$t/up/.cargo/config.toml"
  [[ $(stray_cargo_dirs "$t/up/a/b") == "$t/up/.cargo" ]]; check "a .cargo folder above the build folder is found" $?

  # The fixed path.
  (
    REPRO_FIXED_OVERRIDE=$t/fixed
    mkdir -p "$t/src1" "$t/src2"
    repro_link "$t/src1" && [[ $(readlink "$t/fixed") == "$t/src1" && $REPRO_LINK == "$t/fixed" ]]
  ); check "the fixed path is made to point at the build folder" $?
  (
    REPRO_FIXED_OVERRIDE=$t/fixed
    repro_link "$t/src2" && [[ $(readlink "$t/fixed") == "$t/src2" ]]
  ); check "a stale link of ours is replaced" $?
  rm -f "$t/fixed"; : >"$t/fixed"
  ( REPRO_FIXED_OVERRIDE=$t/fixed; repro_link "$t/src1" 2>/dev/null ) && r=1 || r=0
  check "a fixed path that is not our link is refused" $r
  rm -f "$t/fixed"
  (
    REPRO_FIXED_OVERRIDE=$t/fixed
    mkdir -p "$t/src1/fw/sub/.cargo"; : >"$t/src1/fw/Cargo.toml"
    printf '[target.riscv32imac-unknown-none-elf]\nrustflags = ["-C", "force-frame-pointers"]\n' >"$t/src1/fw/sub/.cargo/config.toml"
    cd "$t/src1/fw/sub" && repro_link "$t/src1" >/dev/null &&
      [[ ${REPRO_CARGO_ARGS[*]} == *"target.riscv32imac-unknown-none-elf.rustflags="* &&
         ${REPRO_CARGO_ARGS[*]} == *"--remap-path-prefix=$t/fixed=/rotorscope"* &&
         ${REPRO_CARGO_ARGS[*]} == *"--remap-path-prefix=$t/src1=/rotorscope"* &&
         ${REPRO_CARGO_ARGS[*]} == *"--manifest-path $t/fixed/fw/Cargo.toml" ]]
  ); check "cargo gets the manifest through the fixed path, and every path remapped" $?

  # The image table matches how the repository builds each image.
  l=$(image_line node-wifi); [[ $(field "$l" 7) == "--features $(alias_features "$root/firmware/node-wifi/c5/.cargo/config.toml" fw24-now | sed 's/ --target-dir.*//')" ]]
  check "node-wifi is built as the fw24-now alias" $?
  l=$(image_line node-sdr); [[ $(field "$l" 7) == "--features $(alias_features "$root/firmware/node-sdr/c5/.cargo/config.toml" fw-now)" ]]
  check "node-sdr is built as the fw-now alias" $?
  grep -q '^fw = "build --release"$' "$root/firmware/gateway/.cargo/config.toml" && [[ -z $(field "$(image_line gateway)" 7) ]]
  check "the gateway is built as its fw alias" $?
  grep -q '^fw = "build --release"$' "$root/firmware/hub-p4/.cargo/config.toml" && [[ -z $(field "$(image_line hub-p4)" 7) ]]
  check "hub-p4 is built as its fw alias" $?
  grep -q 'ble_features=esp32c5,espnow ;;' "$root/tools/flash-node.sh" && [[ $(field "$(image_line node-ble)" 7) == *"--features esp32c5,espnow" ]]
  check "node-ble is built with the features tools/flash-node.sh uses for an ESP-NOW node" $?
  grep -qF -- "$(field "$(image_line rid-beacon-BENCH-ONLY)" 7)" "$root/docs/get-going/kit-flash.md"
  check "rid-beacon is built as docs/get-going/kit-flash.md says" $?
  image_line nonsense >/dev/null && r=1 || r=0; check "an unknown image name is refused" $r
  r=0; for l in "${IMAGES[@]}"; do [[ $(field "$l" 1) == rid-beacon* && $(field "$l" 1) != *BENCH-ONLY ]] && r=1; done
  check "the bench transmitter's name says BENCH-ONLY" $r

  # hub-p4 without the patched checkout.
  mkdir -p "$t/clone/$P4_PATCHES"; cp "$root/$P4_PATCHES/manifest.json" "$root/$P4_PATCHES/esp32p4-pre-v3.patch" "$t/clone/$P4_PATCHES/"
  out=$(p4_prepare "$t/clone" "$t/no-such-checkout") && r=1 || r=0
  [[ $r == 0 && $out == *"tools/p4-prepare.sh"* ]]; check "hub-p4 fails clearly when the esp-hal checkout is missing" $?
  printf 'tampered\n' >>"$t/clone/$P4_PATCHES/esp32p4-pre-v3.patch"
  out=$(p4_prepare "$t/clone" "$t/no-such-checkout") && r=1 || r=0
  [[ $r == 0 && $out == *"SHA-256"* ]]; check "a changed P4 patch is refused" $?

  # Checksums.
  mkdir -p "$t/rel/elf"; printf 'b' >"$t/rel/z.bin"; printf 'a' >"$t/rel/a.bin"; printf 'e' >"$t/rel/elf/a.elf"
  sums_write "$t/rel"
  [[ $(cut -c67- "$t/rel/SHA256SUMS" | tr '\n' ' ') == "a.bin elf/a.elf z.bin " ]]; check "SHA256SUMS lists every file, sorted, relative" $?
  (cd "$t/rel" && sha256sum -c --quiet SHA256SUMS); check "SHA256SUMS checks with sha256sum -c" $?
  sums_write "$t/rel"; ! grep -q 'SHA256SUMS' "$t/rel/SHA256SUMS"; check "SHA256SUMS does not list itself" $?

  # The parts-list script.
  python3 -I "$here/release-parts.py" --self-test >/dev/null; check "tools/release-parts.py --self-test" $?

  echo "release-build self-test: $( ((fails)) && echo "$fails FAILED" || echo "all passed")"
  ((fails == 0))
}

# ---------------------------------------------------------------------------------------
commit=HEAD out='' only='' toolchain=stable p4up=$root/$P4_PATCHES/upstream
while (($#)); do
  case $1 in
    --self-test) self_test; exit $? ;;
    --commit) commit=${2:?}; shift ;;
    --out) out=${2:?}; shift ;;
    --only) only=${2:?}; shift ;;
    --toolchain) toolchain=${2:?}; shift ;;
    --p4-upstream) p4up=${2:?}; shift ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown option $1 (see --help)" ;;
  esac
  shift
done

for tool in git cargo rustup espflash python3 flock sha256sum tar; do
  command -v "$tool" >/dev/null || die "needs $tool on PATH"
done
selected=()
if [[ -n $only ]]; then
  IFS=, read -r -a names <<<"$only"
  for n in "${names[@]}"; do l=$(image_line "$n") || die "no image called $n"; selected+=("$l"); done
else
  selected=("${IMAGES[@]}")
fi

tree_clean "$root" || die "the working tree has changes or untracked files; commit or remove them first (git status)"
full=$(git -C "$root" rev-parse --verify "$commit^{commit}") || die "no commit $commit"
short=${full:0:12}
sde=$(git -C "$root" log -1 --format=%ct "$full")
out=${out:-$root/release-out/rotorscope-$short}
if [[ -e $out ]] && [[ -n $(ls -A "$out") ]]; then die "$out exists and is not empty"; fi

cargo_home=${CARGO_HOME:-$HOME/.cargo}
rustup_home=${RUSTUP_HOME:-$HOME/.rustup}
for c in "$cargo_home/config.toml" "$cargo_home/config"; do
  msg=$(cargo_config_verdict "$c") || die "$msg; move it aside for the release build"
done

work=$(mktemp -d "${TMPDIR:-/tmp}/rotorscope-release.XXXXXX")
cleanup() {
  [[ -n ${REPRO_LINK:-} && -L $REPRO_LINK && $(readlink "$REPRO_LINK") == "$work/src" ]] && rm -f "$REPRO_LINK"
  rm -rf "$work"
}
trap cleanup EXIT
stray=$(stray_cargo_dirs "$work/src")
[[ -z $stray ]] || die "cargo would also read $stray (above the build folder); move it aside or set TMPDIR elsewhere"
(( $(free_kb "$work") >= MIN_FREE_KB )) || die "less than 2 GB free in $(dirname "$work")"

for l in "${selected[@]}"; do
  triple=$(field "$l" 3)
  [[ -d $(rustc "+$toolchain" --print sysroot 2>/dev/null)/lib/rustlib/$triple ]] ||
    die "toolchain $toolchain lacks target $triple (rustup +$toolchain target add $triple)"
done

echo "release-build: commit $full, toolchain $toolchain"
git clone -q --no-checkout --shared "$root" "$work/src"
git -C "$work/src" -c advice.detachedHead=false checkout -q --detach "$full"
src=$work/src

for l in "${selected[@]}"; do
  if [[ $(field "$l" 1) == hub-p4 ]]; then
    p4rev=$(p4_prepare "$src" "$p4up") || die "$p4rev"
  fi
done

if [[ -x $src/tools/check-wifi-blobs.sh ]]; then
  "$src/tools/check-wifi-blobs.sh" >"$work/blobs.log" 2>&1 || { cat "$work/blobs.log" >&2; die "tools/check-wifi-blobs.sh failed for $short"; }
else
  echo "release-build: NOTE: $short has no tools/check-wifi-blobs.sh; radio libraries not checked"
fi

repro_link "$src"
mkdir -p "$out/elf" "$out/parts"
REPRO_LEAK_PATHS=("$work" "$src")
built=()
clean_env=(env -i "PATH=$PATH" "HOME=$HOME" "CARGO_HOME=$cargo_home" "RUSTUP_HOME=$rustup_home"
  "SOURCE_DATE_EPOCH=$sde" "RUSTC_WRAPPER=" "CARGO_TERM_COLOR=never" "LC_ALL=C" "TZ=UTC")

for l in "${selected[@]}"; do
  name=$(field "$l" 1) dir=$(field "$l" 2) triple=$(field "$l" 3) chip=$(field "$l" 4)
  elfname=$(field "$l" 5) kind=$(field "$l" 6)
  read -r -a args <<<"$(field "$l" 7)"
  (( $(free_kb "$work") >= MIN_FREE_KB )) || die "less than 2 GB free before building $name"
  echo "release-build: $name ..."
  tdir=$REPRO_LINK/target-release
  (
    cd "$src/$dir"
    repro_args "$triple"
    "${clean_env[@]}" cargo "+$toolchain" build --release --locked --offline "${args[@]}" \
      --target-dir "$tdir" "${REPRO_CARGO_ARGS[@]}" >"$work/$name.log" 2>&1 ||
      { tail -30 "$work/$name.log" >&2; exit 1; }
    # The parts list, for the same features and target.
    meta_args=(--locked --offline --manifest-path "${REPRO_CARGO_ARGS[-1]}")
    feats=(); skip=0
    for a in "${args[@]}"; do
      if ((skip)); then skip=0; continue; fi
      case $a in --target) skip=1 ;; *) feats+=("$a") ;; esac
    done
    "${clean_env[@]}" cargo "+$toolchain" metadata "${meta_args[@]}" --format-version 1 \
      --filter-platform "$triple" "${feats[@]}" >"$work/$name.meta.json"
    tree=("${clean_env[@]}" cargo "+$toolchain" tree "${meta_args[@]}" --target "$triple" "${feats[@]}" --prefix none -f '{p}')
    "${tree[@]}" -e normal,no-proc-macro >"$work/$name.image.tree"
    "${tree[@]}" -e normal,build >"$work/$name.all.tree"
  ) || die "$name did not build (log above)"
  elf=$src/target-release/$triple/release/$elfname
  [[ -f $elf ]] || die "$name: no ELF at $elf"
  why=$(bench_verdict "$elf" "$kind") || die "$name $why; refusing to ship it"
  espflash save-image --chip "$chip" "$elf" "$out/$name.bin" >"$work/$name.espflash.log" 2>&1 ||
    { cat "$work/$name.espflash.log" >&2; die "$name: espflash save-image failed"; }
  cp "$elf" "$out/elf/$name.elf"
  leaks=$(repro_leaks "$out/$name.bin" "$out/elf/$name.elf") ||
    die "$name: builder details in the output: $leaks (ROTORSCOPE_RELEASE_IGNORE_NAMES=1 skips the user and host name check when it is a common word)"
  manifest_dir=$src/$dir
  while [[ ! -f $manifest_dir/Cargo.toml ]]; do manifest_dir=$(dirname "$manifest_dir"); done
  python3 -I "$here/release-parts.py" --metadata "$work/$name.meta.json" \
    --tree-image "$work/$name.image.tree" --tree-all "$work/$name.all.tree" \
    --lock "$manifest_dir/Cargo.lock" --root "$REPRO_LINK" --image "$name" --commit "$full" \
    --text "$out/parts/$name.txt" --cyclonedx "$out/parts/$name.cdx.json"
  rm -rf "$src/target-release"
  built+=("$l")
  echo "release-build: $name done, $(sha256sum <"$out/$name.bin" | cut -c1-16)..."
done

# The record of what went in. Nothing here depends on the machine or the folders used, so a
# reproduction gives the same file when its inputs match.
{
  echo "# Rotorscope release build inputs (tools/release-build.sh)"
  echo "commit                $full"
  echo "SOURCE_DATE_EPOCH     $sde (the commit's time)"
  echo "build path            $REPRO_FIXED (a link to a fresh clone of the commit)"
  echo "path remapping        \$HOME -> /home, rustup home -> /rustup, cargo home -> /cargo, checkout and build path -> /rotorscope"
  echo "environment           only PATH, HOME, CARGO_HOME, RUSTUP_HOME, SOURCE_DATE_EPOCH, RUSTC_WRAPPER= (empty), LC_ALL=C, TZ=UTC"
  echo "cargo flags           build --release --locked --offline, plus each image's arguments below"
  echo "image                 espflash save-image --chip <chip> <elf> <name>.bin (espflash defaults for flash mode, size, frequency)"
  echo "toolchain             $toolchain"
  rustc "+$toolchain" -vV | sed 's/^/rustc                 /'
  echo "cargo                 $(cargo "+$toolchain" -V)"
  echo "espflash              $(espflash --version)"
  for l in "${built[@]}"; do
    echo "image $(field "$l" 1): $(field "$l" 2), target $(field "$l" 3), chip $(field "$l" 4), arguments: $(field "$l" 7)"
  done
  for l in "${built[@]}"; do
    d=$src/$(field "$l" 2); while [[ ! -f $d/Cargo.lock ]]; do d=$(dirname "$d"); done
    echo "lockfile ${d#"$src"/}/Cargo.lock sha256 $(sha256sum <"$d/Cargo.lock" | cut -d' ' -f1)"
  done | LC_ALL=C sort -u
  [[ -n ${p4rev:-} ]] && echo "hub-p4 esp-hal         $p4rev + $P4_PATCHES/*.patch (applied from the commit)"
  for l in "${built[@]}"; do [[ $(field "$l" 1) == rid-beacon* ]] && echo "NOTE rid-beacon-BENCH-ONLY is the bench test transmitter (fake drone identity). Not for the field."; done
  true
} >"$out/BUILD-INPUTS.txt"

sums_write "$out"
echo
echo "release-build: done: $out"
sed 's/^/  /' "$out/SHA256SUMS"
cat <<EOF

SIGNING STEP (not done by this script; docs/dev/release.md "Signing"):
  sign $out/SHA256SUMS with the project's release key, producing
  $out/SHA256SUMS.sig, before publishing anything. Never publish unsigned sums as a release.
EOF
