//! Path B: PARL_IO RX at 40 MHz into an endless GDMA descriptor ring.
//!
//! esp-hal 1.2 has a PARL_IO driver, but only for bounded transfers; an
//! endless ring needs the descriptors linked in a circle with the owner check
//! off, which it cannot express. So both peripherals are programmed here at
//! register level, in the order ESP-IDF's PARL_IO RX driver uses and
//! esp-sdr-zephyr's `esp_sdr_diag.c` measured gapless for 40 s at 40 MB/s
//! (GPL-3.0-or-later; re-implemented from its LL calls, not copied):
//!
//! - PARL_IO: clock PLL_F240M / 6 = 40 MHz, 8-bit bus, LSB first, software
//!   ("soft delimiter") receive with EOF after the ring length, no timeout.
//! - GDMA (AHB) RX channel 0, peripheral 9 (PARL_IO; both C5VRX and
//!   esp-sdr-zephyr), owner check off, 16 descriptors of 3584 bytes linked
//!   in a circle at the bottom of SRAM block 1, interrupt per finished node.
//!
//! The interrupt walks the ring with [`rawcap::ring::Walker`] and signals
//! the capture loop. Positions are 64-bit stream byte counts.

use core::cell::Cell;
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use critical_section::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use esp_hal::interrupt::{InterruptHandler, Priority};
use esp_hal::peripherals::{DMA, Interrupt, PARL_IO, PCR};
use rawcap::ring::{NODE_BYTES, NODES, RING_BYTES, Walker, dw0};

/// The ring: bottom of SRAM block 1 (ld/sdr_bank.x).
pub const RING_ADDR: usize = 0x4082_0000;
/// Descriptors, right after the ring.
pub const DESC_ADDR: usize = RING_ADDR + RING_BYTES;
const _: () = assert!(DESC_ADDR + NODES * 12 <= 0x4083_0000);

/// GDMA RX channel and the PARL_IO trigger id (ESP-IDF `soc/gdma_channel.h`
/// via esp-sdr-zephyr `DMA_TRIG_PARLIO`; C5VRX looks for 9 too).
const CH: usize = 0;
const PERI_PARL_IO: u8 = 9;
/// PARL_IO RX clock: PLL_F240M (select 2) divided by 6.
const CLK_SEL_PLL_F240M: u8 = 2;
const CLK_DIV: u16 = 6;
pub const BYTE_RATE: u32 = 240_000_000 / CLK_DIV as u32;

#[repr(C)]
struct Desc {
    dw0: u32,
    buf: u32,
    next: u32,
}

fn desc(i: usize) -> *mut Desc {
    (DESC_ADDR + 12 * i) as *mut Desc
}

static WALKER: Mutex<Cell<Walker>> = Mutex::new(Cell::new(Walker::new()));
/// Raised by the interrupt whenever new nodes finished.
pub static NEW_DATA: Signal<CriticalSectionRawMutex, ()> = Signal::new();
pub static ISRS: AtomicU32 = AtomicU32::new(0);
/// Descriptor errors / empty-descriptor and FIFO overflow events.
pub static DMA_ERRORS: AtomicU32 = AtomicU32::new(0);

/// The ring as bytes. Only read data the producer count says is complete.
pub fn bytes() -> &'static [u8] {
    // SAFETY: SRAM block 1 is reserved for us (ld/sdr_bank.x); the DMA writes
    // other parts of it concurrently, which the caller tolerates by only
    // reading complete, not-yet-lapped windows (`rawcap::ring::window_ok`).
    unsafe { core::slice::from_raw_parts(RING_ADDR as *const u8, RING_BYTES) }
}

/// Bytes delivered since [`start`], and whether the walker saw a full lap.
pub fn produced() -> (u64, bool) {
    critical_section::with(|cs| {
        let w = WALKER.borrow(cs).get();
        (w.produced, w.overrun)
    })
}

extern "C" fn dma_isr() {
    let dma = DMA::regs();
    let st = dma.in_int_ch(CH).st().read().bits();
    dma.in_int_ch(CH).clr().write(|w| unsafe { w.bits(st) });
    ISRS.fetch_add(1, Relaxed);
    // in_dscr_err (bit 3), in_dscr_empty (4), infifo_ovf (5), infifo_udf (6).
    if st & 0b111_1000 != 0 {
        DMA_ERRORS.fetch_add(1, Relaxed);
    }
    let walked = critical_section::with(|cs| {
        let cell = WALKER.borrow(cs);
        let mut w = cell.get();
        let n = w.walk(|i| {
            let d = desc(i);
            // SAFETY: our descriptor; the DMA writes dw0 back when a node
            // finishes and we recycle it here.
            unsafe {
                let v = core::ptr::addr_of!((*d).dw0).read_volatile();
                let len = dw0::length(v);
                if len != 0 {
                    core::ptr::addr_of_mut!((*d).dw0).write_volatile(dw0::recycle(v));
                }
                len
            }
        });
        cell.set(w);
        n
    });
    if walked != 0 {
        NEW_DATA.signal(());
    }
}

fn init_descriptors() {
    for i in 0..NODES {
        let d = desc(i);
        // SAFETY: our descriptor memory, DMA stopped.
        unsafe {
            d.write_volatile(Desc {
                dw0: dw0::fresh(NODE_BYTES as u32),
                buf: (RING_ADDR + i * NODE_BYTES) as u32,
                next: desc((i + 1) % NODES) as u32,
            });
        }
    }
}

/// Bind the interrupt once at boot.
pub fn install() {
    esp_hal::interrupt::bind_handler(Interrupt::DMA_IN_CH0, InterruptHandler::new(dma_isr, Priority::Priority2));
}

/// Start (or restart) the ring. The debug bus must already be routed
/// ([`crate::route`]) and the dump engine free-running ([`crate::engine`]).
pub fn start(falling_edge: bool) {
    stop();
    init_descriptors();
    critical_section::with(|cs| WALKER.borrow(cs).set(Walker::new()));
    NEW_DATA.reset();

    let pcr = PCR::regs();
    let p = PARL_IO::regs();
    let dma = DMA::regs();

    // SAFETY (all register writes below): field values within their widths,
    // on peripherals this firmware owns exclusively (PARL_IO, DMA_CH0).
    unsafe {
        // PARL_IO bus clock and reset.
        pcr.parl_io_conf().modify(|_, w| w.parl_clk_en().set_bit());
        pcr.parl_io_conf().modify(|_, w| w.parl_rst_en().set_bit());
        pcr.parl_io_conf().modify(|_, w| w.parl_rst_en().clear_bit());
        // RX core clock: off while configuring; PLL_F240M / 6.
        pcr.parl_clk_rx_conf().modify(|_, w| w.parl_rx_rst_en().set_bit());
        pcr.parl_clk_rx_conf().modify(|_, w| w.parl_rx_rst_en().clear_bit());
        p.fifo_cfg().modify(|_, w| w.rx_fifo_srst().set_bit());
        p.fifo_cfg().modify(|_, w| w.rx_fifo_srst().clear_bit());
        pcr.parl_clk_rx_conf().modify(|_, w| w.parl_clk_rx_en().clear_bit());
        p.rx_start_cfg().modify(|_, w| w.rx_start().clear_bit());
        pcr.parl_clk_rx_conf()
            .modify(|_, w| w.parl_clk_rx_sel().bits(CLK_SEL_PLL_F240M).parl_clk_rx_div_num().bits(CLK_DIV - 1));
        // 8 data lines (select 3), LSB first, no gating, chosen edge.
        p.rx_data_cfg().modify(|_, w| {
            w.rx_bus_wid_sel().bits(3);
            w.rx_data_order_inv().clear_bit();
            // EOF after one ring's worth of bits (fits the 19-bit field).
            w.rx_bitlen().bits((RING_BYTES * 8) as u32)
        });
        p.rx_genrl_cfg().modify(|_, w| {
            w.rx_gating_en().clear_bit();
            w.rx_timeout_en().clear_bit();
            // 0 = EOF when rx_bitlen bits arrived ("RX_FULL").
            w.rx_eof_gen_sel().clear_bit()
        });
        p.rx_clk_cfg().modify(|_, w| w.rx_clk_i_inv().bit(falling_edge).rx_clk_o_inv().bit(falling_edge));
        // Software-started reception (sample mode 2).
        p.rx_mode_cfg().modify(|_, w| w.rx_smp_mode_sel().bits(2).rx_sw_en().clear_bit());
        p.reg_update().write(|w| w.rx_reg_update().set_bit());
        // The update bit self-clears once the RX clock domain has taken the
        // configuration (ESP-IDF `parlio_ll_rx_update_config` spins on it).
        // The PAC marks the register write-only, so read it raw; bounded wait.
        let upd = p.reg_update().as_ptr();
        for _ in 0..10_000 {
            if upd.read_volatile() & 1 == 0 {
                break;
            }
        }

        // GDMA: bus clock (no reset: other channels may be in use by esp-hal
        // drivers in later phases), register clock, memory window.
        pcr.gdma_conf().modify(|_, w| w.gdma_clk_en().set_bit());
        dma.misc_conf().modify(|_, w| w.clk_en().set_bit());
        dma.intr_mem_start_addr().write(|w| w.bits(0x4080_0000));
        dma.intr_mem_end_addr().write(|w| w.bits(0x4400_0000));
        let ch = dma.ch(CH);
        ch.in_conf0().modify(|_, w| w.in_rst().set_bit());
        ch.in_conf0().modify(|_, w| w.in_rst().clear_bit());
        ch.in_conf1().modify(|_, w| w.in_check_owner().clear_bit());
        ch.in_conf0().modify(|_, w| w.mem_trans_en().clear_bit());
        ch.in_peri_sel().modify(|_, w| w.peri_in_sel().bits(PERI_PARL_IO));
        dma.in_int_ch(CH).clr().write(|w| w.bits(u32::MAX));
        dma.in_int_ch(CH).ena().write(|w| {
            w.in_done().set_bit();
            w.in_dscr_err().set_bit();
            w.in_dscr_empty().set_bit();
            w.infifo_ovf().set_bit()
        });
        dma.in_link_addr_ch(CH).write(|w| w.inlink_addr().bits(desc(0) as u32));
        ch.in_link().modify(|_, w| w.inlink_start().set_bit());

        // DMA armed; now the unit, its clock, then the soft start.
        p.rx_start_cfg().modify(|_, w| w.rx_start().set_bit());
        pcr.parl_clk_rx_conf().modify(|_, w| w.parl_clk_rx_en().set_bit());
        p.rx_mode_cfg().modify(|_, w| w.rx_sw_en().set_bit());
    }
}

/// Stop reception and the DMA (freezes the ring's contents).
pub fn stop() {
    let pcr = PCR::regs();
    let p = PARL_IO::regs();
    let dma = DMA::regs();
    // SAFETY: as in `start`.
    unsafe {
        p.rx_mode_cfg().modify(|_, w| w.rx_sw_en().clear_bit());
        p.rx_start_cfg().modify(|_, w| w.rx_start().clear_bit());
        pcr.parl_clk_rx_conf().modify(|_, w| w.parl_clk_rx_en().clear_bit());
        dma.in_int_ch(CH).ena().write(|w| w.bits(0));
        dma.ch(CH).in_link().modify(|_, w| w.inlink_stop().set_bit());
        dma.in_int_ch(CH).clr().write(|w| w.bits(u32::MAX));
    }
    // Account for nodes that finished before the stop.
    dma_isr_walk_only();
}

fn dma_isr_walk_only() {
    critical_section::with(|cs| {
        let cell = WALKER.borrow(cs);
        let mut w = cell.get();
        w.walk(|i| {
            let d = desc(i);
            // SAFETY: as in `dma_isr`.
            unsafe {
                let v = core::ptr::addr_of!((*d).dw0).read_volatile();
                let len = dw0::length(v);
                if len != 0 {
                    core::ptr::addr_of_mut!((*d).dw0).write_volatile(dw0::recycle(v));
                }
                len
            }
        });
        cell.set(w);
    });
}

/// The descriptor the DMA is working on (index), for diagnostics.
pub fn current_node() -> Option<usize> {
    let a = DMA::regs().ch(CH).in_dscr().read().inlink_dscr().bits() as usize;
    (DESC_ADDR..DESC_ADDR + 12 * NODES).contains(&a).then(|| (a - DESC_ADDR) / 12)
}

/// PARL_IO's own count of received cycles (`rx_st0.rx_cnt`), for bring-up:
/// if this moves but `produced` does not, the DMA side is the problem.
pub fn parl_rx_count() -> u32 {
    PARL_IO::regs().rx_st0().read().rx_cnt().bits() as u32
}
