//! Summaries of one `run`'s output files: what the survey saw (burst
//! classes, hop raster, timing, symbol length), what the C5 model's
//! detectors said per LO, a sweep simulation with the real hop tracker, and
//! the DroneID counts.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::Path;
use std::process::ExitCode;

use serde_json::Value;

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn dist(name: &str, v: &[f64]) -> String {
    let mut v = v.to_vec();
    format!(
        "{name}: n {} p10 {:.3} p50 {:.3} p90 {:.3} min {:.3} max {:.3}",
        v.len(),
        pct(&mut v, 0.1),
        pct(&mut v, 0.5),
        pct(&mut v, 0.9),
        pct(&mut v, 0.0),
        pct(&mut v, 1.0)
    )
}

#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub t_ms: f64,
    pub dur_us: f64,
    pub f: f64,
    pub w: f64,
    pub wmax: f64,
    pub peak: f64,
    /// Lag features (NaN when not analysed): best lag us, |r|^2 there, at
    /// 33.33 us, at 66.67 us, median over 2-80 us.
    pub lag: [f64; 5],
}

pub fn read_survey(p: &Path) -> Vec<Row> {
    let Ok(f) = std::fs::File::open(p) else { return vec![] };
    std::io::BufReader::new(f)
        .lines()
        .skip(1)
        .filter_map(|l| {
            let l = l.ok()?;
            let v: Vec<f64> = l.split(',').map(|s| s.parse().unwrap_or(f64::NAN)).collect();
            let g = |i: usize| v.get(i).copied().unwrap_or(f64::NAN);
            Some(Row { t_ms: v[0], dur_us: v[1], f: v[2], w: v[3], wmax: v[4], peak: v[7], lag: [g(9), g(10), g(11), g(12), g(13)] })
        })
        .collect()
}

fn jsonl(p: &Path) -> Vec<Value> {
    let Ok(f) = std::fs::File::open(p) else { return vec![] };
    std::io::BufReader::new(f).lines().filter_map(|l| serde_json::from_str(&l.ok()?).ok()).collect()
}

/// Cluster sorted values: a new cluster when the gap exceeds `gap`.
fn clusters(mut v: Vec<f64>, gap: f64) -> Vec<(f64, usize)> {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut out: Vec<(f64, usize, f64)> = vec![];
    for x in v {
        match out.last_mut() {
            Some((_, n, s)) if x - (*s / *n as f64) <= gap => {
                *n += 1;
                *s += x;
            }
            _ => out.push((x, 1, x)),
        }
    }
    out.into_iter().map(|(_, n, s)| (s / n as f64, n)).collect()
}

fn class_report(name: &str, rows: &[&Row], span_ms: f64) {
    if rows.is_empty() {
        println!("  {name}: none");
        return;
    }
    let d: Vec<f64> = rows.iter().map(|r| r.dur_us).collect();
    let w: Vec<f64> = rows.iter().map(|r| r.w).collect();
    let pk: Vec<f64> = rows.iter().map(|r| r.peak).collect();
    let on: f64 = d.iter().sum::<f64>() / 1e3;
    println!("  {name}: {} bursts, {:.1} /s, on {:.1} % of the time", rows.len(), rows.len() as f64 / span_ms * 1e3, on / span_ms * 100.0);
    println!("    {}", dist("dur_us", &d));
    println!("    {}", dist("width_mhz", &w));
    println!("    {}", dist("peak_db", &pk));
    let mut s: Vec<f64> = rows.iter().map(|r| r.t_ms).collect();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let gaps: Vec<f64> = s.windows(2).map(|p| (p[1] - p[0]) * 1e3).collect();
    println!("    {}", dist("start_to_start_us", &gaps));
    let cl = clusters(rows.iter().map(|r| r.f).collect(), 0.35);
    let mut top = cl.clone();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    let shown: Vec<String> = top.iter().take(24).map(|(f, n)| format!("{f:.3}x{n}")).collect();
    println!("    centres ({} clusters, gap 0.35 MHz): {}", cl.len(), shown.join(" "));
    // Per busiest centre: symbol-lag evidence (mean |r|^2 at 33.33 and
    // 66.67 us over the mean median-lag level), width, length.
    for (c, n) in top.iter().take(12) {
        let rs: Vec<&&Row> = rows.iter().filter(|r| (r.f - c).abs() < 0.35).collect();
        let an: Vec<&&&Row> = rs.iter().filter(|r| r.lag[4].is_finite() && r.lag[4] > 0.0).collect();
        let m = |i: usize| an.iter().map(|r| r.lag[i]).sum::<f64>() / an.len().max(1) as f64;
        let mut bl: Vec<f64> = an.iter().map(|r| r.lag[0]).collect();
        let mut d: Vec<f64> = rs.iter().map(|r| r.dur_us).collect();
        let mut w: Vec<f64> = rs.iter().map(|r| r.w).collect();
        let mut pk: Vec<f64> = rs.iter().map(|r| r.peak).collect();
        println!(
            "      {c:.3} MHz x{n}: dur p50 {:.0} us, width p50 {:.2} MHz, peak p50 {:.0} dB; lag-analysed {}: r33/med {:.2}, r67/med {:.2}, best-lag p50 {:.2} us",
            pct(&mut d, 0.5),
            pct(&mut w, 0.5),
            pct(&mut pk, 0.5),
            an.len(),
            m(2) / m(4),
            m(3) / m(4),
            pct(&mut bl, 0.5)
        );
    }
}

fn lag_report(name: &str, m: &Value) {
    let Some(acc) = m["acc"].as_array() else { return };
    let fs = m["fs"].as_f64().unwrap_or(1.0);
    let nb = m["bursts"].as_u64().unwrap_or(0);
    if nb == 0 {
        println!("  {name}: no bursts analysed");
        return;
    }
    let a: Vec<f64> = acc.iter().map(|v| v.as_f64().unwrap_or(0.0) / nb as f64).collect();
    let us = |l: usize| l as f64 / fs * 1e6;
    let lo = (2e-6 * fs) as usize;
    let mut tail: Vec<f64> = a[lo..].to_vec();
    let med = pct(&mut tail, 0.5);
    let mut peaks: Vec<(usize, f64)> = (lo.max(1)..a.len() - 1).filter(|&l| a[l] >= a[l - 1] && a[l] >= a[l + 1]).map(|l| (l, a[l] / med)).collect();
    peaks.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap());
    let at = |t_us: f64| {
        let l = (t_us * 1e-6 * fs).round() as usize;
        (l.saturating_sub(2)..=(l + 2).min(a.len() - 1)).map(|k| a[k]).fold(0.0, f64::max) / med
    };
    println!(
        "  {name}: {nb} bursts; |r(lag)|^2 over its median, top peaks: {}; at 66.67 us {:.2}, 33.33 us {:.2}, 3.2 us {:.2}",
        peaks.iter().take(6).map(|(l, v)| format!("{:.2}us {:.2}", us(*l), v)).collect::<Vec<_>>().join(", "),
        at(66.667),
        at(33.333),
        at(3.2)
    );
}

pub fn run(dir: &Path, args: &[String]) -> ExitCode {
    let tag = &args[0];
    let p = |s: &str| dir.join(format!("{tag}.{s}"));
    let meta: Value = std::fs::read_to_string(p("meta.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
    let span_ms = meta["dwells"].as_f64().unwrap_or(0.0) * 60.0;
    println!("== {tag}: {} ({:.1} s), centre {} MHz, scale {}, LOs {}", meta["input"], span_ms / 1e3, meta["fc_mhz"], meta["scale"], meta["los"]);

    // Occupied spectrum.
    if let Some(psd) = meta["psd"].as_array() {
        let mut runs = vec![];
        let mut cur: Option<(f64, f64, f64)> = None;
        for v in psd {
            let (f, d) = (v[0].as_f64().unwrap(), v[1].as_f64().unwrap());
            if d >= 3.0 {
                cur = Some(match cur {
                    Some((a, _, m)) => (a, f, m.max(d)),
                    None => (f, f, d),
                });
            } else if let Some(c) = cur.take() {
                runs.push(c);
            }
        }
        runs.extend(cur);
        println!(
            "mean spectrum >= 3 dB over its median: {}",
            runs.iter().filter(|r| r.1 - r.0 >= 0.3).map(|(a, b, m)| format!("{a:.1}-{b:.1} MHz (max {m:.1} dB)")).collect::<Vec<_>>().join(", ")
        );
    }

    // Survey.
    let rows = read_survey(&p("survey.csv"));
    if !rows.is_empty() {
        println!("survey: {} tracks", rows.len());
        let narrow: Vec<&Row> = rows.iter().filter(|r| (0.4..4.0).contains(&r.w) && (100.0..5000.0).contains(&r.dur_us) && r.wmax < 6.0).collect();
        let mid: Vec<&Row> = rows.iter().filter(|r| (4.0..7.0).contains(&r.w) && r.dur_us >= 100.0).collect();
        let did: Vec<&Row> = rows.iter().filter(|r| (7.0..11.0).contains(&r.w) && (400.0..850.0).contains(&r.dur_us)).collect();
        let wide: Vec<&Row> = rows.iter().filter(|r| r.w >= 7.0 && r.dur_us >= 850.0 || r.w >= 11.0 && r.dur_us >= 100.0).collect();
        class_report("narrow (0.4-4 MHz, 0.1-5 ms)", &narrow, span_ms);
        class_report("mid (4-7 MHz)", &mid, span_ms);
        class_report("droneid-shaped (7-11 MHz, 0.4-0.85 ms)", &did, span_ms);
        class_report("wide (>= 7 MHz and >= 0.85 ms, or >= 11 MHz)", &wide, span_ms);
        // Narrow burst raster and hop behaviour.
        if narrow.len() >= 10 {
            let cl = clusters(narrow.iter().map(|r| r.f).collect(), 0.35);
            let strong: Vec<f64> = cl.iter().filter(|c| c.1 >= 3).map(|c| c.0).collect();
            let frac: Vec<f64> = strong.iter().map(|f| (f * 1000.0).rem_euclid(1000.0) / 1000.0).collect();
            let diffs: Vec<f64> = strong.windows(2).map(|w| w[1] - w[0]).collect();
            println!("  narrow raster: {} centres with >= 3 bursts; {}", strong.len(), dist("centre mod 1 MHz", &frac));
            println!("    {}", dist("spacing between adjacent centres (MHz)", &diffs));
            let mut seq: Vec<&&Row> = narrow.iter().collect();
            seq.sort_by(|a, b| a.t_ms.partial_cmp(&b.t_ms).unwrap());
            let same = seq.windows(2).filter(|w| (w[0].f - w[1].f).abs() < 0.35).count();
            let steps: Vec<f64> = seq.windows(2).map(|w| (w[1].f - w[0].f).abs()).filter(|d| *d >= 0.35).collect();
            println!("    consecutive bursts on the same centre: {same} of {}; {}", seq.len() - 1, dist("hop step (MHz)", &steps));
            // Revisit time per centre.
            let mut revisit = vec![];
            for c in &strong {
                let mut t: Vec<f64> = seq.iter().filter(|r| (r.f - c).abs() < 0.35).map(|r| r.t_ms).collect();
                t.sort_by(|a, b| a.partial_cmp(b).unwrap());
                revisit.extend(t.windows(2).map(|w| w[1] - w[0]));
            }
            println!("    {}", dist("revisit interval on one centre (ms)", &revisit));
        }
    }
    println!("lag scan (survey, full rate):");
    lag_report("narrow", &meta["lag_narrow"]);
    lag_report("wide", &meta["lag_wide"]);

    // C5 model: firmware dwells.
    let dwells = jsonl(&p("dwell.jsonl"));
    let mut by_lo: BTreeMap<u64, Vec<&Value>> = BTreeMap::new();
    for d in &dwells {
        by_lo.entry(d["lo"].as_u64().unwrap_or(0)).or_default().push(d);
    }
    println!("C5 model, firmware dwells (one capture per 60 ms dwell): per LO, fpvscan kinds | djilink verdicts | median snr/duty | timeline median bursts/burst_us/period_us");
    for (lo, ds) in &by_lo {
        let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
        let mut verd: BTreeMap<String, usize> = BTreeMap::new();
        for d in ds {
            *kinds.entry(d["kind"].as_str().unwrap_or("?").to_string()).or_default() += 1;
            let v = d["dji"]["v"].as_str().unwrap_or("-").to_string();
            *verd.entry(v).or_default() += 1;
        }
        let mut snr: Vec<f64> = ds.iter().map(|d| d["snr_db"].as_f64().unwrap_or(0.0)).collect();
        let mut duty: Vec<f64> = ds.iter().map(|d| d["duty"].as_f64().unwrap_or(0.0)).collect();
        let tl = |i: usize| -> f64 {
            let mut v: Vec<f64> = ds.iter().map(|d| d["tl"][i].as_f64().unwrap_or(0.0)).collect();
            pct(&mut v, 0.5)
        };
        println!(
            "  LO {lo}: {:?} | {:?} | snr {:.0} dB duty {:.0} % | tl {:.0} / {:.0} / {:.0}",
            kinds,
            verd,
            pct(&mut snr, 0.5),
            pct(&mut duty, 0.5),
            tl(2),
            tl(3),
            tl(4)
        );
    }
    // Sweep simulation with the real hop tracker: the node's built-in sweep
    // (both bands), one 60 ms dwell per channel, started at every offset.
    let sweep = fpvscan::band::sweep();
    let nd = meta["dwells"].as_u64().unwrap_or(0) as usize;
    let t0 = meta["t0_ms"].as_f64().unwrap_or(0.0);
    let key = |lo: u64, k: usize| (lo, k);
    let mut at: BTreeMap<(u64, usize), &Value> = BTreeMap::new();
    for d in &dwells {
        let k = ((d["t_ms"].as_f64().unwrap_or(0.0) - t0) / 60.0).round() as usize;
        at.insert(key(d["lo"].as_u64().unwrap_or(0), k), d);
    }
    if nd > 0 && !dwells.is_empty() {
        let mut confirmed_runs = 0;
        let mut first_confirm = vec![];
        let mut reports: BTreeMap<String, usize> = BTreeMap::new();
        let mut covered = 0;
        for off in 0..sweep.len() {
            let mut hops = djilink::HopTracker::new();
            let mut first = None;
            for k in 0..nd {
                let ch = sweep[(k + off) % sweep.len()];
                let lo = fpvscan::band::lo_for(ch).0 as u64;
                let Some(d) = at.get(&key(lo, k)) else { continue };
                if off == 0 {
                    covered += 1;
                }
                let v = d["dji"]["v"].as_str().unwrap_or("-");
                // As radio/dji.rs: hops from every capture, the controller
                // reported on a controller-verdict dwell once confirmed.
                let now = (k * 60) as u32;
                let mut c = false;
                for h in d["dji"]["hops"].as_array().into_iter().flatten() {
                    c |= hops.burst(h.as_u64().unwrap_or(0) as u32, now);
                }
                if v == "dji_controller_burst" {
                    if c && first.is_none() {
                        first = Some(k as f64 * 0.06);
                    }
                    if c {
                        *reports.entry("dji_controller (confirmed)".into()).or_default() += 1;
                    }
                } else if v != "-" && v != "none" {
                    *reports.entry(v.to_string()).or_default() += 1;
                } else if d["kind"].as_str().is_some_and(|k| k != "quiet") {
                    *reports.entry(format!("fpvscan:{}", d["kind"].as_str().unwrap())).or_default() += 1;
                }
            }
            if let Some(f) = first {
                confirmed_runs += 1;
                first_confirm.push(f);
            }
        }
        println!(
            "sweep simulation ({} channels, 60 ms each; {} of {} dwells of offset 0 land on a covered LO; all {} start offsets):",
            sweep.len(),
            covered,
            nd,
            sweep.len()
        );
        println!("  controller confirmed in {confirmed_runs} of {} offsets; {}", sweep.len(), dist("time to confirm (s)", &first_confirm));
        println!("  reports summed over all offsets: {reports:?}");
    }

    // C5 model: every window.
    let wins = jsonl(&p("win.jsonl"));
    if !wins.is_empty() {
        let mut by_lo: BTreeMap<u64, (BTreeMap<String, usize>, BTreeMap<String, usize>)> = BTreeMap::new();
        for w in &wins {
            let e = by_lo.entry(w["lo"].as_u64().unwrap_or(0)).or_default();
            *e.0.entry(w["v"].as_str().unwrap_or("?").to_string()).or_default() += 1;
            *e.1.entry(w["vc"].as_str().unwrap_or("?").to_string()).or_default() += 1;
        }
        println!("C5 model, every {}th 0.82 ms window: per LO, verdicts (C5 4-bit path) | (clean path)", meta["stride"]);
        for (lo, (a, b)) in &by_lo {
            println!("  LO {lo}: {a:?} | {b:?}");
        }
        // Features per verdict (C5 path).
        let mut per: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
        for w in &wins {
            per.entry(w["v"].as_str().unwrap_or("?").to_string()).or_default().push(&w["f"]);
        }
        for (v, fs) in &per {
            let g = |f: &dyn Fn(&Value) -> Option<f64>| -> Vec<f64> { fs.iter().filter_map(|x| f(x)).collect() };
            println!("  [{v}] n {}", fs.len());
            println!("    {}", dist("n15 metric", &g(&|x| x["n15"][2].as_f64())));
            println!("    {}", dist("n15 thr", &g(&|x| x["n15"][3].as_f64())));
            println!("    {}", dist("n15 cp_samples", &g(&|x| x["n15"][4].as_f64())));
            println!("    {}", dist("e15 metric", &g(&|x| x["e15"][2].as_f64())));
            println!("    {}", dist("n30 metric", &g(&|x| x["n30"][2].as_f64())));
            println!("    {}", dist("w3 kHz", &g(&|x| x["w"][0].as_f64())));
            println!("    {}", dist("w10 kHz", &g(&|x| x["w"][1].as_f64())));
            println!("    {}", dist("centre kHz", &g(&|x| x["c"].as_f64())));
            println!("    {}", dist("on %", &g(&|x| x["on"].as_f64())));
            let bl: Vec<f64> = fs.iter().flat_map(|x| x["b"].as_array().cloned().unwrap_or_default()).filter(|b| b[1].as_u64() == Some(1)).filter_map(|b| b[0].as_f64()).collect();
            println!("    {}", dist("complete burst us", &bl));
            let bw: Vec<f64> = fs.iter().flat_map(|x| x["b"].as_array().cloned().unwrap_or_default()).filter_map(|b| b[3].as_f64()).filter(|w| *w > 0.0).collect();
            println!("    {}", dist("burst width kHz", &bw));
        }
    }

    // DroneID.
    let dids = jsonl(&p("droneid.jsonl"));
    let mut cnt: BTreeMap<(String, u64), (usize, usize, usize, BTreeMap<String, usize>)> = BTreeMap::new();
    for d in &dids {
        let e = cnt.entry((d["path"].as_str().unwrap_or("?").to_string(), d["lo"].as_u64().unwrap_or(0))).or_default();
        if d.get("error").is_some() {
            e.2 += 1;
        } else {
            e.0 += 1;
            if d["crc24_ok"].as_bool() == Some(true) {
                e.1 += 1;
                let r = &d["record"];
                let k = format!("{} {} crc16 {}", r["type"].as_str().unwrap_or("?"), r["product"].as_str().or(r["record"].as_str()).unwrap_or(""), r["crc16_ok"]);
                *e.3.entry(k).or_default() += 1;
            }
        }
    }
    println!("DroneID (path, LO): demodulated / CRC-valid / sync failures / record types:");
    if cnt.is_empty() {
        println!("  nothing found");
    }
    for ((path, lo), (n, ok, fail, rec)) in &cnt {
        println!("  {path} {lo}: {n} / {ok} / {fail} {rec:?}");
    }
    ExitCode::SUCCESS
}
