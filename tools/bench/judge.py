"""Judging logic for the Rotorscope bench tests. Pure functions over captured text: no
serial ports, no subprocesses, so they are tested with saved sample logs
(tools/bench/test_judge.py).

Input formats (see the firmware READMEs):
  node-wifi / node-ble USB output : one JSON object per line, tagged "t"
      {"t":"hello",...}  {"t":"stats",...}  {"t":"rid","tr":"WifiBeacon","ch":6,"rssi":-60,
       "mac":"..","ctr":12,"msgs":[{"BasicId":{"uas_id":"TESTDRONE0000000001",..}},..]}
  rid-beacon USB output           : {"t":"boot",..} {"t":"tx","band":"2.4","ch":6,"beacon":1,
       "nan":1,"ble4":5,"ble5":1,"ble_lr":1,"fail":0,"on":"WNLEC",..} {"t":"cmd",..}
  hub USB output                  : {"t":"hub","links":[{"role":..,"alive":..,"frames":..,..}],..}
Everything that does not start with "{" (bootloader text, panics, log lines) is skipped.
"""
from __future__ import annotations

import json
from collections import Counter
from dataclasses import dataclass, field

# rid-beacon's fake identity (firmware/rid-beacon/src/rid_tx.rs). Not a real aircraft.
IDENT = "TESTDRONE0000000001"

TRANSPORTS = ("WifiBeacon", "WifiNan", "BleLegacy", "BleExtended", "BleLongRange")
# rid-beacon's "tx" counter field and its letter in the "on" string, per transport.
TX_FIELD = {"WifiBeacon": "beacon", "WifiNan": "nan", "BleLegacy": "ble4",
            "BleExtended": "ble5", "BleLongRange": "ble_lr"}
TX_LETTER = {"WifiBeacon": "W", "WifiNan": "N", "BleLegacy": "L",
             "BleExtended": "E", "BleLongRange": "C"}


def parse_lines(text: str) -> list[dict]:
    """The JSON objects in a capture. Lines may be preceded by stray bytes (a reset
    mid-line); we parse from the first '{'. Anything unparseable is skipped."""
    out = []
    for raw in text.splitlines():
        i = raw.find("{")
        if i < 0:
            continue
        try:
            obj = json.loads(raw[i:])
        except ValueError:
            continue
        if isinstance(obj, dict):
            out.append(obj)
    return out


def has_ident(rid_line: dict, ident: str = IDENT) -> bool:
    """True if one of the decoded messages carries the fake UAS ID (Basic ID). Matches the
    text form, or the hex form the serializer falls back to for non-clean text."""
    blob = json.dumps(rid_line.get("msgs", []))
    return ident in blob or ident.encode().hex() in blob.lower()


def band_ok(band: str | None, ch: int) -> bool:
    if band is None:        # Bluetooth: the node reports channel 0
        return True
    if band == "2.4":
        return 1 <= ch <= 14
    if band == "5":
        return ch >= 32
    raise ValueError(f"band {band!r}")


@dataclass
class Verdict:
    passed: bool
    reason: str
    hits: int = 0
    needed: int = 0
    evidence: dict = field(default_factory=dict)

    def summary(self) -> str:
        return f"{'PASS' if self.passed else 'FAIL'}: {self.reason}"


def judge_listener(text: str, transport: str, band: str | None, min_hits: int,
                   ident: str = IDENT) -> Verdict:
    """PASS = at least `min_hits` Remote ID lines that (a) carry `ident`, (b) say the
    wanted transport, (c) are on the wanted band (Wi-Fi only)."""
    objs = parse_lines(text)
    hello = any(o.get("t") == "hello" for o in objs)
    stats = [o for o in objs if o.get("t") == "stats"]
    rids = [o for o in objs if o.get("t") == "rid"]
    mine = [o for o in rids if has_ident(o, ident)]
    good = [o for o in mine if o.get("tr") == transport and band_ok(band, int(o.get("ch", 0) or 0))]
    other_tr = Counter(o.get("tr") for o in mine if o not in good)
    rssi = [o["rssi"] for o in good if isinstance(o.get("rssi"), (int, float))]
    ev = {
        "lines": len(objs), "hello": hello, "stats_lines": len(stats),
        "rid_lines": len(rids), "rid_with_ident": len(mine), "hits": len(good),
        "channels": sorted({int(o.get("ch", 0) or 0) for o in good}),
        "macs": sorted({str(o.get("mac")) for o in good}),
        "rssi_min": min(rssi) if rssi else None, "rssi_max": max(rssi) if rssi else None,
        "other_transports": dict(other_tr),
        "foreign_rid": len(rids) - len(mine),
    }
    if len(good) >= min_hits:
        return Verdict(True, f"{len(good)} detections of {ident} as {transport}"
                       + (f" on ch {ev['channels']}" if band else ""), len(good), min_hits, ev)
    # Why not: most specific cause first.
    if not objs:
        why = "listener printed nothing parseable (not flashed, wrong port, or board in the bootloader)"
    elif not hello:
        why = "listener never printed its hello line (did not boot, or the capture missed the reset)"
    elif other_tr:
        why = (f"heard {ident} but as {dict(other_tr)} (or on the wrong band), "
               f"not {transport}: {len(good)}/{min_hits}")
    elif rids and not mine:
        why = f"heard {len(rids)} Remote ID frames, none with {ident} (someone else's signal?)"
    elif not stats:
        why = "listener booted but printed no stats lines (hung?)"
    else:
        why = f"listener is alive but heard no {transport} from the beacon: {len(good)}/{min_hits}"
    return Verdict(False, why, len(good), min_hits, ev)


def judge_beacon(text: str, transport: str, band: str | None = None) -> dict:
    """Context for a FAIL (never used to decide PASS): did rid-beacon boot, was the wanted
    transport enabled and actually transmitting, on the expected band?"""
    objs = parse_lines(text)
    tx = [o for o in objs if o.get("t") == "tx"]
    boot = any(o.get("t") == "boot" for o in objs)
    f = TX_FIELD[transport]
    sent = sum(int(o.get(f, 0) or 0) for o in tx)
    last_on = tx[-1].get("on") if tx else None
    probe_bad = [o for o in objs if o.get("t") == "ble_probe" and o.get("ok") is False]
    note = []
    if not objs:
        note.append("transmitter printed nothing")
    elif not boot:
        note.append("no boot line from transmitter")
    elif not tx:
        note.append("transmitter booted but printed no tx lines")
    elif sent == 0:
        note.append(f"transmitter sent 0 {transport} frames/payloads")
    if last_on is not None:
        if f"{TX_LETTER[transport]}" not in last_on:
            note.append(f"{transport} not enabled on transmitter (on={last_on!r})")
        extra = [c for c in last_on if c.isupper() and c != TX_LETTER[transport]]
        if extra:
            note.append(f"other transports were also on (on={last_on!r}): isolation keys not applied")
    if probe_bad:
        note.append(f"BLE probe refused: {[o.get('tx') for o in probe_bad]}")
    if band and tx and tx[-1].get("band") not in (None, band):
        note.append(f"transmitter band is {tx[-1].get('band')!r}, wanted {band!r}")
    fails = max((int(o.get("fail", 0) or 0) for o in tx), default=0)
    if fails:
        note.append(f"transmitter reports fail={fails}")
    return {"booted": boot, "tx_lines": len(tx), "sent": sent, "on": last_on,
            "band": tx[-1].get("band") if tx else None, "notes": note}


def tx_ready(text: str, transport: str) -> bool:
    """Has the transmitter reached the state a test wants: only this transport on, and sending?"""
    tx = [o for o in parse_lines(text) if o.get("t") == "tx"]
    if not tx:
        return False
    on = tx[-1].get("on", "")
    return TX_LETTER[transport] in on and not [c for c in on if c.isupper() and c != TX_LETTER[transport]]


def judge_hub(text: str, link: int, min_frames: int) -> Verdict:
    """Node-to-hub UART: PASS if the hub's per-link counter for `link` saw >= min_frames good
    frames from the node. `corrupt` is reported but tolerated (the S3 ROM bootloader prints
    on GPIO43 at reset and the hub discards that as a corrupt frame)."""
    objs = parse_lines(text)
    hubs = [o for o in objs if o.get("t") == "hub"]
    boot = any(o.get("t") == "boot" for o in objs)
    ev = {"lines": len(objs), "hub_lines": len(hubs), "link": link}
    if not hubs:
        why = ("hub printed nothing parseable (not flashed, wrong port?)" if not objs else
               "hub booted but printed no 'hub' status lines" if boot else
               "hub never printed a boot line")
        return Verdict(False, why, 0, min_frames, ev)
    links = hubs[-1].get("links", [])
    if link >= len(links):
        return Verdict(False, f"hub reports only {len(links)} node links, wanted link {link}",
                       0, min_frames, ev)
    cur = links[link]
    frames = int(cur.get("frames", 0) or 0)
    ev.update({"role": cur.get("role"), "alive": cur.get("alive"), "bytes": cur.get("bytes"),
               "frames": frames, "corrupt": cur.get("corrupt"), "ovf": cur.get("ovf"),
               "uart_err": cur.get("uart_err"),
               "frames_all_links": [int(l.get("frames", 0) or 0) for l in links]})
    if frames >= min_frames:
        return Verdict(True, f"hub link {link} decoded {frames} frames from the node "
                       f"(role {cur.get('role')}, corrupt {cur.get('corrupt')})", frames, min_frames, ev)
    others = [i for i, l in enumerate(links) if i != link and int(l.get("frames", 0) or 0) > 0]
    if others:
        why = f"frames arrived on link(s) {others}, not link {link}: node wired to the wrong hub pin"
    elif int(cur.get("bytes", 0) or 0) > 0:
        why = (f"bytes arrived on link {link} ({cur.get('bytes')}) but only {frames} decoded frames "
               f"(corrupt {cur.get('corrupt')}): baud or noise problem")
    else:
        why = f"no bytes at all on hub link {link}: check the TX-to-RX wire and the shared ground"
    return Verdict(False, why, frames, min_frames, ev)


# ------------------------------------------------------------ open FPV links --
#
# rid-beacon's fake open-FPV frames (firmware/rid-beacon, keys f/o/r/d; bytes from
# crates/openlink/src/build.rs). node-wifi reports each stream at most once a second:
#   {"t":"open","sys":"WfbNg","id":8279021,"port":0,"dir":"AirToGround","ch":165,"rssi":-40,
#    "mac":"57:42:7e:57:ed:00","up_us":..,"n":42}

# Beacon short name (its console key and "open_sys" field) -> openlink::System variant.
OPEN_SYSTEMS = {"wfb-ng": "WfbNg", "openhd": "OpenHd", "rubyfpv": "RubyFpv", "dronebridge": "DroneBridge"}
OPEN_KEYS = {"wfb-ng": "f", "openhd": "o", "rubyfpv": "r", "dronebridge": "d"}
# Channel -> rid-beacon console digit.
CHANNEL_KEYS = {6: "6", 149: "7", 161: "8", 165: "9"}
# The fake identifiers (openlink::build::fake_id). OpenHD sends none in the clear.
OPEN_FAKE_ID = {"WfbNg": 0x7E57ED, "OpenHd": None, "RubyFpv": 0x7E570001, "DroneBridge": 0x7E}


def judge_open(text: str, system: str, channel: int, min_hits: int) -> Verdict:
    """PASS = at least `min_hits` "open" lines from the listener with the wanted system
    (`OPEN_SYSTEMS` short name), on the wanted channel, carrying the fake identifier."""
    sysv = OPEN_SYSTEMS[system]
    want_id = OPEN_FAKE_ID[sysv]
    objs = parse_lines(text)
    hello = any(o.get("t") == "hello" for o in objs)
    stats = [o for o in objs if o.get("t") == "stats"]
    opens = [o for o in objs if o.get("t") == "open"]
    mine = [o for o in opens if o.get("sys") == sysv and o.get("id") == want_id]
    good = [o for o in mine if int(o.get("ch", 0) or 0) == channel]
    rssi = [o["rssi"] for o in good if isinstance(o.get("rssi"), (int, float))]
    ev = {
        "lines": len(objs), "hello": hello, "stats_lines": len(stats), "open_lines": len(opens),
        "open_mine": len(mine), "hits": len(good),
        "channels": sorted({int(o.get("ch", 0) or 0) for o in good}),
        "macs": sorted({str(o.get("mac")) for o in good}),
        "rssi_min": min(rssi) if rssi else None, "rssi_max": max(rssi) if rssi else None,
        "open_frames": max((int(o.get("n", 0) or 0) for o in good), default=0),
        "other_channels": sorted({int(o.get("ch", 0) or 0) for o in mine if o not in good}),
        "foreign": Counter(f"{o.get('sys')}:{o.get('id')}" for o in opens if o not in mine),
    }
    ev["foreign"] = dict(ev["foreign"])
    if len(good) >= min_hits:
        return Verdict(True, f"{len(good)} reports of {sysv} id {want_id} on ch {channel} "
                       f"({ev['open_frames']} frames)", len(good), min_hits, ev)
    if not objs:
        why = "listener printed nothing parseable (not flashed, wrong port, or board in the bootloader)"
    elif not hello:
        why = "listener never printed its hello line (did not boot, or the capture missed the reset)"
    elif mine:
        why = f"heard the fake {sysv} but on ch {ev['other_channels']}, not {channel}: {len(good)}/{min_hits}"
    elif opens:
        why = f"heard open links {ev['foreign']}, none the fake {sysv} (someone else's link?)"
    elif not stats:
        why = "listener booted but printed no stats lines (hung?)"
    else:
        data = max((int(o.get("data", 0) or 0) for o in stats), default=0)
        why = f"listener is alive ({data} data frames seen) but heard no {sysv}: 0/{min_hits}"
    return Verdict(False, why, len(good), min_hits, ev)


def open_ready(text: str, system: str, channel: int) -> bool:
    """rid-beacon is sending only the wanted fake link, on the wanted channel."""
    tx = [o for o in parse_lines(text) if o.get("t") == "tx"]
    if not tx:
        return False
    last = tx[-1]
    rid_on = [c for c in str(last.get("on", "")) if c.isupper()]
    return (last.get("open_sys") == system and int(last.get("ch", 0) or 0) == channel
            and int(last.get("open", 0) or 0) > 0 and not rid_on)


def judge_open_beacon(text: str, system: str, channel: int) -> dict:
    """Context for a FAIL (never decides PASS): was the beacon sending the fake link?"""
    objs = parse_lines(text)
    tx = [o for o in objs if o.get("t") == "tx"]
    on = [o for o in tx if o.get("open_sys") == system]
    sent = sum(int(o.get("open", 0) or 0) for o in on)
    fails = sum(int(o.get("fail", 0) or 0) for o in on)
    note = []
    if not objs:
        note.append("transmitter printed nothing")
    elif not tx:
        note.append("transmitter printed no tx lines")
    elif not on:
        note.append(f"transmitter never had {system} on (last open_sys={tx[-1].get('open_sys')!r})")
    elif sent == 0:
        note.append(f"transmitter had {system} on but sent 0 frames")
    if on and int(on[-1].get("ch", 0) or 0) != channel:
        note.append(f"transmitter was on ch {on[-1].get('ch')}, wanted {channel}")
    if fails:
        note.append(f"transmitter reports {fails} failed sends")
    return {"tx_lines": len(tx), "sent": sent, "failed": fails,
            "ch": on[-1].get("ch") if on else None, "notes": note}
