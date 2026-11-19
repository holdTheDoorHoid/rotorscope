#!/usr/bin/env bash
# Lockfile checks that CI runs (.github/workflows/ci.yml), the same on your machine.
#
#   tools/ci-lockfiles.sh list      every tracked Cargo.lock is in the list below
#   tools/ci-lockfiles.sh resolve   each lockfile is current (`cargo metadata --locked`)
#   tools/ci-lockfiles.sh deny      cargo-deny (deny.toml) on each lockfile
#
# `resolve` and `deny` need firmware/hub-p4's patched esp-hal: run tools/p4-prepare.sh
# once first. Neither needs Espressif's toolchain: both use stock stable Rust
# (RUSTUP_TOOLCHAIN=stable unless you set it), because resolving dependencies
# compiles nothing. `resolve` and `deny` download crates (and, for `deny`, the
# RustSec advisory database) the first time.
#
# Exit 0: all good. Exit 1: a check failed (each failure says which lockfile).
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"
export RUSTUP_TOOLCHAIN=${RUSTUP_TOOLCHAIN:-stable}

# Every directory with a Cargo.lock.
DIRS=(
  .
  firmware/gateway
  firmware/hub
  firmware/hub-p4
  firmware/node-ble
  firmware/node-lr
  firmware/node-sdr
  firmware/node-wifi
  firmware/rid-beacon
)

need_p4() {
  [[ -d firmware/patches/esp-hal-p4-pre-v3/upstream ]] || {
    echo "firmware/hub-p4 needs its patched esp-hal: run tools/p4-prepare.sh first" >&2
    exit 1
  }
}

case ${1:-} in
  list)
    fails=0
    while IFS= read -r lock; do
      d=$(dirname "$lock")
      if [[ " ${DIRS[*]} " != *" $d "* ]]; then
        echo "FAIL $lock is not listed in tools/ci-lockfiles.sh, so CI does not check it" >&2
        fails=$((fails + 1))
      fi
    done < <(git ls-files -- 'Cargo.lock' '*/Cargo.lock')
    for d in "${DIRS[@]}"; do
      [[ -f $d/Cargo.lock ]] || { echo "FAIL $d/Cargo.lock is listed but missing" >&2; fails=$((fails + 1)); }
    done
    (( fails == 0 )) || exit 1
    echo "ci-lockfiles: ${#DIRS[@]} lockfiles, all listed"
    ;;
  resolve)
    need_p4
    fails=0
    for d in "${DIRS[@]}"; do
      if (cd "$d" && cargo metadata --locked --all-features --format-version 1 >/dev/null); then
        echo "ok   $d/Cargo.lock is current"
      else
        echo "FAIL $d/Cargo.lock is out of date with its Cargo.toml: run 'cargo update --workspace' in $d and commit the lockfile" >&2
        fails=$((fails + 1))
      fi
    done
    (( fails == 0 )) || { echo "ci-lockfiles resolve: $fails FAILED" >&2; exit 1; }
    echo "ci-lockfiles resolve: all current"
    ;;
  deny)
    need_p4
    command -v cargo-deny >/dev/null || { echo "cargo-deny is not installed (docs/dev/build-test-flash.md, 'What CI checks')" >&2; exit 1; }
    fails=0
    for d in "${DIRS[@]}"; do
      echo "== cargo deny: $d"
      if ! cargo deny --manifest-path "$d/Cargo.toml" --config "$root/deny.toml" --locked \
          check -W unmaintained; then
        echo "FAIL cargo deny: $d" >&2
        fails=$((fails + 1))
      fi
    done
    (( fails == 0 )) || { echo "ci-lockfiles deny: $fails FAILED" >&2; exit 1; }
    echo "ci-lockfiles deny: all passed"
    ;;
  *)
    echo "usage: tools/ci-lockfiles.sh list|resolve|deny" >&2
    exit 2
    ;;
esac
