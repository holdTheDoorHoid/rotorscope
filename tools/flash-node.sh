#!/usr/bin/env bash
# Build and flash a multi-image radio node (XIAO ESP32-C5) through the board lock.
#
#   tools/flash-node.sh [--open] <board> [seconds=20]
#   tools/flash-node.sh --self-test        checks of this script's decisions (no board)
#
# Slots (tools/partitions-node.csv):
#   ota_0  node-wifi  (ESP-NOW, boots as Wi-Fi 2.4 GHz; the hub picks the band)
#   ota_1  node-ble   (ESP-NOW)
#   ota_2  node-sdr   only if its ELF exists (NODE_SDR_ELF, default the ESP-NOW
#                     build firmware/node-sdr/target-espnow/riscv32imac-unknown-none-elf/release/node-sdr,
#                     made with `cd firmware/node-sdr/c5 && cargo fw-now`);
#                     this script never builds it. Absent: the slot is erased and the
#                     node does not offer DroneID / FPV.
# otadata and rsjob are erased, so the board starts node-wifi with its built-in plan.
# rskey (the node's link key record) is never erased or written here: the same
# table keeps it where `tools/link-key node <board>` put it.
# Then it reads the board's USB for [seconds].
#
# Wired (UART) nodes: set NODE_WIRED=1 to build both images without `espnow`.
#
# Wired-bus nodes (many nodes on one hub UART, docs/WIREBUS.md): set NODE_BUS=1
# to build both images with `wirebus` (TX open-drain on D9, RX on D7). The
# node-sdr slot then defaults to its bus build,
# firmware/node-sdr/target-bus/riscv32imac-unknown-none-elf/release/node-sdr
# (build it with `cd firmware/node-sdr/c5 && cargo fw-bus`). Every image on a
# bus node must be a bus build: an image that is not would talk over the
# other nodes when it boots.
#
# Image kinds must match the node: an ESP-NOW node takes only images that run
# the version 3 link (they carry its boot line, `"link":"v3"`); a wired or bus
# node takes none of those (an ESP-NOW image would transmit). This is what
# stops a wired node-sdr ELF from landing in an ESP-NOW node's slot.
#
# Link keys (docs/LINK-KEYS-DESIGN.md 4.4, 4.5): images carry no key and are
# the same for every kit (no build reads a key file or a key variable). Each
# board's key is in its rskey partition, put
# there by `tools/link-key node <board>` (or an open record by `link-key open
# <board>`). After an ESP-NOW flash this script reads the boot line and calls
# the flash good (exit 0) only if node-wifi says it is keyed
# (`{"t":"uplink","auth":"kit:...`); with `--open`, an open record is good too.
# Otherwise it exits 1 and says what the board reported (missing, damaged,
# wrong_board, no_rng, open, or no boot line at all).
#
# Bench builds (docs/BENCH.md "Bench builds"): an image built with a
# bench-only feature or switch (faulttest, bringup, ROTORSCOPE_BENCH_CAN, ...)
# is refused unless ROTORSCOPE_BENCH=1 is set, which the build itself needs
# anyway; e.g. `ROTORSCOPE_BENCH=1 ROTORSCOPE_BENCH_CAN=ble tools/flash-node.sh c5a`.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(dirname "$here")

# The marker of an image that runs the version 3 node link (nodeimage::uplink's boot line).
V3_MARK='"link":"v3"'

# check_images <espnow|wired|bus> <elf>...: refuse a set of images that does
# not fit the node. Prints why and returns 1.
check_images() {
  local kind=$1 f; shift
  for f in "$@"; do
    case $kind in
      espnow)
        if ! grep -aqF "$V3_MARK" "$f"; then
          echo "flash-node: $f is not an ESP-NOW (version 3 link) build: refusing it for an ESP-NOW node" >&2
          echo "flash-node: (a node-sdr ELF: build it with 'cd firmware/node-sdr/c5 && cargo fw-now' and pass NODE_SDR_ELF)" >&2
          return 1
        fi ;;
      wired|bus)
        if grep -aqF "$V3_MARK" "$f"; then
          echo "flash-node: $f is an ESP-NOW build: refusing it for a $kind node (it would transmit)" >&2
          return 1
        fi
        if [[ $kind == bus ]] && ! grep -aq '"t":"bus","addr"' "$f"; then
          echo "flash-node: $f is not a wired-bus build (NODE_BUS=1 needs every image built with wirebus)" >&2
          return 1
        fi ;;
    esac
  done
}

# check_bench <elf>...: no bench image into a node unless the caller says so
# (the hello line and the boot warning carry the list; the ELF carries
# "bench-build:<list>").
check_bench() {
  local f b
  for f in "$@"; do
    b=$(grep -aoE 'bench-build:[A-Za-z0-9_,:-]+' "$f" 2>/dev/null | grep -v '^bench-build:none$' | head -1 || true)
    if [[ -n $b ]]; then
      if [[ ${ROTORSCOPE_BENCH:-} != 1 ]]; then
        echo "flash-node: $(basename "$f") is a BENCH build (${b#bench-build:}); refusing without ROTORSCOPE_BENCH=1 (docs/BENCH.md \"Bench builds\")" >&2
        return 1
      fi
      echo "flash-node: WARNING: $(basename "$f") is a BENCH build (${b#bench-build:}): never leave it in a fielded node" >&2
    fi
  done
}

# boot_verdict <log> <open>: what the board said about its link. Exit 0 when
# the flash is good: keyed, or open when <open> is 1.
boot_verdict() {
  local log=$1 open=$2 why
  if grep -aq '"t":"uplink","auth":"kit:' "$log"; then
    echo "flash-node: link keyed: $(grep -aoE '"auth":"kit:[^"]*"' "$log" | head -1)"
    return 0
  fi
  if grep -aq '"what":"link_key","auth":"open"' "$log"; then
    if (( open )); then
      echo "flash-node: link OPEN (open provisioning record), as --open asked"
      return 0
    fi
    echo "flash-node: the board has an OPEN provisioning record: its link is not authenticated." >&2
    echo "flash-node: seal it with 'tools/link-key node <board>', or pass --open if an open kit is meant." >&2
    return 1
  fi
  why=$(grep -aoE '"what":"link_key","auth":"none","why":"[a-z_]+"' "$log" | head -1 | sed 's/.*"why":"//; s/"$//' || true)
  if [[ -n $why ]]; then
    echo "flash-node: the node sends NO reports: its link key record is '$why'." >&2
    case $why in
      missing) echo "flash-node: provision it: tools/link-key node <board> (tools/link-key open <board> for an open kit)" >&2 ;;
      damaged) echo "flash-node: provision it again: tools/link-key node <board> --slot <n>" >&2 ;;
      wrong_board) echo "flash-node: the record was made for another board: tools/link-key node <board>" >&2 ;;
      no_rng) echo "flash-node: the hardware random source did not start: keyed link refused (see the boot lines)" >&2 ;;
    esac
    return 1
  fi
  echo "flash-node: no link boot line seen: cannot call this flash good (read longer: tools/flash-node.sh <board> 40)" >&2
  return 1
}

self_test() {
  local t fails=0
  set +e  # the checks look at failures
  t=$(mktemp -d); trap 'rm -rf "$t"' RETURN
  check() { if [[ $2 == 0 ]]; then echo "ok   $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi; }
  printf 'abc ","link":"v3"} xyz' >"$t/v3.elf"
  printf 'abc "t":"bus","addr" xyz' >"$t/bus.elf"
  printf 'abc xyz' >"$t/wired.elf"
  printf '%s bench-build:faulttest' "$V3_MARK" >"$t/bench.elf"
  check_images espnow "$t/v3.elf" 2>/dev/null; check "espnow node takes a v3 image" $?
  check_images espnow "$t/v3.elf" "$t/wired.elf" 2>/dev/null; check "espnow node refuses a wired node-sdr image" $(( ! $? ))
  check_images wired "$t/wired.elf" 2>/dev/null; check "wired node takes a wired image" $?
  check_images wired "$t/v3.elf" 2>/dev/null; check "wired node refuses an ESP-NOW image" $(( ! $? ))
  check_images bus "$t/bus.elf" 2>/dev/null; check "bus node takes a bus image" $?
  check_images bus "$t/wired.elf" 2>/dev/null; check "bus node refuses a plain wired image" $(( ! $? ))
  ROTORSCOPE_BENCH= check_bench "$t/bench.elf" 2>/dev/null; check "bench image refused without ROTORSCOPE_BENCH=1" $(( ! $? ))
  ROTORSCOPE_BENCH=1 check_bench "$t/bench.elf" 2>/dev/null; check "bench image taken with ROTORSCOPE_BENCH=1" $?
  [[ $(sdr_default espnow) == */target-espnow/* ]]; check "node-sdr slot defaults to the ESP-NOW build" $?
  [[ $(sdr_default bus) == */target-bus/* ]]; check "bus node-sdr default unchanged" $?
  printf '{"t":"hello"}\n{"t":"uplink","auth":"kit:0a1b2c3d slot:2 gen:1 check:44556677","rng":"radio","link":"v3"}\n' >"$t/keyed.log"
  printf '{"t":"warn","what":"link_key","auth":"open","rng":"radio","link":"v1","note":"x"}\n' >"$t/open.log"
  printf '{"t":"warn","what":"link_key","auth":"none","why":"wrong_board","rng":"radio"}\n' >"$t/wrong.log"
  printf '{"t":"hello"}\n' >"$t/none.log"
  boot_verdict "$t/keyed.log" 0 >/dev/null 2>&1; check "keyed boot line: good" $?
  boot_verdict "$t/open.log" 0 >/dev/null 2>&1; check "open record without --open: refused" $(( ! $? ))
  boot_verdict "$t/open.log" 1 >/dev/null 2>&1; check "open record with --open: good" $?
  [[ $(boot_verdict "$t/wrong.log" 0 2>&1) == *"'wrong_board'"* ]]; check "wrong_board: refused, named" $?
  boot_verdict "$t/none.log" 1 >/dev/null 2>&1; check "no boot line: refused" $(( ! $? ))
  # The flash goes through dev-board.sh, which never erases rskey.
  "$here/dev-board.sh" --self-test >/dev/null 2>&1; check "dev-board.sh self-test (rskey never erased)" $?
  (( fails == 0 )) && echo "flash-node self-test: all passed" || echo "flash-node self-test: $fails FAILED"
  return $(( fails > 0 ))
}

# The node-sdr ELF used when NODE_SDR_ELF is not set.
sdr_default() {
  case $1 in
    bus) echo "$root/firmware/node-sdr/target-bus/riscv32imac-unknown-none-elf/release/node-sdr" ;;
    wired) echo "$root/firmware/node-sdr/target/riscv32imac-unknown-none-elf/release/node-sdr" ;;
    *) echo "$root/firmware/node-sdr/target-espnow/riscv32imac-unknown-none-elf/release/node-sdr" ;;
  esac
}

if [[ ${1:-} == --self-test ]]; then self_test; exit $?; fi

open=0
if [[ ${1:-} == --open ]]; then open=1; shift; fi
board=${1:?usage: flash-node.sh [--open] <board> [seconds]}
secs=${2:-20}
pt=$here/partitions-node.csv

if [[ ${NODE_BUS:-0} == 1 && ${NODE_WIRED:-0} == 1 ]]; then
  echo "flash-node: NODE_BUS and NODE_WIRED are two different wirings; set one" >&2
  exit 1
fi
kind=espnow
[[ ${NODE_WIRED:-0} == 1 ]] && kind=wired
[[ ${NODE_BUS:-0} == 1 ]] && kind=bus

chip=$("$here/dev-board.sh" --resolve "$board" | awk '{print $2}')
[[ $chip == esp32c5 ]] || { echo "$board is $chip; multi-image nodes are ESP32-C5 only" >&2; exit 1; }

# Images that do not depend on where or when they were built (the commit's
# time, checkout paths remapped): tools/repro-env.sh, tools/check-image-identity.sh.
repro() { (cd "$1" && . "$here/repro-env.sh" riscv32imac-unknown-none-elf && shift && "$@" "${REPRO_CARGO_ARGS[@]}"); }

case $kind in
  bus)
    repro "$root/firmware/node-wifi/c5" cargo fw24-bus
    wifi=$root/firmware/node-wifi/target-bus/riscv32imac-unknown-none-elf/release/node-wifi
    ble_features=esp32c5,wirebus ;;
  wired)
    repro "$root/firmware/node-wifi/c5" cargo fw24
    wifi=$root/firmware/node-wifi/target-band24/riscv32imac-unknown-none-elf/release/node-wifi
    ble_features=esp32c5 ;;
  *)
    repro "$root/firmware/node-wifi/c5" cargo fw24-now
    wifi=$root/firmware/node-wifi/target-espnow/riscv32imac-unknown-none-elf/release/node-wifi
    ble_features=esp32c5,espnow ;;
esac
repro "$root/firmware/node-ble" cargo +stable build --release --no-default-features \
   --features "$ble_features" --target riscv32imac-unknown-none-elf
ble=$root/firmware/node-ble/target/riscv32imac-unknown-none-elf/release/node-ble

apps=(--app "ota_0=$wifi" --app "ota_1=$ble")
sdr=${NODE_SDR_ELF:-$(sdr_default "$kind")}
images=("$wifi" "$ble")
if [[ -f $sdr ]]; then
  apps+=(--app "ota_2=$sdr")
  images+=("$sdr")
else
  echo "flash-node: no node-sdr ELF at $sdr; slot ota_2 erased (the node will not offer DroneID / FPV)"
  apps+=(--erase ota_2)
fi

check_images "$kind" "${images[@]}" || exit 1
check_bench "${images[@]}" || exit 1

if [[ $kind != espnow ]]; then
  exec "$here/dev-board.sh" --pt "$pt" "${apps[@]}" --multi "$secs" "$board"
fi

# ESP-NOW: flash, read, and judge the boot line.
logdir=$(mktemp -d); trap 'rm -rf "$logdir"' EXIT
"$here/dev-board.sh" --pt "$pt" "${apps[@]}" --multi "$secs" "$board" | tee "$logdir/boot.log"
boot_verdict "$logdir/boot.log" "$open"
