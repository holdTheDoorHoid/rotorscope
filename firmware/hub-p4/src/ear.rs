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
use tracker::sound::{SoundId, SoundKind, SoundReport};

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
    pub fn get(&self, n: u64) -> Option<i16> {
        if n >= self.head || self.head - n > TAPE_SAMPLES as u64 {
            return None;
        }
        // SAFETY: as `push`; the index is inside the tape.
        Some(unsafe { self.base.add((n % TAPE_SAMPLES as u64) as usize).read() })
    }
}
const _: () = assert!(TAPE_SAMPLES % HOP == 0);

/// Sound kept from before the detection, after it ended, and at most.
const PRE_ROLL: u64 = 5 * mic::RATE as u64;
const POST_ROLL: u64 = 2 * mic::RATE as u64;
pub const CLIP_MAX: u64 = 60 * mic::RATE as u64;

/// A stretch of the tape to be saved as a clip (`sd.rs` writes it).
#[derive(Debug, Clone, Copy)]
pub struct ClipJob {
    /// First sample (tape numbering).
    pub from: u64,
    /// One past the last sample, once known.
    pub end: Option<u64>,
    pub contact: Option<SoundId>,
}

pub struct Ear {
    /// `audio clips on|off` (SITE.TXT). On by default (owner decision
    /// 2026-11-17).
    pub clips_on: bool,
    /// The clip being recorded or written out.
    pub clip: Option<ClipJob>,
    /// The contact whose clip was last started: one clip per contact.
    clipped: Option<SoundId>,
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
    /// Pitch and class in the latest frame that was in the heard state.
    heard_pitch: (f32, bool),
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
            clips_on: true,
            clip: None,
            clipped: None,
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
            heard_pitch: (0.0, false),
            last: None,
            state: State::Warmup,
            max_us: 0,
            sum_us: 0,
            calls: 0,
            last_print_ms: 0,
        })
    }

    pub fn tick(&mut self, tracker: &mut HubTracker, ev: &mut crate::evlog::Evidence, ms: u64) {
        if !self.started {
            // The microphone has been running since `new`, through the rest
            // of start-up (SD mount, evidence boot): drop that backlog.
            self.started = true;
            self.mic.poll(|_| {});
            self.mic.overruns = 0;
            return;
        }
        let Self { tape, mic, det, chan, chan_fixed, energy, frames, heard, peak_conf, heard_pitch, last, max_us, sum_us, calls, .. } = self;
        mic.poll(|stereo| {
            if !*chan_fixed {
                for i in 0..mic::FRAMES * 2 {
                    let s = stereo.sample(i);
                    energy[i & 1] += u64::from(s.unsigned_abs()) * u64::from(s.unsigned_abs());
                }
                *chan = usize::from(energy[1] > energy[0]);
            }
            // Copied out first: everything after this works on `hop`, not
            // on the DMA's buffer (mic.rs "Ring").
            let mut hop = [0i16; HOP];
            for (i, h) in hop.iter_mut().enumerate() {
                *h = stereo.sample(2 * i + *chan);
            }
            #[cfg(feature = "earfake")]
            fake::mix(&mut hop, tape.head);
            tape.push(&hop);
            let t0 = Instant::now();
            let out = det.process(&hop);
            let us = t0.elapsed().as_micros() as u32;
            *max_us = (*max_us).max(us);
            *sum_us += us;
            *calls += 1;
            *frames += 1;
            if out.state == State::Heard {
                *heard += 1;
                *heard_pitch = (out.f0_hz, out.class == Some(acoustic::SizeClass::SmallProps));
            }
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
            pitch_hz: if heard { self.heard_pitch.0 } else { out.f0_hz } as u16,
            confidence: if heard { self.peak_conf } else { out.confidence },
            level_dbfs: out.level_dbfs.clamp(-127.0, 0.0) as i8,
            small_props: if heard { self.heard_pitch.1 } else { out.class == Some(acoustic::SizeClass::SmallProps) },
        };
        let contact = tracker.ingest_sound(&report, ms);
        self.clip_step(heard, contact);
        // Alert once per contact that has lasted (and is not an aircraft we
        // already track): console line and evidence event.
        while let Some(id) = tracker.take_new_sound(ms) {
            let Some(c) = tracker.get_sound(id) else { continue };
            let mut text: heapless::String<160> = heapless::String::new();
            let _ = core::fmt::Write::write_fmt(
                &mut text,
                format_args!(
                    "{}: pitch {} Hz ({}), confidence {}, level {} dBFS, for {} s",
                    c.kind.label(), c.pitch_hz, c.size_label(), c.peak_confidence, c.peak_level_dbfs, c.duration_ms() / 1000
                ),
            );
            let mut data = [0u8; 8];
            data[..4].copy_from_slice(&c.id.0.to_le_bytes());
            data[4] = match c.kind {
                SoundKind::NoRadio => 0,
                SoundKind::Aircraft => 1,
                SoundKind::WithRadio => 2,
            };
            data[5..7].copy_from_slice(&c.pitch_hz.to_le_bytes());
            data[7] = c.peak_confidence;
            ev.event(evidence::format::EventCode::Sound, text.as_bytes(), &data);
            println!("hub-p4: SOUND {}", text);
        }
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

/// Bench only (`--features earfake`): a made-up rotor sound mixed into what
/// the microphone hears, from 20 s to 45 s after start-up and again from
/// 70 s to 80 s, so the detector, the alert, the clip and the evidence
/// entry can be proven on the board with no drone and no loudspeaker. Four
/// "rotors" near 210 Hz, each a little off and drifting, eight harmonics.
#[cfg(feature = "earfake")]
mod fake {
    use super::{mic, HOP};
    /// Phase of each rotor, in sine-table steps (the table has 1024).
    static mut PHASE: [f32; 4] = [0.0; 4];
    static mut SINE: [i16; 1024] = [0; 1024];

    pub fn mix(hop: &mut [i16; HOP], head: u64) {
        let t0 = head as f32 / mic::RATE as f32;
        if !((20.0..45.0).contains(&t0) || (70.0..80.0).contains(&t0)) {
            return;
        }
        // SAFETY: bench only; called from the main loop alone.
        let (phase, sine) = unsafe { (&mut *(&raw mut PHASE), &mut *(&raw mut SINE)) };
        if sine[256] == 0 {
            for (i, v) in sine.iter_mut().enumerate() {
                *v = (libm::sinf(core::f32::consts::TAU * i as f32 / 1024.0) * 16384.0) as i16;
            }
        }
        // Each rotor's pitch is set once per hop (it drifts slowly).
        let mut step = [0.0f32; 4];
        for (r, st) in step.iter_mut().enumerate() {
            let off = [-0.03, -0.01, 0.012, 0.031][r];
            let f = 210.0 * (1.0 + off + 0.008 * libm::sinf(core::f32::consts::TAU * (0.25 + 0.07 * r as f32) * t0));
            *st = f * 1024.0 / mic::RATE as f32;
        }
        for s in hop.iter_mut() {
            let mut v = 0i32;
            for (ph, st) in phase.iter_mut().zip(step) {
                *ph += st;
                if *ph >= 1024.0 {
                    *ph -= 1024.0;
                }
                let p = *ph as usize;
                for h in 1..=8usize {
                    v += i32::from(sine[(p * h) & 1023]) / h as i32;
                }
            }
            // 16384 full scale in the table; about 60 counts per harmonic.
            *s = s.saturating_add((v / 273) as i16);
        }
    }
}

/// Console `w` was pressed.
#[cfg(feature = "eardump")]
pub static DUMP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

impl Ear {
    /// Start, extend or close the clip for what is being heard.
    fn clip_step(&mut self, heard: bool, contact: Option<SoundId>) {
        let head = self.tape.head;
        match self.clip.as_mut() {
            None => {
                if heard && self.clips_on && contact.is_some() && contact != self.clipped {
                    self.clipped = contact;
                    self.clip = Some(ClipJob { from: head.saturating_sub(PRE_ROLL), end: None, contact });
                }
            }
            Some(c) if c.end.is_none() => {
                if !heard {
                    c.end = Some((head + POST_ROLL).min(c.from + CLIP_MAX));
                } else if head - c.from >= CLIP_MAX {
                    c.end = Some(c.from + CLIP_MAX);
                }
            }
            Some(_) => {}
        }
    }
}

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
