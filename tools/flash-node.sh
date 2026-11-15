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
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(dirname "$here")
board=${1:?usage: flash-node.sh <board> [seconds]}
secs=${2:-20}
pt=$here/partitions-node.csv

chip=$("$here/dev-board.sh" --resolve "$board" | awk '{print $2}')
[[ $chip == esp32c5 ]] || { echo "$board is $chip; multi-image nodes are ESP32-C5 only" >&2; exit 1; }

if [[ ${NODE_WIRED:-0} == 1 ]]; then
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
sdr=${NODE_SDR_ELF:-$root/firmware/node-sdr/target/riscv32imac-unknown-none-elf/release/node-sdr}
if [[ -f $sdr ]]; then
  apps+=(--app "ota_2=$sdr")
else
  echo "flash-node: no node-sdr ELF at $sdr; slot ota_2 erased (the node will not offer DroneID / FPV)"
  apps+=(--erase ota_2)
fi
exec "$here/dev-board.sh" --pt "$pt" "${apps[@]}" --multi "$secs" "$board"
