//! PSRAM bring-up: initialise, then write and read back an address-derived
//! pattern over all of it, and time both directions.

use esp_hal::psram::{Psram, PsramConfig, SpiRamFreq};
use esp_hal::time::Instant;

/// Returns the PSRAM (base, size) when the whole chip passed.
pub fn run(peri: esp_hal::peripherals::PSRAM<'static>, freq: SpiRamFreq) -> Option<(*mut u32, usize)> {
    println!("hub-p4: psram init at {freq:?}");
    println!("hub-p4: pmu ext_ldo before {:08x?}", ldo_regs());
    let t = Instant::now();
    let psram = Psram::new(peri, PsramConfig { ram_frequency: freq, ..Default::default() });
    let (base, size) = psram.raw_parts();
    println!(
        "hub-p4: psram base {:p} size {} KiB, init {} ms; ldo after {:08x?}",
        base, size / 1024, t.elapsed().as_millis(), ldo_regs()
    );
    if size == 0 {
        return None;
    }
    let words = size / 4;
    let p = base.cast::<u32>();
    let pat = |i: usize| (i as u32).wrapping_mul(0x9E37_79B9) ^ 0xA5A5_5A5A;

    let t = Instant::now();
    for i in 0..words {
        // SAFETY: `i < words` stays inside the mapped PSRAM range.
        unsafe { p.add(i).write_volatile(pat(i)) };
    }
    let w_us = t.elapsed().as_micros().max(1);
    let t = Instant::now();
    let mut bad = 0usize;
    let mut first_bad = None;
    for i in 0..words {
        // SAFETY: as above.
        let v = unsafe { p.add(i).read_volatile() };
        if v != pat(i) {
            bad += 1;
            first_bad.get_or_insert((i * 4, v, pat(i)));
        }
    }
    let r_us = t.elapsed().as_micros().max(1);
    println!(
        "hub-p4: psram {} MiB: write {} MB/s, read {} MB/s, {} bad words, first bad {:x?}",
        size >> 20,
        size as u64 / w_us,
        size as u64 / r_us,
        bad,
        first_bad
    );
    core::mem::forget(psram);
    (bad == 0).then_some((p, size))
}

/// PMU external-LDO control registers (PMU base 0x5011_5000, 0x1b8..0x1e4):
/// p0_0p1a, its _ana, p0_0p2a, _ana, p0_0p3a, _ana, then the same for p1.
pub fn ldo_regs() -> [u32; 12] {
    let mut r = [0u32; 12];
    for (i, v) in r.iter_mut().enumerate() {
        // SAFETY: read-only access to documented PMU registers.
        *v = unsafe { ((0x5011_5000u32 + 0x1b8 + 4 * i as u32) as *const u32).read_volatile() };
    }
    r
}


