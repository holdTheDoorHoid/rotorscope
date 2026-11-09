//! `log` records become `{"t":"log",...}` JSON lines on the USB stream.
//!
//! The USB port carries machine-readable JSON lines, so plain log text would
//! be noise a parser has to skip. Wrapping each record keeps the stream pure
//! JSON while still surfacing warnings from esp-radio and trouble-host.

use core::fmt::Write;

use log::{LevelFilter, Log, Metadata, Record};
use serde::Serialize;

use crate::adv::COUNTERS;
use crate::out::{USB, emit};

struct JsonLogger;

static LOGGER: JsonLogger = JsonLogger;

/// Level from `ESP_LOG` at build time (see `.cargo/esp-config.toml`), default
/// warn. Only a bare level is understood, not esp-println's per-module syntax.
fn level() -> LevelFilter {
    match option_env!("ESP_LOG").unwrap_or("warn").trim() {
        l if l.eq_ignore_ascii_case("off") => LevelFilter::Off,
        l if l.eq_ignore_ascii_case("error") => LevelFilter::Error,
        l if l.eq_ignore_ascii_case("info") => LevelFilter::Info,
        l if l.eq_ignore_ascii_case("debug") => LevelFilter::Debug,
        l if l.eq_ignore_ascii_case("trace") => LevelFilter::Trace,
        _ => LevelFilter::Warn,
    }
}

pub fn init() {
    // Fails only if a logger is already set; then that one stays.
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(level());
    }
}

/// Keeps what fits and silently drops the rest, so a long message is
/// truncated instead of lost.
struct Truncating<const N: usize>(heapless::String<N>);

impl<const N: usize> Write for Truncating<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for c in s.chars() {
            if self.0.push(c).is_err() {
                break;
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct LogLine<'a> {
    t: &'static str,
    lvl: &'static str,
    target: &'a str,
    msg: &'a str,
}

impl Log for JsonLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Small buffers: this can run on a radio thread with a modest stack.
        let mut msg = Truncating::<160>(heapless::String::new());
        let _ = write!(msg, "{}", record.args());
        let line = LogLine { t: "log", lvl: record.level().as_str(), target: record.target(), msg: &msg.0 };
        let mut buf = [0u8; 320];
        if let Ok(n) = serde_json_core::to_slice(&line, &mut buf[..319]) {
            buf[n] = b'\n';
            emit(&USB, &buf[..=n], &COUNTERS.usb_drops);
        }
    }

    fn flush(&self) {}
}
