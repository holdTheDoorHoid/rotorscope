#!/usr/bin/env bash
# Build and flash a multi-image radio node (XIAO ESP32-C5) through the board lock.
#
#   tools/flash-node.sh <board> [seconds=20]
#
# Slots (tools/partitions-node.csv):
#   ota_0  node-wifi  (ESP-NOW, boots as Wi-Fi 2.4 GHz; the hub picks the band)
#   ota_1  node-ble   (ESP-NOW)
#   ota_2  node-sdr   only if its ELF exists (NODE_SDR_ELF, default
#                     firmware/node-sdr/target/riscv32imac-unknown-none-elf/release/node-sdr);
#                     this script never builds it. Absent: the slot is erased and the
#                     node does not offer DroneID / FPV.
# otadata and rsjob are erased, so the board starts node-wifi with its built-in plan.
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
# ESP-NOW link key (docs/ESPNOW.md "Securing the link"): the images are built
# with the kit key in tools/link.key (or $ROTORSCOPE_LINK_KEY_FILE; "none" for
# an open build). Every image on the board must carry the same key as the
# gateway; this script refuses to flash a board whose images disagree (e.g. a
# node-sdr ELF built before the key was made).
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(dirname "$here")
board=${1:?usage: flash-node.sh <board> [seconds]}
secs=${2:-20}
pt=$here/partitions-node.csv

keyfile=${ROTORSCOPE_LINK_KEY_FILE:-$here/link.key}
if [[ $keyfile != none && -f $keyfile ]]; then
  export ROTORSCOPE_LINK_KEY_FILE=$keyfile
  echo "flash-node: building with the link key in $keyfile"
else
  export ROTORSCOPE_LINK_KEY_FILE=none
  echo "flash-node: WARNING: no link key ($here/link.key): the ESP-NOW link will NOT be authenticated." >&2
  echo "flash-node:          Make one with: cargo run -p link-key -- new  (then reflash the gateway and every node)" >&2
fi

chip=$("$here/dev-board.sh" --resolve "$board" | awk '{print $2}')
[[ $chip == esp32c5 ]] || { echo "$board is $chip; multi-image nodes are ESP32-C5 only" >&2; exit 1; }

if [[ ${NODE_BUS:-0} == 1 && ${NODE_WIRED:-0} == 1 ]]; then
  echo "flash-node: NODE_BUS and NODE_WIRED are two different wirings; set one" >&2
  exit 1
fi
if [[ ${NODE_BUS:-0} == 1 ]]; then
  (cd "$root/firmware/node-wifi/c5" && cargo fw24-bus)
  wifi=$root/firmware/node-wifi/target-bus/riscv32imac-unknown-none-elf/release/node-wifi
  ble_features=esp32c5,wirebus
elif [[ ${NODE_WIRED:-0} == 1 ]]; then
  (cd "$root/firmware/node-wifi/c5" && cargo fw24)
  wifi=$root/firmware/node-wifi/target-band24/riscv32imac-unknown-none-elf/release/node-wifi
  ble_features=esp32c5
else
  (cd "$root/firmware/node-wifi/c5" && cargo fw24-now)
  wifi=$root/firmware/node-wifi/target-espnow/riscv32imac-unknown-none-elf/release/node-wifi
  ble_features=esp32c5,espnow
fi
(cd "$root/firmware/node-ble" && cargo +stable build --release --no-default-features \
   --features "$ble_features" --target riscv32imac-unknown-none-elf)
ble=$root/firmware/node-ble/target/riscv32imac-unknown-none-elf/release/node-ble

apps=(--app "ota_0=$wifi" --app "ota_1=$ble")
if [[ ${NODE_BUS:-0} == 1 ]]; then
  sdr=${NODE_SDR_ELF:-$root/firmware/node-sdr/target-bus/riscv32imac-unknown-none-elf/release/node-sdr}
else
  sdr=${NODE_SDR_ELF:-$root/firmware/node-sdr/target/riscv32imac-unknown-none-elf/release/node-sdr}
fi
images=("$wifi" "$ble")
if [[ -f $sdr ]]; then
  apps+=(--app "ota_2=$sdr")
  images+=("$sdr")
else
  echo "flash-node: no node-sdr ELF at $sdr; slot ota_2 erased (the node will not offer DroneID / FPV)"
  apps+=(--erase ota_2)
fi

# A bus node must run bus images only (an image that is not would drive the
# shared wire on its own): the bus images print "t":"bus" status lines.
if [[ ${NODE_BUS:-0} == 1 ]]; then
  for f in "${images[@]}"; do
    if ! grep -aq '"t":"bus","addr"' "$f"; then
      echo "flash-node: $f is not a wired-bus build (NODE_BUS=1 needs every image built with wirebus)" >&2
      exit 1
    fi
  done
fi

# Same link key in every ESP-NOW image (wired images carry none and are skipped).
keys=()
for f in "${images[@]}"; do
  k=$(grep -aoE 'link-key:(none|[0-9a-f]{8})' "$f" 2>/dev/null | head -1 | cut -d: -f2 || true)
  [[ -n $k ]] && keys+=("$(basename "$f")=$k")
done
if (( ${#keys[@]} )) && [[ $(printf '%s\n' "${keys[@]}" | cut -d= -f2 | sort -u | wc -l) -gt 1 ]]; then
  echo "flash-node: the images carry different link keys: ${keys[*]}" >&2
  echo "flash-node: rebuild the odd one out with the same link key (a node-sdr ELF is not rebuilt by this script: rebuild it and pass NODE_SDR_ELF)" >&2
  exit 1
fi
exec "$here/dev-board.sh" --pt "$pt" "${apps[@]}" --multi "$secs" "$board"
