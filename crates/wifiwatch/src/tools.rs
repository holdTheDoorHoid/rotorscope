//! Known attack tools' own over-the-air signatures.
//!
//! [`TOOLS`] is the table the hub uses to name `ThreatKind::AttackTool`
//! sightings: `ThreatSighting::detail` is an index into it. Entries are only
//! ever appended (the index travels on the wire); retire one by leaving its
//! name and never matching it.
//!
//! Only facts about what the tools put on the air are used here, taken from
//! their public documentation or source; no code from them is copied.
//!
//! | # | Tool | What gives it away | Source |
//! |---|---|---|---|
//! | 0 | Pwnagotchi | Beacons whose transmitter address is `de:ad:be:ef:de:ad`, carrying its peer advertisement in elements 222-226 (222 = payload chunks, 223 = compression flag, 224 identity, 225 signature, 226 stream header). | pwngrid `wifi/defines.go` (`SignatureAddr`, `IDWhisper*`) and `wifi/pack.go`, github.com/evilsocket/pwngrid |
//! | 1 | Wi-Fi Pineapple | Its open setup network is named `Pineapple_XXXX` (last four hex digits of its MAC); Hak5 hardware uses the OUI `00:13:37` (also seen locally administered as `02:13:37`). | Hak5 Mark VII setup docs (docs.hak5.org, "Pineapple_XXXX"); Lab401 PineScan write-up (OUI list) |
//! | 2 | ESP8266 Deauther | Its control access point defaults to `pwned` (password `deauther`). | SpacehuhnTech esp8266_deauther wiki, "Setup" |
//! | 3 | Ghost ESP | Its web-UI access point defaults to `GhostNet`. | Ghost ESP zine (Iffy Books) and release notes v1.4.9 ("GhostNet as fallback default webUI credentials") |
//! | 4 | RemoteIDSpoofer | Its setup access point is `ESP_RIDS` (up for two minutes at boot, before it starts sending fake Remote ID). | github.com/jjshoots/RemoteIDSpoofer `RemoteIDSpoofer/frontend.cpp` line 8 (`WiFi.softAP("ESP_RIDS", ...)`), commit c79ab77 |
//! | 5 | DroneSploit | A drone renamed by its "change AP SSID" modules with the default new name: `TELLO-abcdef` (DJI Tello) or `C-me-abcdef` (Hobbico C-me). A drone wearing it has been taken over. | github.com/dhondta/dronesploit `src/dronesploit/_src/modules/command/dji/tello.py` line 28 and `.../command/hobicco/cme.py` line 47 (`NEW_SSID` defaults), commit 6db6f12 |
//! | 6 | Wifiphisher | Its rogue access point's address is randomised under the OUI `00:00:00` (`DEFAULT_OUI`) unless run with `--no-mac-randomization` / `-iAM`. | github.com/wifiphisher/wifiphisher `wifiphisher/common/constants.py` line 50, `wifiphisher/common/interfaces.py` lines 932-943 (`generate_random_address`), `wifiphisher/pywifiphisher.py` lines 526-529 (applied to the AP interface), commit 4ae3365 |
//!
//! Not in the table, on purpose:
//! - **ESP32 Marauder** (also the Flipper Zero Wi-Fi dev board's usual
//!   firmware) has no fixed beacon of its own: its beacon spam uses random or
//!   user-chosen names and its "Rick Roll" list sends song lyrics as names.
//!   The beacon-flood, Karma and deauth detectors catch what it does; the
//!   lyric names are not stored here.
//! - Tools that only deauth (aireplay-ng, mdk4) are caught by behaviour.
//! - **Drone hijack tools** (SkyJack, DroneJack, Drone-Hacking-Tool,
//!   DroneSploit's attack modules, Hack-a-drone) drive aireplay-ng and the
//!   operating system's own Wi-Fi client: nothing of theirs is fixed on the
//!   air (checked in their sources: samyk/skyjack `skyjack.pl` lines 16 and
//!   120, brospars/wic-ter-dronejack `attack/deauth-takeover/app.js` lines 50
//!   and 150, HKSSY/Drone-Hacking-Tool `drone_hacking_tool.py` lines 935,
//!   1075, 1215). `crate::drones` catches what they do.

use crate::mac::BssInfo;

/// Names for `ThreatSighting::detail` of `ThreatKind::AttackTool`.
pub const TOOLS: &[&str] = &[
    "Pwnagotchi",
    "Wi-Fi Pineapple",
    "ESP8266 Deauther",
    "Ghost ESP",
    "RemoteIDSpoofer",
    "DroneSploit",
    "Wifiphisher",
];

pub const PWNAGOTCHI: u8 = 0;
pub const PINEAPPLE: u8 = 1;
pub const DEAUTHER: u8 = 2;
pub const GHOST_ESP: u8 = 3;
pub const RID_SPOOFER: u8 = 4;
pub const DRONESPLOIT: u8 = 5;
pub const WIFIPHISHER: u8 = 6;

/// pwngrid's fixed transmitter address.
pub const PWNAGOTCHI_ADDR: [u8; 6] = [0xDE, 0xAD, 0xBE, 0xEF, 0xDE, 0xAD];
/// Hak5's OUI, and the same with the locally administered bit set.
const HAK5_OUI: [u8; 3] = [0x00, 0x13, 0x37];
const HAK5_OUI_LA: [u8; 3] = [0x02, 0x13, 0x37];
/// Wifiphisher's `DEFAULT_OUI`.
const WIFIPHISHER_OUI: [u8; 3] = [0x00, 0x00, 0x00];

/// The tool a beacon or probe response comes from, if it matches one.
/// `tx` is the transmitter address (addr2).
pub fn match_tool(tx: &[u8; 6], info: &BssInfo<'_>) -> Option<u8> {
    // Pwnagotchi: the address alone, or the payload element carrying the
    // peer's JSON advertisement (uncompressed starts with `{`; compressed
    // ones come with the compression element 223).
    let whisper_payload = info.whisper & 1 != 0 && (info.whisper_first == Some(b'{') || info.whisper & 0b10 != 0);
    if *tx == PWNAGOTCHI_ADDR || whisper_payload {
        return Some(PWNAGOTCHI);
    }
    if tx[..3] == HAK5_OUI || tx[..3] == HAK5_OUI_LA || pineapple_setup_name(info.ssid) {
        return Some(PINEAPPLE);
    }
    if tx[..3] == WIFIPHISHER_OUI && tx[3..] != [0, 0, 0] {
        return Some(WIFIPHISHER);
    }
    match info.ssid {
        b"pwned" => Some(DEAUTHER),
        b"GhostNet" => Some(GHOST_ESP),
        b"ESP_RIDS" => Some(RID_SPOOFER),
        b"TELLO-abcdef" | b"C-me-abcdef" => Some(DRONESPLOIT),
        _ => None,
    }
}

/// `Pineapple_` followed by exactly four hex digits.
fn pineapple_setup_name(ssid: &[u8]) -> bool {
    ssid.len() == 14 && ssid.starts_with(b"Pineapple_") && ssid[10..].iter().all(u8::is_ascii_hexdigit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac::build::*;
    use crate::mac::{bss_info, header};

    fn tool(tx: [u8; 6], f: &[u8]) -> Option<u8> {
        match_tool(&tx, &bss_info(&header(f).unwrap(), f).unwrap())
    }

    const AP: [u8; 6] = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];

    #[test]
    fn table_is_named() {
        assert_eq!(TOOLS.len(), 7);
        assert_eq!(TOOLS[usize::from(WIFIPHISHER)], "Wifiphisher");
        assert_eq!(TOOLS[usize::from(PWNAGOTCHI)], "Pwnagotchi");
        assert_eq!(TOOLS[usize::from(GHOST_ESP)], "Ghost ESP");
    }

    #[test]
    fn pwnagotchi() {
        // Address match, no SSID (pwngrid adds none).
        let f = mgmt(8, [0xFF; 6], PWNAGOTCHI_ADDR, AP, &[0; 12]);
        assert_eq!(tool(PWNAGOTCHI_ADDR, &f), Some(PWNAGOTCHI));
        // Payload element from another address (a fork that changed it).
        let f = beacon(AP, b"", false, &ie(222, b"{\"face\":\"(-_-)\"}"));
        assert_eq!(tool(AP, &f), Some(PWNAGOTCHI));
        let mut extra = ie(222, &[0x1F, 0x8B]);
        extra.extend(ie(223, &[1]));
        assert_eq!(tool(AP, &beacon(AP, b"", false, &extra)), Some(PWNAGOTCHI));
        // A lone element 222 with other content is not enough.
        assert_eq!(tool(AP, &beacon(AP, b"x", false, &ie(222, &[0x01]))), None);
    }

    #[test]
    fn pineapple() {
        assert_eq!(tool(AP, &open(AP, b"Pineapple_1A2b")), Some(PINEAPPLE));
        assert_eq!(tool(AP, &open(AP, b"Pineapple_1A2")), None);
        assert_eq!(tool(AP, &open(AP, b"Pineapple_Home")), None);
        let hak5 = [0x00, 0x13, 0x37, 1, 2, 3];
        assert_eq!(tool(hak5, &wpa2(hak5, b"CoffeeShop")), Some(PINEAPPLE));
        let hak5la = [0x02, 0x13, 0x37, 1, 2, 3];
        assert_eq!(tool(hak5la, &open(hak5la, b"x")), Some(PINEAPPLE));
    }

    #[test]
    fn defaults() {
        assert_eq!(tool(AP, &wpa2(AP, b"pwned")), Some(DEAUTHER));
        assert_eq!(tool(AP, &wpa2(AP, b"GhostNet")), Some(GHOST_ESP));
        assert_eq!(tool(AP, &wpa2(AP, b"pwned2")), None);
        assert_eq!(tool(AP, &wpa2(AP, b"Home")), None);
    }

    #[test]
    fn drone_tools() {
        assert_eq!(tool(AP, &wpa2(AP, b"ESP_RIDS")), Some(RID_SPOOFER));
        assert_eq!(tool(AP, &open(AP, b"TELLO-abcdef")), Some(DRONESPLOIT));
        assert_eq!(tool(AP, &open(AP, b"C-me-abcdef")), Some(DRONESPLOIT));
        // Exact names only: a real Tello's own name, another case.
        assert_eq!(tool(AP, &open(AP, b"TELLO-ABCDEF")), None);
        assert_eq!(tool(AP, &open(AP, b"TELLO-5A1B2C")), None);
        assert_eq!(tool(AP, &open(AP, b"ESP_RIDS2")), None);
        let phish = [0x00, 0x00, 0x00, 0x4F, 0x12, 0x9A];
        assert_eq!(tool(phish, &open(phish, b"TELLO-5A1B2C")), Some(WIFIPHISHER));
        // An all-zero transmitter is a broken frame, not a tool.
        assert_eq!(tool([0; 6], &open([0; 6], b"x")), None);
    }
}
