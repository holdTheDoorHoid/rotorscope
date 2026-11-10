//! Path B's circular DMA ring: geometry, producer accounting and windows.
//!
//! The GDMA RX channel runs over [`NODES`] descriptors of [`NODE_BYTES`]
//! each, the last linked back to the first, with the owner check off so the
//! hardware never stops (esp-sdr-zephyr `lib/esp_sdr/src/esp_sdr_diag.c`:
//! 16 nodes of 3584 bytes, 57,344 bytes, "measured 40 MB/s for 40 s, 0
//! skipped blocks"). A descriptor holds at most 4095 bytes, 4-byte aligned.
//!
//! The hardware writes the received length into each descriptor it
//! finishes. The interrupt handler walks the ring from the last node it saw,
//! adds every non-zero length to a 64-bit producer count and clears it
//! ([`Walker`]), so merged interrupts lose nothing. Stream positions are
//! that count: the byte at position `p` sits at ring offset `p % RING_BYTES`.

/// Bytes per descriptor.
pub const NODE_BYTES: usize = 3584;
/// Descriptors in the ring.
pub const NODES: usize = 16;
/// Ring size: 57,344 bytes, 1.43 ms at 40 MS/s.
pub const RING_BYTES: usize = NODE_BYTES * NODES;

/// GDMA descriptor word 0 fields (ESP-IDF `hal/dma_types.h`).
pub mod dw0 {
    pub const SIZE_MASK: u32 = 0xfff;
    pub const LENGTH_SHIFT: u32 = 12;
    pub const LENGTH_MASK: u32 = 0xfff << LENGTH_SHIFT;
    pub const SUC_EOF: u32 = 1 << 30;
    pub const OWNER_DMA: u32 = 1 << 31;

    /// A fresh RX descriptor of `size` bytes, owned by the DMA, length 0.
    pub const fn fresh(size: u32) -> u32 {
        OWNER_DMA | (size & SIZE_MASK)
    }
    pub const fn length(w: u32) -> u32 {
        (w & LENGTH_MASK) >> LENGTH_SHIFT
    }
    /// `w` with the length cleared, EOF cleared, and handed back to the DMA.
    pub const fn recycle(w: u32) -> u32 {
        (w & !(LENGTH_MASK | SUC_EOF)) | OWNER_DMA
    }
}

const _: () = assert!(NODE_BYTES <= 4095 && NODE_BYTES.is_multiple_of(4));

/// The interrupt handler's view of the ring.
#[derive(Clone, Copy, Debug, Default)]
pub struct Walker {
    /// Next node expected to finish.
    pub head: usize,
    /// Bytes the DMA has delivered since the ring started.
    pub produced: u64,
    /// Set when one visit found every node finished: the ring may have lapped
    /// the walker itself (it then cannot tell how many laps).
    pub overrun: bool,
}

impl Walker {
    pub const fn new() -> Self {
        Walker { head: 0, produced: 0, overrun: false }
    }

    /// Walk finished nodes. `take(i)` returns node `i`'s written length and
    /// clears it (0 means "not finished yet"). Returns how many nodes were
    /// consumed.
    pub fn walk(&mut self, mut take: impl FnMut(usize) -> u32) -> usize {
        let mut walked = 0;
        while walked < NODES {
            let len = take(self.head);
            if len == 0 {
                break;
            }
            self.produced += len as u64;
            self.head = (self.head + 1) % NODES;
            walked += 1;
        }
        if walked == NODES {
            self.overrun = true;
        }
        walked
    }
}

/// Is the stream window `[start, end)` complete and still intact in the
/// ring, given `produced` bytes so far? `guard` bytes are kept back for the
/// node the DMA is writing right now (pass [`NODE_BYTES`]).
pub fn window_ok(start: u64, end: u64, produced: u64, guard: usize) -> bool {
    end >= start && end <= produced && produced - start <= (RING_BYTES - guard) as u64
}

/// The ring slices holding stream window `[start, start + len)`:
/// `(offset, len)` pairs, the second empty unless the window wraps.
pub fn segments(start: u64, len: usize) -> [(usize, usize); 2] {
    assert!(len <= RING_BYTES);
    let off = (start % RING_BYTES as u64) as usize;
    let first = len.min(RING_BYTES - off);
    [(off, first), (0, len - first)]
}

/// Byte at stream position `p`.
#[inline(always)]
pub fn at(ring: &[u8], p: u64) -> u8 {
    ring[(p % RING_BYTES as u64) as usize]
}

/// Copy stream window `[start, start + out.len())` out of `ring`.
pub fn copy_window(ring: &[u8], start: u64, out: &mut [u8]) {
    let [(o1, l1), (o2, l2)] = segments(start, out.len());
    out[..l1].copy_from_slice(&ring[o1..o1 + l1]);
    out[l1..l1 + l2].copy_from_slice(&ring[o2..o2 + l2]);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake DMA: writes `n` bytes of stream (byte value = position & 0xff)
    /// into the ring and its descriptor lengths, the way the hardware does.
    struct FakeDma {
        ring: Vec<u8>,
        lens: [u32; NODES],
        pos: u64,
    }

    impl FakeDma {
        fn new() -> Self {
            FakeDma { ring: vec![0; RING_BYTES], lens: [0; NODES], pos: 0 }
        }
        fn run(&mut self, n: usize) {
            for _ in 0..n {
                let off = (self.pos % RING_BYTES as u64) as usize;
                self.ring[off] = self.pos as u8;
                self.pos += 1;
                if self.pos.is_multiple_of(NODE_BYTES as u64) {
                    let node = off / NODE_BYTES;
                    self.lens[node] = NODE_BYTES as u32;
                }
            }
        }
    }

    #[test]
    fn walker_counts_every_finished_node() {
        let mut d = FakeDma::new();
        let mut w = Walker::new();
        let mut total = 0;
        // Irregular interrupt timing, including several nodes per visit.
        for step in [100, 3584, 9000, 1, 20000, 3583, 40000] {
            d.run(step);
            total += step;
            w.walk(|i| core::mem::take(&mut d.lens[i]));
            assert_eq!(w.produced, (total / NODE_BYTES * NODE_BYTES) as u64);
            assert!(!w.overrun);
        }
    }

    #[test]
    fn walker_flags_a_full_lap() {
        let mut d = FakeDma::new();
        let mut w = Walker::new();
        d.run(RING_BYTES + 10);
        w.walk(|i| core::mem::take(&mut d.lens[i]));
        assert!(w.overrun);
    }

    #[test]
    fn descriptor_words() {
        let f = dw0::fresh(NODE_BYTES as u32);
        assert_eq!(f & dw0::SIZE_MASK, 3584);
        assert_eq!(dw0::length(f), 0);
        assert!(f & dw0::OWNER_DMA != 0);
        let done = (f & !dw0::OWNER_DMA) | (3584 << dw0::LENGTH_SHIFT) | dw0::SUC_EOF;
        assert_eq!(dw0::length(done), 3584);
        assert_eq!(dw0::recycle(done), f);
    }

    #[test]
    fn windows_and_wrap() {
        let mut d = FakeDma::new();
        d.run(5 * RING_BYTES + 1234);
        let produced = d.pos / NODE_BYTES as u64 * NODE_BYTES as u64;
        let len = 32_768;
        let start = produced - len as u64;
        assert!(window_ok(start, start + len as u64, produced, NODE_BYTES));
        let mut out = vec![0u8; len];
        copy_window(&d.ring, start, &mut out);
        for (k, b) in out.iter().enumerate() {
            assert_eq!(*b, (start + k as u64) as u8);
        }
        // Too old: lapped.
        let old = produced - (RING_BYTES - NODE_BYTES) as u64 - 1;
        assert!(!window_ok(old, old + 100, produced, NODE_BYTES));
        // Not complete yet.
        assert!(!window_ok(produced - 10, produced + 1, produced, NODE_BYTES));
        // Every alignment splits into slices that add up.
        for s in [0u64, 1, 57_343, 57_344, 100_000] {
            let [(_, a), (_, b)] = segments(s, 40_000);
            assert_eq!(a + b, 40_000);
        }
        assert_eq!(at(&d.ring, start), start as u8);
    }
}
