# Source this (bash) in the firmware directory you are about to build in, to make the image
# independent of where and when it is built:
#
#   cd firmware/gateway && . ../../tools/repro-env.sh riscv32imac-unknown-none-elf
#   cargo build --release "${REPRO_CARGO_ARGS[@]}"
#
# It does three things (tools/check-image-identity.sh shows the difference with --plain):
#   - SOURCE_DATE_EPOCH: the commit's time, unless already set. esp-bootloader-esp-idf puts
#     the build time and date in every app descriptor; with this set they are the commit's.
#   - REPRO_CARGO_ARGS: a `--config` that adds `--remap-path-prefix` for the home directory
#     (-> /home), the rustup home (-> /rustup), the cargo home (-> /cargo) and this checkout
#     (-> /rotorscope) to the rustflags the firmware already has. Panic messages name source
#     files, and the debug info names the directory rustc ran in; for crates outside the
#     firmware's own workspace (crates/*, the registry) rustc uses absolute paths, which differ
#     between checkouts and name the builder's home directory. Cargo joins this array onto the
#     one in .cargo/config.toml; it goes under `target.<triple>.rustflags` when the firmware's
#     config sets rustflags there (which replaces `build.rustflags`), else under
#     `build.rustflags`. Rustflags do not change symbol names (measured 2026-11-19, cargo
#     1.99), so the remapped paths themselves do not make images differ.
#   - With REPRO_LINK set (see repro_link below): the same remap for that path, and a
#     `--manifest-path` through it.
#
# The checkout's directory: cargo hashes the absolute location of every path dependency
# outside the firmware's own workspace (crates/*) into symbol names (-C metadata), and the
# linker then lays the image out differently. Remapping cannot undo that. What does: build
# through one fixed path, the same on every machine. `repro_link <checkout>` (defined below)
# points REPRO_FIXED (/tmp/rotorscope-repro) at the checkout, holding a lock so only one build
# uses it at a time, and sets REPRO_LINK; cargo is then given the manifest through that path
# (`--manifest-path`, which cargo does not resolve to the real directory; a `cd` through the
# link would be). tools/release-build.sh and tools/check-image-identity.sh build that way;
# images built so are identical whatever directory the checkout is in.
#
# Argument: the target triple being built (default riscv32imac-unknown-none-elf).
# Environment: REPRO_ROOT, the checkout being built (default: the one holding this script).

REPRO_FIXED=/tmp/rotorscope-repro

# repro_args [triple]: (re)compute REPRO_CARGO_ARGS for the current directory.
repro_args() {
  local triple=${1:-riscv32imac-unknown-none-elf} root key=build.rustflags dir cfg flags rel
  root=$(cd -P "${REPRO_ROOT:-$(dirname "${BASH_SOURCE[0]}")/..}" && pwd)
  # Where this firmware's config keeps its rustflags for the triple: the nearest
  # .cargo/config.toml (from here up to the repository root) with a [target.<triple>] table
  # holding rustflags wins; otherwise build.rustflags.
  dir=$(pwd -P)
  while :; do
    cfg=$dir/.cargo/config.toml
    if [[ -f $cfg ]] && awk -v t="[target.$triple]" '
        /^\[/ { in_t = ($0 == t) } in_t && /^rustflags[[:space:]]*=/ { found = 1 } END { exit !found }' "$cfg"; then
      key="target.$triple.rustflags"
      break
    fi
    [[ $dir == "$root" || $dir == / ]] && break
    dir=$(dirname "$dir")
  done
  # The last prefix that matches wins, so the most specific come last.
  flags="\"--remap-path-prefix=$HOME=/home\""
  flags+=", \"--remap-path-prefix=${RUSTUP_HOME:-$HOME/.rustup}=/rustup\""
  flags+=", \"--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo\""
  flags+=", \"--remap-path-prefix=$root=/rotorscope\""
  [[ -n ${REPRO_LINK:-} ]] && flags+=", \"--remap-path-prefix=$REPRO_LINK=/rotorscope\""
  REPRO_CARGO_ARGS=(--config "$key=[$flags]")
  if [[ -n ${REPRO_LINK:-} ]]; then
    # The manifest cargo would find from here (the first Cargo.toml up the tree), reached
    # through the link.
    dir=$(pwd -P)
    while [[ ! -f $dir/Cargo.toml && $dir != "$root" && $dir != / ]]; do dir=$(dirname "$dir"); done
    rel=${dir#"$root"}
    REPRO_CARGO_ARGS+=(--manifest-path "$REPRO_LINK$rel/Cargo.toml")
  fi
}

# repro_link <checkout>: point REPRO_FIXED at <checkout> and set REPRO_LINK. Holds a lock
# (REPRO_FIXED.lock, on file descriptor 8) for as long as the calling shell lives; waits up to
# an hour for another build to finish. Refuses a link it did not make (owned by another user):
# /tmp is shared, and the link decides what source is built.
repro_link() {
  local target=$1 fixed=${REPRO_FIXED_OVERRIDE:-$REPRO_FIXED}
  command -v flock >/dev/null || { echo "repro-env: needs flock (util-linux)" >&2; return 1; }
  exec 8>>"$fixed.lock" || { echo "repro-env: cannot open $fixed.lock" >&2; return 1; }
  flock -w 3600 8 || { echo "repro-env: $fixed is in use by another build (lock $fixed.lock)" >&2; return 1; }
  if [[ -e $fixed || -L $fixed ]]; then
    [[ -L $fixed && -O $fixed ]] || { echo "repro-env: $fixed exists and is not a link of yours; remove it (or ask whoever made it)" >&2; return 1; }
    rm -f "$fixed"
  fi
  ln -s "$target" "$fixed"
  [[ -L $fixed && -O $fixed && $(readlink "$fixed") == "$target" ]] || { echo "repro-env: could not make $fixed point at $target" >&2; return 1; }
  REPRO_LINK=$fixed
  REPRO_ROOT=$target
  repro_args "${REPRO_TRIPLE:-riscv32imac-unknown-none-elf}"
}

# repro_leaks <file>...: report (by kind, never the value) the builder's home directory path,
# user name or host name, or any path in the array REPRO_LEAK_PATHS, found in the files; 1 when
# any is. User and host name are matched as whole words of 3 or more characters;
# ROTORSCOPE_RELEASE_IGNORE_NAMES=1 skips them (for a name that is a common word).
REPRO_LEAK_PATHS=()
repro_leaks() {
  local f p bad=0 u h
  u=$(id -un 2>/dev/null || echo "${USER:-}")
  h=$(hostname 2>/dev/null || uname -n)
  for f; do
    if (( ${#HOME} >= 4 )) && grep -aqF -- "$HOME" "$f"; then echo "$(basename "$f"): holds the builder's home directory path"; bad=1; fi
    if [[ -z ${ROTORSCOPE_RELEASE_IGNORE_NAMES:-} ]]; then
      if (( ${#u} >= 3 )) && grep -aqwF -- "$u" "$f"; then echo "$(basename "$f"): holds the builder's user name"; bad=1; fi
      if (( ${#h} >= 3 )) && grep -aqwF -- "$h" "$f"; then echo "$(basename "$f"): holds the builder's host name"; bad=1; fi
    fi
    for p in "${REPRO_LEAK_PATHS[@]}"; do
      if [[ -n $p ]] && grep -aqF -- "$p" "$f"; then echo "$(basename "$f"): holds a build folder path"; bad=1; fi
    done
  done
  return $bad
}

if [[ -z ${SOURCE_DATE_EPOCH:-} ]]; then
  SOURCE_DATE_EPOCH=$(git -C "${REPRO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}" log -1 --format=%ct 2>/dev/null || echo 0)
fi
export SOURCE_DATE_EPOCH
REPRO_TRIPLE=${1:-riscv32imac-unknown-none-elf}
repro_args "$REPRO_TRIPLE"
