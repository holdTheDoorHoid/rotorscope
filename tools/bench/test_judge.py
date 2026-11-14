"""Tests for judge.py against saved sample logs. Run:  python3 -m unittest discover -s tools/bench

The logs in samples/ are SYNTHETIC: written by hand from the output formats documented in the
firmware READMEs, not captured from hardware. When real captures exist (bench-logs/<ts>/), copy
a good and a bad one in here and add them to the cases below; if the real format differs, this
is where the difference shows up first.
"""
import pathlib
import unittest

import judge
import run

S = pathlib.Path(__file__).parent / "samples"


def rx(name, tr, band, m=3):
    return judge.judge_listener((S / name).read_text(), tr, band, m)


class Listener(unittest.TestCase):
    def test_pass_cases(self):
        for name, tr, band in [
            ("rx_wifi_beacon_pass.log", "WifiBeacon", "2.4"),
            ("rx_wifi_nan_pass.log", "WifiNan", "2.4"),
            ("rx_wifi5_beacon_pass.log", "WifiBeacon", "5"),
            ("rx_ble4_pass.log", "BleLegacy", None),
            ("rx_ble_lr_pass.log", "BleLongRange", None),
            ("rx_ident_as_hex_pass.log", "BleExtended", None),
            ("rx_garbled_first_line.log", "WifiBeacon", "2.4"),
        ]:
            with self.subTest(name):
                v = rx(name, tr, band)
                self.assertTrue(v.passed, v.reason)
                self.assertGreaterEqual(v.hits, 3)

    def test_evidence(self):
        v = rx("rx_wifi5_beacon_pass.log", "WifiBeacon", "5")
        self.assertEqual(v.hits, 10)
        self.assertEqual(v.evidence["channels"], [149])
        self.assertEqual(v.evidence["macs"], ["02:52:49:44:00:43"])
        self.assertLess(v.evidence["rssi_min"], v.evidence["rssi_max"] + 1)

    def test_wrong_transport_fails_and_says_so(self):
        v = rx("rx_wifi_nan_wrong_transport.log", "WifiNan", "2.4")
        self.assertFalse(v.passed)
        self.assertIn("WifiBeacon", v.reason)

    def test_wrong_band_fails(self):
        v = rx("rx_wifi5_wrong_band.log", "WifiBeacon", "5")
        self.assertFalse(v.passed)
        self.assertEqual(v.hits, 0)

    def test_threshold_edge(self):
        self.assertFalse(rx("rx_too_few.log", "WifiBeacon", "2.4", m=3).passed)
        self.assertTrue(rx("rx_too_few.log", "WifiBeacon", "2.4", m=2).passed)

    def test_someone_elses_drone_does_not_count(self):
        v = rx("rx_foreign_drone.log", "WifiBeacon", "2.4")
        self.assertFalse(v.passed)
        self.assertEqual(v.evidence["foreign_rid"], 15)
        self.assertIn("none with", v.reason)

    def test_messages_without_basic_id_do_not_count(self):
        self.assertFalse(rx("rx_ble4_only_other_messages.log", "BleLegacy", None).passed)

    def test_listener_alive_but_deaf(self):
        v = rx("rx_silent.log", "BleLongRange", None)
        self.assertFalse(v.passed)
        self.assertIn("alive but heard no", v.reason)

    def test_listener_dead(self):
        self.assertIn("never printed its hello", rx("rx_no_hello.log", "WifiBeacon", "2.4").reason)
        self.assertIn("nothing parseable", judge.judge_listener("", "WifiBeacon", "2.4", 3).reason)
        v = rx("rx_bootloader_only.log", "WifiBeacon", "2.4")
        self.assertFalse(v.passed)
        self.assertIn("nothing parseable", v.reason)

    def test_bad_band_name(self):
        with self.assertRaises(ValueError):
            judge.band_ok("6", 1)


class Beacon(unittest.TestCase):
    def tx(self, name, tr, band=None):
        return judge.judge_beacon((S / name).read_text(), tr, band)

    def test_ok(self):
        r = self.tx("tx_wifi_beacon_ok.log", "WifiBeacon", "2.4")
        self.assertEqual(r["notes"], [])
        self.assertEqual(r["sent"], 5)

    def test_ready_means_isolated(self):
        self.assertFalse(judge.tx_ready((S / "tx_not_isolated.log").read_text(), "BleLongRange"))
        self.assertTrue(judge.tx_ready((S / "tx_wifi_beacon_ok.log").read_text(), "WifiBeacon"))
        self.assertFalse(judge.tx_ready("", "WifiBeacon"))

    def test_notes(self):
        self.assertTrue(any("not enabled" in n for n in self.tx("tx_nan_not_enabled.log", "WifiNan")["notes"]))
        self.assertTrue(any("isolation" in n for n in self.tx("tx_not_isolated.log", "BleLongRange")["notes"]))
        self.assertTrue(any("printed nothing" in n for n in self.tx("tx_silent.log", "WifiBeacon")["notes"]))
        self.assertTrue(any("band" in n for n in self.tx("tx_wifi_beacon_ok.log", "WifiBeacon", "5")["notes"]))


class Hub(unittest.TestCase):
    def hub(self, name, link=0, m=10):
        return judge.judge_hub((S / name).read_text(), link, m)

    def test_pass(self):
        v = self.hub("hub_link0_pass.log")
        self.assertTrue(v.passed, v.reason)
        self.assertEqual(v.evidence["frames"], 40)

    def test_no_bytes(self):
        v = self.hub("hub_no_bytes.log")
        self.assertFalse(v.passed)
        self.assertIn("no bytes at all", v.reason)

    def test_wrong_pin(self):
        v = self.hub("hub_wrong_link.log")
        self.assertFalse(v.passed)
        self.assertIn("wrong hub pin", v.reason)

    def test_silent_and_missing_link(self):
        self.assertFalse(self.hub("hub_silent.log").passed)
        self.assertIn("only 3", self.hub("hub_link0_pass.log", link=5).reason)


class Matrix(unittest.TestCase):
    """The test table in run.py: every entry must be self-consistent."""

    def test_table(self):
        self.assertGreaterEqual(len(run.TESTS), 20)
        for t in run.TESTS.values():
            with self.subTest(t.id):
                self.assertIn(t.transport or "hub", (*judge.TRANSPORTS, "hub"))
                if t.kind == "rf":
                    self.assertIn(t.tx_keys[0], "x")
                    self.assertEqual(t.band is None, t.transport.startswith("Ble"))
                    self.assertNotEqual(t.tx_board, t.rx_board)
                    # 5 GHz needs C5 at both ends: the S3 has no 5 GHz radio.
                    if t.band == "5":
                        self.assertEqual((t.tx_chip, t.rx_chip), ("c5", "c5"))
                    # node-wifi on a C5 listens on 5 GHz, or on 2.4 GHz when built `band24`.
                    if t.rx_chip == "c5" and t.transport.startswith("Wifi"):
                        self.assertEqual(t.band, "2.4" if t.rx_variant == "band24" else "5")
        self.assertEqual(len({t.id for t in run.TESTS.values()}), len(run.TESTS))


# Real lines, from the first on-air run (bench-logs/20261114-180904/open-wfb-ng-ch149).
OPEN_RX = """\
{"t":"hello","fw":"node-wifi"}
{"t":"open","sys":"WfbNg","id":8280045,"port":0,"dir":"AirToGround","ch":149,"rssi":-42,"mac":"57:42:7e:57:ed:00","up_us":1040345,"n":4}
{"t":"open","sys":"WfbNg","id":8280045,"port":0,"dir":"AirToGround","ch":149,"rssi":-42,"mac":"57:42:7e:57:ed:00","up_us":2040372,"n":19}
{"t":"open","sys":"WfbNg","id":8280045,"port":0,"dir":"AirToGround","ch":149,"rssi":-43,"mac":"57:42:7e:57:ed:00","up_us":3040347,"n":34}
{"t":"stats","up_us":29794716,"ch":149,"seen":643,"fps":24,"data":560,"open":429}
"""
OPEN_TX = """\
{"t":"tx","up":44,"band":"5","ch":149,"beacon":0,"nan":0,"ble4":0,"ble5":0,"ble_lr":0,"open_sys":"wfb-ng","open":20,"fail":0,"on":"wnlec"}
"""


class OpenLinks(unittest.TestCase):
    def test_pass(self):
        v = judge.judge_open(OPEN_RX, "wfb-ng", 149, 3)
        self.assertTrue(v.passed, v.reason)
        self.assertEqual(v.evidence["rssi_min"], -43)
        self.assertEqual(v.evidence["open_frames"], 34)

    def test_wrong_channel_system_or_id(self):
        self.assertIn("on ch [149]", judge.judge_open(OPEN_RX, "wfb-ng", 165, 3).reason)
        self.assertIn("none the fake", judge.judge_open(OPEN_RX, "rubyfpv", 149, 3).reason)
        # The real default link ID is someone else's link, never ours.
        foreign = OPEN_RX.replace("8280045", str(0x7505D6))
        v = judge.judge_open(foreign, "wfb-ng", 149, 3)
        self.assertFalse(v.passed)
        self.assertIn("someone else", v.reason)

    def test_too_few(self):
        self.assertFalse(judge.judge_open(OPEN_RX, "wfb-ng", 149, 4).passed)

    def test_silent_listener(self):
        v = judge.judge_open('{"t":"hello"}\n{"t":"stats","data":12}\n', "openhd", 6, 3)
        self.assertIn("12 data frames", v.reason)

    def test_ready(self):
        self.assertTrue(judge.open_ready(OPEN_TX, "wfb-ng", 149))
        self.assertFalse(judge.open_ready(OPEN_TX, "wfb-ng", 165))
        self.assertFalse(judge.open_ready(OPEN_TX.replace("wnlec", "Wnlec"), "wfb-ng", 149))
        self.assertFalse(judge.open_ready(OPEN_TX.replace('"open":20', '"open":0'), "wfb-ng", 149))

    def test_table(self):
        opens = [t for t in run.TESTS.values() if t.kind == "open"]
        self.assertEqual(len(opens), 16)
        for t in opens:
            with self.subTest(t.id):
                self.assertTrue(t.tx_keys.startswith("x"))
                self.assertEqual(t.rx_variant == "band24", t.channel == 6)
                self.assertEqual((t.tx_board, t.rx_board), ("c5b", "c5a"))


if __name__ == "__main__":
    unittest.main()
