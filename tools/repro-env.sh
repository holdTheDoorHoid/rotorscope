# Source this (bash) in the firmware directory you are about to build in, to make the image
# independent of where and when it is built:
#
#   cd firmware/gateway && . ../../tools/repro-env.sh riscv32imac-unknown-none-elf
#   cargo build --release "${REPRO_CARGO_ARGS[@]}"
#
# It does two things (tools/check-image-identity.sh shows the difference with --plain):
#   - SOURCE_DATE_EPOCH: the commit's time, unless already set. esp-bootloader-esp-idf puts
#     the build time and date in every app descriptor; with this set they are the commit's.
#   - REPRO_CARGO_ARGS: a `--config` that adds `--remap-path-prefix` for this checkout
#     (-> /rotorscope), the cargo home (-> /cargo) and the rustup home (-> /rustup) to the
#     rustflags the firmware already has. Panic messages name source files; for crates
#     outside the firmware's own workspace (crates/*, the registry, build-std's core) rustc
#     uses absolute paths, which differ between checkouts and name the builder's home
#     directory. Cargo joins this array onto the one in .cargo/config.toml; it goes under
#     `target.<triple>.rustflags` when the firmware's config sets rustflags there (which
#     replaces `build.rustflags`), else under `build.rustflags`.
#
# Argument: the target triple being built (default riscv32imac-unknown-none-elf).

_repro_triple=${1:-riscv32imac-unknown-none-elf}
_repro_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
if [[ -z ${SOURCE_DATE_EPOCH:-} ]]; then
  SOURCE_DATE_EPOCH=$(git -C "$_repro_root" log -1 --format=%ct 2>/dev/null || echo 0)
fi
export SOURCE_DATE_EPOCH

# Where this firmware's config keeps its rustflags for the triple: the nearest
# .cargo/config.toml (from here up to the repository root) with a [target.<triple>] table
# holding rustflags wins; otherwise build.rustflags.
_repro_key=build.rustflags
_repro_dir=$PWD
while :; do
  _repro_cfg=$_repro_dir/.cargo/config.toml
  if [[ -f $_repro_cfg ]] && awk -v t="[target.$_repro_triple]" '
      /^\[/ { in_t = ($0 == t) } in_t && /^rustflags[[:space:]]*=/ { found = 1 } END { exit !found }' "$_repro_cfg"; then
    _repro_key="target.$_repro_triple.rustflags"
    break
  fi
  [[ $_repro_dir == "$_repro_root" || $_repro_dir == / ]] && break
  _repro_dir=$(dirname "$_repro_dir")
done

_repro_cargo=${CARGO_HOME:-$HOME/.cargo}
_repro_rustup=${RUSTUP_HOME:-$HOME/.rustup}
REPRO_CARGO_ARGS=(--config "$_repro_key=[\"--remap-path-prefix=$_repro_root=/rotorscope\", \"--remap-path-prefix=$_repro_cargo=/cargo\", \"--remap-path-prefix=$_repro_rustup=/rustup\"]")
unset _repro_triple _repro_root _repro_key _repro_dir _repro_cfg _repro_cargo _repro_rustup
