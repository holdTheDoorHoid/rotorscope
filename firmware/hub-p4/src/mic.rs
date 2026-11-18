//! The board's microphone: PDM receive on I2S0, filtered to 16 kHz PCM by
//! the chip, delivered by the AHB DMA into a ring of buffers.
//!
//! esp-hal has no I2S driver for the P4 at all, so this is a register-level
//! port of ESP-IDF v5.4.2 (`esp_driver_i2s/i2s_pdm.c`, `hal/i2s_hal.c`,
//! `hal/esp32p4/include/hal/i2s_ll.h`, `ahb_dma_ll.h`), the IDF Elecrow's
//! factory firmware uses on this v1.3 chip. Register offsets are IDF's
//! (`soc/esp32p4/register/soc/*.h`), written raw as in `rgb.rs`.
//!
//! **The microphone.** One PDM MEMS microphone (Elecrow schematic V1.1:
//! "PDM MIC"), clock on GPIO24, data on GPIO25. Those two pads are also the
//! chip's USB-Serial-JTAG pins, which is why this board has no second USB
//! serial port. Which half of the clock the microphone answers on (its L/R
//! strap) is not marked, so both halves are received as a stereo pair and
//! [`Mic::poll`] hands out both; the ear picks the one with sound on it.
//!
//! **Clocks** (IDF's defaults for 16 kHz): the 40 MHz crystal divided by
//! 4 + 113/128 gives 8.192 MHz, divided by 8 gives the 1.024 MHz microphone
//! clock, and the chip's PDM-to-PCM filter divides by 64: 16 000 samples a
//! second. Hardware PDM-to-PCM exists on I2S0 only.
//!
//! **Cache.** On the P4 internal RAM is behind the CPU's L1 data cache and
//! the DMA is not: descriptors are written back after every change and
//! descriptors and buffers are invalidated before they are read. (Found the
//! hard way: without it the DMA reads zeros and reports a descriptor error.)
//!
//! **Ring.** [`BUFS`] buffers of [`FRAMES`] stereo frames (32 ms each). The
//! DMA fills them in a circle without asking; [`Mic::poll`] takes the ones
//! that are complete. If the loop stays away for a whole lap (a quarter of
//! a second) sound is lost and counted in [`Mic::overruns`].

use esp_hal::gpio::{AnyPin, Input, InputConfig, Level, Output, OutputConfig, Pin};

/// Samples per second.
pub const RATE: u32 = 16_000;
/// Stereo frames per buffer: one detector hop.
pub const FRAMES: usize = 512;
/// Buffers in the ring.
const BUFS: usize = 8;
const BUF_BYTES: usize = FRAMES * 4;

// ---- Register map (IDF v5.4.2) ----

const CLKRST: usize = 0x500E_6000; // HP_SYS_CLKRST
const SOC_CLK_CTRL1: usize = CLKRST + 0x18; // bit 3: ahb_pdma_sys_clk_en
const SOC_CLK_CTRL2: usize = CLKRST + 0x1C; // bit 14: i2s0_apb_clk_en
const PERI_CLK_CTRL11: usize = CLKRST + 0x44; // bit 24 i2s0_rx_clk_en, [26:25] i2s0_rx_clk_src_sel
const PERI_CLK_CTRL12: usize = CLKRST + 0x48; // [7:0] rx_div_n, [16:8] rx_div_x, [25:17] rx_div_y
const PERI_CLK_CTRL13: usize = CLKRST + 0x4C; // [8:0] rx_div_z, bit 9 rx_div_yn1
const PERI_CLK_CTRL14: usize = CLKRST + 0x50; // bit 19: i2s0_mst_clk_sel (0 = master clock from RX)
const HP_RST_EN1: usize = CLKRST + 0xC4; // bit 1: rst_en_ahb_pdma
const HP_RST_EN2: usize = CLKRST + 0xC8; // bit 4: rst_en_i2s0_apb

const I2S0: usize = 0x500C_6000;
const RX_CONF: usize = I2S0 + 0x20;
const RX_CONF1: usize = I2S0 + 0x28;
const RX_PDM2PCM_CONF: usize = I2S0 + 0x48;
const RX_TDM_CTRL: usize = I2S0 + 0x50;
const RXEOF_NUM: usize = I2S0 + 0x64;

const RX_RESET: u32 = 1 << 0;
const RX_FIFO_RESET: u32 = 1 << 1;
const RX_START: u32 = 1 << 2;
const RX_SLAVE_MOD: u32 = 1 << 3;
const RX_MONO: u32 = 1 << 6;
const RX_UPDATE: u32 = 1 << 8;
const RX_MONO_FST_VLD: u32 = 1 << 9;
const RX_TDM_EN: u32 = 1 << 19;
const RX_PDM_EN: u32 = 1 << 20;

const DMA: usize = 0x5008_5000; // AHB_DMA, channel 0 receive side
const IN_INT_RAW: usize = DMA + 0x00;
const IN_INT_CLR: usize = DMA + 0x0C;
const MISC_CONF: usize = DMA + 0x64; // bit 0 ahbm_rst_inter, bit 3 clk_en
const IN_CONF0: usize = DMA + 0x70; // bit 0 in_rst, 2 indscr_burst, 3 in_data_burst, 4 mem_trans_en
const IN_CONF1: usize = DMA + 0x74; // bit 12 in_check_owner
const IN_LINK: usize = DMA + 0x80; // bit 1 stop, bit 2 start
const IN_PERI_SEL: usize = DMA + 0xA0;
const IN_LINK_ADDR: usize = DMA + 0x3AC;
const INTR_MEM_START: usize = DMA + 0x3C4;
const INTR_MEM_END: usize = DMA + 0x3C8;
/// AHB-DMA peripheral id of I2S0 (`SOC_GDMA_TRIG_PERIPH_I2S0`).
const PERI_I2S0: u32 = 3;
/// Descriptor error, FIFO overflow, FIFO underflow.
const DMA_ERR_MASK: u32 = (1 << 3) | (1 << 5) | (1 << 6);

const GPIO: usize = 0x500E_0000;
const FUNC_IN_SEL_CFG: usize = GPIO + 0x158; // + 4 * signal
const FUNC_OUT_SEL_CFG: usize = GPIO + 0x558; // + 4 * gpio
const OEN_SEL: u32 = 1 << 10;
const SIG_IN_SEL: u32 = 1 << 7;
/// GPIO-matrix signals (IDF `gpio_sig_map.h`): in PDM receive the
/// microphone clock leaves on the RX word-select output.
const SIG_I2S0_I_WS: u32 = 30;
const SIG_I2S0_I_SD: usize = 28;

/// DMA descriptor: word 0 = size [11:0], length [23:12], owner bit 31.
const OWNER_DMA: u32 = 1 << 31;

// ROM cache maintenance (same addresses in the eco0_4 and v3 ROMs). On the P4
// even internal RAM sits behind the CPU's L1 data cache, and the DMA reads
// and writes the RAM itself: what the CPU writes for the DMA must be written
// back, and what the DMA wrote must be invalidated before the CPU reads it.
const CACHE_MAP_L1_DCACHE: u32 = 1 << 4;
const CACHE_LINE: usize = 64;
unsafe extern "C" {
    fn Cache_WriteBack_Addr(map: u32, addr: u32, size: u32) -> i32;
    fn Cache_Invalidate_Addr(map: u32, addr: u32, size: u32) -> i32;
}

/// One descriptor alone in its cache line, so writing one back can never
/// put a stale copy over a neighbour the DMA has just updated.
#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct Desc([u32; 3]);

#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct Buf([i16; FRAMES * 2]);
const _: () = assert!(core::mem::size_of::<Buf>() % CACHE_LINE == 0 && core::mem::size_of::<Desc>() == CACHE_LINE);

#[repr(C, align(64))]
struct Ring {
    desc: [Desc; BUFS],
    buf: [Buf; BUFS],
}

static mut RING: Ring = Ring { desc: [Desc([0; 3]); BUFS], buf: [Buf([0; FRAMES * 2]); BUFS] };

/// Push the CPU's copy of descriptor `i` to RAM.
fn write_back(ring: &Ring, i: usize) {
    // SAFETY: ROM cache maintenance on one whole cache line of our own ring.
    unsafe { Cache_WriteBack_Addr(CACHE_MAP_L1_DCACHE, (&raw const ring.desc[i]) as u32, CACHE_LINE as u32) };
}

fn rd(a: usize) -> u32 {
    // SAFETY: callers pass only the peripheral register addresses above.
    unsafe { (a as *const u32).read_volatile() }
}
fn wr(a: usize, v: u32) {
    // SAFETY: as `rd`.
    unsafe { (a as *mut u32).write_volatile(v) }
}
/// Read-modify-write. Init only, from the single main thread.
fn set(a: usize, mask: u32, val: u32) {
    wr(a, (rd(a) & !mask) | (val & mask));
}
fn set_field(a: usize, shift: u32, width: u32, v: u32) {
    let m = ((1u32 << width) - 1) << shift;
    set(a, m, v << shift);
}
fn pulse(a: usize, bit: u32) {
    set(a, bit, bit);
    set(a, bit, 0);
}

pub struct Mic {
    /// Next buffer to hand out.
    head: usize,
    /// Buffers delivered since start.
    pub buffers: u32,
    /// Times the DMA lapped the reader (sound lost).
    pub overruns: u32,
    /// DMA error flags seen (descriptor error, FIFO over/underflow).
    pub dma_errors: u32,
}

impl Mic {
    /// Start capture. Never stops; the pins stay with the microphone.
    pub fn start(clk: AnyPin<'static>, data: AnyPin<'static>) -> Self {
        // -- Bus clocks and resets (i2s_ll_enable_bus_clock / reset_register,
        //    gdma_new_ahb_channel).
        set(SOC_CLK_CTRL2, 1 << 14, 1 << 14);
        pulse(HP_RST_EN2, 1 << 4);
        set(SOC_CLK_CTRL1, 1 << 3, 1 << 3);
        pulse(HP_RST_EN1, 1 << 1);

        // -- Pins. esp-hal does the pad setup (and releases the USB pads);
        //    the matrix selects are written raw, as in rgb.rs.
        let (clk_n, data_n) = (clk.number() as usize, data.number() as u32);
        core::mem::forget(Output::new(clk, Level::Low, OutputConfig::default()));
        wr(FUNC_OUT_SEL_CFG + 4 * clk_n, SIG_I2S0_I_WS | OEN_SEL);
        core::mem::forget(Input::new(data, InputConfig::default()));
        wr(FUNC_IN_SEL_CFG + 4 * SIG_I2S0_I_SD, data_n | SIG_IN_SEL);

        // -- Slot format (i2s_hal_pdm_set_rx_slot, hardware version 2).
        pulse(RX_CONF, RX_RESET);
        set(RX_CONF, RX_SLAVE_MOD | RX_MONO | RX_MONO_FST_VLD, 0);
        set_field(RX_CONF1, 14, 5, 16 - 1); // rx_bits_mod
        set_field(RX_CONF1, 27, 5, 16 - 1); // rx_tdm_chan_bits
        set_field(RX_CONF1, 19, 8, 16 - 1); // rx_half_sample_bits
        set(RX_TDM_CTRL, 0xFFFF, 0b11); // both PDM slots: left and right
        // High-pass at 35.5 Hz (IDF default), no extra gain, 8x down-sampling.
        set(RX_PDM2PCM_CONF, 1 << 25, 0); // rx_pdm_hp_bypass off
        set_field(RX_PDM2PCM_CONF, 29, 3, 6); // rx_iir_hp_mult12_0
        set_field(RX_PDM2PCM_CONF, 26, 3, 7); // rx_iir_hp_mult12_5
        set_field(RX_PDM2PCM_CONF, 21, 4, 1); // rx_pdm2pcm_amplify_num
        set(RX_PDM2PCM_CONF, 1 << 20, 0); // rx_pdm_sinc_dsr_16_en off

        // -- Clock (i2s_hal_set_rx_clock): crystal / (4 + 113/128) / 8.
        //    113/128 in IDF's x/y/z/yn1 form: yn1 = 1, z = 128 - 113 = 15,
        //    x = 128 / 15 - 1 = 7, y = 128 % 15 = 8.
        set(PERI_CLK_CTRL11, 1 << 24, 1 << 24);
        set(PERI_CLK_CTRL14, 1 << 19, 0);
        set_field(PERI_CLK_CTRL11, 25, 2, 0); // crystal
        set_field(PERI_CLK_CTRL12, 0, 8, 2);
        set(PERI_CLK_CTRL13, 1 << 9, 0);
        set_field(PERI_CLK_CTRL12, 17, 9, 1);
        set_field(PERI_CLK_CTRL13, 0, 9, 0);
        set_field(PERI_CLK_CTRL12, 8, 9, 0);
        set(PERI_CLK_CTRL13, 1 << 9, 1 << 9);
        set_field(PERI_CLK_CTRL13, 0, 9, 15);
        set_field(PERI_CLK_CTRL12, 17, 9, 8);
        set_field(PERI_CLK_CTRL12, 8, 9, 7);
        set_field(PERI_CLK_CTRL12, 0, 8, 4);
        set_field(RX_CONF, 21, 6, 8 - 1); // rx_bck_div_num

        // -- PDM receive with the hardware PDM-to-PCM filter.
        set(RX_CONF, RX_PDM_EN | RX_TDM_EN, RX_PDM_EN);
        set(RX_PDM2PCM_CONF, 1 << 19, 1 << 19);
        // One "end of frame" per buffer, so each descriptor is closed and
        // handed back as it fills.
        wr(RXEOF_NUM, BUF_BYTES as u32);

        // -- Descriptor ring in internal RAM (no cache between it and the DMA).
        // SAFETY: the ring is touched only here and in `poll`, on the main
        // thread; the DMA writes buffers and descriptor word 0.
        let ring = unsafe { &mut *(&raw mut RING) };
        for i in 0..BUFS {
            ring.desc[i].0[0] = BUF_BYTES as u32 | OWNER_DMA;
            ring.desc[i].0[1] = (&raw const ring.buf[i]) as u32;
            ring.desc[i].0[2] = (&raw const ring.desc[(i + 1) % BUFS]) as u32;
            write_back(ring, i);
        }

        // -- AHB DMA channel 0, receive (gdma_ahb_hal_init / i2s start).
        set(MISC_CONF, 1 << 3, 1 << 3);
        pulse(MISC_CONF, 1 << 0);
        wr(INTR_MEM_START, 0x4000_0000);
        wr(INTR_MEM_END, 0x4FFC_0000);
        pulse(IN_CONF0, 1 << 0);
        set(IN_CONF0, (1 << 2) | (1 << 3) | (1 << 4), 0);
        set(IN_CONF1, 1 << 12, 0);
        wr(IN_PERI_SEL, PERI_I2S0);
        wr(IN_INT_CLR, 0x7F);
        wr(IN_LINK_ADDR, (&raw const ring.desc[0]) as u32);

        // -- Go (i2s_rx_channel_start): reset, start the DMA, start I2S.
        pulse(RX_CONF, RX_RESET);
        pulse(RX_CONF, RX_FIFO_RESET);
        set(IN_LINK, 1 << 2, 1 << 2);
        set(RX_CONF, RX_UPDATE, RX_UPDATE);
        let mut spins = 0u32;
        while rd(RX_CONF) & RX_UPDATE != 0 && spins < 100_000 {
            spins += 1;
        }
        set(RX_CONF, RX_START, RX_START);
        Self { head: 0, buffers: 0, overruns: 0, dma_errors: 0 }
    }

    /// Hand each completed buffer to `f`, oldest first: [`FRAMES`] stereo
    /// frames, left then right.
    pub fn poll(&mut self, mut f: impl FnMut(&[i16; FRAMES * 2])) {
        let raw = rd(IN_INT_RAW);
        if raw & DMA_ERR_MASK != 0 {
            self.dma_errors += 1;
            wr(IN_INT_CLR, raw & DMA_ERR_MASK);
        }
        // SAFETY: as in `start`. A buffer is read only after the DMA has
        // closed its descriptor and moved on.
        let ring = unsafe { &mut *(&raw mut RING) };
        let mut taken = 0;
        while taken < BUFS {
            let d = &raw mut ring.desc[self.head];
            // SAFETY: ROM cache maintenance on whole cache lines of our own
            // ring; the descriptor is then read as the DMA left it in RAM.
            let w0 = unsafe {
                Cache_Invalidate_Addr(CACHE_MAP_L1_DCACHE, d as u32, CACHE_LINE as u32);
                (&raw const (*d).0[0]).read_volatile()
            };
            if w0 & OWNER_DMA != 0 {
                break;
            }
            // SAFETY: as above, over the buffer the DMA has finished with.
            unsafe { Cache_Invalidate_Addr(CACHE_MAP_L1_DCACHE, (&raw const ring.buf[self.head]) as u32, BUF_BYTES as u32) };
            f(&ring.buf[self.head].0);
            // Hand the buffer back to the DMA.
            ring.desc[self.head].0[0] = BUF_BYTES as u32 | OWNER_DMA;
            write_back(ring, self.head);
            self.head = (self.head + 1) % BUFS;
            self.buffers = self.buffers.wrapping_add(1);
            taken += 1;
        }
        // Every buffer was waiting: the DMA has very likely gone round and
        // overwritten some of them.
        if taken == BUFS {
            self.overruns += 1;
        }
    }
}
