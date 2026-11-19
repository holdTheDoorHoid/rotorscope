//! Deterministic fuzz-style testing with no dependencies.
//!
//! Every crate that parses bytes an outsider can influence runs its parsers
//! through [`run`] from an ordinary `#[test]`, so `cargo test` throws random
//! and mutated inputs at them on every run. A failing input is printed as hex
//! with the seed and iteration that produced it, so it can be replayed.
//!
//! Environment:
//!
//! - `ROTORSCOPE_FUZZ_ITERS`: iterations per target. A plain number (`200000`)
//!   replaces every target's default; `x50` multiplies every default by 50.
//! - `ROTORSCOPE_FUZZ_SEED`: the base seed (decimal or `0x` hex). Default is
//!   fixed, so every run and CI see the same inputs.
//! - `ROTORSCOPE_FUZZ_REPLAY`: one input as hex. Every target then runs only
//!   that input, once; use it with a test name filter.
//! - `ROTORSCOPE_FUZZ_HANG_SECS`: how long one input may run before it is
//!   reported as a hang (default 20). A hang ends the test binary.
//!
//! This crate is a `[dev-dependencies]` path dependency only; it is never
//! built into firmware.

use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The default base seed when `ROTORSCOPE_FUZZ_SEED` is unset.
pub const DEFAULT_SEED: u64 = 0x5EED_0F_70_70_5C_0F;

/// xorshift64* generator. Small, fast and repeatable; not for cryptography.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // Zero is a fixed point of xorshift; mix the seed first.
        Rng(splitmix(seed) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    pub fn byte(&mut self) -> u8 {
        (self.next_u64() >> 56) as u8
    }

    /// A value in `0..n`; `0` when `n` is zero.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next_u64() % n as u64) as usize }
    }

    /// True with probability `1 / n`.
    pub fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.byte()).collect()
    }

    pub fn fill(&mut self, buf: &mut [u8]) {
        for b in buf {
            *b = self.byte();
        }
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    /// A length biased towards small values and edges: 0..8, 0..64 or 0..max.
    pub fn len(&mut self, max: usize) -> usize {
        match self.below(4) {
            0 => self.below(9.min(max + 1)),
            1 => self.below(65.min(max + 1)),
            _ => self.below(max + 1),
        }
    }
}

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// FNV-1a, to give each target its own stream from the base seed.
fn name_hash(name: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Byte values that tend to sit on a parser's edges.
pub const INTERESTING_8: [u8; 8] = [0x00, 0x01, 0x7f, 0x80, 0xff, 0xfe, 0x10, 0x40];

const INTERESTING_16: [u16; 8] = [0, 1, 0x7f, 0x80, 0xff, 0x7fff, 0x8000, 0xffff];
const INTERESTING_32: [u32; 8] =
    [0, 1, 0xff, 0xffff, 0x7fff_ffff, 0x8000_0000, 0xffff_fffe, 0xffff_ffff];

/// Apply one random mutation to `data`. `others` are splice partners;
/// `dict` holds tokens worth inserting (keywords for text parsers). The
/// result is kept at or under `max_len` bytes.
pub fn mutate(rng: &mut Rng, data: &mut Vec<u8>, others: &[&[u8]], dict: &[&[u8]], max_len: usize) {
    let n = data.len();
    match rng.below(15) {
        // Flip one bit.
        0 if n > 0 => {
            let i = rng.below(n);
            data[i] ^= 1 << rng.below(8);
        }
        // Overwrite a byte with an interesting value.
        1 if n > 0 => {
            let i = rng.below(n);
            data[i] = *rng.pick(&INTERESTING_8);
        }
        // Overwrite a byte with a random value.
        2 if n > 0 => {
            let i = rng.below(n);
            data[i] = rng.byte();
        }
        // Insert random bytes.
        3 => {
            let at = rng.below(n + 1);
            let k = 1 + rng.below(8);
            let ins = rng.bytes(k);
            data.splice(at..at, ins);
        }
        // Delete a span.
        4 if n > 0 => {
            let at = rng.below(n);
            let k = 1 + rng.below((n - at).min(16));
            data.drain(at..at + k);
        }
        // Duplicate a span in place.
        5 if n > 0 => {
            let at = rng.below(n);
            let k = 1 + rng.below((n - at).min(32));
            let span = data[at..at + k].to_vec();
            let to = rng.below(data.len() + 1);
            data.splice(to..to, span);
        }
        // Splice: our head, another input's tail.
        6 if !others.is_empty() => {
            let other = *rng.pick(others);
            let cut = rng.below(n + 1);
            let from = rng.below(other.len() + 1);
            data.truncate(cut);
            data.extend_from_slice(&other[from..]);
        }
        // Truncate.
        7 if n > 0 => {
            let k = rng.below(n);
            data.truncate(k);
        }
        // Grow with random or zero bytes.
        8 => {
            let k = 1 + rng.below(32);
            if rng.one_in(2) {
                data.extend(std::iter::repeat_n(0u8, k));
            } else {
                let more = rng.bytes(k);
                data.extend(more);
            }
        }
        // 16-bit field with a boundary value, either byte order.
        9 if n >= 2 => {
            let at = rng.below(n - 1);
            let v = *rng.pick(&INTERESTING_16);
            let b = if rng.one_in(2) { v.to_le_bytes() } else { v.to_be_bytes() };
            data[at..at + 2].copy_from_slice(&b);
        }
        // 32-bit field with a boundary value, either byte order.
        10 if n >= 4 => {
            let at = rng.below(n - 3);
            let v = *rng.pick(&INTERESTING_32);
            let b = if rng.one_in(2) { v.to_le_bytes() } else { v.to_be_bytes() };
            data[at..at + 4].copy_from_slice(&b);
        }
        // Add or subtract a small amount from a byte (length fields).
        11 if n > 0 => {
            let i = rng.below(n);
            let d = 1 + rng.below(4) as u8;
            data[i] = if rng.one_in(2) { data[i].wrapping_add(d) } else { data[i].wrapping_sub(d) };
        }
        // Insert or overwrite with a dictionary token.
        12 if !dict.is_empty() => {
            let tok = *rng.pick(dict);
            let at = rng.below(n + 1);
            if rng.one_in(2) || at + tok.len() > n {
                data.splice(at..at, tok.iter().copied());
            } else {
                data[at..at + tok.len()].copy_from_slice(tok);
            }
        }
        // Repeat one byte many times (long runs, missing terminators).
        13 if n > 0 => {
            let b = data[rng.below(n)];
            let at = rng.below(n + 1);
            let k = 1 + rng.below(64);
            data.splice(at..at, std::iter::repeat_n(b, k));
        }
        // Swap two bytes.
        _ if n >= 2 => {
            let i = rng.below(n);
            let j = rng.below(n);
            data.swap(i, j);
        }
        _ => data.push(rng.byte()),
    }
    if data.len() > max_len {
        data.truncate(max_len);
    }
}

/// How a target is fuzzed. Build with [`Config::new`] and the setters.
#[derive(Clone, Debug)]
pub struct Config<'a> {
    /// Longest input generated.
    pub max_len: usize,
    /// Tokens the mutator may insert.
    pub dict: &'a [&'a [u8]],
    /// Out of 100, how many inputs are pure random rather than mutated seeds.
    pub random_percent: usize,
    /// Most mutations stacked on one seed.
    pub max_mutations: usize,
}

impl<'a> Config<'a> {
    pub const fn new() -> Self {
        Config { max_len: 512, dict: &[], random_percent: 25, max_mutations: 4 }
    }
    pub const fn max_len(mut self, n: usize) -> Self {
        self.max_len = n;
        self
    }
    pub const fn dict(mut self, d: &'a [&'a [u8]]) -> Self {
        self.dict = d;
        self
    }
    pub const fn random_percent(mut self, p: usize) -> Self {
        self.random_percent = p;
        self
    }
}

impl Default for Config<'_> {
    fn default() -> Self {
        Self::new()
    }
}

/// The base seed: `ROTORSCOPE_FUZZ_SEED` or [`DEFAULT_SEED`].
pub fn base_seed() -> u64 {
    match std::env::var("ROTORSCOPE_FUZZ_SEED") {
        Ok(v) => {
            let v = v.trim();
            let parsed = if let Some(h) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
                u64::from_str_radix(h, 16).ok()
            } else {
                v.parse().ok()
            };
            parsed.unwrap_or_else(|| panic!("ROTORSCOPE_FUZZ_SEED={v:?} is not a number"))
        }
        Err(_) => DEFAULT_SEED,
    }
}

/// Iterations for a target whose default is `default`, after
/// `ROTORSCOPE_FUZZ_ITERS` (`N` replaces, `xN` multiplies).
pub fn iters(default: usize) -> usize {
    match std::env::var("ROTORSCOPE_FUZZ_ITERS") {
        Ok(v) => {
            let v = v.trim();
            let mult = v.strip_prefix('x').or_else(|| v.strip_suffix('x'));
            match mult {
                Some(m) => default.saturating_mul(m.parse().unwrap_or_else(|_| panic!("bad ROTORSCOPE_FUZZ_ITERS={v:?}"))),
                None => v.parse().unwrap_or_else(|_| panic!("bad ROTORSCOPE_FUZZ_ITERS={v:?}")),
            }
        }
        Err(_) => default,
    }
}

/// The generator a target's test should use for anything it builds itself
/// (valid values for round trips), seeded from the base seed and the name.
pub fn rng_for(name: &str) -> Rng {
    Rng::new(base_seed() ^ name_hash(name).rotate_left(17))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Hex to bytes, ignoring whitespace. Panics on bad hex (test helper).
pub fn unhex(s: &str) -> Vec<u8> {
    let s: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    assert!(s.len() % 2 == 0, "odd hex length");
    s.chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).expect("bad hex"))
        .collect()
}

struct Watch {
    progress: AtomicU64,
    current: Mutex<Vec<u8>>,
    done: AtomicBool,
}

fn hang_limit() -> Duration {
    let secs = std::env::var("ROTORSCOPE_FUZZ_HANG_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
    Duration::from_secs(secs)
}

/// Fuzz one target with the default [`Config`]. See [`run_with`].
pub fn run(name: &str, seeds: &[&[u8]], default_iters: usize, f: impl FnMut(&[u8])) {
    run_with(name, &Config::new(), seeds, default_iters, f)
}

/// Fuzz one target: `iters(default_iters)` inputs, a share pure random of
/// varied length and the rest one to a few mutations of a seed, each passed
/// to `f`. `f` checks its properties with ordinary asserts.
///
/// On a panic the target name, base seed, iteration and the input as hex are
/// printed and the panic is passed on, failing the test. An input that runs
/// longer than `ROTORSCOPE_FUZZ_HANG_SECS` is printed the same way and the
/// process exits, since a stuck call cannot be cancelled.
pub fn run_with(name: &str, cfg: &Config<'_>, seeds: &[&[u8]], default_iters: usize, mut f: impl FnMut(&[u8])) {
    let seed = base_seed();
    if let Ok(h) = std::env::var("ROTORSCOPE_FUZZ_REPLAY") {
        let input = unhex(&h);
        eprintln!("fuzz {name}: replaying {} bytes", input.len());
        f(&input);
        return;
    }
    let n = iters(default_iters);
    let mut rng = Rng::new(seed ^ name_hash(name));
    let watch = Arc::new(Watch { progress: AtomicU64::new(0), current: Mutex::new(Vec::new()), done: AtomicBool::new(false) });
    let limit = hang_limit();
    let dog = {
        let watch = watch.clone();
        let name = name.to_string();
        std::thread::spawn(move || {
            let mut last = u64::MAX;
            let mut since = Instant::now();
            while !watch.done.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));
                let p = watch.progress.load(Ordering::Relaxed);
                if p != last {
                    last = p;
                    since = Instant::now();
                } else if since.elapsed() > limit {
                    let cur = watch.current.lock().map(|c| hex(&c)).unwrap_or_default();
                    eprintln!(
                        "\nFUZZ HANG in {name}: one input ran over {limit:?}\n  seed: {seed:#x}, iteration {p}\n  input ({} hex chars): {cur}\n  replay: ROTORSCOPE_FUZZ_REPLAY={cur}",
                        cur.len()
                    );
                    std::process::exit(101);
                }
            }
        })
    };
    let mut buf = Vec::new();
    for i in 0..n {
        buf.clear();
        if seeds.is_empty() || rng.below(100) < cfg.random_percent {
            let len = rng.len(cfg.max_len);
            buf.resize(len, 0);
            rng.fill(&mut buf);
            // Some random inputs are mostly one byte value: zeros, 0xff.
            if rng.one_in(8) && len > 0 {
                let b = *rng.pick(&INTERESTING_8);
                for x in buf.iter_mut() {
                    if !rng.one_in(8) {
                        *x = b;
                    }
                }
            }
        } else {
            buf.extend_from_slice(rng.pick(seeds));
            let k = 1 + rng.below(cfg.max_mutations.max(1));
            // An unmutated seed now and then, as a sanity baseline.
            if !rng.one_in(64) {
                for _ in 0..k {
                    mutate(&mut rng, &mut buf, seeds, cfg.dict, cfg.max_len.max(1));
                }
            }
        }
        if let Ok(mut c) = watch.current.lock() {
            c.clear();
            c.extend_from_slice(&buf);
        }
        let r = panic::catch_unwind(AssertUnwindSafe(|| f(&buf)));
        watch.progress.store(i as u64 + 1, Ordering::Relaxed);
        if let Err(e) = r {
            watch.done.store(true, Ordering::Relaxed);
            let h = hex(&buf);
            eprintln!(
                "\nFUZZ FAILURE in {name}\n  seed: {seed:#x}, iteration {i} of {n}\n  input ({} bytes): {h}\n  replay: ROTORSCOPE_FUZZ_REPLAY={h} cargo test <this test>",
                buf.len()
            );
            panic::resume_unwind(e);
        }
    }
    watch.done.store(true, Ordering::Relaxed);
    let _ = dog.join();
}

/// Bytes as text the way a line parser would see them: lossy UTF-8.
pub fn text(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_repeatable_and_not_stuck() {
        let mut a = Rng::new(0);
        let mut b = Rng::new(0);
        let xs: Vec<u64> = (0..8).map(|_| a.next_u64()).collect();
        let ys: Vec<u64> = (0..8).map(|_| b.next_u64()).collect();
        assert_eq!(xs, ys);
        assert!(xs.windows(2).all(|w| w[0] != w[1]));
    }

    #[test]
    fn mutate_respects_max_len() {
        let mut r = Rng::new(1);
        let seed = [1u8, 2, 3, 4, 5, 6, 7, 8];
        for _ in 0..10_000 {
            let mut d = seed.to_vec();
            for _ in 0..4 {
                mutate(&mut r, &mut d, &[&seed], &[b"TOKEN"], 16);
            }
            assert!(d.len() <= 16);
        }
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(unhex(&hex(&[0, 0x7f, 0xff])), vec![0, 0x7f, 0xff]);
    }

    #[test]
    fn run_reaches_every_iteration_and_reports_panics() {
        let mut count = 0;
        run("fuzzkit::count", &[b"abc"], 500, |_| count += 1);
        assert_eq!(count, iters(500));
        let r = panic::catch_unwind(|| {
            run("fuzzkit::boom", &[b"\x00"], 1000, |b| assert!(b.len() < 3));
        });
        assert!(r.is_err());
    }
}
