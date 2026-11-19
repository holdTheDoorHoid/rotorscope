//! The ESP-NOW link glue shared by node-wifi, node-ble, node-sdr and node-lr
//! (feature `espnow`; docs/LINK-KEYS-DESIGN.md 4.4, 7.3, 7.4). The decisions
//! are `nodeimage::uplink` (host-tested); this is the part that needs the
//! chip: the hardware random source, the boot line, the published counters
//! and the status LED. Each image's `uplink.rs` keeps the radio (peers,
//! channel, sends) and calls in here.
//!
//! Start-up, in each image, after the Wi-Fi controller exists:
//!
//! 1. [`start`] with the [`crate::keystore::Keystore`] read at boot. It takes
//!    the random source ([`rng_source`]: `esp_hal::rng::Trng::try_new` on the
//!    S3 and C3, which only works once the radio has raised the entropy
//!    source; the radio-fed RNG on the C5, whose TRNG esp-hal 1.2 does not
//!    model), builds the [`nodeimage::uplink::Link`] and prints the boot
//!    line. No random source and a keyed record: the keyed link does not
//!    start (`no_rng`).
//! 2. Every received packet through `Link::receive`, every send through
//!    `Link::packet` / `Link::sent` / `Link::given_up`, with one monotonic
//!    clock ([`now_ms`]).
//! 3. Once a second [`publish`] the counters (for `NodeHealth`, built by
//!    [`crate::node_health`] in whichever task sends it, and for the deaf
//!    fall-back, [`heard`]); while blocked, [`blocked_tick`].
//!
//! The link relinks by itself after 60 s without a packet taken
//! (`airlink::v3::RELINK_MS`); nothing here resets it, the deaf fall-back
//! included.

use core::cell::Cell;
use core::fmt::Write as _;
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use critical_section::Mutex;
pub use nodeimage::uplink::{
    self, ANNOUNCE_MS, Counts, GATEWAY_TIMEOUT_MS, IMAGE_BLE, IMAGE_LR, IMAGE_SDR, IMAGE_WIFI, Link, MacText, Mode, Rx, Why,
};

use crate::keystore::Keystore;

/// One monotonic clock for the link (ms since boot).
pub fn now_ms() -> u64 {
    esp_hal::time::Instant::now().duration_since_epoch().as_millis()
}

/// A 32-bit value from the hardware random source, for the link's sessions.
/// Only ever called after [`start`] found the source running; the radio
/// keeps it running for good (no node ever drops its Wi-Fi or Bluetooth
/// controller), so a failure here means something is badly wrong: stop
/// rather than start a session that might repeat (the panic handler
/// reboots, and the next boot refuses the keyed link if the source is
/// still missing).
#[cfg(any(feature = "esp32s3", feature = "esp32c3"))]
pub fn trng() -> u32 {
    match esp_hal::rng::Trng::try_new() {
        Ok(t) => t.random(),
        Err(_) => panic!("TRNG source went away after the link started"),
    }
}

/// The C5: esp-hal 1.2 has no `Trng` type for it (its metadata does not
/// list the C5's TRNG), so there is nothing to ask. The hardware RNG is
/// true random once the RF subsystem runs (Espressif's TRM), and [`start`]
/// is only ever called after the Wi-Fi controller has started.
#[cfg(feature = "esp32c5")]
pub fn trng() -> u32 {
    esp_hal::rng::Rng::new().random()
}

/// The random source, if it is running: on the S3 and C3 the HAL's TRNG
/// (it exists only once the radio has raised its entropy source), on the
/// C5 the RNG after the radio started (`"radio"`, see [`trng`]); either way
/// only if its first draws pass `nodeimage::uplink::rng_sane`.
pub fn rng_source() -> Option<uplink::RngSource> {
    #[cfg(any(feature = "esp32s3", feature = "esp32c3"))]
    let (running, name) = (esp_hal::rng::Trng::try_new().is_ok(), "trng");
    #[cfg(feature = "esp32c5")]
    let (running, name) = (true, "radio");
    if !running {
        return None;
    }
    let mut d = [0u32; 8];
    for x in &mut d {
        *x = trng();
    }
    uplink::rng_sane(&d).then_some(uplink::RngSource { next: trng, name })
}

/// Start the link (after the radio) and print the boot line with `say`.
#[inline(never)]
pub fn start(ks: Keystore, say: impl Fn(&str)) -> Link {
    if !matches!(ks.found, nodeimage::keystore::Found::At { .. }) {
        let mut s: heapless::String<224> = heapless::String::new();
        let _ = write!(
            s,
            r#"{{"t":"warn","what":"rskey","why":"{}","note":"no usable rskey partition: flash with tools/partitions-node.csv or tools/partitions-single.csv"}}"#,
            ks.found.name()
        );
        say(&s);
    }
    let link = Link::new(ks.state, ks.mac, rng_source());
    say_boot_line(&link, &say);
    link
}

/// The boot line (also the blocked node's repeated warning).
#[inline(never)]
pub fn say_boot_line(link: &Link, say: &impl Fn(&str)) {
    let mut s: heapless::String<480> = heapless::String::new();
    if link.boot_line(&mut s).is_err() {
        s.clear();
        let _ = write!(s, r#"{{"t":"warn","what":"link_key","auth":"?","state":"{}"}}"#, link.state_name());
    }
    say(&s);
}

/// The `link_auth` line, when anything was refused.
#[inline(never)]
pub fn say_rejects(c: &Counts, say: &impl Fn(&str)) {
    if c.rejects == Default::default() {
        return;
    }
    let mut s: heapless::String<320> = heapless::String::new();
    if uplink::link_auth_line(c, &mut s).is_ok() {
        say(&s);
    }
}

/// A blocked node, called often (every poll): every 10 s print the
/// warning and return the `Unprovisioned` announcement to broadcast on the
/// gateway channel. `name` and `version` name the image.
pub fn blocked_tick<'b>(
    link: &mut Link,
    image: u8,
    name: &str,
    version: &str,
    buf: &'b mut [u8; airlink::v3::UNPROVISIONED_LEN],
    say: &impl Fn(&str),
) -> Option<&'b [u8]> {
    let (_, p) = link.announce(now_ms(), image, name, version, buf)?;
    say_boot_line(link, say);
    Some(p)
}

// ----------------------------------------------------------- published --

static COUNTS: Mutex<Cell<Option<Counts>>> = Mutex::new(Cell::new(None));
/// [`Counts::heard`], published as it changes (one writer: the uplink).
static HEARD: AtomicU32 = AtomicU32::new(0);

/// Publish the link's counters for `NodeHealth` and the deaf fall-back.
/// Cheap: call after every receive pass.
pub fn publish(link: &Link) {
    HEARD.store(link.counts.heard, Relaxed);
    let c = link.counts;
    critical_section::with(|cs| COUNTS.borrow(cs).set(Some(c)));
}

/// The link's counters as last published (all zero before the link starts).
pub fn counts() -> Counts {
    critical_section::with(|cs| COUNTS.borrow(cs).get()).unwrap_or_default()
}

/// Gateway packets heard so far (taken, repeated, or genuine but refused
/// for their epoch): any change means the gateway was heard. What the deaf
/// fall-back watches.
pub fn heard() -> u32 {
    HEARD.load(Relaxed)
}

// ------------------------------------------------------------ status LED --

/// The board's status LED, blinking while the node is blocked (missing,
/// damaged or wrong-board record, or no TRNG). The XIAO's user LED is
/// active low: off is high.
pub struct Led {
    pin: Option<esp_hal::gpio::Output<'static>>,
    on: bool,
}

impl Led {
    pub fn new(pin: impl esp_hal::gpio::OutputPin + 'static) -> Self {
        let out = esp_hal::gpio::Output::new(pin, esp_hal::gpio::Level::High, esp_hal::gpio::OutputConfig::default());
        Led { pin: Some(out), on: false }
    }

    /// A board without one.
    pub const fn none() -> Self {
        Led { pin: None, on: false }
    }

    /// Call often: blinks at 1 Hz while `blocked`, off otherwise.
    pub fn update(&mut self, blocked: bool) {
        let want = blocked && (now_ms() / 500) % 2 == 0;
        if want == self.on {
            return;
        }
        self.on = want;
        if let Some(p) = &mut self.pin {
            if want { p.set_low() } else { p.set_high() }
        }
    }
}
