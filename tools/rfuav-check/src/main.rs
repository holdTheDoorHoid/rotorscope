//! Run Rotorscope's C5 FPV-job detectors and the DroneID decoder over an
//! RFUAV recording (docs/DJI-LINKS.md, "Validation against RFUAV").
//!
//! ```text
//! rfuav-check run <file.iq|-> --fc-mhz <recording centre> --out <dir> --tag <name>
//!             [--fs 100e6] [--lo all|<MHz,MHz,...>] [--rec-noise-db -6] [--level-db 25 | --scale <s>]
//!             [--stride 4] [--max-ms <ms>] [--t0-ms <ms>] [--no-survey]
//! rfuav-check summary <dir> <tag>
//! ```
//!
//! **Input**: RFUAV `.iq` files: interleaved little-endian `f32` I, Q
//! ("Complex Float" in each pack's XML), 100 MS/s, centred on the XML's
//! `CenterFrequency` (`-` reads stdin, e.g. `unrar p -inul <rar> <member> |`).
//!
//! **What it does, per 60 ms (one FPV dwell) of input:**
//!
//! - **survey** (full band, the recording's own rate): spectrogram tracks,
//!   one CSV row per burst; autocorrelation of narrow and wide bursts.
//! - **C5 model**, per LO (every 2.4/5.8 GHz sweep channel the recording
//!   covers, LO = channel - 1 MHz as `fpvscan::band::lo_for`, plus the
//!   DroneID LOs): the 20 MHz slice resampled to 40 MS/s, scaled to ADC
//!   codes, the C5's receiver noise added, quantised by `rawcap`'s clip
//!   window, then the firmware's dwell logic (trigger, meter, timeline,
//!   capture, `fpvscan`, `djilink`). One `dwell` JSON line each, plus
//!   `win` lines for every `stride`-th capture-length window (C5 path and a
//!   clean path without noise or quantisation).
//! - **DroneID** at the LOs the node uses (whole MHz below each burst centre
//!   in the recording): the C5 path's 20 MS/s stream and the clean stream
//!   through `droneid`'s receiver. Serial numbers, UUIDs and positions are
//!   **never written**: only that a burst was found, its CRC status and the
//!   record's type and product model.
//!
//! The scale (recording units to ADC codes) is chosen on the first dwell:
//! the recording's own noise (median over LOs of the 10th-percentile
//! 51.2 µs power) `--rec-noise-db` (-6) under the C5's receiver noise, but
//! the loudest LO's 90th percentile at most `--level-db` (25) over it. Pass
//! `--scale` (printed in the meta file) to keep one gain across several
//! packs. The C5 has a fixed gain (AGC off), so relative levels between LOs
//! are kept. (A first version set the loudest LO to +15 dB; on the EVO Nano
//! pack that put the USRP's own noise 9 dB over the C5's, which the model
//! then saw as a flat wideband signal.)

mod c5;
mod dsp;
mod probe;
mod summary;
mod survey;

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use droneid::math::{C16, remove_dc};
use droneid::rx::Receiver;
use rawcap::dsp::decimate_nibbles;
use serde_json::{Value, json};

use dsp::{Cf, Channel};

const DWELL_MS: f64 = 60.0;
/// DroneID decode overlap between blocks (samples at 20 MS/s): 2 ms.
const DID_OVERLAP: usize = 40_000;

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  rfuav-check run <file|-> --fc-mhz <MHz> --out <dir> --tag <name> [--fs 100e6] [--lo all|a,b,..] [--rec-noise-db -6] [--level-db 25|--scale s] [--stride 4] [--max-ms ms] [--t0-ms ms] [--no-survey]\n  rfuav-check summary <dir> <tag>"
    );
    ExitCode::from(2)
}

struct DidState {
    rx: Box<Receiver>,
    buf: Vec<C16>,
    /// Absolute 20 MS/s index of `buf[0]` and of the first not-yet-owned sample.
    abs0: u64,
    owned: u64,
}

impl DidState {
    fn new() -> DidState {
        DidState { rx: Box::new(Receiver::new(20_000_000).unwrap()), buf: vec![], abs0: 0, owned: 0 }
    }

    /// Append, decode, and return found bursts (redacted JSON).
    fn feed(&mut self, x: &[C16], last: bool, lo: u16, path: &str, t_base_ms: f64) -> Vec<Value> {
        self.buf.extend_from_slice(x);
        let mut out = vec![];
        let limit = if last { u64::MAX } else { self.abs0 + self.buf.len().saturating_sub(DID_OVERLAP) as u64 };
        let (abs0, owned) = (self.abs0, self.owned);
        droneid::host::decode_recording(&mut self.rx, &self.buf, |f| {
            let start = match &f.result {
                Ok(b) => f.chunk_start as f64 + b.start,
                Err((s, _)) => (f.chunk_start + s.start) as f64,
            };
            let abs = abs0 + start.max(0.0) as u64;
            if abs < owned || abs >= limit {
                return;
            }
            let t_ms = t_base_ms + abs as f64 / 20e3;
            match f.result {
                Ok(b) => {
                    let rec = if b.crc24_ok { b.parsed.as_ref().map(redacted_record) } else { None };
                    out.push(json!({
                        "t": "droneid", "path": path, "lo": lo, "t_ms": t_ms, "layout": b.layout.name(),
                        "cfo_hz": b.cfo_hz.round(), "snr_db": (b.snr_db * 10.0).round() / 10.0,
                        "crc24_ok": b.crc24_ok, "record": rec,
                    }));
                }
                Err((_, e)) => out.push(json!({"t": "droneid", "path": path, "lo": lo, "t_ms": t_ms, "error": format!("{e:?}")})),
            }
        });
        if !last {
            self.owned = limit;
            let drop = self.buf.len().saturating_sub(DID_OVERLAP);
            self.buf.drain(..drop);
            self.abs0 += drop as u64;
        }
        out
    }
}

/// Type, CRC and model only: never a serial, UUID or position.
fn redacted_record(p: &droneid::payload::Parsed) -> Value {
    use droneid::payload::Record;
    match &p.record {
        Record::FlightInfo(f) => json!({"type": "flight_info", "crc16_ok": p.crc16_ok, "product": f.product_name()}),
        Record::Encrypted(e) => json!({"type": "o4_encrypted", "crc16_ok": p.crc16_ok, "record": e.kind.name()}),
        Record::Info11 { .. } => json!({"type": "info_0x11", "crc16_ok": p.crc16_ok}),
        Record::Unknown { selector, .. } => json!({"type": "unknown", "crc16_ok": p.crc16_ok, "selector": selector}),
    }
}

struct Worker {
    lo: c5::Lo,
    ch: Channel,
    /// DroneID LO: decode both paths.
    did: Option<(DidState, DidState)>,
    /// A sweep LO (dwell lines) or only a DroneID LO.
    sweep: bool,
    re: Vec<f32>,
    im: Vec<f32>,
}

fn read_block(r: &mut impl Read, want: usize) -> std::io::Result<Vec<Cf>> {
    let mut b = vec![0u8; want * 8];
    let mut got = 0;
    while got < b.len() {
        let n = r.read(&mut b[got..])?;
        if n == 0 {
            break;
        }
        got += n;
    }
    b.truncate(got / 8 * 8);
    Ok(b.chunks_exact(8)
        .map(|c| [f32::from_le_bytes([c[0], c[1], c[2], c[3]]), f32::from_le_bytes([c[4], c[5], c[6], c[7]])])
        .collect())
}

fn run(args: &[String]) -> ExitCode {
    let mut input = None;
    let (mut fc_mhz, mut fs) = (None::<f64>, 100e6);
    let (mut out, mut tag) = (None::<PathBuf>, None::<String>);
    let mut lo_arg = "all".to_string();
    let (mut level_db, mut scale) = (25.0f64, None::<f64>);
    let mut rec_noise_db = -6.0f64;
    let (mut stride, mut max_ms, mut t0_ms) = (4usize, f64::INFINITY, 0.0f64);
    let mut do_survey = true;
    let mut save: Option<PathBuf> = None;
    let mut save_ms = 3000.0f64;
    let mut do_droneid = true;
    let mut extra_did: Vec<u16> = vec![];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().unwrap_or_default();
        match a.as_str() {
            "--fc-mhz" => fc_mhz = val().parse().ok(),
            "--fs" => fs = val().parse().unwrap_or(fs),
            "--out" => out = Some(PathBuf::from(val())),
            "--tag" => tag = Some(val()),
            "--lo" => lo_arg = val(),
            "--level-db" => level_db = val().parse().unwrap_or(level_db),
            "--rec-noise-db" => rec_noise_db = val().parse().unwrap_or(rec_noise_db),
            "--scale" => scale = val().parse().ok(),
            "--stride" => stride = val().parse().unwrap_or(stride),
            "--max-ms" => max_ms = val().parse().unwrap_or(max_ms),
            "--t0-ms" => t0_ms = val().parse().unwrap_or(0.0),
            "--no-survey" => do_survey = false,
            "--save" => save = Some(PathBuf::from(val())),
            "--save-ms" => save_ms = val().parse().unwrap_or(save_ms),
            "--no-droneid" => do_droneid = false,
            "--droneid-lo" => extra_did = val().split(',').filter_map(|v| v.trim().parse().ok()).collect(),
            a if input.is_none() && (a == "-" || !a.starts_with('-')) => input = Some(a.to_string()),
            _ => return usage(),
        }
    }
    let (Some(input), Some(fc_mhz), Some(out), Some(tag)) = (input, fc_mhz, out, tag) else { return usage() };
    std::fs::create_dir_all(&out).expect("out dir");
    let fc_hz = fc_mhz * 1e6;
    let half = fs / 2.0;
    // A 20 MHz slice around LO fits when LO +- 10 MHz is inside the
    // recording's band (keep 2 MHz from its edges).
    let fits = |lo_mhz: f64| (lo_mhz * 1e6 - fc_hz).abs() + 10e6 <= half - 2e6;
    let mut los: Vec<(u16, u16)> = vec![]; // (lo, channel)
    if lo_arg == "all" {
        for &ch in fpvscan::band::sweep() {
            let (lo, _) = fpvscan::band::lo_for(ch);
            if fits(lo as f64) && !los.iter().any(|&(l, _)| l == lo) {
                los.push((lo, ch));
            }
        }
    } else {
        for s in lo_arg.split(',') {
            if let Ok(ch) = s.trim().parse::<u16>() {
                los.push((fpvscan::band::lo_for(ch).0, ch));
            }
        }
    }
    // DroneID LOs: the whole MHz below each centre in the recording.
    let mut did_los: Vec<u16> = vec![];
    for &c in djilink::plan::DRONEID_KHZ.iter().chain([djilink::plan::DRONEID_O4_EXTRA_KHZ].iter()) {
        let lo = (c / 1000) as u16;
        if do_droneid && fits(lo as f64) {
            did_los.push(lo);
        }
    }
    for lo in extra_did {
        if !did_los.contains(&lo) {
            did_los.push(lo);
        }
    }
    let mut workers: Vec<Worker> = vec![];
    let mk = |lo: u16| Channel::new(fs, lo as f64 * 1e6 - fc_hz, 2, 5 * (fs / 100e6).round() as usize, 13e6, 64);
    for (k, &(lo, ch)) in los.iter().enumerate() {
        let did = did_los.contains(&lo).then(|| (DidState::new(), DidState::new()));
        workers.push(Worker { lo: c5::Lo::new(lo, ch, 1000 + k as u64), ch: mk(lo), did, sweep: true, re: vec![], im: vec![] });
    }
    for &lo in &did_los {
        if !los.iter().any(|&(l, _)| l == lo) {
            workers.push(Worker { lo: c5::Lo::new(lo, lo + 1, 5000 + lo as u64), ch: mk(lo), did: Some((DidState::new(), DidState::new())), sweep: false, re: vec![], im: vec![] });
        }
    }
    eprintln!("LOs (sweep): {:?}; DroneID LOs: {:?}", los, did_los);
    let floor = c5::noise_floor(77);
    let reader: Box<dyn Read> = if input == "-" { Box::new(std::io::stdin().lock()) } else { Box::new(std::fs::File::open(&input).expect("input")) };
    let mut reader = BufReader::with_capacity(1 << 24, reader);
    let p = |s: &str| out.join(format!("{tag}.{s}"));
    let mut dwell_out = std::io::BufWriter::new(std::fs::File::create(p("dwell.jsonl")).unwrap());
    let mut win_out = std::io::BufWriter::new(std::fs::File::create(p("win.jsonl")).unwrap());
    let mut did_out = std::io::BufWriter::new(std::fs::File::create(p("droneid.jsonl")).unwrap());
    let mut sv = do_survey.then(|| survey::Survey::new(fs, fc_hz, &p("survey.csv")));
    let blk = (fs * DWELL_MS / 1e3) as usize;
    let mut k = 0usize;
    let threads = workers.len().max(1); // one thread per LO; the OS balances them
    loop {
        let t_ms = t0_ms + k as f64 * DWELL_MS;
        if (k as f64) * DWELL_MS >= max_ms {
            break;
        }
        let x = read_block(&mut reader, blk).expect("read");
        let last = x.len() < blk || reader.fill_buf().map(|b| b.is_empty()).unwrap_or(true) || ((k + 1) as f64) * DWELL_MS >= max_ms;
        if x.len() < blk / 10 {
            break;
        }
        let tp0 = std::time::Instant::now();
        // Phase 1: channelise (and survey) in parallel.
        std::thread::scope(|s| {
            if let Some(sv) = sv.as_mut() {
                let x = &x;
                s.spawn(move || sv.push(x));
            }
            let per = workers.len().div_ceil(threads.max(1)).max(1);
            for part in workers.chunks_mut(per) {
                let x = &x;
                s.spawn(move || {
                    for w in part {
                        w.re.clear();
                        w.im.clear();
                        w.ch.push(x, &mut w.re, &mut w.im);
                    }
                });
            }
        });
        if scale.is_none() {
            // Per LO, the 10th and 90th percentile 51.2 µs power.
            let (mut p10s, mut pmax) = (vec![], 0f64);
            for w in &workers {
                let mut ps: Vec<f64> = w
                    .re
                    .chunks_exact(2048)
                    .zip(w.im.chunks_exact(2048))
                    .map(|(a, b)| a.iter().zip(b).map(|(x, y)| (x * x + y * y) as f64).sum::<f64>() / 2048.0)
                    .collect();
                ps.sort_by(|a, b| a.partial_cmp(b).unwrap());
                if !ps.is_empty() {
                    p10s.push(ps[ps.len() / 10]);
                    pmax = pmax.max(ps[ps.len() * 9 / 10]);
                }
            }
            p10s.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let rec_noise = p10s.get(p10s.len() / 2).copied().unwrap_or(1e-30).max(1e-30);
            let c5_noise = 2.0 * (c5::NOISE as f64).powi(2);
            // The recording's own noise well under the C5's (or the model
            // sees the USRP's noise as a flat wideband signal), the loudest
            // LO's 90th percentile at most `level_db` over the C5's noise.
            let by_noise = (c5_noise * 10f64.powf(rec_noise_db / 10.0) / rec_noise).sqrt();
            let by_level = (c5_noise * 10f64.powf(level_db / 10.0) / pmax.max(1e-30)).sqrt();
            scale = Some(by_noise.min(by_level));
            eprintln!(
                "scale {:.6e}: recording noise {:+.1} dB, loudest LO p90 {:+.1} dB against the C5's noise",
                scale.unwrap(),
                10.0 * (rec_noise * scale.unwrap().powi(2) / c5_noise).log10(),
                10.0 * (pmax * scale.unwrap().powi(2) / c5_noise).log10()
            );
        }
        let tp1 = tp0.elapsed();
        let sc = scale.unwrap() as f32;
        // Phase 2: the C5 model and DroneID per LO.
        let save_now = save.is_some() && (k as f64) * DWELL_MS < save_ms;
        let results: Vec<(Vec<Value>, Vec<Value>, Vec<Value>, Option<(u16, u16, Vec<u8>)>)> = std::thread::scope(|s| {
            let per = workers.len().div_ceil(threads.max(1)).max(1);
            let hs: Vec<_> = workers
                .chunks_mut(per)
                .map(|part| {
                    s.spawn(move || {
                        let mut r = vec![];
                        for w in part {
                            for v in w.re.iter_mut().chain(w.im.iter_mut()) {
                                *v *= sc;
                            }
                            let n = w.re.len().min(w.im.len()) / 2048 * 2048;
                            if n < c5::WINDOW_BYTES + 2048 {
                                continue;
                            }
                            let d = c5::DwellIn { re: &w.re[..n], im: &w.im[..n] };
                            let (line, wins, bytes) = w.lo.dwell(&d, floor, t_ms, if w.sweep { stride } else { 0 });
                            let mut dids = vec![];
                            if let Some((dc5, dcl)) = w.did.as_mut() {
                                let m = (bytes.len() - 10) / 2;
                                let mut c = vec![C16::default(); m];
                                decimate_nibbles(|i| bytes[i], &mut c);
                                remove_dc(&mut c);
                                dids.extend(dc5.feed(&c, last, w.lo.lo_mhz, "c5", t0_ms));
                                let h = dsp::halfband(&w.re[..n], &w.im[..n]);
                                let mut c: Vec<C16> = h
                                    .iter()
                                    .map(|v| C16 { re: (v[0] * 32.0).round().clamp(-32768.0, 32767.0) as i16, im: (v[1] * 32.0).round().clamp(-32768.0, 32767.0) as i16 })
                                    .collect();
                                remove_dc(&mut c);
                                dids.extend(dcl.feed(&c, last, w.lo.lo_mhz, "clean", t0_ms));
                            }
                            let keep = if w.sweep && save_now { Some((w.lo.lo_mhz, w.lo.ch_mhz, bytes)) } else { None };
                            r.push((if w.sweep { vec![line] } else { vec![] }, wins, dids, keep));
                        }
                        r
                    })
                })
                .collect();
            hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
        });
        if std::env::var_os("RFUAV_TIMING").is_some() {
            eprintln!("block {k}: phase1 {:?} phase2 {:?}", tp1, tp0.elapsed() - tp1);
        }
        for (lines, wins, dids, keep) in results {
            if let (Some(dir), Some((lo, ch, bytes))) = (save.as_ref(), keep) {
                save_dwell(dir, &tag, lo, ch, t_ms, &bytes);
            }
            for l in lines {
                writeln!(dwell_out, "{l}").unwrap();
            }
            for l in wins {
                writeln!(win_out, "{l}").unwrap();
            }
            for l in dids {
                writeln!(did_out, "{l}").unwrap();
            }
        }
        k += 1;
        if k % 16 == 0 {
            eprintln!("{:.2} s", k as f64 * DWELL_MS / 1e3);
        }
        if last {
            break;
        }
    }
    dwell_out.flush().unwrap();
    win_out.flush().unwrap();
    did_out.flush().unwrap();
    let mut meta = json!({
        "input": input, "fc_mhz": fc_mhz, "fs": fs, "scale": scale, "level_db": level_db, "floor_x10": floor,
        "los": los, "droneid_los": did_los, "dwells": k, "t0_ms": t0_ms, "stride": stride,
    });
    if let Some(sv) = sv.as_mut() {
        sv.finish();
        meta["survey_bursts"] = json!(sv.bursts);
        meta["psd"] = json!(sv.psd_db().iter().map(|&(f, d)| [((f * 1000.0).round() / 1000.0) as f32, (d * 10.0).round() / 10.0]).collect::<Vec<_>>());
        for (name, a) in [("lag_narrow", &sv.narrow), ("lag_wide", &sv.wide)] {
            meta[name] = json!({"fs": a.fs, "bursts": a.bursts, "acc": a.acc.iter().map(|v| (v * 1e4).round() / 1e4).collect::<Vec<_>>()});
        }
    }
    std::fs::write(p("meta.json"), serde_json::to_string(&meta).unwrap()).unwrap();
    eprintln!("done: {k} dwells");
    ExitCode::SUCCESS
}

/// Append one dwell of quantised bytes: `t_ms` (f64), channel (u16),
/// length (u32), bytes. These are derived from a recording: keep them with
/// the recordings (never in the repository) and delete them with them.
fn save_dwell(dir: &std::path::Path, tag: &str, lo: u16, ch: u16, t_ms: f64, bytes: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(format!("{tag}.lo{lo}.nib"));
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p).unwrap();
    f.write_all(&t_ms.to_le_bytes()).unwrap();
    f.write_all(&ch.to_le_bytes()).unwrap();
    f.write_all(&(bytes.len() as u32).to_le_bytes()).unwrap();
    f.write_all(bytes).unwrap();
}

/// `replay <nib dir> <tag> <out dir> [--stride n]`: the firmware's dwell
/// logic again over saved quantised dwells (after a detector change).
fn replay(args: &[String]) -> ExitCode {
    let (dir, tag, out) = (PathBuf::from(&args[0]), &args[1], PathBuf::from(&args[2]));
    let stride: usize = args.iter().position(|a| a == "--stride").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(4);
    std::fs::create_dir_all(&out).unwrap();
    let floor = c5::noise_floor(77);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&format!("{tag}.lo")) && n.ends_with(".nib")))
        .collect();
    files.sort();
    let results: Vec<(Vec<Value>, Vec<Value>)> = std::thread::scope(|s| {
        let hs: Vec<_> = files
            .iter()
            .map(|p| {
                s.spawn(move || {
                    let b = std::fs::read(p).unwrap();
                    let lo: u16 = p.file_name().unwrap().to_str().unwrap().rsplit(".lo").next().unwrap().trim_end_matches(".nib").parse().unwrap();
                    let mut o = (vec![], vec![]);
                    let mut i = 0;
                    let mut st: Option<c5::Lo> = None;
                    while i + 14 <= b.len() {
                        let t = f64::from_le_bytes(b[i..i + 8].try_into().unwrap());
                        let ch = u16::from_le_bytes(b[i + 8..i + 10].try_into().unwrap());
                        let n = u32::from_le_bytes(b[i + 10..i + 14].try_into().unwrap()) as usize;
                        let st = st.get_or_insert_with(|| c5::Lo::new(lo, ch, 1));
                        let (l, w) = st.judge(&b[i + 14..i + 14 + n], None, floor, t, stride);
                        o.0.push(l);
                        o.1.extend(w);
                        i += 14 + n;
                    }
                    o
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let p = |s: &str| out.join(format!("{tag}.{s}"));
    let mut d = std::io::BufWriter::new(std::fs::File::create(p("dwell.jsonl")).unwrap());
    let mut w = std::io::BufWriter::new(std::fs::File::create(p("win.jsonl")).unwrap());
    let mut nd = 0;
    for (ls, ws) in results {
        nd = nd.max(ls.len());
        for l in ls {
            writeln!(d, "{l}").unwrap();
        }
        for l in ws {
            writeln!(w, "{l}").unwrap();
        }
    }
    std::fs::write(p("meta.json"), json!({"input": dir, "dwells": nd, "t0_ms": 0.0, "stride": stride, "replay": true}).to_string()).unwrap();
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("run") => run(&args[1..]),
        Some("probe") => probe::run(&args[1..]),
        Some("replay") if args.len() >= 4 => replay(&args[1..]),
        Some("summary") if args.len() >= 3 => summary::run(&PathBuf::from(&args[1]), &args[2..]),
        _ => usage(),
    }
}
