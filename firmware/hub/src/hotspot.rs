//! The hub's own Wi-Fi hotspot (PLAN.md O1): name, password, radio setup,
//! DHCP and captive-portal DNS.
//!
//! - SSID `Rotorscope-XXXX`, XXXX = last two bytes of the chip's MAC.
//! - Channel 11. Remote ID lives on channel 6, where Node A spends ~75% of its
//!   time; the hub is the only transmitter in the box, so it stays as far from
//!   6 as the 2.4 GHz band allows. (Station mode can override this; see
//!   `station.rs`.)
//! - WPA2-PSK with a per-device password (below), or open with the
//!   `open-hotspot` feature.
//! - 192.168.4.1/24. Not 192.168.1.x or 192.168.0.x: those are the most common
//!   home-router subnets, and a laptop joined to both would get confused.

use core::fmt::Write as _;
use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use edge_dhcp::io::{DEFAULT_SERVER_PORT, server as dhcp_server};
use edge_dhcp::server::{Server as DhcpServer, ServerOptions};
use edge_nal::UdpBind as _;
use edge_nal_embassy::{Udp, UdpBuffers};
use embassy_net::{Ipv4Cidr, Runner, Stack, StaticConfigV4};
use embassy_sync::once_lock::OnceLock;
use embassy_time::{Duration, Timer};
use esp_radio::wifi::ap::AccessPointConfig;
use esp_radio::wifi::{AuthenticationMethodConfig, Interface};
use serde::Serialize;
use sha2::{Digest, Sha256};
use static_cell::StaticCell;

use crate::usb;

pub const CHANNEL: u8 = 11;
pub const IP: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);
pub const PREFIX: u8 = 24;
/// Phones joined at once. Browsers open 1-2 connections each; the HTTP server
/// runs `http::AP_TASKS` sockets.
pub const MAX_CLIENTS: u16 = 4;
/// Open hotspot (bench builds only).
pub const OPEN: bool = cfg!(feature = "open-hotspot");

/// Hotspot identity, computed once at boot.
pub struct Identity {
    pub ssid: heapless::String<32>,
    /// Empty for an open hotspot.
    pub password: heapless::String<16>,
    /// True if the password mixes in the chip's factory unique ID (secret);
    /// false if it fell back to the MAC alone (anyone can work that out from
    /// the hotspot's BSSID and this source code).
    pub secret_backed: bool,
}

static IDENTITY: OnceLock<Identity> = OnceLock::new();

/// Base MAC (the Wi-Fi station address).
fn base_mac() -> [u8; 6] {
    let mac = esp_hal::efuse::base_mac_address();
    let mut out = [0u8; 6];
    out.copy_from_slice(mac.as_bytes());
    out
}

/// Crockford base32, lower case: no i, l, o, u, so nothing is mistaken for
/// 1 or 0 when read off a small screen and typed on a phone.
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// The per-device password: `xxxx-xxxx-xxxx`, 60 bits.
///
/// Why not a hash of the MAC alone: the MAC is broadcast in every beacon (the
/// BSSID differs only in the locally-administered bit) and this code is
/// public, so anyone nearby could compute it. The chip's 128-bit
/// `OPTIONAL_UNIQUE_ID` eFuse is written at the factory and never leaves the
/// chip except through USB, so hashing it in makes the password unguessable
/// from the air while staying stable across reflashes. If that eFuse is
/// blank (all zeros) on some batch, the password falls back to the MAC alone
/// and the boot line says so (`"pw_secret":false`).
fn derive_password(mac: &[u8; 6]) -> (heapless::String<16>, bool) {
    let uid: [u8; 16] = esp_hal::efuse::read_field_le(esp_hal::efuse::OPTIONAL_UNIQUE_ID);
    let secret_backed = uid.iter().any(|&b| b != 0);
    let mut h = Sha256::new();
    h.update(b"rotorscope hotspot password v1");
    h.update(uid);
    h.update(mac);
    let digest = h.finalize();

    // 12 symbols x 5 bits from the first 60 bits of the digest.
    let bits = u64::from_be_bytes(digest[..8].try_into().unwrap_or([0; 8]));
    let mut pw = heapless::String::new();
    for i in 0..12 {
        if i > 0 && i % 4 == 0 {
            let _ = pw.push('-');
        }
        let sym = (bits >> (59 - 5 * i)) & 0x1f;
        let _ = pw.push(char::from(ALPHABET[sym as usize]));
    }
    (pw, secret_backed)
}

/// Work out the SSID and password. Call once, before `identity()`.
pub fn init_identity() -> &'static Identity {
    let mac = base_mac();
    let mut ssid = heapless::String::new();
    let _ = write!(ssid, "Rotorscope-{:02X}{:02X}", mac[4], mac[5]);
    let (password, secret_backed) = if OPEN { (heapless::String::new(), false) } else { derive_password(&mac) };
    let _ = IDENTITY.init(Identity { ssid, password, secret_backed });
    identity()
}

/// The hotspot identity (for the screen seam and USB `h`).
pub fn identity() -> &'static Identity {
    IDENTITY.try_get().expect("init_identity not called")
}

#[derive(Serialize)]
struct InfoLine<'a> {
    t: &'static str,
    ssid: &'a str,
    channel: u8,
    security: &'static str,
    /// Printed so the owner can join; the screen will show it too.
    password: &'a str,
    pw_secret: bool,
    ip: &'static str,
    url: &'static str,
}

/// `{"t":"hotspot",...}` on USB.
pub fn print_info() {
    let id = identity();
    usb::line(&InfoLine {
        t: "hotspot",
        ssid: &id.ssid,
        channel: CHANNEL,
        security: if OPEN { "open" } else { "wpa2" },
        password: &id.password,
        pw_secret: id.secret_backed,
        ip: "192.168.4.1",
        url: "http://192.168.4.1/",
    });
    if OPEN {
        usb::msg("warn", "OPEN hotspot build: anyone nearby can join and read the track list");
    } else if !id.secret_backed {
        usb::msg("warn", "OPTIONAL_UNIQUE_ID eFuse is blank: hotspot password derives from the MAC only");
    }
}

/// The access-point half of the Wi-Fi configuration.
pub fn ap_config() -> AccessPointConfig {
    let id = identity();
    let auth = if OPEN {
        AuthenticationMethodConfig::Open
    } else {
        AuthenticationMethodConfig::Wpa2Personal(id.password.as_str().try_into().expect("password length"))
    };
    AccessPointConfig::default()
        .with_ssid(id.ssid.as_str().try_into().expect("ssid length"))
        .with_channel(CHANNEL)
        .with_authentication(auth)
        .with_max_connections(MAX_CLIENTS)
}

/// Sockets on the hotspot stack: HTTP tasks + DHCP + DNS + CoT, plus one spare.
pub const AP_SOCKETS: usize = crate::http::AP_TASKS + 4;

/// Bring up embassy-net on the AP interface with the static hub address.
pub fn ap_stack(iface: Interface, seed: u64) -> (Stack<'static>, Runner<'static, Interface>) {
    static RES: StaticCell<embassy_net::StackResources<AP_SOCKETS>> = StaticCell::new();
    let config = embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(IP, PREFIX),
        gateway: Some(IP),
        dns_servers: Default::default(),
    });
    embassy_net::new(iface, config, RES.init(embassy_net::StackResources::new()), seed)
}

#[embassy_executor::task(pool_size = 2)]
pub async fn net_runner(mut runner: Runner<'static, Interface>) -> ! {
    runner.run().await
}

/// DHCP: hands out 192.168.4.50-57 (`webui::dhcp`) with the hub as router
/// and DNS, and the RFC 8910 captive-portal URL (Android 11+ and iOS 14+
/// read it and open the page without probing).
#[embassy_executor::task]
pub async fn dhcp(stack: Stack<'static>) {
    static BUFFERS: StaticCell<UdpBuffers<1, 600, 600, 2>> = StaticCell::new();
    let buffers = BUFFERS.init(UdpBuffers::new());
    let udp = Udp::new(stack, buffers);
    let mut buf = [0u8; 600];
    let mut gw = [Ipv4Addr::UNSPECIFIED];
    let dns = [IP];
    let mut options = ServerOptions::new(IP, Some(&mut gw));
    options.dns = &dns;
    options.captive_url = Some("http://192.168.4.1/");
    // Short leases: phones come and go, and a reboot of the hub loses the table.
    options.lease_duration_secs = webui::dhcp::LEASE_SECS;
    // A range exactly as large as the lease table: only then does edge-dhcp
    // reuse an expired lease once the table is full (see webui::dhcp, which
    // drives this same server type past the table size in its tests).
    let mut server = DhcpServer::<_, { webui::dhcp::LEASES }>::new_with_et(IP);
    server.range_start = Ipv4Addr::new(192, 168, 4, webui::dhcp::FIRST);
    server.range_end = Ipv4Addr::new(192, 168, 4, webui::dhcp::LAST);
    loop {
        let mut socket = match udp
            .bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, DEFAULT_SERVER_PORT)))
            .await
        {
            Ok(s) => s,
            Err(_) => {
                usb::msg("err", "dhcp bind");
                Timer::after(Duration::from_secs(1)).await;
                continue;
            }
        };
        if dhcp_server::run(&mut server, &options, &mut socket, &mut buf).await.is_err() {
            usb::msg("err", "dhcp server stopped; restarting");
        }
        drop(socket);
        Timer::after(Duration::from_millis(500)).await;
    }
}

/// Captive-portal DNS: every name resolves to the hub, so a phone's
/// connectivity check lands on our HTTP server, which redirects it to `/`.
#[embassy_executor::task]
pub async fn dns(stack: Stack<'static>) {
    static BUFFERS: StaticCell<UdpBuffers<1, 512, 512, 2>> = StaticCell::new();
    let buffers = BUFFERS.init(UdpBuffers::new());
    let udp = Udp::new(stack, buffers);
    let mut tx = [0u8; 512];
    let mut rx = [0u8; 512];
    loop {
        let r = edge_captive::io::run(
            &udp,
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 53)),
            &mut tx,
            &mut rx,
            IP,
            // Short TTL so a phone that later joins a real network does not
            // keep resolving everything to 192.168.4.1.
            core::time::Duration::from_secs(60),
        )
        .await;
        if r.is_err() {
            usb::msg("err", "captive dns stopped; restarting");
        }
        Timer::after(Duration::from_millis(500)).await;
    }
}
