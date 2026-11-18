//! The hub's ear (PLAN.md Phase 15d): microphone -> rotor-sound detector ->
//! the tracker's sound contacts.
//!
//! Runs from the main loop. Each 32 ms buffer from [`crate::mic`] goes
//! through `acoustic::Detector`; once a second the result is handed to the
//! tracker (`tracker::sound`) and, while anything is heard or changing,
//! printed.
//!
//! The detector (about 40 KB) lives in PSRAM at [`EAR_OFFSET`].

use acoustic::{Detector, FrameOut, State, FRAMES_PER_S, HOP};
use esp_hal::gpio::AnyPin;
use esp_hal::time::Instant;
use tracker::sound::SoundReport;

use crate::app::HubTracker;
use crate::mic::{self, Mic};

/// Where the detector lives in PSRAM: after the tracker (2 MB), before the
/// evidence queue (4 MB).
pub const EAR_OFFSET: usize = 3 << 20;
const _: () = assert!(
    crate::TRACKER_OFFSET + core::mem::size_of::<HubTracker>() <= EAR_OFFSET,
    "tracker overlaps the ear"
);
const _: () = assert!(
    EAR_OFFSET + core::mem::size_of::<Detector>() <= crate::evlog::QUEUE_OFFSET,
    "ear overlaps the evidence queue"
);
const _: () = assert!(mic::FRAMES == HOP && mic::RATE == acoustic::SAMPLE_RATE);

/// Where the tape lives in PSRAM: above the airspace pack.
pub const TAPE_OFFSET: usize = 16 << 20;
/// Samples on the tape: 131 s at 16 kHz, 4 MB.
pub const TAPE_SAMPLES: usize = 1 << 21;
const _: () = assert!(crate::faa::PACK_OFFSET + crate::faa::PACK_MAX <= TAPE_OFFSET, "airspace pack overlaps the tape");

/// The last two minutes of sound, going round in PSRAM. It is what an audio
/// clip is cut from (the seconds before a detection included). Nothing reads
/// it unless the detector hears a drone; it is gone at power-off.
pub struct Tape {
    base: *mut i16,
    /// Samples recorded since start; sample `n` is at `n % TAPE_SAMPLES`.
    pub head: u64,
}

impl Tape {
    fn push(&mut self, hop: &[i16; HOP]) {
        let at = (self.head % TAPE_SAMPLES as u64) as usize;
        // SAFETY: the tape is [TAPE_OFFSET, + 2 * TAPE_SAMPLES) of PSRAM
        // (size checked in `Ear::new`), used by nothing else; HOP divides
        // TAPE_SAMPLES, so a hop never wraps.
        unsafe { core::ptr::copy_nonoverlapping(hop.as_ptr(), self.base.add(at), HOP) };
        self.head += HOP as u64;
    }

    /// Sample `n`, if it is still on the tape.
    #[cfg_attr(not(feature = "eardump"), allow(dead_code))]
    pub fn get(&self, n: u64) -> Option<i16> {
        if n >= self.head || self.head - n > TAPE_SAMPLES as u64 {
            return None;
        }
        // SAFETY: as `push`; the index is inside the tape.
        Some(unsafe { self.base.add((n % TAPE_SAMPLES as u64) as usize).read() })
    }
}
const _: () = assert!(TAPE_SAMPLES % HOP == 0);

pub struct Ear {
    pub tape: Tape,
    mic: Mic,
    det: &'static mut Detector,
    /// Which half of the stereo pair carries the microphone (0 left, 1
    /// right), chosen in the first second by loudness.
    chan: usize,
    chan_fixed: bool,
    started: bool,
    energy: [u64; 2],
    /// Frames since the last report to the tracker.
    frames: u32,
    heard: u32,
    peak_conf: u8,
    last: Option<FrameOut>,
    state: State,
    /// Longest single detector call, microseconds.
    pub max_us: u32,
    sum_us: u32,
    calls: u32,
    last_print_ms: u64,
}

impl Ear {
    /// `None` without PSRAM (the detector lives there).
    pub fn new(psram: Option<(*mut u32, usize)>, clk: AnyPin<'static>, data: AnyPin<'static>) -> Option<Self> {
        let (base, size) = psram?;
        if size < TAPE_OFFSET + 2 * TAPE_SAMPLES {
            return None;
        }
        // SAFETY: [EAR_OFFSET, + size_of) is inside PSRAM (checked above),
        // used by nothing else (the asserts above) and 3 MB is aligned for
        // any type. Called once.
        let det = unsafe {
            let slot = &mut *base.cast::<u8>().add(EAR_OFFSET).cast::<core::mem::MaybeUninit<Detector>>();
            Detector::init_in_place(slot)
        };
        // SAFETY: inside PSRAM (checked above); 16 MB is aligned for i16.
        let tape = Tape { base: unsafe { base.cast::<u8>().add(TAPE_OFFSET).cast::<i16>() }, head: 0 };
        Some(Self {
            tape,
            mic: Mic::start(clk, data),
            det,
            chan: 0,
            chan_fixed: false,
            started: false,
            energy: [0; 2],
            frames: 0,
            heard: 0,
            peak_conf: 0,
            last: None,
            state: State::Warmup,
            max_us: 0,
            sum_us: 0,
            calls: 0,
            last_print_ms: 0,
        })
    }

    pub fn tick(&mut self, tracker: &mut HubTracker, ms: u64) {
        if !self.started {
            // The microphone has been running since `new`, through the rest
            // of start-up (SD mount, evidence boot): drop that backlog.
            self.started = true;
            self.mic.poll(|_| {});
            self.mic.overruns = 0;
            return;
        }
        let Self { tape, mic, det, chan, chan_fixed, energy, frames, heard, peak_conf, last, max_us, sum_us, calls, .. } = self;
        mic.poll(|stereo| {
            if !*chan_fixed {
                for (i, s) in stereo.iter().enumerate() {
                    energy[i & 1] += u64::from(s.unsigned_abs()) * u64::from(s.unsigned_abs());
                }
                *chan = usize::from(energy[1] > energy[0]);
            }
            let mut hop = [0i16; HOP];
            for (i, h) in hop.iter_mut().enumerate() {
                *h = stereo[2 * i + *chan];
            }
            tape.push(&hop);
            let t0 = Instant::now();
            let out = det.process(&hop);
            let us = t0.elapsed().as_micros() as u32;
            *max_us = (*max_us).max(us);
            *sum_us += us;
            *calls += 1;
            *frames += 1;
            *heard += u32::from(out.state == State::Heard);
            *peak_conf = (*peak_conf).max(out.confidence);
            *last = Some(out);
        });
        if self.frames < FRAMES_PER_S {
            return;
        }
        let Some(out) = self.last else { return };
        if !self.chan_fixed {
            self.chan_fixed = true;
            let db = |e: u64| 10.0 * libm::log10f(e as f32 / (FRAMES_PER_S as f32 * HOP as f32 * 32768.0 * 32768.0) + 1e-12);
            println!(
                "hub-p4: ear: microphone on the {} half (left {:.0} dBFS, right {:.0} dBFS)",
                if self.chan == 0 { "left" } else { "right" },
                db(self.energy[0]),
                db(self.energy[1])
            );
        }
        // "Heard" for this second if the detector was in that state for
        // most of it.
        let heard = 2 * self.heard >= self.frames;
        let report = SoundReport {
            heard,
            pitch_hz: out.f0_hz as u16,
            confidence: if heard { self.peak_conf } else { out.confidence },
            level_dbfs: out.level_dbfs.clamp(-127.0, 0.0) as i8,
            small_props: out.class == Some(acoustic::SizeClass::SmallProps),
        };
        let contact = tracker.ingest_sound(&report, ms);
        let changed = out.state != self.state;
        self.state = out.state;
        // One line when the state changes, one a second while something is
        // followed, else one every ten seconds.
        let quiet = matches!(out.state, State::Quiet | State::Warmup) && !out.steady;
        if changed || !quiet || ms.saturating_sub(self.last_print_ms) >= 10_000 {
            self.last_print_ms = ms;
            println!(
                "hub-p4: ear {:?}{} pitch {:.0} Hz conf {} level {:.0} dBFS contact {:?} | bufs {} overruns {} dma_err {} detector avg {} max {} us",
                out.state,
                if out.steady { " (steady hum)" } else { "" },
                out.f0_hz,
                report.confidence,
                out.level_dbfs,
                contact.map(|c| c.0),
                self.mic.buffers,
                self.mic.overruns,
                self.mic.dma_errors,
                self.sum_us / self.calls.max(1),
                self.max_us
            );
            self.max_us = 0;
            self.sum_us = 0;
            self.calls = 0;
        }
        self.frames = 0;
        self.heard = 0;
        self.peak_conf = 0;
    }
}

/// Console `w` was pressed.
#[cfg(feature = "eardump")]
pub static DUMP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

#[cfg(feature = "eardump")]
impl Ear {
    /// Bench only (`--features eardump`, console `w`): print the last
    /// `seconds` of the tape as hex, 32 samples a line, so recorded sound
    /// can be checked on a laptop. Blocks the loop while it prints.
    pub fn dump(&self, seconds: u32) {
        let n = u64::from(seconds) * u64::from(mic::RATE);
        let from = self.tape.head.saturating_sub(n);
        println!("EARPCM begin rate {} samples {}", mic::RATE, self.tape.head - from);
        let mut line: heapless::String<160> = heapless::String::new();
        for i in from..self.tape.head {
            let v = self.tape.get(i).unwrap_or(0);
            let _ = core::fmt::Write::write_fmt(&mut line, format_args!("{:04x}", v as u16));
            if line.len() >= 128 {
                println!("EARPCM {}", line);
                line.clear();
            }
        }
        if !line.is_empty() {
            println!("EARPCM {}", line);
        }
        println!("EARPCM end");
    }
}
