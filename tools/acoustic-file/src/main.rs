//! Run recordings through the rotor-sound detector.
//!
//! ```text
//! acoustic-file [--timeline] FILE.wav ...        whole recordings, as the hub hears them
//! acoustic-file --clips DIR_OR_FILE ...          short clips (1-2 s): frame-level only
//! acoustic-file --join DIR ...                   a folder of short clips played end to end
//! acoustic-file --tones FILE.wav ...             the strongest steady tones, per ten seconds
//! ```
//!
//! Any sample rate and width; channel 1 of a multi-channel file is used.
//! Everything is resampled to 16 kHz first.
//!
//! - Whole recordings pass through the complete detector, including the
//!   five-second warm-up and the three-second decision, and the tool prints
//!   when "heard" started and what share of the file it covered.
//! - A short clip ends before the detector may decide anything. `--clips`
//!   therefore reports the share of 32 ms frames that showed a rotor-like
//!   ladder, and calls a clip "rotor-like" at half or more. It is a test of
//!   the ladder score, not of the detector's decision.
//! - `--join` plays every clip in a folder end to end as one recording.

use std::path::{Path, PathBuf};

use acoustic::{Detector, FrameOut, State, FRAMES_PER_S, HOP, SAMPLE_RATE};

fn main() {
    let mut mode = Mode::Whole;
    let mut timeline = false;
    let mut paths = Vec::new();
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--timeline" => timeline = true,
            "--clips" => mode = Mode::Clips,
            "--join" => mode = Mode::Join,
            "--tones" => mode = Mode::Tones,
            "-h" | "--help" => return println!("see the top of tools/acoustic-file/src/main.rs"),
            _ => paths.push(PathBuf::from(a)),
        }
    }
    if paths.is_empty() {
        eprintln!("usage: acoustic-file [--timeline] [--clips | --join] FILE_OR_DIR ...");
        std::process::exit(2);
    }
    match mode {
        Mode::Whole => {
            let mut files = Vec::new();
            for p in &paths {
                collect(p, &mut files);
            }
            println!("{:>7} {:>7} {:>6} {:>7} {:>5}  file", "length", "heard", "first", "pitch", "conf");
            for f in files {
                match load(&f) {
                    Ok(pcm) => whole(&f.display().to_string(), &pcm, timeline),
                    Err(e) => eprintln!("{}: {e}", f.display()),
                }
            }
        }
        Mode::Join => {
            println!("{:>7} {:>7} {:>6} {:>7} {:>5}  folder", "length", "heard", "first", "pitch", "conf");
            for p in &paths {
                let mut files = Vec::new();
                collect(p, &mut files);
                let mut pcm = Vec::new();
                for f in &files {
                    match load(f) {
                        Ok(mut x) => pcm.append(&mut x),
                        Err(e) => eprintln!("{}: {e}", f.display()),
                    }
                }
                whole(&format!("{} ({} clips)", p.display(), files.len()), &pcm, timeline);
            }
        }
        Mode::Tones => {
            for p in &paths {
                let mut files = Vec::new();
                collect(p, &mut files);
                for f in files {
                    match load(&f) {
                        Ok(pcm) => tones(&f.display().to_string(), &pcm),
                        Err(e) => eprintln!("{}: {e}", f.display()),
                    }
                }
            }
        }
        Mode::Clips => {
            println!("{:>6} {:>10} {:>9}  folder", "clips", "rotor-like", "share");
            for p in &paths {
                let mut files = Vec::new();
                collect(p, &mut files);
                let (mut n, mut pos) = (0u32, 0u32);
                for f in &files {
                    let Ok(pcm) = load(f) else { continue };
                    if pcm.len() < 4 * HOP {
                        continue;
                    }
                    n += 1;
                    pos += u32::from(clip_is_rotor_like(&pcm));
                }
                let share = if n > 0 { 100.0 * pos as f32 / n as f32 } else { 0.0 };
                println!("{n:>6} {pos:>10} {share:>8.1}%  {}", p.display());
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Tones,
    Whole,
    Clips,
    Join,
}

fn collect(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut inner: Vec<_> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
        inner.sort();
        for e in inner {
            collect(&e, out);
        }
    } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")) {
        out.push(p.to_path_buf());
    }
}

fn frames(pcm: &[i16], cfg: Option<acoustic::Config>) -> Vec<FrameOut> {
    let mut d = Box::new(Detector::zeroed());
    if let Some(c) = cfg {
        d.cfg = c;
    }
    pcm.as_chunks::<HOP>().0.iter().map(|c| d.process(c)).collect()
}

fn whole(name: &str, pcm: &[i16], timeline: bool) {
    let out = frames(pcm, None);
    if out.is_empty() {
        return;
    }
    let heard: Vec<&FrameOut> = out.iter().filter(|f| f.state == State::Heard).collect();
    let first = out.iter().position(|f| f.state == State::Heard);
    let secs = out.len() as f32 / FRAMES_PER_S as f32;
    let share = 100.0 * heard.len() as f32 / out.len() as f32;
    let mut pitches: Vec<f32> = heard.iter().map(|f| f.f0_hz).collect();
    pitches.sort_by(f32::total_cmp);
    let pitch = pitches.get(pitches.len() / 2).copied().unwrap_or(0.0);
    let conf = heard.iter().map(|f| u32::from(f.confidence)).sum::<u32>() / heard.len().max(1) as u32;
    let first = first.map_or("-".into(), |i| format!("{:.1}s", i as f32 / FRAMES_PER_S as f32));
    println!("{secs:>6.1}s {share:>6.1}% {first:>6} {pitch:>5.0}Hz {conf:>5}  {name}");
    if timeline {
        for (i, s) in out.chunks(FRAMES_PER_S as usize).enumerate() {
            let hits = s.iter().filter(|f| f.hit).count();
            let l = s.last().unwrap();
            let state = if l.steady { "Steady".to_string() } else { format!("{:?}", l.state) };
            println!(
                "    {i:4}s {state:9} hits {hits:2}/{} pitch {:6.1} Hz  score {:5.1}  rungs {:2}  wander {:4.2}%  near {}  level {:5.1} dBFS",
                s.len(), l.f0_hz, l.score, l.harmonics, l.wander_pct, l.neighbours, l.level_dbfs
            );
        }
    }
}

/// The strongest steady tones in each ten seconds of a recording: the
/// spectrum averaged over the ten seconds, and its peaks above the level of
/// the bins around them. For looking at a recording the detector missed.
fn tones(name: &str, pcm: &[i16]) {
    println!("{name}");
    let mut d = Box::new(Detector::zeroed());
    let per = 10 * FRAMES_PER_S as usize;
    let mut avg = vec![0.0f64; acoustic::BINS];
    for (i, c) in pcm.as_chunks::<HOP>().0.iter().enumerate() {
        d.process(c);
        for (a, p) in avg.iter_mut().zip(d.spectrum()) {
            *a += f64::from(*p);
        }
        if (i + 1) % per != 0 {
            continue;
        }
        let mut found = Vec::new();
        for k in 8..acoustic::BINS - 30 {
            if avg[k] <= avg[k - 1] || avg[k] < avg[k + 1] {
                continue;
            }
            let mut around: Vec<f64> = avg[k.saturating_sub(30)..k + 30].to_vec();
            around.sort_by(f64::total_cmp);
            let db = 10.0 * (avg[k] / around[around.len() / 2]).log10();
            if db >= 6.0 {
                found.push((db, k as f32 * acoustic::BIN_HZ));
            }
        }
        found.sort_by(|a, b| b.0.total_cmp(&a.0));
        found.truncate(14);
        found.sort_by(|a, b| a.1.total_cmp(&b.1));
        let text: Vec<String> = found.iter().map(|(db, hz)| format!("{hz:.0}Hz+{db:.0}")).collect();
        println!("  {:3}-{:3}s  {}", (i + 1 - per) / FRAMES_PER_S as usize, (i + 1) / FRAMES_PER_S as usize, text.join(" "));
        avg.iter_mut().for_each(|a| *a = 0.0);
    }
}

/// Frame-level check for a clip too short for the detector's decision: no
/// warm-up, and the share of frames with a ladder at or above the hit score.
fn clip_is_rotor_like(pcm: &[i16]) -> bool {
    let cfg = acoustic::Config { warmup_frames: 0, ..acoustic::Config::DEFAULT };
    let out = frames(pcm, Some(cfg));
    // The first frames hold less than one full window of sound.
    let body = &out[out.len().min(3)..];
    !body.is_empty() && 2 * body.iter().filter(|f| f.hit).count() >= body.len()
}

/// Read a WAV file as 16 kHz mono.
fn load(path: &Path) -> Result<Vec<i16>, String> {
    let mut r = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = r.spec();
    let ch = usize::from(spec.channels.max(1));
    let mono: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().step_by(ch).map(|s| s.unwrap_or(0.0)).collect(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().step_by(ch).map(|s| s.unwrap_or(0) as f32 * scale).collect()
        }
    };
    let out = if spec.sample_rate == SAMPLE_RATE { mono } else { resample(&mono, spec.sample_rate, SAMPLE_RATE) };
    Ok(out.iter().map(|v| (v * 32768.0).clamp(-32768.0, 32767.0) as i16).collect())
}

/// Windowed-sinc resampling (Hann, 24 zero crossings each side).
fn resample(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    let step = f64::from(from) / f64::from(to);
    // Cut off just under the lower of the two Nyquist rates.
    let cut = (f64::from(to) / f64::from(from)).min(1.0) * 0.94;
    let half = (24.0 / cut).ceil() as isize;
    let n = (x.len() as f64 / step) as usize;
    let mut y = Vec::with_capacity(n);
    for i in 0..n {
        let pos = i as f64 * step;
        let centre = pos.floor() as isize;
        let mut acc = 0.0f64;
        for k in (centre - half + 1)..=(centre + half) {
            if k < 0 || k as usize >= x.len() {
                continue;
            }
            let d = pos - k as f64;
            let a = std::f64::consts::PI * d * cut;
            let sinc = if a.abs() < 1e-9 { 1.0 } else { a.sin() / a };
            let w = 0.5 + 0.5 * (std::f64::consts::PI * d / half as f64).cos();
            acc += f64::from(x[k as usize]) * sinc * w * cut;
        }
        y.push(acc as f32);
    }
    y
}
