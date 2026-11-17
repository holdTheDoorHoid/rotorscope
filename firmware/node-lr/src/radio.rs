//! The radio task: keeps the LR2021 on what the plan asks for, retunes at
//! FLARM / OGN slot boundaries, runs the 1090 watchdog, and turns each
//! received packet into a `Frame::Air` (or a rejection). For an
//! `AeroMode::Control` entry it hands the radio to `control.rs` for the
//! entry's dwell.
//!
//! Receive only. Every command it sends comes from `lr2021cfg`, which has
//! no transmit command.

use embassy_time::{Duration, Instant, Timer, with_timeout};
use link::{AeroMode, AirFrame, Frame};
use lr2021cfg::config::{self, Modem};
use lr2021cfg::node::{AIR_PER_S, Es1090Watchdog, PlanCursor, RateLimit, hop_target};
use lr2021cfg::{accept, cmd, roles};
use portable_atomic::Ordering::Relaxed;

use crate::board;
use crate::control::Control;
use crate::lr::Lr;
use crate::out::{self, COUNTERS, Hex, bump};
use crate::state::{self, PLAN};

/// Longest wait between checks of the plan, the clock and the watchdog.
const TICK: Duration = Duration::from_millis(100);

fn now_ms() -> u64 {
    Instant::now().as_millis()
}

/// Wait for the chip, retrying the version read every 10 s; true once it
/// answers as an LR2021 / LR2022.
async fn bring_up(lr: &mut Lr) {
    loop {
        lr.reset().await;
        match lr.version() {
            Ok((major, minor)) if cmd::version_ok(major, minor) => {
                out::line(format_args!(r#"{{"t":"radio","chip":"LR2021","fw":"{major}.{minor:#04x}"}}"#));
                if lr.run(&config::init(&board::LR2021)).is_ok() {
                    state::set_status(|s| s.radio_ok = true);
                    return;
                }
                out::line(format_args!(r#"{{"t":"warn","what":"radio","note":"init sequence failed"}}"#));
            }
            Ok((major, minor)) => {
                out::line(format_args!(r#"{{"t":"warn","what":"radio","note":"not an LR2021","version":"{major}.{minor}"}}"#))
            }
            Err(e) => out::line(format_args!(r#"{{"t":"warn","what":"radio","note":"no answer: {e:?}"}}"#)),
        }
        state::set_status(|s| {
            s.radio_ok = false;
            s.mode = 0;
        });
        Timer::after_secs(10).await;
    }
}

/// The control scan's state (energy maps and all, ~20 KB): built in place,
/// never on the stack.
static CONTROL: static_cell::ConstStaticCell<Control> = static_cell::ConstStaticCell::new(Control::new());

#[embassy_executor::task]
pub async fn run(mut lr: Lr) {
    bring_up(&mut lr).await;
    let ctl = CONTROL.take();
    let mut cursor = PlanCursor::new(&[], now_ms());
    let mut applied: Option<(AeroMode, u32)> = None;
    let mut wd = Es1090Watchdog::new(now_ms());
    let mut limit = RateLimit::new(AIR_PER_S, state::now_us());
    let mut fifo = [0u8; 64];
    let mut last_mode: Option<AeroMode> = None;
    loop {
        if let Some(a) = PLAN.try_take() {
            cursor = PlanCursor::new(&a.plan, now_ms());
            if cursor.unknown > 0 {
                out::line(format_args!(r#"{{"t":"warn","what":"plan","unknown_entries":{}}}"#, cursor.unknown));
            }
        }
        cursor.poll(now_ms());
        let mode = cursor.current();

        if let AeroMode::Control { regions, energy } = mode {
            if cursor_is_new(&mut last_mode, mode) {
                out::line(format_args!(
                    r#"{{"t":"mode","code":{},"control":{{"regions":{regions},"energy":{energy},"ids":{},"content":{}}}}}"#,
                    mode.code(),
                    cursor.flags.read_ids,
                    cursor.flags.decode_content
                ));
            }
            applied = None;
            let up = state::now_us();
            state::set_status(|s| {
                s.mode = mode.code();
                s.parked = false;
                s.time = state::time_state(up);
                s.rejected = COUNTERS.rejected.load(Relaxed);
                s.rate_dropped = COUNTERS.rate_dropped.load(Relaxed);
            });
            // To the entry's end, or a second at a time when it stays (the
            // scan keeps its place between calls).
            let until_us = match cursor.ends_ms() {
                Some(end) => up + end.saturating_sub(now_ms()) * 1000,
                None => up + 1_000_000,
            };
            if let Err(e) = ctl.run(&mut lr, regions, energy, cursor.flags, until_us).await {
                bump(&COUNTERS.radio_errors);
                out::line(format_args!(r#"{{"t":"warn","what":"radio","note":"{e:?}"}}"#));
                state::set_status(|s| s.radio_ok = false);
                ctl.forget_radio();
                Timer::after_secs(1).await;
                bring_up(&mut lr).await;
            }
            continue;
        }
        last_mode = Some(mode);

        // Where to be now, and until when.
        let up = state::now_us();
        let utc = state::utc_ms(up);
        let (freq, parked, hop_until_us) = match mode {
            AeroMode::FlarmHop { band, ogn } => match hop_target(band, ogn, utc) {
                Some(h) => {
                    let until = match (h.until_utc_ms, utc) {
                        (Some(end), Some(now)) => Some(up + end.saturating_sub(now) * 1000),
                        _ => None,
                    };
                    (h.freq_hz, h.parked, until)
                }
                // PlanCursor only keeps known bands; never here.
                None => (roles::ES1090_HZ, false, None),
            },
            m => (roles::config(m, 0).map_or(roles::ES1090_HZ, |c| c.freq_hz), false, None),
        };

        let result = match applied {
            Some((m, f)) if m == mode && f == freq => Ok(()),
            Some((m, _)) if m == mode => {
                bump(&COUNTERS.retunes);
                lr.run(&config::retune(freq))
            }
            _ => {
                out::line(format_args!(r#"{{"t":"mode","code":{},"freq_khz":{}}}"#, mode.code(), freq / 1000));
                wd = Es1090Watchdog::new(now_ms());
                ctl.forget_radio();
                lr.run(&config::apply(&board::LR2021, &roles::config(mode, freq).unwrap_or(roles::es1090())))
            }
        };
        match result {
            Ok(()) => applied = Some((mode, freq)),
            Err(e) => {
                bump(&COUNTERS.radio_errors);
                applied = None;
                out::line(format_args!(r#"{{"t":"warn","what":"radio","note":"{e:?}"}}"#));
                state::set_status(|s| s.radio_ok = false);
                ctl.forget_radio();
                Timer::after_secs(1).await;
                bring_up(&mut lr).await;
                continue;
            }
        }
        state::set_status(|s| {
            s.mode = mode.code();
            s.freq_khz = freq / 1000;
            s.parked = parked;
            s.time = state::time_state(up);
            s.rx_restarts = wd.restarts;
            s.rejected = COUNTERS.rejected.load(Relaxed);
            s.rate_dropped = COUNTERS.rate_dropped.load(Relaxed);
        });

        // Wait for a packet, the next slot edge, the plan's next entry or a tick.
        let mut wait_us = TICK.as_micros();
        if let Some(t) = hop_until_us {
            wait_us = wait_us.min(t.saturating_sub(state::now_us()));
        }
        if let Some(end) = cursor.ends_ms() {
            wait_us = wait_us.min(end.saturating_sub(now_ms()) * 1000);
        }
        let got = lr.irq.is_high() || with_timeout(Duration::from_micros(wait_us), lr.irq.wait_for_high()).await.is_ok();
        if got {
            on_irq(&mut lr, mode, freq, &mut wd, &mut limit, &mut fifo);
        }
        if mode == AeroMode::Es1090 && wd.poll(now_ms()) {
            bump(&COUNTERS.rx_restarts);
            if lr.run(&config::restart_rx()).is_err() {
                bump(&COUNTERS.radio_errors);
            }
        }
    }
}

fn on_irq(lr: &mut Lr, mode: AeroMode, freq: u32, wd: &mut Es1090Watchdog, limit: &mut RateLimit, fifo: &mut [u8; 64]) {
    let up = state::now_us();
    let irq = match lr.irq_status() {
        Ok(i) => i,
        Err(_) => {
            bump(&COUNTERS.radio_errors);
            return;
        }
    };
    if irq & cmd::irq::RX_DONE == 0 {
        return;
    }
    let (Some(cfg), Some(proto)) = (roles::config(mode, freq), roles::proto(mode)) else {
        return;
    };
    let n = match lr.packet_len() {
        Ok(n) => (n as usize).min(fifo.len()),
        Err(_) => {
            bump(&COUNTERS.radio_errors);
            return;
        }
    };
    if lr.read_fifo(&mut fifo[..n]).is_err() {
        bump(&COUNTERS.radio_errors);
        return;
    }
    let rssi = match lr.packet_status(&config::packet_status_req(&cfg.modem)) {
        Ok(rsp) if matches!(cfg.modem, Modem::Ook { .. }) => cmd::parse_ook_rssi(&rsp),
        Ok(rsp) => cmd::parse_fsk_rssi(&rsp),
        Err(_) => i8::MIN,
    };
    bump(&COUNTERS.packets);
    wd.on_rx(now_ms());
    let raw = match accept::accept(proto, &fifo[..n]) {
        Ok(raw) => raw,
        Err(_) => {
            bump(&COUNTERS.rejected);
            return;
        }
    };
    if !limit.allow(up) {
        bump(&COUNTERS.rate_dropped);
        return;
    }
    bump(&COUNTERS.air);
    let utc_ms = state::utc_ms(up);
    out::line(format_args!(r#"{{"t":"air","p":"{proto:?}","f":{},"rssi":{rssi},"raw":"{}"}}"#, freq / 1000, Hex(&raw)));
    out::hub_frame(&Frame::Air(AirFrame { proto, freq_khz: freq / 1000, rssi, utc_ms, uptime_us: up, raw }));
}

/// True the first time a control entry runs after something else (for the
/// mode line).
fn cursor_is_new(last: &mut Option<AeroMode>, mode: AeroMode) -> bool {
    let new = *last != Some(mode);
    *last = Some(mode);
    new
}
