use crate::synth::Sound;
use crate::*;

/// Run a sound through a fresh detector; one [`FrameOut`] per 32 ms.
fn run(pcm: &[i16]) -> Vec<FrameOut> {
    let mut d = Box::new(Detector::zeroed());
    pcm.chunks_exact(HOP).map(|c| d.process(c.try_into().unwrap())).collect()
}

fn heard_at(out: &[FrameOut]) -> Option<f32> {
    out.iter().position(|f| f.state == State::Heard).map(|i| i as f32 / FRAMES_PER_S as f32)
}

fn heard_share(out: &[FrameOut], from_s: f32, to_s: f32) -> f32 {
    let (a, b) = ((from_s * FRAMES_PER_S as f32) as usize, (to_s * FRAMES_PER_S as f32) as usize);
    let w = &out[a..b.min(out.len())];
    w.iter().filter(|f| f.state == State::Heard).count() as f32 / w.len() as f32
}

#[test]
fn fft_finds_a_tone() {
    let pcm = Sound::silence(1.0).ladder(0.0, 1.0, 0.25, 1, 1.0, |_| 1000.0).into_pcm();
    let mut d = Box::new(Detector::zeroed());
    for c in pcm.chunks_exact(HOP).take(8) {
        d.process(c.try_into().unwrap());
    }
    let s = d.spectrum();
    let top = (0..BINS).max_by(|&a, &b| s[a].total_cmp(&s[b])).unwrap();
    assert_eq!(top, 128, "1000 Hz is bin 128");
    // Hann: a full-scale sine of amplitude A gives |X| = A*N/4 at its bin.
    let want = (0.25f32 * FFT_N as f32 / 4.0).powi(2);
    assert!((s[top] / want - 1.0).abs() < 0.05, "{} vs {want}", s[top]);
}

#[test]
fn floor_estimate_is_unbiased() {
    // In white noise the background estimate must equal the mean level.
    let pcm = Sound::silence(20.0).noise(0.02, 7).into_pcm();
    let mut d = Box::new(Detector::zeroed());
    let (mut est, mut mean, mut n) = (0.0f64, 0.0f64, 0);
    for (i, c) in pcm.chunks_exact(HOP).enumerate() {
        d.process(c.try_into().unwrap());
        if i < 30 {
            continue;
        }
        for k in (96..900).step_by(16) {
            est += f64::from(d.local[k / 16]);
            mean += f64::from(d.averaged()[k]);
            n += 1;
        }
    }
    let ratio = est / mean;
    assert!((0.93..1.07).contains(&ratio), "estimate / mean = {ratio:.3} over {n} points");
}

#[test]
fn noise_alone_is_never_heard() {
    for seed in 1..=6 {
        let pcm = Sound::silence(60.0).noise(0.02, seed).into_pcm();
        let out = run(&pcm);
        assert_eq!(heard_at(&out), None, "seed {seed}");
        let cand = out.iter().filter(|f| f.state == State::Candidate).count();
        assert_eq!(cand, 0, "seed {seed}: candidate frames in pure noise");
    }
}

#[test]
fn rumble_alone_is_never_heard() {
    for seed in 1..=4 {
        let pcm = Sound::silence(60.0).rumble(0.05, seed).noise(0.005, seed + 9).into_pcm();
        assert_eq!(heard_at(&run(&pcm)), None, "seed {seed}");
    }
}

#[test]
fn camera_drone_pitch_is_heard_and_classed() {
    // Quiet start, then a quadcopter at 170 Hz under noise.
    let pcm = Sound::silence(30.0).noise(0.01, 3).quad(8.0, 28.0, 0.01, 170.0, 5).into_pcm();
    let out = run(&pcm);
    let at = heard_at(&out).expect("heard");
    assert!((8.0..13.0).contains(&at), "heard at {at:.1} s");
    assert!(heard_share(&out, 13.0, 27.5) > 0.95);
    let f = out[(20 * FRAMES_PER_S) as usize];
    assert!((f.f0_hz / 170.0 - 1.0).abs() < 0.06, "pitch {}", f.f0_hz);
    assert_eq!(f.class, Some(SizeClass::LargeProps));
    assert!(f.confidence >= 50, "confidence {}", f.confidence);
    // And it lets go afterwards.
    assert_eq!(out.last().unwrap().state, State::Quiet);
}

#[test]
fn fpv_pitch_is_heard_and_classed() {
    let pcm = Sound::silence(24.0).noise(0.01, 4).quad(7.0, 22.0, 0.01, 640.0, 8).into_pcm();
    let out = run(&pcm);
    let at = heard_at(&out).expect("heard");
    assert!((7.0..12.0).contains(&at), "heard at {at:.1} s");
    let f = out[(15 * FRAMES_PER_S) as usize];
    assert!((f.f0_hz / 640.0 - 1.0).abs() < 0.06, "pitch {}", f.f0_hz);
    assert_eq!(f.class, Some(SizeClass::SmallProps));
}

#[test]
fn a_fly_past_is_followed_through_its_pitch_drop() {
    // 30 m/s past the microphone: the pitch falls about 17% over a few seconds.
    let pitch = |t: f32| 600.0 * (1.0 - 0.085 * libm::tanhf((t - 14.0) / 1.5));
    let pcm = Sound::silence(24.0).noise(0.01, 6).ladder(7.0, 21.0, 0.01, 10, 1.0, pitch).into_pcm();
    let out = run(&pcm);
    assert!(heard_at(&out).is_some());
    assert!(heard_share(&out, 11.0, 20.5) > 0.95, "lost during the pass");
    let (before, after) = (out[(11 * FRAMES_PER_S) as usize], out[(19 * FRAMES_PER_S) as usize]);
    assert!(before.f0_hz > 630.0 && after.f0_hz < 570.0, "{} -> {}", before.f0_hz, after.f0_hz);
}

#[test]
fn hum_present_from_power_up_is_absorbed() {
    // Mains hum is a perfect ladder at 120 Hz. It must not read as a drone.
    let pcm = Sound::silence(90.0).noise(0.005, 2).ladder(0.0, 90.0, 0.01, 8, 1.0, |_| 120.0).into_pcm();
    let out = run(&pcm);
    assert_eq!(heard_at(&out), None);
}

#[test]
fn hum_that_starts_later_fades_out() {
    // A fan or compressor switching on is heard at first (it is a ladder),
    // then sinks into the background because its pitch never moves.
    let pcm = Sound::silence(300.0).noise(0.005, 2).ladder(20.0, 300.0, 0.004, 8, 1.0, |_| 120.0).into_pcm();
    let out = run(&pcm);
    assert_eq!(heard_share(&out, 240.0, 300.0), 0.0, "still heard after four minutes");
}

#[test]
fn a_drone_over_hum_is_still_heard() {
    let pcm = Sound::silence(60.0)
        .noise(0.005, 2)
        .ladder(0.0, 60.0, 0.01, 8, 1.0, |_| 120.0)
        .quad(30.0, 55.0, 0.004, 205.0, 11)
        .into_pcm();
    let out = run(&pcm);
    let at = heard_at(&out).expect("heard");
    assert!((30.0..36.0).contains(&at), "heard at {at:.1} s");
    let f = out[(45 * FRAMES_PER_S) as usize];
    assert!((f.f0_hz / 205.0 - 1.0).abs() < 0.06, "pitch {}", f.f0_hz);
}

#[test]
fn speech_rhythm_is_not_heard() {
    for seed in 1..=6 {
        let pcm = Sound::silence(60.0).noise(0.005, seed).speechlike(6.0, 58.0, 0.03, seed + 20).into_pcm();
        assert_eq!(heard_at(&run(&pcm)), None, "seed {seed}");
    }
}

#[test]
fn a_single_tone_is_not_a_ladder() {
    // A whistle or a beeper: one tone, no multiples.
    let pcm = Sound::silence(30.0).noise(0.005, 1).ladder(6.0, 28.0, 0.05, 1, 1.0, |t| 900.0 + 20.0 * libm::sinf(t)).into_pcm();
    assert_eq!(heard_at(&run(&pcm)), None);
}

#[test]
fn nothing_is_reported_during_warm_up() {
    let pcm = Sound::silence(8.0).noise(0.005, 1).quad(0.0, 8.0, 0.02, 300.0, 3).into_pcm();
    let out = run(&pcm);
    assert!(out[..(5 * FRAMES_PER_S) as usize].iter().all(|f| f.state == State::Warmup));
}

#[test]
fn wav_header_is_well_formed() {
    let h = wav::header(16_000, 16_000);
    assert_eq!(&h[0..4], b"RIFF");
    assert_eq!(u32::from_le_bytes(h[4..8].try_into().unwrap()), 36 + 32_000);
    assert_eq!(u32::from_le_bytes(h[40..44].try_into().unwrap()), 32_000);
    assert_eq!(u32::from_le_bytes(h[24..28].try_into().unwrap()), 16_000);
}

#[test]
fn built_in_place_behaves_like_a_fresh_one() {
    let pcm = Sound::silence(12.0).noise(0.01, 4).quad(1.0, 12.0, 0.01, 640.0, 8).into_pcm();
    let mut slot = Box::new(core::mem::MaybeUninit::<Detector>::uninit());
    let d = Detector::init_in_place(&mut slot);
    let got: Vec<FrameOut> = pcm.chunks_exact(HOP).map(|c| d.process(c.try_into().unwrap())).collect();
    assert_eq!(got, run(&pcm));
}
