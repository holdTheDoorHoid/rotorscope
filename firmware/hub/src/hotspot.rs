//! The hub's own Wi-Fi hotspot (PLAN.md O1): name, password, radio setup,
//! DHCP and captive-portal DNS.
//!
//! - SSID `Rotorscope-XXXX`, XXXX = last two bytes of the chip's MAC.
//! - Channel 11. Remote ID lives on channel 6, where Node A spends ~75% of its
//!   time; the hub is the only transmitter in the box, so it stays as far from
//!   6 as the 2.4 GHz band allows. (Station mode can override this; see
//!   `station.rs`.)
//! - WPA2-PSK with a per-device password (below), or open with the
//!   `open-hotspot` feature (a bench build: `build.rs`).
//! - 192.168.4.1/24. Not 192.168.1.x or 192.168.0.x: those are the most common
//!   home-router subnets, and a laptop joined to both would get confused.
//!
//! The password (`webui::hotspot`, host-tested) is a hash of a random salt
//! kept in flash, the chip's unique-ID eFuse and its MAC. The salt is made
//! on the first boot from the true random number generator and stored in
//! the last sector of the `nvs` partition (0xe000 with espflash's default
//! table; see `webui::hotspot::salt_sector` for why that sector is free).
//! Holding BOOT and pressing `R` on USB makes a new one (a new password).
//! The password is shown only on USB, only while BOOT is held (`usb.rs`).

use core::fmt::Write as _;
use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use edge_dhcp::io::{DEFAULT_SERVER_PORT, server as dhcp_server};
use edge_dhcp::server::{Server as DhcpServer, ServerOptions};
use edge_nal::UdpBind as _;
use edge_nal_embassy::{Udp, UdpBuffers};
use embassy_net::{Ipv4Cidr, Runner, Stack, StaticConfigV4};
use embassy_sync::once_lock::OnceLock;
use embassy_time::{Duration, Timer};
use esp_bootloader_esp_idf::partitions::FlashStorage;
#[cfg(feature = "esp32s3")]
use esp_hal::peripherals::{ADC1, RNG};
use esp_hal::peripherals::FLASH;
use esp_radio::wifi::ap::AccessPointConfig;
use esp_radio::wifi::{AuthenticationMethodConfig, Interface};
use serde::Serialize;
use static_cell::StaticCell;
use webui::hotspot::{self as pw, SALT_LEN, Stored};

use crate::usb;

pub const CHANNEL: u8 = 11;
pub const IP: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);
pub const PREFIX: u8 = 24;
/// Phones joined at once. Browsers open 1-2 connections each; the HTTP server
/// runs `http::AP_TASKS` sockets.
pub const MAX_CLIENTS: u16 = 4;
/// Open hotspot (bench builds only).
pub const OPEN: bool = cfg!(feature = "open-hotspot");

/// Where this boot's password salt came from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Salt {
    /// Read from flash: the same password as last boot.
    Stored,
    /// Made and stored this boot: the first boot, or after BOOT + `R`.
    New,
    /// Could not be read or stored (the reason): a random salt for this
    /// boot only, so the hotspot is never up with a guessable password, but
    /// the password changes at every boot until the flash is fixed.
    Volatile(&'static str),
    /// `open-hotspot` build: no password at all.
    Open,
}

impl Salt {
    const fn as_str(self) -> &'static str {
        match self {
            Salt::Stored => "stored",
            Salt::New => "new",
            Salt::Volatile(_) => "this_boot_only",
            Salt::Open => "none",
        }
    }
}

/// Hotspot identity, computed once at boot.
pub struct Identity {
    pub ssid: heapless::String<32>,
    /// Empty for an open hotspot.
    pub password: heapless::String<16>,
    pub salt: Salt,
}

static IDENTITY: OnceLock<Identity> = OnceLock::new();

/// Base MAC (the Wi-Fi station address).
fn base_mac() -> [u8; 6] {
    let mac = esp_hal::efuse::base_mac_address();
    let mut out = [0u8; 6];
    out.copy_from_slice(mac.as_bytes());
    out
}

/// "Make a new salt on the next boot": survives `software_reset` in RTC
/// fast memory. A 64-bit magic, because persistent RAM holds noise after a
/// power-up and a random match must not wipe the password.
const REGEN_MAGIC: u64 = 0x7273_2d6e_6577_2d70; // "rs-new-p"
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut REGEN: u64 = 0;

/// Make a new password: ask the next boot for a new salt, and reboot now.
/// Every phone is dropped and has to join again with the new password.
pub fn regenerate_and_reset() -> ! {
    // SAFETY: plain volatile store to a static only this module touches;
    // the reset follows immediately.
    unsafe { core::ptr::addr_of_mut!(REGEN).write_volatile(REGEN_MAGIC) };
    esp_hal::system::software_reset()
}

fn take_regen_request() -> bool {
    let p = core::ptr::addr_of_mut!(REGEN);
    // SAFETY: read and clear a static only this module touches, at boot,
    // before anything else runs.
    unsafe {
        let v = p.read_volatile();
        p.write_volatile(0);
        v == REGEN_MAGIC
    }
}

/// Word-aligned bytes: esp-storage's `read_nor` / `write_nor` then use the
/// buffer as it is. (Unaligned buffers, and its plain `read` / `write`,
/// bounce through a 4 KB sector buffer on the stack, which the main stack
/// cannot spare.)
#[repr(C, align(4))]
struct Words<const N: usize>([u8; N]);

/// The salt sector, from the partition table read one entry at a time.
fn salt_sector(flash: &mut FlashStorage<'_>) -> Option<u32> {
    let mut e = Words([0u8; pw::ENTRY_LEN]);
    let mut i = 0usize;
    pw::salt_sector(core::iter::from_fn(|| {
        let at = pw::TABLE_OFFSET + (i * pw::ENTRY_LEN) as u32;
        i += 1;
        flash.read_nor(at, &mut e.0).ok()?;
        Some(e.0)
    }))
}

/// The salt sector, held between reading the stored salt and writing a new
/// one (which needs random bytes first).
pub struct SaltStore {
    flash: FlashStorage<'static>,
    /// `Err` = why the salt cannot be kept in flash.
    sector: Result<u32, &'static str>,
}

/// What `load_salt` found.
pub enum Loaded {
    /// The stored salt.
    Stored([u8; SALT_LEN]),
    /// None stored (first boot), or a new one was asked for (BOOT + `R`):
    /// get 16 true-random bytes and hand them to [`SaltStore::store`].
    NeedRandom(SaltStore),
}

/// Read the password salt from flash. Call once, first thing in `main`.
///
/// On the S3 everything happens before Wi-Fi (the password is part of its
/// configuration) and before the second core starts (esp-storage refuses
/// to write flash while it runs, and a write stops both cores' flash
/// cache): the random bytes come from [`trng_salt`]. The C5 has no ADC
/// entropy source in esp-hal 1.2, so there the radio is started first
/// (station mode, no hotspot yet) and [`radio_salt`] reads the RNG once it
/// runs; the C5 has one core.
pub fn load_salt(flash: FLASH<'static>) -> Loaded {
    let regen = take_regen_request();
    let mut flash = FlashStorage::new(flash);
    let sector = salt_sector(&mut flash).ok_or("no nvs partition in the partition table");
    if let Ok(at) = sector {
        let mut rec = Words([0u8; pw::RECORD_LEN]);
        if flash.read_nor(at, &mut rec.0).is_err() {
            return Loaded::NeedRandom(SaltStore { flash, sector: Err("flash read failed") });
        }
        match pw::decode_record(&rec.0) {
            Stored::Salt(s) if !regen => return Loaded::Stored(s),
            // Corrupt: an interrupted write, or something else's bytes
            // (nothing in this firmware uses NVS): replaced like an erased
            // sector.
            _ => {}
        }
    }
    Loaded::NeedRandom(SaltStore { flash, sector })
}

impl SaltStore {
    /// Keep `salt` as the password salt: write it and read it back. If that
    /// fails (or there is nowhere to write it), the salt is still used, for
    /// this boot only: the hotspot never comes up with a guessable password,
    /// and the failure is loud (`Salt::Volatile`, an `err` line on USB).
    ///
    /// Flash wear: one sector erase per new password.
    pub fn store(mut self, salt: [u8; SALT_LEN]) -> ([u8; SALT_LEN], Salt) {
        if !pw::salt_usable(&salt) {
            return (salt, Salt::Volatile("random number generator returned a constant"));
        }
        let at = match self.sector {
            Ok(at) => at,
            Err(why) => return (salt, Salt::Volatile(why)),
        };
        let new = Words(pw::encode_record(&salt));
        let mut back = Words([0u8; pw::RECORD_LEN]);
        let written = self.flash.erase(at, at + pw::SECTOR).is_ok()
            && self.flash.write_nor(at, &new.0).is_ok()
            && self.flash.read_nor(at, &mut back.0).is_ok()
            && back.0 == new.0;
        if written { (salt, Salt::New) } else { (salt, Salt::Volatile("flash write failed")) }
    }
}

/// 16 bytes from the true RNG: the hardware RNG with the SAR ADC as its
/// entropy source (`TrngSource`), the guarantee the radio gives once it
/// runs, available before Wi-Fi starts. ADC1 is lent and handed back.
#[cfg(feature = "esp32s3")]
pub fn trng_salt(rng: RNG<'_>, adc: ADC1<'_>) -> [u8; SALT_LEN] {
    use esp_hal::rng::{Trng, TrngSource};
    let source = TrngSource::new(rng, adc);
    let mut salt = [0u8; SALT_LEN];
    if let Ok(trng) = Trng::try_new() {
        // An all-one-value salt would be a broken RNG; read again.
        for _ in 0..4 {
            trng.read(&mut salt);
            if pw::salt_usable(&salt) {
                break;
            }
        }
    }
    // The Trng is gone here, so the source may be dropped (it asserts that).
    drop(source);
    salt
}

/// 16 bytes from the hardware RNG. Only call with the radio running: the
/// RF subsystem is then the RNG's entropy source (esp-hal `rng` docs).
#[cfg(not(feature = "esp32s3"))]
pub fn radio_salt() -> [u8; SALT_LEN] {
    let rng = esp_hal::rng::Rng::new();
    let mut salt = [0u8; SALT_LEN];
    for _ in 0..4 {
        rng.read(&mut salt);
        if pw::salt_usable(&salt) {
            break;
        }
    }
    salt
}

/// S3: settle the salt and the identity now (`load_salt`).
#[cfg(feature = "esp32s3")]
pub fn init_salt_and_identity(flash: FLASH<'static>, rng: RNG<'_>, adc: ADC1<'_>) {
    let (salt, how) = if OPEN {
        ([0; SALT_LEN], Salt::Open)
    } else {
        match load_salt(flash) {
            Loaded::Stored(s) => (s, Salt::Stored),
            Loaded::NeedRandom(store) => store.store(trng_salt(rng, adc)),
        }
    };
    init_identity(&salt, how);
}

/// C5: settle the identity now if the salt is stored; otherwise return the
/// store, to be finished with [`radio_salt`] once the radio runs (`main`
/// starts Wi-Fi without the hotspot until then).
#[cfg(not(feature = "esp32s3"))]
pub fn init_identity_unless_new(flash: FLASH<'static>) -> Option<SaltStore> {
    if OPEN {
        init_identity(&[0; SALT_LEN], Salt::Open);
        return None;
    }
    match load_salt(flash) {
        Loaded::Stored(s) => {
            init_identity(&s, Salt::Stored);
            None
        }
        Loaded::NeedRandom(store) => Some(store),
    }
}

/// Work out the SSID and password from this boot's salt. Call once, before
/// `identity()`.
pub fn init_identity(salt: &[u8; SALT_LEN], how: Salt) -> &'static Identity {
    let mac = base_mac();
    let mut ssid = heapless::String::new();
    let _ = write!(ssid, "Rotorscope-{:02X}{:02X}", mac[4], mac[5]);
    let mut password = heapless::String::new();
    if !OPEN {
        // Blank (all zero) on some chips; the salt carries the secret then.
        let uid: [u8; 16] = esp_hal::efuse::read_field_le(esp_hal::efuse::OPTIONAL_UNIQUE_ID);
        for b in pw::derive_password(salt, &uid, &mac) {
            let _ = password.push(char::from(b));
        }
    }
    let _ = IDENTITY.init(Identity { ssid, password, salt: how });
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
    /// Only with BOOT held (USB `h`); never in the boot output, which ends
    /// up in logs and bug reports.
    #[serde(skip_serializing_if = "Option::is_none")]
    password: Option<&'a str>,
    /// Where the password's salt came from: `stored`, `new`,
    /// `this_boot_only` (flash trouble; see the `err` line) or `none`.
    salt: &'static str,
    ip: &'static str,
    url: &'static str,
}

/// `{"t":"hotspot",...}` on USB. The password only when `show_password`
/// (the BOOT button is held: `usb.rs`).
pub fn print_info(show_password: bool) {
    let id = identity();
    usb::line(&InfoLine {
        t: "hotspot",
        ssid: &id.ssid,
        channel: CHANNEL,
        security: if OPEN { "open" } else { "wpa2" },
        password: (show_password && !OPEN).then_some(id.password.as_str()),
        salt: id.salt.as_str(),
        ip: "192.168.4.1",
        url: "http://192.168.4.1/",
    });
    match id.salt {
        Salt::Open => usb::msg("warn", "OPEN hotspot build: anyone nearby can join and read the track list"),
        Salt::Volatile(why) => {
            usb::msg("err", why);
            usb::msg(
                "err",
                "hotspot salt not stored: this boot's password is random and changes at every boot (hold BOOT, press h)",
            );
        }
        Salt::New if !show_password => {
            usb::msg("warn", "new hotspot password (first boot or regenerated): hold BOOT and press h to read it")
        }
        _ => {}
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
