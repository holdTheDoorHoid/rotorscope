//! 16-bit parallel RGB (DPI) panel output: LCD_CAM in RGB mode, fed by the
//! AXI-GDMA from a framebuffer in PSRAM, looping on its own forever.
//!
//! esp-hal has no P4 RGB driver, so this is a register-level port of ESP-IDF
//! v5.4.2 `esp_lcd_panel_rgb.c` (stream mode, one framebuffer, no bounce
//! buffer) and its `lcd_ll.h` / `axi_dma_ll.h`. IDF 5.4.2 is what Elecrow's
//! factory firmware was built with on this exact v1.3 chip, so its register
//! maps (`components/soc/esp32p4/register/soc/*.h`, pre-v3 silicon) are the
//! ones used here, as raw offsets. The esp32p4 PAC is not used: it may follow
//! the v3 layout, and the offsets below were checked against IDF directly.
//!
//! Once started, the CPU does nothing per frame: the descriptor ring is
//! circular and LCD_CAM's `next_frame_en` keeps asking the DMA for data.
//! `poll` only counts the VSYNC and DMA EOF flags for the bring-up log.

use esp_hal::gpio::{AnyPin, Level, Output, OutputConfig, Pin};

pub const H_RES: usize = 800;
pub const V_RES: usize = 480;
const FB_BYTES: usize = H_RES * V_RES * 2;

// Timing per Elecrow (pulse / back porch / front porch).
const HSW: u32 = 4;
const HBP: u32 = 8;
const HFP: u32 = 8;
const VSW: u32 = 4;
const VBP: u32 = 16;
const VFP: u32 = 16;
pub const H_TOTAL: u32 = HSW + HBP + H_RES as u32 + HFP;
pub const V_TOTAL: u32 = VSW + VBP + V_RES as u32 + VFP;

/// PLL_F160M / (4 + 4/9) = 36 MHz LCD_CLK, then / 2 = 18 MHz PCLK. These are
/// the exact values IDF's `lcd_hal_cal_pclk_freq` picks for 18 MHz (it prefers
/// a PCLK prescale of 2 "due to some unstable hardware issue").
const CLK_DIV_NUM: u32 = 4;
const CLK_DIV_A: u32 = 9; // denominator
const CLK_DIV_B: u32 = 4; // numerator
const PCLK_PRESCALE: u32 = 2;
pub const PCLK_HZ: u32 = 18_000_000;

/// Polarity flags, named as in IDF's `esp_lcd_rgb_timing_t`. Unknown for this
/// panel; these are IDF defaults except `pclk_active_neg` (common on these
/// 800x480 panels). Change here if the picture is wrong.
const PCLK_ACTIVE_NEG: bool = true;
const PCLK_IDLE_HIGH: bool = false;
const HSYNC_IDLE_LOW: bool = false;
const VSYNC_IDLE_LOW: bool = false;
const DE_IDLE_HIGH: bool = false;

// ---- Register map (IDF v5.4.2, soc/esp32p4/register/soc) ----

const CLKRST: usize = 0x500E_6000; // HP_SYS_CLKRST
const SOC_CLK_CTRL1: usize = CLKRST + 0x18; // bit 4: axi_pdma_sys_clk_en
const SOC_CLK_CTRL3: usize = CLKRST + 0x20; // bit 1: lcdcam_apb_clk_en
const REF_CLK_CTRL2: usize = CLKRST + 0x2C; // bit 0: ref_160m_clk_en
const PERI_CLK_CTRL19: usize = CLKRST + 0x64; // [30:29] lcd_clk_src_sel, 31 lcd_clk_en
const PERI_CLK_CTRL110: usize = CLKRST + 0x68; // [7:0] div_num, [15:8] numerator, [23:16] denominator
const HP_RST_EN1: usize = CLKRST + 0xC4; // bit 2: rst_en_axi_pdma
const HP_RST_EN2: usize = CLKRST + 0xC8; // bit 9: rst_en_lcdcam

const LCD: usize = 0x500D_C000; // LCD_CAM
const LCD_CLOCK: usize = LCD + 0x00;
const LCD_RGB_YUV: usize = LCD + 0x10;
const LCD_USER: usize = LCD + 0x14;
const LCD_MISC: usize = LCD + 0x18;
const LCD_CTRL: usize = LCD + 0x1C;
const LCD_CTRL1: usize = LCD + 0x20;
const LCD_CTRL2: usize = LCD + 0x24;
const LCD_DLY_MODE_CFG1: usize = LCD + 0x30;
const LC_DMA_INT_ENA: usize = LCD + 0x64;
const LC_DMA_INT_RAW: usize = LCD + 0x68;
const LC_DMA_INT_CLR: usize = LCD + 0x70;

const USER_ALWAYS_OUT_EN: u32 = 1 << 13;
const USER_UPDATE_REG: u32 = 1 << 21;
const USER_DOUT: u32 = 1 << 24;
const USER_START: u32 = 1 << 27;
const USER_RESET: u32 = 1 << 28;
const MISC_AFIFO_RESET: u32 = 1 << 27;
const INT_VSYNC: u32 = 1 << 0;

const AXI_DMA: usize = 0x5008_A000;
// TX channel 0 (channel stride not needed: only ch0 is used).
const OUT_INT_RAW: usize = AXI_DMA + 0x138;
const OUT_INT_ENA: usize = AXI_DMA + 0x140;
const OUT_INT_CLR: usize = AXI_DMA + 0x144;
const OUT_CONF0: usize = AXI_DMA + 0x148;
const OUT_CONF1: usize = AXI_DMA + 0x14C;
const OUTFIFO_STATUS: usize = AXI_DMA + 0x150;
const OUT_LINK1: usize = AXI_DMA + 0x158;
const OUT_LINK2: usize = AXI_DMA + 0x15C;
const OUT_STATE: usize = AXI_DMA + 0x160;
const OUT_DSCR: usize = AXI_DMA + 0x16C;
const OUT_PERI_SEL: usize = AXI_DMA + 0x17C;
const INTR_MEM_START: usize = AXI_DMA + 0x27C;
const INTR_MEM_END: usize = AXI_DMA + 0x280;
const EXTR_MEM_START: usize = AXI_DMA + 0x284;
const EXTR_MEM_END: usize = AXI_DMA + 0x288;

const DMA_EOF: u32 = 1 << 1;
/// Descriptor error, plus every L1/L2/L3 FIFO overflow/underflow bit.
const DMA_ERR_MASK: u32 = 0x3F4;
/// AXI-DMA peripheral id of LCD_CAM (`SOC_GDMA_TRIG_PERIPH_LCD0`).
const PERI_LCD: u32 = 0;

const GPIO_FUNC_OUT_SEL_CFG: usize = 0x500E_0000 + 0x558; // + 4 * gpio
const OEN_SEL: u32 = 1 << 10; // output enable from GPIO_ENABLE (set by esp-hal)

/// GPIO-matrix output signal indices (IDF `gpio_sig_map.h`).
const SIG_PCLK: u32 = 149;
const SIG_DE: u32 = 151;
const SIG_HSYNC: u32 = 152;
const SIG_VSYNC: u32 = 153;
const SIG_DATA0: u32 = 154;

// ROM cache maintenance (same address in the eco0_4 and v3 ROMs).
const CACHE_MAP_L1_DCACHE: u32 = 1 << 4;
const CACHE_MAP_L2_CACHE: u32 = 1 << 5;
unsafe extern "C" {
    fn Cache_WriteBack_Addr(map: u32, addr: u32, size: u32) -> i32;
}

// ---- DMA descriptors ----

/// One AXI-GDMA link item. IDF's `gdma_link_list_item_t`, 8-byte aligned as
/// the AXI DMA requires (`GDMA_LL_AXI_DESC_ALIGNMENT`).
#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct Desc {
    /// size[11:0], length[23:12], suc_eof bit 30, owner bit 31.
    dw0: u32,
    buf: u32,
    next: u32,
}

/// Two lines per descriptor: 3200 bytes, under the 4095-byte limit and a
/// multiple of the 64-byte burst, so every buffer stays burst-aligned.
const DESC_BYTES: usize = H_RES * 2 * 2;
const N_DESC: usize = FB_BYTES / DESC_BYTES;

static mut DESCS: [Desc; N_DESC] = [Desc { dw0: 0, buf: 0, next: 0 }; N_DESC];

fn rd(a: usize) -> u32 {
    // SAFETY: callers pass only the peripheral register addresses above.
    unsafe { (a as *const u32).read_volatile() }
}
fn wr(a: usize, v: u32) {
    // SAFETY: as `rd`.
    unsafe { (a as *mut u32).write_volatile(v) }
}
/// Read-modify-write `bits` (mask) to `val`. Only used during init from the
/// single main thread, so the RMW on shared clock registers is not racy.
fn set(a: usize, mask: u32, val: u32) {
    wr(a, (rd(a) & !mask) | (val & mask));
}
fn field(v: u32, shift: u32, width: u32) -> (u32, u32) {
    let m = ((1u32 << width) - 1) << shift;
    (m, (v << shift) & m)
}
fn set_field(a: usize, shift: u32, width: u32, v: u32) {
    let (m, b) = field(v, shift, width);
    set(a, m, b);
}

pub struct Panel {
    fb: *mut u16,
    pub vsync: u32,
    pub eof: u32,
    pub dma_err: u32,
    pub dma_err_count: u32,
}

/// The 20 panel signal pins, in IDF `esp_lcd_rgb_panel_config_t` order.
pub struct Pins {
    pub de: AnyPin<'static>,
    pub pclk: AnyPin<'static>,
    pub hsync: AnyPin<'static>,
    pub vsync: AnyPin<'static>,
    /// data0..15 = B0..B4, G0..G5, R0..R4 (RGB565 bit order).
    pub data: [AnyPin<'static>; 16],
}

/// Puts `pin` under GPIO-matrix control with output signal `sig`. esp-hal's
/// `Output` does the pad setup (IO_MUX function GPIO, output enable); its
/// `OutputSignal` enum lacks LCD data 0..23 on the P4, so the matrix select
/// itself is written raw. The `Output` is leaked so it never resets the pad.
fn route(pin: AnyPin<'static>, sig: u32) {
    let n = pin.number() as usize;
    let out = Output::new(pin, Level::Low, OutputConfig::default());
    core::mem::forget(out);
    wr(GPIO_FUNC_OUT_SEL_CFG + 4 * n, sig | OEN_SEL);
}

impl Panel {
    /// Configures LCD_CAM + AXI-GDMA and starts continuous output from a
    /// framebuffer at `fb` (PSRAM, cache-line aligned, `FB_BYTES` long).
    pub fn start(pins: Pins, fb: *mut u16) -> Self {
        // -- Bus clocks and resets (IDF new_rgb_panel / gdma_new_axi_channel).
        set(SOC_CLK_CTRL3, 1 << 1, 1 << 1);
        set(HP_RST_EN2, 1 << 9, 1 << 9);
        set(HP_RST_EN2, 1 << 9, 0);
        set(SOC_CLK_CTRL1, 1 << 4, 1 << 4);
        set(HP_RST_EN1, 1 << 2, 1 << 2);
        set(HP_RST_EN1, 1 << 2, 0);

        // -- LCD module clock: PLL_F160M, fractional divider (IDF lcd_ll).
        set(REF_CLK_CTRL2, 1, 1);
        set_field(PERI_CLK_CTRL19, 29, 2, 1); // 1 = PLL160M
        set(PERI_CLK_CTRL19, 1 << 31, 1 << 31);
        set_field(PERI_CLK_CTRL110, 0, 8, CLK_DIV_NUM - 1);
        set_field(PERI_CLK_CTRL110, 8, 8, CLK_DIV_B);
        set_field(PERI_CLK_CTRL110, 16, 8, CLK_DIV_A);

        set(LCD_MISC, MISC_AFIFO_RESET, MISC_AFIFO_RESET);
        set(LCD_USER, USER_RESET, USER_RESET);
        wr(LC_DMA_INT_ENA, 0); // polled, never routed to the CPU
        wr(LC_DMA_INT_CLR, 0x3);

        // -- Pins (lcd_rgb_panel_configure_gpio). 16-bit wire mode.
        set_field(LCD_MISC, 4, 2, 1);
        let Pins { de, pclk, hsync, vsync, data } = pins;
        for (i, p) in data.into_iter().enumerate() {
            route(p, SIG_DATA0 + i as u32);
        }
        route(hsync, SIG_HSYNC);
        route(vsync, SIG_VSYNC);
        route(pclk, SIG_PCLK);
        route(de, SIG_DE);

        // -- Descriptor ring over the framebuffer, EOF on the last item only.
        let descs = &raw mut DESCS;
        let base = descs as u32;
        for i in 0..N_DESC {
            let last = i == N_DESC - 1;
            let d = Desc {
                dw0: (1 << 31) // owner = DMA
                    | ((last as u32) << 30) // suc_eof
                    | ((DESC_BYTES as u32) << 12)
                    | DESC_BYTES as u32,
                buf: fb as u32 + (i * DESC_BYTES) as u32,
                next: if last { base } else { base + ((i + 1) * size_of::<Desc>()) as u32 },
            };
            // SAFETY: in-bounds write; the DMA is not running yet.
            unsafe { (descs as *mut Desc).add(i).write_volatile(d) };
        }

        let mut panel = Self { fb, vsync: 0, eof: 0, dma_err: 0, dma_err_count: 0 };
        panel.draw_test_pattern();
        // SAFETY: ROM cache maintenance on our own descriptor array; the DMA
        // reads memory, not the CPU's L1 cache.
        unsafe { Cache_WriteBack_Addr(CACHE_MAP_L1_DCACHE, base, size_of::<[Desc; N_DESC]>() as u32) };

        // -- AXI-GDMA TX channel 0 -> LCD (gdma_connect, apply_strategy,
        // config_transfer: no owner check, no write-back, EOF when the data
        // is read, descriptor burst on, 64-byte data burst).
        wr(INTR_MEM_START, 0x4FC0_0000);
        wr(INTR_MEM_END, 0x4FFC_0000);
        wr(EXTR_MEM_START, 0x4000_0000);
        wr(EXTR_MEM_END, 0x4C00_0000);
        set(OUT_CONF0, 1, 1);
        set(OUT_CONF0, 1, 0);
        wr(OUT_PERI_SEL, PERI_LCD);
        wr(OUT_CONF0, (3 << 5) | (1 << 10)); // burst_size_sel 3 = 64 B, outdscr_burst_en
        set(OUT_CONF1, 1 << 12, 0); // check_owner off
        wr(OUT_INT_ENA, 0);
        wr(OUT_INT_CLR, 0xFFFF_FFFF);

        // -- LCD timing and mode (rgb_panel_init).
        let mut clk = rd(LCD_CLOCK);
        clk &= !(0x3F | (1 << 6) | (1 << 7) | (1 << 8));
        clk |= PCLK_PRESCALE - 1; // clkcnt_n, clk_equ_sysclk = 0
        clk |= (PCLK_IDLE_HIGH as u32) << 7;
        clk |= (PCLK_ACTIVE_NEG as u32) << 8;
        wr(LCD_CLOCK, clk);
        wr(LCD_RGB_YUV, rd(LCD_RGB_YUV) & !(1 << 31)); // converter off
        set_field(LCD_CTRL, 31, 1, 1); // rgb_mode_en
        let mut user = rd(LCD_USER);
        user &= !(0x1FFF | (0x3 << 19) | (0xF << 14) | (1 << 18) | (0xF << 22) | (0x7 << 29));
        user |= 1 << 19; // byte_mode: 16-bit DMA stride
        user |= USER_DOUT | (3 << 29); // data phase only; dummy_cyclelen = 0 - 1 as IDF
        user |= USER_ALWAYS_OUT_EN;
        wr(LCD_USER, user);
        set_field(LCD_CTRL2, 23, 1, !HSYNC_IDLE_LOW as u32);
        set_field(LCD_CTRL2, 7, 1, !VSYNC_IDLE_LOW as u32);
        set_field(LCD_CTRL2, 8, 1, DE_IDLE_HIGH as u32);
        // Blank (porch) cycles: bk_en, vfk/vbk = 1 - 1.
        set(LCD_MISC, (1 << 26) | (0x3F << 6) | (0x1FFF << 12), 1 << 26);
        // Horizontal.
        set_field(LCD_CTRL2, 16, 7, HSW - 1);
        set_field(LCD_CTRL, 0, 11, HBP + HSW - 1);
        set_field(LCD_CTRL1, 8, 12, H_RES as u32 - 1);
        set_field(LCD_CTRL1, 20, 12, H_TOTAL - 1);
        // Vertical.
        set_field(LCD_CTRL2, 0, 7, VSW - 1);
        set_field(LCD_CTRL1, 0, 8, VBP + VSW - 1);
        set_field(LCD_CTRL, 11, 10, V_RES as u32 - 1);
        set_field(LCD_CTRL, 21, 10, V_TOTAL - 1);
        set_field(LCD_CTRL2, 9, 1, 1); // hs_blank_en
        set_field(LCD_CTRL2, 24, 8, 0); // hsync_position
        wr(LCD_DLY_MODE_CFG1, 0);
        set_field(LCD_MISC, 25, 1, 1); // next_frame_en: stream forever

        // -- Start (lcd_rgb_panel_start_transmission): DMA before LCD.
        set(OUT_CONF0, 1, 1);
        set(OUT_CONF0, 1, 0);
        set(LCD_USER, USER_START, 0);
        set(LCD_USER, USER_UPDATE_REG, USER_UPDATE_REG);
        set(LCD_USER, USER_RESET, USER_RESET);
        set(LCD_MISC, MISC_AFIFO_RESET, MISC_AFIFO_RESET);
        wr(OUT_LINK2, base);
        set(OUT_LINK1, 1 << 1, 1 << 1);
        esp_hal::rom::ets_delay_us(1);
        set(LCD_USER, USER_UPDATE_REG, USER_UPDATE_REG);
        set(LCD_USER, USER_START, USER_START);
        wr(LC_DMA_INT_CLR, 0x3);
        panel
    }

    /// 8 vertical bars (white, yellow, cyan, green, magenta, red, blue,
    /// black) with a 1-pixel white border, then written back to PSRAM.
    fn draw_test_pattern(&mut self) {
        const BARS: [u16; 8] = [0xFFFF, 0xFFE0, 0x07FF, 0x07E0, 0xF81F, 0xF800, 0x001F, 0x0000];
        // SAFETY: `fb` is the caller's PSRAM region of at least FB_BYTES.
        let fb = unsafe { core::slice::from_raw_parts_mut(self.fb, H_RES * V_RES) };
        for y in 0..V_RES {
            for x in 0..H_RES {
                let border = x == 0 || y == 0 || x == H_RES - 1 || y == V_RES - 1;
                fb[y * H_RES + x] = if border { 0xFFFF } else { BARS[x * 8 / H_RES] };
            }
        }
        // SAFETY: ROM cache maintenance over the framebuffer just written.
        unsafe {
            Cache_WriteBack_Addr(CACHE_MAP_L1_DCACHE | CACHE_MAP_L2_CACHE, self.fb as u32, FB_BYTES as u32)
        };
    }

    /// Counts end-of-frame events. Call often (more than once per frame);
    /// the raw flags are sticky, so a late call only merges, never loses, one.
    pub fn poll(&mut self) {
        if rd(LC_DMA_INT_RAW) & INT_VSYNC != 0 {
            wr(LC_DMA_INT_CLR, INT_VSYNC);
            self.vsync += 1;
        }
        let raw = rd(OUT_INT_RAW);
        if raw & (DMA_EOF | DMA_ERR_MASK) != 0 {
            wr(OUT_INT_CLR, raw);
            if raw & DMA_EOF != 0 {
                self.eof += 1;
            }
            if raw & DMA_ERR_MASK != 0 {
                self.dma_err |= raw & DMA_ERR_MASK;
                self.dma_err_count += 1;
            }
        }
    }

    /// (current descriptor, out_state, outfifo_status, lcd_user) for the log.
    pub fn regs(&self) -> [u32; 4] {
        [rd(OUT_DSCR), rd(OUT_STATE), rd(OUTFIFO_STATUS), rd(LCD_USER)]
    }

    pub fn descs_base() -> u32 {
        (&raw const DESCS) as u32
    }
}
