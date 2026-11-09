//! Station mode (PLAN.md O5, fixed sensor): join an existing network as a
//! client, alongside the hotspot.
//!
//! Compiled only when `wifi.local.toml` exists at build time (`build.rs` sets
//! `cfg(hub_station)`); otherwise none of this is in the firmware.
//!
//! esp-radio 1.0.0-beta.1 supports AP+STA (`Config::AccessPointStation`), and
//! that is what runs. Two physical facts of a single-radio chip to know:
//! - **The hotspot follows the router's channel.** Once the station associates,
//!   the AP must move to the same channel. If the router is on channel 6, the
//!   hub then transmits on the Remote ID channel. The hub reports it
//!   (`{"t":"sta","ch":6,...}` plus a warning); pick a router channel of 1 or
//!   11 (or a 5 GHz-only SSID is not an option: the S3 is 2.4 GHz only).
//! - **Searching for the router disturbs the hotspot.** While the station is
//!   disconnected, each connect attempt scans all channels and phones on the
//!   hotspot see stalls, so retries back off to one per `RETRY`.

use embassy_net::{Runner, Stack};
use embassy_time::{Duration, Timer};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{AuthenticationMethodConfig, Interface, WifiController};
use serde::Serialize;
use static_cell::StaticCell;

use crate::usb;

pub const SSID: &str = env!("HUB_STA_SSID");
const PSK: &str = env!("HUB_STA_PSK");
const RETRY: Duration = Duration::from_secs(30);

/// Sockets on the station stack: DHCP client, CoT, HTTP, plus one spare.
const STA_SOCKETS: usize = crate::http::STA_TASKS + 3;

pub fn config() -> StationConfig {
    let auth = if PSK.is_empty() {
        AuthenticationMethodConfig::Open
    } else {
        AuthenticationMethodConfig::Wpa2Personal(PSK.try_into().expect("psk length checked by build.rs"))
    };
    StationConfig::default()
        .with_ssid(SSID.try_into().expect("ssid length checked by build.rs"))
        .with_authentication(auth)
}

pub fn stack(iface: Interface, seed: u64) -> (Stack<'static>, Runner<'static, Interface>) {
    static RES: StaticCell<embassy_net::StackResources<STA_SOCKETS>> = StaticCell::new();
    let config = embassy_net::Config::dhcpv4(Default::default());
    embassy_net::new(iface, config, RES.init(embassy_net::StackResources::new()), seed)
}

#[derive(Serialize)]
struct StaLine<'a> {
    t: &'static str,
    state: &'static str,
    ssid: &'a str,
    ch: Option<u8>,
}

/// Keep the station connected. Owns the Wi-Fi controller.
#[embassy_executor::task]
pub async fn task(mut wifi: WifiController<'static>) {
    loop {
        match wifi.connect_async().await {
            Ok(info) => {
                usb::line(&StaLine { t: "sta", state: "up", ssid: SSID, ch: Some(info.channel) });
                if info.channel == 6 {
                    usb::msg(
                        "warn",
                        "router is on channel 6: the hotspot had to follow it onto the Remote ID channel",
                    );
                }
                let _ = wifi.wait_for_disconnect_async().await;
                usb::line(&StaLine { t: "sta", state: "down", ssid: SSID, ch: None });
            }
            Err(_) => {
                usb::line(&StaLine { t: "sta", state: "fail", ssid: SSID, ch: None });
            }
        }
        Timer::after(RETRY).await;
    }
}
