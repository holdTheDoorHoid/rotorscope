//! Bench only (`gwforge` feature, fenced with `ROTORSCOPE_BENCH=1`): a
//! captured gateway, for the bench checks of docs/LINK-KEYS-DESIGN.md
//! section 9. Typed on USB, one per line:
//!
//! | Command | Does | The hub / node should show |
//! |---|---|---|
//! | `gwforge batch` | sends the hub a made-up packet claiming the first roster address | hub `bad_tag` for that slot |
//! | `gwforge flip <mac>` | alters one byte of the next transmit to that node | node `bad_tag` |
//! | `gwforge replay` | sends the hub the last relayed packet again | hub `replay` |
//! | `gwforge delay <ms>` | holds the next urgent transmit that long, past the 300 ms rule | node `late` |
//! | `gwforge drop <mac> <percent>` | discards that share of the address's packets after the radio acknowledged them | hub `LinkLoss` within 60 s |
//! | `gwforge drop off` | stops dropping | |
//!
//! Nothing here transmits anything of its own: it alters, holds or drops
//! the kit's own traffic, and the made-up and replayed packets go to the hub
//! over the wire only. No key is involved: a captured gateway has none.

use core::cell::RefCell;
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use airlink::relay::{DropPattern, Forge, parse_forge};
use critical_section::Mutex;
use embassy_time::{Duration, Timer};
use link::gw::{self, GwUp};

use crate::usb::usb_line;
use crate::{Mac, UP};

struct State {
    batch: bool,
    replay: bool,
    flip: Option<[u8; 6]>,
    delay: Option<u16>,
    drop: Option<([u8; 6], DropPattern)>,
    /// The last `GwUp::Packet` message as written to the hub.
    last: [u8; gw::MAX_GW_FRAME],
    last_len: usize,
}

static STATE: Mutex<RefCell<State>> = Mutex::new(RefCell::new(State {
    batch: false,
    replay: false,
    flip: None,
    delay: None,
    drop: None,
    last: [0; gw::MAX_GW_FRAME],
    last_len: 0,
}));
static BATCHES: AtomicU32 = AtomicU32::new(0);
static REPLAYS: AtomicU32 = AtomicU32::new(0);
static FLIPS: AtomicU32 = AtomicU32::new(0);
static DELAYS: AtomicU32 = AtomicU32::new(0);
static DROPPED: AtomicU32 = AtomicU32::new(0);
static SEQ: AtomicU32 = AtomicU32::new(0x4000);

/// `gwforge drop`: discard this packet from `src`?
pub fn drop_this(src: &[u8; 6]) -> bool {
    let d = critical_section::with(|cs| match STATE.borrow_ref_mut(cs).drop.as_mut() {
        Some((mac, p)) if mac == src => p.should_drop(),
        _ => false,
    });
    if d {
        DROPPED.fetch_add(1, Relaxed);
    }
    d
}

/// `gwforge flip` / `delay` on a transmit from the hub: the bytes to send
/// (altered in `scratch` for a flip) and the hold for a delay.
pub fn on_transmit<'a>(to: [u8; 6], urgent: bool, data: &'a [u8], scratch: &'a mut [u8; airlink::MAX_PACKET]) -> (&'a [u8], Option<u64>) {
    let (flip, hold) = critical_section::with(|cs| {
        let mut s = STATE.borrow_ref_mut(cs);
        let flip = s.flip == Some(to);
        if flip {
            s.flip = None;
        }
        let hold = if urgent { s.delay.take() } else { None };
        (flip, hold)
    });
    let hold = hold.map(|ms| {
        DELAYS.fetch_add(1, Relaxed);
        usb_line!(r#"{{"t":"gwforge","did":"delay","to":"{}","ms":{ms}}}"#, Mac(to));
        u64::from(ms)
    });
    if !flip || data.is_empty() || data.len() > scratch.len() {
        return (data, hold);
    }
    let n = data.len();
    scratch[..n].copy_from_slice(data);
    // A byte in the middle: inside what the tag covers, whatever the kind.
    scratch[n / 2] ^= 0x01;
    FLIPS.fetch_add(1, Relaxed);
    usb_line!(r#"{{"t":"gwforge","did":"flip","to":"{}","at":{}}}"#, Mac(to), n / 2);
    (&scratch[..n], hold)
}

/// A packet message just went to the hub: keep it for `gwforge replay`.
pub fn relayed(msg: &[u8]) {
    critical_section::with(|cs| {
        let mut s = STATE.borrow_ref_mut(cs);
        let n = msg.len().min(gw::MAX_GW_FRAME);
        s.last[..n].copy_from_slice(&msg[..n]);
        s.last_len = n;
    });
}

/// Send what the console asked for (`batch`, `replay`), from the hub writer.
pub async fn pending(buf: &mut [u8; gw::MAX_GW_FRAME]) {
    let (batch, replay) = critical_section::with(|cs| {
        let mut s = STATE.borrow_ref_mut(cs);
        let r = (s.batch, s.replay);
        s.batch = false;
        s.replay = false;
        if r.1 && s.last_len > 0 {
            let n = s.last_len;
            buf[..n].copy_from_slice(&s.last[..n]);
        }
        (r.0, r.1.then_some(s.last_len).filter(|n| *n > 0))
    });
    if let Some(n) = replay {
        crate::hubuart::write_all(&buf[..n]).await;
        REPLAYS.fetch_add(1, Relaxed);
        usb_line!(r#"{{"t":"gwforge","did":"replay","bytes":{n}}}"#);
    }
    if batch {
        let Some(src) = critical_section::with(|cs| UP.borrow_ref(cs).roster().first().copied()) else {
            usb_line!(r#"{{"t":"gwforge","err":"batch: no roster yet"}}"#);
            return;
        };
        // A version 3 Frames packet: one small frame, a made-up stamp and
        // a made-up tag. Without the node's key it cannot be anything else.
        let mut p = [0u8; airlink::HEADER + 4 + airlink::v3::TRAILER];
        p[..2].copy_from_slice(&airlink::MAGIC);
        p[2] = airlink::v3::VERSION_V3;
        p[3] = airlink::v3::Kind::Frames as u8;
        let seq = SEQ.fetch_add(1, Relaxed) as u16;
        p[4..6].copy_from_slice(&seq.to_le_bytes());
        p[6..10].copy_from_slice(&[3, 0x42, 0x42, 0]);
        for (i, b) in p[10..].iter_mut().enumerate() {
            *b = 0xA5 ^ i as u8;
        }
        if let Ok(m) = gw::encode_up(&GwUp::Packet { src, rssi: -50, held_us: 0, data: &p }, buf) {
            let n = m.len();
            crate::hubuart::write_all(&buf[..n]).await;
            BATCHES.fetch_add(1, Relaxed);
            usb_line!(r#"{{"t":"gwforge","did":"batch","claims":"{}"}}"#, Mac(src));
        }
    }
}

/// Once a second with the stats.
pub fn stats_line() {
    let drop = critical_section::with(|cs| STATE.borrow_ref(cs).drop.map(|(m, p)| (m, p.percent)));
    let (mac, pct) = drop.unwrap_or(([0; 6], 0));
    usb_line!(
        r#"{{"t":"gwforge","batches":{},"replays":{},"flips":{},"delays":{},"dropped":{},"drop_mac":"{}","drop_pct":{pct}}}"#,
        BATCHES.load(Relaxed),
        REPLAYS.load(Relaxed),
        FLIPS.load(Relaxed),
        DELAYS.load(Relaxed),
        DROPPED.load(Relaxed),
        Mac(mac)
    );
}

/// The console: one `gwforge ...` command per line on USB.
#[embassy_executor::task]
pub async fn console(mut rx: esp_hal::usb::usb_serial_jtag::UsbSerialJtagRx<'static, esp_hal::Async>) {
    let mut line: heapless::Vec<u8, 64> = heapless::Vec::new();
    let mut buf = [0u8; 32];
    usb_line!(r#"{{"t":"gwforge","note":"GWFORGE BENCH BUILD: this gateway behaves as a captured one. Commands: gwforge batch | flip <mac> | replay | delay <ms> | drop <mac> <percent> | drop off"}}"#);
    loop {
        Timer::after(Duration::from_millis(50)).await;
        let n = rx.drain_rx_fifo(&mut buf);
        for &b in &buf[..n] {
            if b != b'\r' && b != b'\n' {
                if line.push(b).is_err() {
                    line.clear();
                }
                continue;
            }
            let text = core::str::from_utf8(&line).unwrap_or("").trim();
            if !text.is_empty() {
                match parse_forge(text) {
                    Some(f) => apply(f),
                    None => usb_line!(r#"{{"t":"gwforge","err":"unknown; gwforge batch | flip <mac> | replay | delay <ms> | drop <mac> <percent> | drop off"}}"#),
                }
            }
            line.clear();
        }
    }
}

fn apply(f: Forge) {
    critical_section::with(|cs| {
        let mut s = STATE.borrow_ref_mut(cs);
        match f {
            Forge::Batch => s.batch = true,
            Forge::Replay => s.replay = true,
            Forge::Flip(m) => s.flip = Some(m),
            Forge::Delay(ms) => s.delay = Some(ms),
            Forge::Drop(m, p) => s.drop = Some((m, DropPattern::new(p))),
            Forge::DropOff => s.drop = None,
        }
    });
    crate::OUT_WAKE.signal(());
    usb_line!(r#"{{"t":"gwforge","armed":"{:?}"}}"#, f);
}
