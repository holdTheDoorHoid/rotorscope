//! Bench only (`forgetest`, docs/BENCH.md "Origin checks"): send a frame
//! this node must never originate, to see the gateway and the hub refuse
//! it. Type on USB, + Enter:
//!
//! - `forge hubfix`: a `HubFix` placing the kit at 0 N, 0 E with a clock in
//!   2001 (what an attacker would use to move the web head's map and clock);
//! - `forge csbee`: an ADS-B line for a made-up aircraft;
//! - `forge flags`: an FAA flag report;
//! - `forge via`: a `Via` naming 02:00:00:00:00:99, then a `Stats` (the
//!   frame the `Via` would steal);
//! - `forge hello`: a `Hello` claiming the gateway role, "gw key FAKE";
//! - `forge threat`: a `ForgedLink` report naming 02:00:00:00:00:99;
//! - `forge join`: a wired-bus `BusJoin`.
//!
//! Each line prints `{"t":"forge","sent":"<what>"}`; the frame goes into the
//! normal hub queue (ESP-NOW batch, wired bus answer or plain UART). Expected:
//! a fixed gateway's `refused` counter goes up and the hub gets a
//! `forged_link` threat, `detail` 5, naming this node; nothing reaches the
//! hub's tracker or the web head.

use embassy_time::{Duration, Timer};

use crate::report::to_hub;

const VICTIM: [u8; 6] = [0x02, 0, 0, 0, 0, 0x99];

fn frame(what: &str) -> Option<link::Frame> {
    use link::Frame;
    Some(match what {
        "hubfix" => Frame::HubFix(link::HubFix {
            state: link::GnssState::Fix3d,
            sats: Some(12),
            position: Some(link::HubPosition { lat_e7: 0, lon_e7: 0, alt_hae_mm: Some(0), age_ms: 0 }),
            utc_ms: Some(978_307_200_000),
            utc_pps: true,
        }),
        "csbee" => Frame::Csbee(heapless::Vec::from_slice(b"#A:FA4E00,,,,,,,,,,,,,,,,0000\r\n").ok()?),
        "flags" => Frame::Flags(link::FlagReport {
            who: heapless::String::try_from("FORGED").ok()?,
            level: 3,
            items: heapless::Vec::new(),
            pack_date: 0,
        }),
        "via" => Frame::Via { node: VICTIM, rssi: -20 },
        "hello" => Frame::Hello {
            role: link::NodeRole::Gateway,
            protocol: link::PROTOCOL_VERSION,
            firmware: heapless::String::try_from("gw key FAKE").ok()?,
        },
        "threat" => Frame::Threat(link::ThreatSighting {
            kind: link::ThreatKind::ForgedLink,
            channel: 6,
            freq: 24_370,
            width_khz: 0,
            level: -20,
            count: 99,
            window_ms: 1000,
            source: VICTIM,
            target: [0; 6],
            detail: 1,
            ssid: heapless::String::new(),
            uptime_us: 0,
        }),
        "join" => Frame::BusJoin { mac: VICTIM, want: 1, check: 0 },
        _ => return None,
    })
}

pub async fn console(mut read: impl FnMut(&mut [u8]) -> usize) -> ! {
    let mut line: heapless::Vec<u8, 32> = heapless::Vec::new();
    let mut buf = [0u8; 32];
    esp_println::println!(
        r#"{{"t":"forge","note":"FORGE TEST BUILD: type 'forge hubfix|csbee|flags|via|hello|threat|join' + Enter"}}"#
    );
    loop {
        Timer::after(Duration::from_millis(50)).await;
        let n = read(&mut buf);
        for &b in &buf[..n] {
            if b != b'\r' && b != b'\n' {
                if line.push(b).is_err() {
                    line.clear();
                }
                continue;
            }
            let cmd = core::str::from_utf8(&line).unwrap_or("").trim();
            if let Some(what) = cmd.strip_prefix("forge ") {
                match frame(what.trim()) {
                    Some(f) => {
                        to_hub(&f);
                        if matches!(f, link::Frame::Via { .. }) {
                            // The frame the Via would claim for the victim.
                            to_hub(&link::Frame::Stats {
                                uptime_us: 0,
                                frames_seen: 0,
                                rid_found: 0,
                                dropped: 0,
                                by_transport: heapless::Vec::new(),
                            });
                        }
                        esp_println::println!(r#"{{"t":"forge","sent":"{}"}}"#, what.trim());
                    }
                    None => esp_println::println!(r#"{{"t":"forge","err":"unknown; hubfix|csbee|flags|via|hello|threat|join"}}"#),
                }
            } else if !cmd.is_empty() {
                esp_println::println!(r#"{{"t":"forge","err":"type 'forge <what>'"}}"#);
            }
            line.clear();
        }
    }
}
