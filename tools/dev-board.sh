#!/usr/bin/env bash
# Flash an ELF to a dev board and capture its serial output, holding a lock so
# parallel agents take turns. The boards are not in `lab`, so this lock is the
# only thing stopping two flashes from colliding.
#
#   tools/dev-board.sh <elf> [seconds=15] [board=s3]
#   tools/dev-board.sh --log [seconds=15] [board=s3]    # reset + read, no flash
#
# Boards are addressed by their USB by-id path (MAC), never by /dev/ttyACMn.
set -euo pipefail
declare -A BOARDS=(
  [s3]=/dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_E8:F6:0A:81:6D:F0-if00
)
if [[ ${1:-} == --log ]]; then elf=""; shift; else elf=${1:?usage: dev-board.sh <elf>|--log [seconds] [board]}; shift; fi
secs=${1:-15}; board=${2:-s3}
port=${BOARDS[$board]:?unknown board $board (known: ${!BOARDS[*]})}
lock=/tmp/rotorscope-board-$board.lock
chip=$( [[ $board == c5* ]] && echo esp32c5 || echo esp32s3 )

exec 9>"$lock"
if ! flock -w 600 9; then echo "board $board busy for 10 min; giving up" >&2; exit 1; fi
if [[ -n $elf ]]; then
  espflash flash --chip "$chip" --port "$port" --non-interactive "$elf" 2>&1 | tail -3
fi
python3 -I - "$port" "$secs" <<'PY'
import sys, time, serial
port, secs = sys.argv[1], float(sys.argv[2])
s = serial.Serial(port, 115200, timeout=0.2)
s.dtr = False; s.rts = True; time.sleep(0.1); s.rts = False   # USB-JTAG hard reset
end = time.time() + secs
while time.time() < end:
    b = s.read(4096)
    if b: sys.stdout.write(b.decode(errors="replace")); sys.stdout.flush()
PY
