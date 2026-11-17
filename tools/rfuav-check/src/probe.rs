//! `probe`: measure one burst's OFDM symbol structure at the recording's
//! own rate. The burst is mixed to DC and low-passed to its width; the
//! useful symbol length is the lag `L` (searched in 0.01 µs steps) that
//! maximises `|sum x[n] conj(x[n+L])|`; at that lag the product, summed
//! over a sliding window, peaks once per symbol (inside each cyclic prefix):
//! the peaks' spacing is the symbol period and their width the prefix.

use std::io::Read;
use std::process::ExitCode;

use crate::dsp::{Cf, lowpass};

pub fn run(args: &[String]) -> ExitCode {
    let mut it = args.iter();
    let (mut input, mut fs, mut fc, mut f, mut bw) = (None::<String>, 100e6, None::<f64>, None::<f64>, 10e6);
    let (mut skip_ms, mut len_ms, mut lag_lo, mut lag_hi, mut win_us) = (0.0, 1.0, 20.0, 80.0, 1.0);
    while let Some(a) = it.next() {
        let mut v = || it.next().and_then(|s| s.parse::<f64>().ok());
        match a.as_str() {
            "--fs" => fs = v().unwrap_or(fs),
            "--fc-mhz" => fc = v().map(|x| x * 1e6),
            "--f-mhz" => f = v().map(|x| x * 1e6),
            "--bw-mhz" => bw = v().map(|x| x * 1e6).unwrap_or(bw),
            "--skip-ms" => skip_ms = v().unwrap_or(0.0),
            "--len-ms" => len_ms = v().unwrap_or(1.0),
            "--lag-us" => {
                lag_lo = v().unwrap_or(lag_lo);
                lag_hi = v().unwrap_or(lag_hi);
            }
            "--win-us" => win_us = v().unwrap_or(win_us),
            s => input = Some(s.to_string()),
        }
    }
    let (Some(input), Some(fc), Some(f)) = (input, fc, f) else {
        eprintln!("usage: rfuav-check probe <file|-> --fc-mhz <rec centre> --f-mhz <burst centre> [--bw-mhz 10] [--skip-ms 0] [--len-ms 1] [--lag-us 20 80] [--win-us 1]");
        return ExitCode::from(2);
    };
    let mut r: Box<dyn Read> = if input == "-" { Box::new(std::io::stdin().lock()) } else { Box::new(std::fs::File::open(&input).expect("input")) };
    let skip = (skip_ms * 1e-3 * fs) as u64 * 8;
    std::io::copy(&mut r.by_ref().take(skip), &mut std::io::sink()).unwrap();
    let n = (len_ms * 1e-3 * fs) as usize;
    let mut b = vec![0u8; n * 8];
    r.read_exact(&mut b).expect("short input");
    let x: Vec<Cf> = b.chunks_exact(8).map(|c| [f32::from_le_bytes([c[0], c[1], c[2], c[3]]), f32::from_le_bytes([c[4], c[5], c[6], c[7]])]).collect();
    // Mix and filter (full rate).
    let a = -2.0 * std::f64::consts::PI * (f - fc) / fs;
    let h = lowpass(129, bw / 2.0 + 0.3e6, fs, 1.0);
    let m: Vec<[f64; 2]> = x
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let (s, c) = (a * i as f64).sin_cos();
            [v[0] as f64 * c - v[1] as f64 * s, v[0] as f64 * s + v[1] as f64 * c]
        })
        .collect();
    let y: Vec<[f64; 2]> = (h.len()..m.len())
        .map(|k| {
            let mut o = [0.0, 0.0];
            for (j, &t) in h.iter().enumerate() {
                o[0] += t * m[k - j][0];
                o[1] += t * m[k - j][1];
            }
            o
        })
        .collect();
    let pw: f64 = y.iter().map(|v| v[0] * v[0] + v[1] * v[1]).sum();
    // Lag search.
    let corr = |l: usize| {
        let (mut re, mut im) = (0.0, 0.0);
        for k in 0..y.len() - l {
            let (p, q) = (y[k], y[k + l]);
            re += p[0] * q[0] + p[1] * q[1];
            im += p[1] * q[0] - p[0] * q[1];
        }
        (re * re + im * im).sqrt() / pw
    };
    let (l0, l1) = ((lag_lo * 1e-6 * fs) as usize, ((lag_hi * 1e-6 * fs) as usize).min(y.len() / 2));
    // Coarse (0.1 µs) then fine.
    let step = ((0.1e-6 * fs) as usize).max(1);
    let mut best = (0usize, 0.0f64);
    let mut coarse = vec![];
    let mut l = l0;
    while l <= l1 {
        let c = corr(l);
        coarse.push((l, c));
        if c > best.1 {
            best = (l, c);
        }
        l += step;
    }
    for l in best.0.saturating_sub(step)..=best.0 + step {
        let c = corr(l);
        if c > best.1 {
            best = (l, c);
        }
    }
    let mut cs: Vec<f64> = coarse.iter().map(|c| c.1).collect();
    cs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = cs[cs.len() / 2];
    let mut top: Vec<(usize, f64)> = coarse.iter().copied().collect();
    top.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    println!(
        "best lag {:.2} us, |r| {:.4} (median over {:.0}-{:.0} us: {:.4}); next coarse: {}",
        best.0 as f64 / fs * 1e6,
        best.1,
        lag_lo,
        lag_hi,
        med,
        top.iter().take(8).map(|(l, c)| format!("{:.1}us {:.3}", *l as f64 / fs * 1e6, c)).collect::<Vec<_>>().join(", ")
    );
    // Profile at the best lag.
    let lag = best.0;
    let w = ((win_us * 1e-6 * fs) as usize).max(1);
    let p: Vec<[f64; 3]> = (0..y.len() - lag)
        .map(|k| {
            let (a, b) = (y[k], y[k + lag]);
            [a[0] * b[0] + a[1] * b[1], a[1] * b[0] - a[0] * b[1], (a[0] * a[0] + a[1] * a[1] + b[0] * b[0] + b[1] * b[1]) / 2.0]
        })
        .collect();
    let mut prof = vec![0.0; p.len().saturating_sub(w)];
    let mut s = [0.0; 3];
    for k in 0..p.len() {
        for j in 0..3 {
            s[j] += p[k][j];
        }
        if k >= w {
            for j in 0..3 {
                s[j] -= p[k - w][j];
            }
        }
        if k + 1 >= w && k + 1 - w < prof.len() {
            prof[k + 1 - w] = (s[0] * s[0] + s[1] * s[1]).sqrt() / s[2].max(1e-30);
        }
    }
    // Peaks: local maxima over +-half a symbol above 0.5.
    let half = lag / 2;
    let mut peaks = vec![];
    for k in 0..prof.len() {
        if prof[k] < 0.5 {
            continue;
        }
        let a = k.saturating_sub(half);
        let b = (k + half).min(prof.len() - 1);
        if prof[a..=b].iter().all(|&v| v <= prof[k]) && peaks.last().is_none_or(|&q: &usize| k - q > half) {
            peaks.push(k);
        }
    }
    let us = |k: usize| k as f64 / fs * 1e6;
    let widths: Vec<f64> = peaks
        .iter()
        .map(|&k| {
            let th = prof[k] / 2.0;
            let mut a = k;
            while a > 0 && prof[a] >= th {
                a -= 1;
            }
            let mut b = k;
            while b + 1 < prof.len() && prof[b] >= th {
                b += 1;
            }
            // Window w widens the plateau: CP ~ half-height width - w + ...
            us(b - a)
        })
        .collect();
    println!("profile window {win_us} us: {} peaks over 0.5", peaks.len());
    for (i, &k) in peaks.iter().enumerate() {
        println!(
            "  t {:9.3} us  coh {:.2}  fwhm {:.2} us  gap {}",
            us(k),
            prof[k],
            widths[i],
            if i > 0 { format!("{:.3} us", us(k - peaks[i - 1])) } else { "-".into() }
        );
    }
    ExitCode::SUCCESS
}
