#!/usr/bin/env bash
# Flash an ELF to a dev board and capture its serial output, holding a per-board
# lock so parallel agents (and the two halves of a bench test) take turns. The
# boards are not in `lab`, so this lock is the only thing stopping two flashes
# from colliding.
#
#   tools/dev-board.sh <elf> [seconds=15] [board=s3]   flash, reset, read
#   tools/dev-board.sh --log [seconds=15] [board=s3]   reset + read, no flash
#   tools/dev-board.sh --register [--no-probe]          find attached boards, name them,
#                                                       write tools/boards.local
#   tools/dev-board.sh --list                           registered boards: attached? busy?
#   tools/dev-board.sh --resolve <board>                print "<port> <chip>" and exit
#   tools/dev-board.sh --hold <seconds> [board]         take the lock and sit on it
#   tools/dev-board.sh --pt <csv> --app <label>=<elf> [--app ...] --multi [seconds] [board]
#                                                       multi-image flash (tools/flash-node.sh):
#                                                       partition table, otadata + rsjob (and any
#                                                       --erase LABEL) erased so it boots ota_0,
#                                                       each ELF into its slot
#
# Options (before the positional arguments):
#   --send KEYS        after the reset, type these characters on the board's console
#                      (rid-beacon's one-key commands, e.g. "xw"). Default delay 8 s.
#   --send-after SECS  how long after the reset to start typing (default 8)
#   --run CMD          instead of resetting and reading: run CMD (bash -c) with the lock
#                      held and BOARD_PORT set to the board's port, for at most [seconds].
#                      No reset. For host tools that talk to the board themselves
#                      (e.g. tools/droneid-hil). Combine with an <elf> to flash first.
#   --dry-run          print what would happen; touch no hardware, take no lock
#
# ESP-NOW link key: before flashing, every ELF that carries a link-key tag
# (gateway, ESP-NOW nodes) is checked against the kit key (tools/link.key, or
# $ROTORSCOPE_LINK_KEY_FILE): a WARNING if it was built without a key or with
# another one (docs/ESPNOW.md "Securing the link"). Only key IDs are printed,
# never a key.
#
# Boards are addressed by their USB by-id path (it contains the MAC), never by
# /dev/ttyACMn and never by a serial number. Names (s3, s3b, c5a, c5b, ...) are
# looked up in tools/boards.local (gitignored; created by --register; override
# with $ROTORSCOPE_BOARDS). The built-in `s3` below is only a fallback.
#
# Locks: one file per board, keyed on the by-id path (so two names for one board
# share a lock), in $ROTORSCOPE_LOCK_DIR (default ~/.local/state/rotorscope/locks),
# NOT in /tmp: sandboxed agents can have a private /tmp, which gives each of them
# its own "shared" lock (see docs/BENCH.md, "How the lock works").
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
boards_file=${ROTORSCOPE_BOARDS:-$here/boards.local}
lock_dir=${ROTORSCOPE_LOCK_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/rotorscope/locks}

declare -A BOARDS=(
  [s3]=/dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_E8:F6:0A:81:6D:F0-if00
)
declare -A CHIPS=()
if [[ -f $boards_file ]]; then
  while read -r bname bpath bchip _; do
    [[ -z ${bname:-} || $bname == \#* ]] && continue
    BOARDS[$bname]=$bpath
    [[ -n ${bchip:-} && $bchip != "?" && $bchip != \#* ]] && CHIPS[$bname]=$bchip
  done <"$boards_file"
fi

chip_of() {  # chip_of <board>
  if [[ -n ${CHIPS[$1]:-} ]]; then echo "${CHIPS[$1]}"
  elif [[ $1 == c5* ]]; then echo esp32c5
  else echo esp32s3; fi
}
lock_name() { local b; b=$(basename "$1"); echo "${b//[^A-Za-z0-9_.:-]/_}.lock"; }

# The link-key tag an ELF was built with ("none" or 8 hex digits), empty if it
# has none (not an ESP-NOW image).
elf_link_key() { grep -aoE 'link-key:(none|[0-9a-f]{8})' "$1" 2>/dev/null | head -1 | cut -d: -f2 || true; }
check_link_keys() {  # check_link_keys <elf>...: report each image's key, warn on a mismatch
  local f k kit="" keyfile=${ROTORSCOPE_LINK_KEY_FILE:-$here/link.key}
  for f in "$@"; do
    [[ -f $f ]] || continue
    k=$(elf_link_key "$f"); [[ -n $k ]] || continue
    if [[ -z $kit ]]; then
      if [[ $keyfile != none && -f $keyfile ]]; then
        kit=$(cd "$here/.." && cargo run -q -p link-key -- id "$keyfile" 2>/dev/null) || kit="?"
      else
        kit=none
      fi
    fi
    if [[ $k == none ]]; then
      echo "dev-board: WARNING: $(basename "$f") was built WITHOUT a link key; its ESP-NOW link is not authenticated" >&2
    else
      echo "dev-board: $(basename "$f"): link key $k"
    fi
    if [[ $kit != "?" && $k != "$kit" ]]; then
      echo "dev-board: WARNING: $(basename "$f") has link key '$k' but the kit key is '$kit' ($keyfile): it will not talk to the rest of the kit; rebuild it" >&2
    fi
  done
}

# ---- sub-commands that do not flash -----------------------------------------
case ${1:-} in
  --resolve)
    b=${2:?usage: --resolve <board>}
    [[ -n ${BOARDS[$b]:-} ]] || { echo "unknown board $b (known: ${!BOARDS[*]})" >&2; exit 1; }
    echo "${BOARDS[$b]} $(chip_of "$b")"; exit 0 ;;
  --list)
    printf '%-6s %-8s %-9s %s\n' NAME CHIP STATE PORT
    for b in $(printf '%s\n' "${!BOARDS[@]}" | sort); do
      p=${BOARDS[$b]}; state=attached; [[ -e $p ]] || state=ABSENT
      if [[ $state == attached ]]; then
        lf=$lock_dir/$(lock_name "$p")
        if [[ -e $lf ]] && ! flock -n "$lf" true 2>/dev/null; then state="BUSY"; fi
      fi
      printf '%-6s %-8s %-9s %s\n' "$b" "$(chip_of "$b")" "$state" "$p"
      if [[ $state == BUSY ]]; then printf '         held by: %s\n' "$(head -c 200 "$lock_dir/$(lock_name "$p")" 2>/dev/null | tr '\n' ' ')"; fi
    done
    exit 0 ;;
  --register)
    shift
    export BOARDS_FILE=$boards_file LOCK_DIR=$lock_dir PROBE=1
    [[ ${1:-} == --no-probe ]] && PROBE=0
    mkdir -p "$lock_dir"
    exec python3 -I - <<'PY'
import glob, os, re, subprocess, sys
bf, lockdir, probe = os.environ["BOARDS_FILE"], os.environ["LOCK_DIR"], os.environ["PROBE"] == "1"
def lock_name(p):  # keep in step with lock_name() in dev-board.sh
    return re.sub(r"[^A-Za-z0-9_.:-]", "_", os.path.basename(p)) + ".lock"
known = {}   # path -> (name, chip)
lines = []
if os.path.exists(bf):
    for ln in open(bf):
        lines.append(ln.rstrip("\n"))
        f = ln.split()
        if len(f) >= 2 and not f[0].startswith("#"):
            known[f[1]] = (f[0], f[2] if len(f) > 2 and f[2] != "?" else "?")
ports = sorted(glob.glob("/dev/serial/by-id/usb-Espressif_*"))
if not ports:
    print("No Espressif boards attached (nothing under /dev/serial/by-id/usb-Espressif_*).")
    print("Plug a board in with a data-capable USB cable and run this again.")
    sys.exit(1)
taken = {n for n, _ in known.values()}
def next_name(chip):
    if chip == "esp32c5":
        for c in "abcdefgh":
            if f"c5{c}" not in taken: return f"c5{c}"
    else:
        for n in ["s3"] + [f"s3{c}" for c in "bcdefgh"]:
            if n not in taken: return n
    return None
new = []
print(f"{'NAME':<6} {'CHIP':<9} {'MAC':<18} PORT")
for p in ports:
    m = re.search(r"_([0-9A-Fa-f]{2}(?::[0-9A-Fa-f]{2}){5})-if", p)
    mac = m.group(1) if m else "?"
    if p in known:
        name, chip = known[p]
        print(f"{name:<6} {chip:<9} {mac:<18} {p}   (already registered)")
        continue
    chip = "?"
    if probe:
        # The chip type is not in the by-id name (S3 and C5 look the same), so ask the
        # board. board-info resets it; hold its lock so nobody is mid-flash.
        os.makedirs(lockdir, exist_ok=True)
        r = subprocess.run(["flock", "-w", "120", os.path.join(lockdir, lock_name(p)),
                            "espflash", "board-info", "--port", p, "--non-interactive"],
                           capture_output=True, text=True)
        m2 = re.search(r"Chip type:\s*(esp32\w*)", r.stdout + r.stderr, re.I)
        if m2: chip = m2.group(1).lower()
        else: print(f"  could not read chip type of {mac}: {(r.stdout + r.stderr).strip()[-200:]}")
    name = next_name(chip) if chip != "?" else None
    if name is None:
        print(f"{'?':<6} {chip:<9} {mac:<18} {p}   (NOT registered: chip unknown; edit {bf} by hand)")
        continue
    taken.add(name); known[p] = (name, chip); new.append((name, p, chip, mac))
    print(f"{name:<6} {chip:<9} {mac:<18} {p}   NEW")
if new:
    with open(bf, "a") as fh:
        if not lines: fh.write("# Local bench boards. name  by-id-path  chip   # mac\n# Edit names freely; paths must stay /dev/serial/by-id/... (never ttyACMn).\n")
        for name, p, chip, mac in new:
            fh.write(f"{name}  {p}  {chip}   # {mac}\n")
    print(f"\nWrote {len(new)} new board(s) to {bf}.")
    print("Put a sticky label with the NAME and MAC on each board now.")
else:
    print("\nNothing new to register.")
absent = [n for pth, (n, _) in known.items() if not os.path.exists(pth)]
if absent: print("Registered but not attached right now:", ", ".join(sorted(absent)))
PY
    ;;
esac

# ---- option parsing -----------------------------------------------------------
send=""; send_after=8; dry=0; hold=""; run=""; pt=""; apps=(); erase=(otadata rsjob); multi=0
while [[ ${1:-} == --* && ${1:-} != --log && ${1:-} != --multi ]]; do
  case $1 in
    --pt) pt=${2:?--pt needs a CSV}; shift 2 ;;
    --app) apps+=("${2:?--app needs LABEL=ELF}"); shift 2 ;;
    --erase) erase+=("${2:?--erase needs a partition label}"); shift 2 ;;
    --send) send=${2:?--send needs KEYS}; shift 2 ;;
    --send-after) send_after=${2:?}; shift 2 ;;
    --dry-run) dry=1; shift ;;
    --hold) hold=${2:?--hold needs seconds}; shift 2 ;;
    --run) run=${2:?--run needs a command}; shift 2 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
if [[ ${1:-} == --multi ]]; then
  shift; elf=""; multi=1; secs=${1:-15}; board=${2:-s3}
  [[ -f $pt && ${#apps[@]} -gt 0 ]] || { echo "--multi needs --pt <csv> and at least one --app <label>=<elf>" >&2; exit 2; }
  for a in "${apps[@]}"; do [[ -f ${a#*=} ]] || { echo "no such ELF: ${a#*=}" >&2; exit 1; }; done
elif [[ -n $hold ]]; then elf=""; secs=$hold; board=${1:-s3}
elif [[ -n $run && ( $# -lt 1 || ${1:-} =~ ^[0-9]+$ ) ]]; then elf=""; secs=${1:-15}; board=${2:-s3}
elif [[ ${1:-} == --log ]]; then elf=""; shift; secs=${1:-15}; board=${2:-s3}
else elf=${1:?usage: dev-board.sh <elf>|--log [seconds] [board]  (or --register / --list)}; shift; secs=${1:-15}; board=${2:-s3}; fi
port=${BOARDS[$board]:-}
if [[ -z $port ]]; then echo "unknown board $board (known: ${!BOARDS[*]}). Run: tools/dev-board.sh --register" >&2; exit 1; fi
chip=$(chip_of "$board")

link_elfs=(); [[ -n $elf ]] && link_elfs+=("$elf")
for a in "${apps[@]}"; do link_elfs+=("${a#*=}"); done
(( ${#link_elfs[@]} )) && check_link_keys "${link_elfs[@]}"

if (( dry )); then
  echo "[dry-run] board=$board chip=$chip port=$port"
  echo "[dry-run] lock: $lock_dir/$(lock_name "$port")"
  [[ -n $elf ]] && echo "[dry-run] espflash flash --chip $chip --port $port --non-interactive $elf"
  if (( multi )); then
    echo "[dry-run] espflash erase-parts --chip $chip --port $port --partition-table $pt -a no-reset ${erase[*]}"
    for a in "${apps[@]}"; do echo "[dry-run] espflash flash --chip $chip --port $port --non-interactive --partition-table $pt --target-app-partition ${a%%=*} ${a#*=}"; done
  fi
  echo "[dry-run] reset (DTR/RTS) then read ${secs}s${send:+; after ${send_after}s type '$send'}"
  exit 0
fi
# A C5 leaves USB for a moment while its app starts: allow it up to 10 s to come back.
for _ in $(seq 50); do [[ -e $port ]] && break; sleep 0.2; done
[[ -e $port ]] || { echo "board $board: $port is not attached" >&2; exit 1; }
[[ -z $elf || -f $elf ]] || { echo "no such ELF: $elf" >&2; exit 1; }

mkdir -p "$lock_dir"
lock=$lock_dir/$(lock_name "$port")
old_lock=/tmp/rotorscope-board-$board.lock   # what older copies of this script used

exec 9>>"$lock"
if ! flock -w 600 9; then echo "board $board busy for 10 min; giving up" >&2; exit 1; fi
# Also take the legacy /tmp lock, always second, so an old copy of the script that is
# still running (or a checkout that has not pulled yet) still excludes us.
exec 8>>"$old_lock"
if ! flock -w 600 8; then echo "board $board busy (legacy lock) for 10 min; giving up" >&2; exit 1; fi
label=${elf:-log}; (( multi )) && label="multi:${apps[*]##*/}"
printf 'pid=%s since=%s cmd=%s %s\n' "$$" "$(date +%T)" "$(basename "$label")" "$board" >"$lock"

if [[ -n $hold ]]; then echo "holding $board for ${hold}s"; sleep "$hold"; exit 0; fi
if [[ -n $elf ]]; then
  espflash flash --chip "$chip" --port "$port" --non-interactive "$elf" 2>&1 | tail -3
fi
if (( multi )); then
  # Erase the boot selection (so the bootloader starts ota_0) and any stored
  # job, then write each image into its slot. The chip stays in the ROM
  # loader between steps; the last one resets it into the new firmware.
  espflash erase-parts --chip "$chip" --port "$port" --non-interactive --partition-table "$pt" -a no-reset "${erase[@]}" 2>&1 | tail -2
  n=${#apps[@]}; i=0
  for a in "${apps[@]}"; do
    i=$((i + 1)); after=no-reset; (( i == n )) && after=hard-reset
    echo "dev-board: ${a%%=*} <- $(basename "${a#*=}")"
    espflash flash --chip "$chip" --port "$port" --non-interactive --partition-table "$pt" \
      --target-app-partition "${a%%=*}" -a "$after" "${a#*=}" 2>&1 | tail -2
    # The C5 re-enumerates USB when it changes mode: wait for the port.
    for _ in $(seq 50); do [[ -e $port ]] && break; sleep 0.2; done
  done
fi
if [[ -n $run ]]; then
  for _ in $(seq 50); do [[ -e $port ]] && break; sleep 0.2; done
  BOARD_PORT=$port timeout "$secs" bash -c "$run"
  exit $?
fi
python3 -I - "$port" "$secs" "$send" "$send_after" <<'PY'
import os, sys, time, termios, serial
port, secs, keys, after = sys.argv[1], float(sys.argv[2]), sys.argv[3], float(sys.argv[4])
# The ESP32-C5 drops off USB for a moment as its app starts (the USB-Serial-JTAG
# re-enumerates; the S3 does not). Ride it out: wait for the by-id path to come
# back and reopen it, without resetting the board again.
def reopen(deadline):
    while time.time() < deadline:
        if os.path.exists(port):
            try: return serial.Serial(port, 115200, timeout=0.2)
            except (serial.SerialException, OSError): pass
        time.sleep(0.1)
    return None
s = serial.Serial(port, 115200, timeout=0.2)
s.dtr = False; s.rts = True; time.sleep(0.1); s.rts = False   # USB-JTAG hard reset
t0 = time.time(); end = t0 + secs; drops = 0
while time.time() < end:
    try:
        if keys and time.time() - t0 >= after:
            for ch in keys:              # one character at a time, like a person typing
                s.write(ch.encode()); s.flush()
                # Keep reading while we pause: Linux buffers only 4 KB of unread
                # input, after which the board's output stalls and is dropped
                # (a typed `dump` used to lose everything after ~4 KB).
                t_key = time.time()
                while time.time() - t_key < 0.4:
                    b = s.read(4096)
                    if b: sys.stdout.write(b.decode(errors="replace")); sys.stdout.flush()
            keys = ""
        b = s.read(4096)
        if b: sys.stdout.write(b.decode(errors="replace")); sys.stdout.flush()
    except (serial.SerialException, OSError, termios.error):
        drops += 1
        try: s.close()
        except Exception: pass
        s = reopen(min(end, time.time() + 10))
        if s is None:
            sys.stderr.write("dev-board: %s left USB and did not come back\n" % port); sys.exit(3)
        sys.stderr.write("dev-board: USB re-enumerated (%d), reopened\n" % drops)
PY
