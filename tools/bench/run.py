#!/usr/bin/env python3
"""Rotorscope two-board bench tests: one command per test.

    tools/bench/run.py --list
    tools/bench/run.py s3-to-s3-wifi-beacon            # flash both boards, run, PASS/FAIL
    tools/bench/run.py --group s3 --seconds 40
    tools/bench/run.py --all --dry-run                 # show the plan, touch nothing

Per RF test: build (or reuse) rid-beacon and the listener, start the transmitter board, type the
console keys that leave only the wanted transport on, wait until it is sending, flash and start
the listener board, capture N seconds, stop the transmitter, and judge the LISTENER's output
(judge.judge_listener): at least M decoded detections of the fake identity TESTDRONE0000000001
with the right transport (and band). Raw logs go to bench-logs/<timestamp>/<test>/.

Hardware access is only ever through tools/dev-board.sh (per-board lock, by-id paths). This
script never opens a serial port, never calls adb/espflash directly, and never transmits except
through the existing rid-beacon firmware with its default conduct settings.
"""
from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import time
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import judge  # noqa: E402

ROOT = HERE.parents[1]
DEV_BOARD = Path(os.environ.get("ROTORSCOPE_DEV_BOARD") or ROOT / "tools" / "dev-board.sh")  # override: tests only
FW = ROOT / "firmware"
S3_TARGET, C5_TARGET = "xtensa-esp32s3-none-elf", "riscv32imac-unknown-none-elf"
CHIP_NAME = {"s3": "esp32s3", "c5": "esp32c5"}

# ------------------------------------------------------------------ firmware --


@dataclass(frozen=True)
class Firmware:
    name: str
    chip: str  # "s3" | "c5"

    @property
    def dir(self) -> Path:
        """Directory to run cargo from."""
        if self.name in ("node-wifi", "hub"):
            return FW / self.name / self.chip          # chip chosen by directory; `cargo fw`
        return FW / self.name

    @property
    def cmd(self) -> str:
        if self.name in ("node-wifi", "hub"):
            return "cargo fw"
        if self.chip == "s3":
            return "cargo build --release"
        return (f"cargo +stable build --release --target {C5_TARGET} "
                f"--no-default-features --features esp32c5")

    @property
    def elf(self) -> Path:
        t = S3_TARGET if self.chip == "s3" else C5_TARGET
        return FW / self.name / "target" / t / "release" / self.name

    def build_argv(self) -> list[str]:
        pre = ". ~/export-esp.sh >/dev/null 2>&1; " if self.chip == "s3" else ""
        return ["bash", "-c", f"{pre}cd {self.dir} && {self.cmd}"]


# --------------------------------------------------------------------- tests --


@dataclass(frozen=True)
class Test:
    id: str
    kind: str                 # "rf" | "hub"
    desc: str
    tx_chip: str              # rf: beacon chip.  hub: node chip
    rx_chip: str              # rf: listener chip. hub: hub chip
    tx_board: str
    rx_board: str
    listener: str = ""        # rf: "node-wifi" | "node-ble".  hub: the node firmware
    transport: str = ""       # rf: judge.TRANSPORTS entry
    band: str | None = None   # rf Wi-Fi: "2.4" | "5"
    tx_keys: str = ""         # rid-beacon console keys (leading x = silence all first)
    hub_link: int = 0         # hub: which hub node link the node is wired to


_KEYS = {"WifiBeacon": "w", "WifiNan": "n", "BleLegacy": "l", "BleExtended": "e", "BleLongRange": "c"}


def _rf(tx, rx, name, transport, band):
    keys = "x" + _KEYS[transport]
    if tx == "c5" and band == "2.4":
        keys += "b"       # C5 beacon defaults to 5 GHz ch149; 'b' moves it to 2.4 GHz ch6
    boards = {("s3", "s3"): ("s3", "s3b"), ("c5", "c5"): ("c5a", "c5b"),
              ("c5", "s3"): ("c5a", "s3"), ("s3", "c5"): ("s3", "c5a")}[(tx, rx)]
    bandtxt = {None: "", "2.4": " 2.4 GHz", "5": " 5 GHz"}[band]
    return Test(f"{tx}-to-{rx}-{name}", "rf", f"{tx.upper()} rid-beacon -> {rx.upper()} "
                f"{'node-wifi' if transport.startswith('Wifi') else 'node-ble'}: {transport}{bandtxt}",
                tx, rx, boards[0], boards[1],
                "node-wifi" if transport.startswith("Wifi") else "node-ble", transport, band, keys)


def _build_tests() -> dict[str, Test]:
    t: list[Test] = []
    ble = [("ble4", "BleLegacy"), ("ble5", "BleExtended"), ("ble-lr", "BleLongRange")]
    for name, tr in [("wifi-beacon", "WifiBeacon"), ("wifi-nan", "WifiNan")]:
        t.append(_rf("s3", "s3", name, tr, "2.4"))
    t += [_rf("s3", "s3", n, tr, None) for n, tr in ble]
    for name, tr in [("wifi5-beacon", "WifiBeacon"), ("wifi5-nan", "WifiNan")]:
        t.append(_rf("c5", "c5", name, tr, "5"))
    t += [_rf("c5", "c5", n, tr, None) for n, tr in ble]
    # C5 transmits on 2.4 GHz (console 'b'); the S3 listener hears it. The reverse (S3 -> C5
    # Wi-Fi) makes no sense: the S3 has no 5 GHz radio and node-wifi on a C5 only listens on 5.
    for name, tr in [("wifi24-beacon", "WifiBeacon"), ("wifi24-nan", "WifiNan")]:
        t.append(_rf("c5", "s3", name, tr, "2.4"))
    t += [_rf("c5", "s3", n, tr, None) for n, tr in ble]
    t += [_rf("s3", "c5", n, tr, None) for n, tr in ble]
    # Node -> hub over UART. Hub is always an S3. Link numbers follow firmware/hub/src/pins.rs:
    # link 0 = Wi-Fi 2.4 node, link 1 = Bluetooth node, link 2 = Wi-Fi 5 GHz node.
    for chip, fwname, link in [("s3", "node-wifi", 0), ("s3", "node-ble", 1),
                               ("c5", "node-wifi", 2), ("c5", "node-ble", 1)]:
        kind = "wifi" if fwname == "node-wifi" else "ble"
        t.append(Test(f"hub-uart-{chip}-{kind}", "hub",
                      f"{chip.upper()} {fwname} -> S3 hub, UART link {link}",
                      chip, "s3", "s3b" if chip == "s3" else "c5a", "s3", fwname, hub_link=link))
    return {x.id: x for x in t}


TESTS = _build_tests()
GROUPS = {"s3": lambda t: t.id.startswith("s3-to-s3"), "c5": lambda t: t.id.startswith("c5-to-c5"),
          "cross": lambda t: t.id.startswith(("c5-to-s3", "s3-to-c5")),
          "hub": lambda t: t.kind == "hub", "ble": lambda t: t.kind == "rf" and t.transport.startswith("Ble"),
          "wifi": lambda t: t.kind == "rf" and t.transport.startswith("Wifi")}

TX_BASE = 180          # transmitter capture = seconds + this; it is stopped as soon as the listener ends
START_TIMEOUT = 720    # lock wait (10 min) + flash
READY_EXTRA = 60       # how long to wait for isolation keys to take effect before going on anyway


# ------------------------------------------------------------------- helpers --


def dev_board(*args) -> list[str]:
    return [str(DEV_BOARD), *map(str, args)]


def resolve(board: str) -> tuple[str, str] | None:
    r = subprocess.run(dev_board("--resolve", board), capture_output=True, text=True)
    if r.returncode != 0:
        return None
    port, chip = r.stdout.split()
    return port, chip


def tail(path: Path, n=2) -> str:
    """Last few lines of a log, squeezed onto one line for the summary table."""
    try:
        return " | ".join(path.read_text(errors="replace").splitlines()[-n:])[:240] or "(log is empty)"
    except OSError:
        return "(no log)"


def kill_group(p: subprocess.Popen | None):
    if p and p.poll() is None:
        try:
            os.killpg(p.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            p.wait(10)
        except subprocess.TimeoutExpired:
            os.killpg(p.pid, signal.SIGKILL)


def launch(log: Path, elf: Path, secs: int, board: str, keys: str = "") -> subprocess.Popen:
    args = (["--send", keys] if keys else []) + [elf, secs, board]
    f = open(log, "wb")
    return subprocess.Popen(dev_board(*args), stdout=f, stderr=subprocess.STDOUT, start_new_session=True)


def wait_for(p: subprocess.Popen, log: Path, ready, timeout: float, what: str) -> str:
    """Poll until ready(text) or the process exits or timeout. Returns 'ready'|'exited'|'timeout'."""
    t0, last = time.time(), 0.0
    while time.time() - t0 < timeout:
        if ready(log.read_text(errors="replace") if log.exists() else ""):
            return "ready"
        if p.poll() is not None:
            return "ready" if ready(log.read_text(errors="replace")) else "exited"
        if time.time() - last > 30:
            print(f"    ... waiting for {what} ({int(time.time() - t0)}s; a busy board makes this wait)")
            last = time.time()
        time.sleep(1)
    return "timeout"


# ----------------------------------------------------------------- the plan ---


def plan_lines(t: Test, a) -> list[str]:
    tx_fw = Firmware("rid-beacon" if t.kind == "rf" else t.listener, t.tx_chip)
    rx_fw = Firmware(t.listener if t.kind == "rf" else "hub", t.rx_chip)
    L = [f"{t.id}: {t.desc}"]
    for role, board, chip in (("transmitter" if t.kind == "rf" else "node       ", t.tx_board, t.tx_chip),
                              ("listener   " if t.kind == "rf" else "hub        ", t.rx_board, t.rx_chip)):
        r = resolve(board)
        L.append(f"  {role} board {board:<4} " + (f"{r[0]}" if r else "NOT REGISTERED (run: tools/dev-board.sh --register)")
                 + (f"  [registered chip {r[1]}, test wants {CHIP_NAME[chip]}]" if r and r[1] != CHIP_NAME[chip] else ""))
    for fw in (tx_fw, rx_fw):
        L.append(f"  build: {' '.join(fw.build_argv()[2:])}" + ("" if not a.no_build else "   (skipped: --no-build)"))
        L.append(f"         -> {fw.elf.relative_to(ROOT)}")
    if t.kind == "rf":
        L.append(f"  1. flash+reset transmitter, after 8 s type '{t.tx_keys}' on its console "
                 f"(x = silence all, then only {t.transport}{' on 2.4 GHz' if t.tx_keys.endswith('b') else ''})")
        L.append(f"  2. wait until the transmitter reports only {t.transport} on (up to {READY_EXTRA} s)")
        L.append(f"  3. flash+reset listener, capture {a.seconds} s")
        L.append(f"  PASS: >= {a.min_hits} lines with tr={t.transport}"
                 + (f", ch in the {t.band} GHz band" if t.band else "") + f", carrying {judge.IDENT}")
    else:
        L.append("  1. flash+reset the node, wait for its hello line")
        L.append(f"  2. flash+reset the hub, capture {a.seconds} s")
        L.append(f"  PASS: hub link {t.hub_link} counts >= {a.min_frames} decoded frames")
    L.append("  dev-board.sh dry-runs:")
    for board, fw in ((t.tx_board, tx_fw), (t.rx_board, rx_fw)):
        if resolve(board):
            r = subprocess.run(dev_board("--dry-run", fw.elf, a.seconds, board), capture_output=True, text=True)
            L += ["    " + x for x in r.stdout.splitlines()]
    return L


# ----------------------------------------------------------------- running ----


def build(fws: list[Firmware], a) -> str | None:
    done = set()
    for fw in fws:
        if fw in done:
            continue
        done.add(fw)
        if a.no_build:
            if not fw.elf.exists():
                return f"--no-build but {fw.elf} does not exist"
            continue
        print(f"  building {fw.name} ({fw.chip}) ...")
        r = subprocess.run(fw.build_argv(), capture_output=True, text=True)
        (a.logdir / f"build-{fw.name}-{fw.chip}.log").write_text(r.stdout + r.stderr)
        if r.returncode != 0 or not fw.elf.exists():
            return f"build of {fw.name} ({fw.chip}) failed; see {a.logdir}/build-{fw.name}-{fw.chip}.log\n" + "\n".join((r.stderr or "").splitlines()[-8:])
    return None


def check_boards(t: Test) -> str | None:
    if t.tx_board == t.rx_board:
        return "both roles resolved to the same board"
    for board, chip in ((t.tx_board, t.tx_chip), (t.rx_board, t.rx_chip)):
        r = resolve(board)
        if not r:
            return f"board '{board}' is not registered (tools/dev-board.sh --register)"
        # A C5 vanishes from USB for a moment while its app starts; give it time to return.
        deadline = time.time() + 10
        while not os.path.exists(r[0]) and time.time() < deadline:
            time.sleep(0.2)
        if not os.path.exists(r[0]):
            return f"board '{board}' is registered but not plugged in ({r[0]})"
        if r[1] != CHIP_NAME[chip]:
            return f"board '{board}' is a {r[1]}, this test needs {CHIP_NAME[chip]}"
    return None


def run_rf(t: Test, a, d: Path) -> dict:
    tx_fw, rx_fw = Firmware("rid-beacon", t.tx_chip), Firmware(t.listener, t.rx_chip)
    txlog, rxlog = d / "tx.log", d / "rx.log"
    tx = rx = None
    try:
        print(f"  [1/3] transmitter {t.tx_board}: flash + keys '{t.tx_keys}'")
        tx = launch(txlog, tx_fw.elf, a.seconds + TX_BASE, t.tx_board, t.tx_keys)
        st = wait_for(tx, txlog, lambda s: judge.tx_ready(s, t.transport), START_TIMEOUT, "transmitter")
        note = []
        if st == "exited":
            return dict(result="ERROR", reason="transmitter's dev-board.sh exited early: " + tail(txlog), hits=0, d=d)
        if st == "timeout":
            if not judge.parse_lines(txlog.read_text(errors="replace")):
                return dict(result="ERROR", reason="transmitter never produced output (stuck lock, wrong board?)", hits=0, d=d)
        if not judge.tx_ready(txlog.read_text(errors="replace"), t.transport):
            # keys may not have taken effect; the beacon is on air with everything enabled, which
            # still tests the same transport, so go on and say so.
            time.sleep(READY_EXTRA)
            if not judge.tx_ready(txlog.read_text(errors="replace"), t.transport):
                note.append("transmitter isolation not confirmed (other transports may be on)")
        print(f"  [2/3] listener {t.rx_board}: flash, capture {a.seconds} s")
        rx = launch(rxlog, rx_fw.elf, a.seconds, t.rx_board)
        try:
            rx.wait(a.seconds + START_TIMEOUT)
        except subprocess.TimeoutExpired:
            return dict(result="ERROR", reason="listener did not finish in time (busy board?)", hits=0, d=d)
    finally:
        kill_group(tx)
        kill_group(rx)
    v = judge.judge_listener(rxlog.read_text(errors="replace"), t.transport, t.band, a.min_hits)
    ctx = judge.judge_beacon(txlog.read_text(errors="replace"), t.transport, t.band)
    reason = v.reason
    if not v.passed and ctx["notes"]:
        reason += "  | transmitter: " + "; ".join(ctx["notes"])
    elif note:
        reason += "  | " + "; ".join(note)
    return dict(result="PASS" if v.passed else "FAIL", reason=reason, hits=v.hits, needed=v.needed,
                evidence=v.evidence, tx=ctx, d=d)


def run_hub(t: Test, a, d: Path) -> dict:
    node_fw, hub_fw = Firmware(t.listener, t.tx_chip), Firmware("hub", "s3")
    nlog, hlog = d / "node.log", d / "hub.log"
    node = hub = None
    try:
        print(f"  [1/3] node {t.tx_board}: flash {t.listener}")
        node = launch(nlog, node_fw.elf, a.seconds + TX_BASE, t.tx_board)
        st = wait_for(node, nlog, lambda s: any(o.get("t") == "hello" for o in judge.parse_lines(s)),
                      START_TIMEOUT, "node hello")
        if st != "ready":
            return dict(result="ERROR", reason=f"node never printed hello ({st}): " + tail(nlog), hits=0, d=d)
        print(f"  [2/3] hub {t.rx_board}: flash, capture {a.seconds} s")
        hub = launch(hlog, hub_fw.elf, a.seconds, t.rx_board)
        try:
            hub.wait(a.seconds + START_TIMEOUT)
        except subprocess.TimeoutExpired:
            return dict(result="ERROR", reason="hub did not finish in time (busy board?)", hits=0, d=d)
    finally:
        kill_group(node)
        kill_group(hub)
    v = judge.judge_hub(hlog.read_text(errors="replace"), t.hub_link, a.min_frames)
    return dict(result="PASS" if v.passed else "FAIL", reason=v.reason, hits=v.hits, needed=v.needed,
                evidence=v.evidence, d=d)


def evidence_str(r: dict) -> str:
    e = r.get("evidence") or {}
    if r["result"] != "PASS" or not e:
        return r["reason"]
    if "frames" in e:
        return f"{e['frames']} frames, corrupt {e.get('corrupt')}, role {e.get('role')}"
    ch = ",".join(map(str, e.get("channels", []))) or "-"
    return (f"ch {ch}, rssi {e['rssi_min']}..{e['rssi_max']}, from {','.join(e['macs'])}")


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("tests", nargs="*", help="test ids (see --list)")
    ap.add_argument("--all", action="store_true", help="every test")
    ap.add_argument("--group", choices=sorted(GROUPS), help="s3, c5, cross, hub, ble, wifi")
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--seconds", "-n", type=int, default=30, help="listener capture length (default 30)")
    ap.add_argument("--min-hits", "-m", type=int, default=3, help="RF: detections needed (default 3)")
    ap.add_argument("--min-frames", type=int, default=10, help="hub: node frames needed (default 10)")
    ap.add_argument("--dry-run", action="store_true", help="print the plan; build, flash and lock nothing")
    ap.add_argument("--no-build", action="store_true", help="reuse already-built ELFs")
    ap.add_argument("--tx-board"), ap.add_argument("--rx-board")
    ap.add_argument("--logs", default=str(ROOT / "bench-logs"), help="log root (default bench-logs/)")
    a = ap.parse_args(argv)

    if a.list:
        for t in TESTS.values():
            print(f"{t.id:<24} {t.tx_board:>4} -> {t.rx_board:<4} {t.desc}")
        return 0
    ids = list(TESTS) if a.all else [i for i in TESTS if a.group and GROUPS[a.group](TESTS[i])] if a.group else a.tests
    if not ids:
        ap.error("name a test, or use --group / --all (see --list)")
    bad = [i for i in ids if i not in TESTS]
    if bad:
        ap.error(f"unknown test(s): {', '.join(bad)} (see --list)")
    tests = [TESTS[i] for i in ids]
    if a.tx_board or a.rx_board:
        from dataclasses import replace
        tests = [replace(t, tx_board=a.tx_board or t.tx_board, rx_board=a.rx_board or t.rx_board) for t in tests]

    if a.dry_run:
        for t in tests:
            print("\n".join(plan_lines(t, a)), end="\n\n")
        print("dry run: nothing was built, flashed or locked.")
        return 0

    stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    a.logdir = Path(a.logs) / stamp
    a.logdir.mkdir(parents=True, exist_ok=True)
    print(f"logs: {a.logdir}")
    results = []
    for t in tests:
        print(f"\n== {t.id}: {t.desc}")
        d = a.logdir / t.id
        d.mkdir()
        err = check_boards(t)
        if err:
            r = dict(result="ERROR", reason=err, hits=0, d=d)
        else:
            err = build([Firmware("rid-beacon" if t.kind == "rf" else t.listener, t.tx_chip),
                         Firmware(t.listener if t.kind == "rf" else "hub", t.rx_chip)], a)
            r = dict(result="ERROR", reason=err, hits=0, d=d) if err else (run_rf if t.kind == "rf" else run_hub)(t, a, d)
        print(f"  -> {r['result']}: {r['reason']}")
        results.append((t, r))
        (d / "result.json").write_text(json.dumps({k: (str(v) if isinstance(v, Path) else v) for k, v in r.items()}, indent=1, default=str))

    w = max(len(t.id) for t, _ in results)
    lines = [f"\n{'TEST':<{w}}  {'TX':<4} {'RX':<4}  {'HITS':<7} RESULT  EVIDENCE"]
    for t, r in results:
        hits = f"{r.get('hits', 0)}/{r.get('needed', a.min_hits if t.kind == 'rf' else a.min_frames)}"
        lines.append(f"{t.id:<{w}}  {t.tx_board:<4} {t.rx_board:<4}  {hits:<7} {r['result']:<6}  {evidence_str(r)}")
    out = "\n".join(lines)
    print(out)
    (a.logdir / "summary.txt").write_text(out + "\n")
    print(f"\nraw logs: {a.logdir}")
    if any(r["result"] == "ERROR" for _, r in results):
        return 2
    return 0 if all(r["result"] == "PASS" for _, r in results) else 1


if __name__ == "__main__":
    sys.exit(main())
