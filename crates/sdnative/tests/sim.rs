//! The driver against a simulated card, clock edge by clock edge.
//!
//! The simulated card follows the bus timing the driver assumes: it samples
//! host-driven CMD/DAT0 on the rising edge of CLK and changes its own
//! outputs on the falling edge. It answers the identification sequence,
//! single and multiple block reads and writes, and models the CRC status
//! token and the busy signal. Faults (bad data CRC, rejected writes, no
//! card, data overlapping the response) are switches on [`Card`].

use std::collections::VecDeque;

use sdnative::{BLOCK, Error, Pins, SdNative, crc7, crc16};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Idle,
    Ready,
    Ident,
    Stby,
    Tran,
}

struct Card {
    present: bool,
    v2: bool,
    hc: bool,
    store: Vec<[u8; BLOCK]>,
    /// ACMD41 replies "busy" this many times first.
    busy_polls: u32,
    /// Clocks from the read command's end bit to the data start bit. Small
    /// values make the data start while the response is still going.
    nac: usize,
    /// Clocks DAT0 is held low after each written block.
    busy_clocks: usize,
    corrupt_read: bool,
    reject_write_crc: bool,
    // bus state
    cmd_n: u32,
    cmd_bits: u64,
    cmd_q: VecDeque<Option<bool>>,
    dat_q: VecDeque<Option<bool>>,
    state: State,
    app: bool,
    rca: u16,
    /// Multi-block read in progress: next block to queue.
    reading: Option<u32>,
    /// Write in progress: (next block, multi, bits collected).
    writing: Option<(u32, bool, Vec<bool>)>,
    /// Data being clocked in after the start bit.
    wr_started: bool,
    // counters for assertions
    commands: Vec<u8>,
    writes: u32,
}

impl Card {
    fn new(blocks: usize, v2: bool, hc: bool) -> Self {
        Self {
            present: true,
            v2,
            hc,
            store: vec![[0u8; BLOCK]; blocks],
            busy_polls: 3,
            nac: 12,
            busy_clocks: 40,
            corrupt_read: false,
            reject_write_crc: false,
            cmd_n: 0,
            cmd_bits: 0,
            cmd_q: VecDeque::new(),
            dat_q: VecDeque::new(),
            state: State::Idle,
            app: false,
            rca: 0x1234,
            reading: None,
            writing: None,
            wr_started: false,
            commands: Vec::new(),
            writes: 0,
        }
    }

    fn cid(&self) -> [u8; 16] {
        let mut c = [0u8; 16];
        c[0] = 0x1B; // Samsung
        c[1..3].copy_from_slice(b"SM");
        c[3..8].copy_from_slice(b"EB1QT");
        c[8] = 0x30;
        c[9..13].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // PSN
        c[13] = 0x01; // year hi nibble 1 -> (0x17 = 23)
        c[14] = 0x75; // year lo nibble 7, month 5
        c[15] = crc7(&c[..15]) << 1 | 1;
        c
    }

    fn csd(&self) -> [u8; 16] {
        let mut c = [0u8; 16];
        let blocks = self.store.len() as u32;
        if self.hc {
            c[0] = 0x40;
            let c_size = blocks / 1024 - 1; // bits 69:48
            c[7] = (c_size >> 16) as u8 & 0x3F;
            c[8] = (c_size >> 8) as u8;
            c[9] = c_size as u8;
        } else {
            // READ_BL_LEN 9 (bits 83:80), C_SIZE_MULT 0 -> blocks = (C_SIZE+1)*4
            c[5] = 0x09;
            let c_size = blocks / 4 - 1; // bits 73:62
            c[6] = (c_size >> 10) as u8 & 0x03;
            c[7] = (c_size >> 2) as u8;
            c[8] = ((c_size & 3) << 6) as u8;
        }
        c[15] = crc7(&c[..15]) << 1 | 1;
        c
    }

    fn status(&self) -> u32 {
        let st = match self.state {
            State::Idle => 0,
            State::Ready => 1,
            State::Ident => 2,
            State::Stby => 3,
            State::Tran => 4,
        };
        st << 9 | 1 << 8 | u32::from(self.app) << 5
    }

    fn queue_resp(&mut self, bytes: &[u8]) {
        // NCR = 2 clocks, then the bits, then release.
        self.cmd_q.extend([None, None]);
        for b in bytes {
            for i in (0..8).rev() {
                self.cmd_q.push_back(Some(b >> i & 1 == 1));
            }
        }
        self.cmd_q.push_back(None);
    }

    fn r1(&mut self, index: u8, arg: u32) {
        let mut r = [index, 0, 0, 0, 0, 0];
        r[1..5].copy_from_slice(&arg.to_be_bytes());
        r[5] = crc7(&r[..5]) << 1 | 1;
        self.queue_resp(&r);
    }

    fn r2(&mut self, reg: [u8; 16]) {
        let mut r = [0x3Fu8; 17];
        r[1..].copy_from_slice(&reg);
        self.queue_resp(&r);
    }

    fn queue_block(&mut self, block: u32, gap: usize) {
        let mut data = self.store[block as usize];
        let crc = crc16(&data);
        if self.corrupt_read {
            data[100] ^= 0x10;
        }
        self.dat_q.extend(std::iter::repeat_n(None, gap));
        self.dat_q.push_back(Some(false));
        for b in data {
            for i in (0..8).rev() {
                self.dat_q.push_back(Some(b >> i & 1 == 1));
            }
        }
        for i in (0..16).rev() {
            self.dat_q.push_back(Some(crc >> i & 1 == 1));
        }
        self.dat_q.push_back(Some(true));
        self.dat_q.push_back(None);
    }

    fn block_of(&self, arg: u32) -> u32 {
        if self.hc { arg } else { arg / BLOCK as u32 }
    }

    fn command(&mut self, index: u8, arg: u32) {
        self.commands.push(index);
        let app = std::mem::replace(&mut self.app, false);
        match (app, index) {
            (_, 0) => {
                self.state = State::Idle;
                self.reading = None;
                self.writing = None;
            }
            (false, 8) if self.v2 => self.r1(8, arg & 0xFFF),
            (false, 8) => {} // v1: illegal command, no response
            (_, 55) => {
                self.app = true;
                self.r1(55, self.status());
            }
            (true, 41) => {
                let ready = self.busy_polls == 0;
                self.busy_polls = self.busy_polls.saturating_sub(1);
                let hcs = arg & 0x4000_0000 != 0;
                let mut ocr = 0x00FF_8000u32;
                if ready {
                    ocr |= 0x8000_0000;
                    self.state = State::Ready;
                    if self.hc && hcs {
                        ocr |= 0x4000_0000;
                    }
                }
                let b = ocr.to_be_bytes();
                self.queue_resp(&[0x3F, b[0], b[1], b[2], b[3], 0xFF]);
            }
            (false, 2) if self.state == State::Ready => {
                self.state = State::Ident;
                self.r2(self.cid());
            }
            (false, 3) => {
                self.state = State::Stby;
                self.r1(3, u32::from(self.rca) << 16 | 0x0500);
            }
            (false, 9) => self.r2(self.csd()),
            (false, 7) if arg >> 16 == u32::from(self.rca) => {
                self.r1(7, self.status());
                self.state = State::Tran;
                self.dat_q.extend([None, None, None]);
                self.dat_q.extend(std::iter::repeat_n(Some(false), 20));
                self.dat_q.push_back(None);
            }
            (true, 6) => self.r1(6, self.status()),
            (false, 16) => {
                assert!(!self.hc, "CMD16 sent to a high-capacity card");
                assert_eq!(arg, 512);
                self.r1(16, self.status());
            }
            (false, 13) => self.r1(13, self.status()),
            (false, 17) | (false, 18) => {
                assert!(!self.hc || index == 17 || index == 18);
                let block = self.block_of(arg);
                self.r1(index, self.status());
                self.queue_block(block, self.nac);
                self.reading = (index == 18).then_some(block + 1);
            }
            (false, 24) | (false, 25) => {
                self.r1(index, self.status());
                self.writing = Some((self.block_of(arg), index == 25, Vec::new()));
                self.wr_started = false;
            }
            (false, 12) => {
                self.reading = None;
                self.writing = None;
                // Data stops two clocks after the stop command's end bit.
                self.dat_q.truncate(2);
                self.dat_q.push_back(None);
                self.r1(12, self.status());
            }
            other => panic!("unexpected command {other:?} in state {:?}", self.state),
        }
    }

    /// Rising edge: sample what the host drives.
    fn rising(&mut self, cmd: Option<bool>, dat: Option<bool>) {
        if let Some(bit) = cmd
            && (self.cmd_n > 0 || !bit) {
                self.cmd_bits = self.cmd_bits << 1 | u64::from(bit);
                self.cmd_n += 1;
                if self.cmd_n == 48 {
                    self.cmd_n = 0;
                    let f = self.cmd_bits.to_be_bytes();
                    let f = &f[2..8];
                    assert_eq!(f[0] & 0xC0, 0x40, "start/transmission bits");
                    assert_eq!(crc7(&f[..5]) << 1 | 1, f[5], "command CRC7");
                    if self.present {
                        self.command(f[0] & 0x3F, u32::from_be_bytes([f[1], f[2], f[3], f[4]]));
                    }
                }
            }
        if let (Some(bit), Some((_, _, bits))) = (dat, self.writing.as_mut()) {
            if !self.wr_started {
                self.wr_started = !bit;
                return;
            }
            bits.push(bit);
            if bits.len() == BLOCK * 8 + 17 {
                let bits = std::mem::take(bits);
                self.wr_started = false;
                let mut data = [0u8; BLOCK];
                for (i, &b) in bits[..BLOCK * 8].iter().enumerate() {
                    data[i / 8] |= u8::from(b) << (7 - i % 8);
                }
                let crc = bits[BLOCK * 8..BLOCK * 8 + 16].iter().fold(0u16, |c, &b| c << 1 | u16::from(b));
                assert!(bits[BLOCK * 8 + 16], "write end bit");
                let ok = crc == crc16(&data) && !self.reject_write_crc;
                let (block, multi, _) = self.writing.take().unwrap();
                if ok {
                    self.store[block as usize] = data;
                    self.writes += 1;
                }
                if multi && ok {
                    self.writing = Some((block + 1, true, Vec::new()));
                }
                let token = if ok { [false, true, false] } else { [true, false, true] };
                self.dat_q.extend([None, None, Some(false)]);
                self.dat_q.extend(token.map(Some));
                self.dat_q.push_back(Some(true));
                self.dat_q.extend(std::iter::repeat_n(Some(false), self.busy_clocks));
                self.dat_q.push_back(None);
            }
        }
    }
}

struct Sim {
    card: Card,
    clk: bool,
    host_cmd: Option<bool>,
    host_dat: Option<bool>,
    card_cmd: Option<bool>,
    card_dat: Option<bool>,
    clocks: u64,
}

impl Sim {
    fn new(card: Card) -> Self {
        Self { card, clk: true, host_cmd: None, host_dat: None, card_cmd: None, card_dat: None, clocks: 0 }
    }

    fn check_contention(&self) {
        assert!(!(self.host_cmd.is_some() && self.card_cmd.is_some()), "CMD driven by both at clock {}", self.clocks);
        assert!(!(self.host_dat.is_some() && self.card_dat.is_some()), "DAT0 driven by both at clock {}", self.clocks);
    }
}

impl Pins for Sim {
    fn clk(&mut self, high: bool) {
        if high && !self.clk {
            self.clocks += 1;
            self.check_contention();
            self.card.rising(self.host_cmd, self.host_dat);
        } else if !high && self.clk {
            self.card_cmd = self.card.cmd_q.pop_front().flatten();
            self.card_dat = self.card.dat_q.pop_front().flatten();
            if self.card.dat_q.is_empty()
                && let Some(next) = self.card.reading
                    && (next as usize) < self.card.store.len() {
                        self.card.queue_block(next, 2);
                        self.card.reading = Some(next + 1);
                    }
        }
        self.clk = high;
    }
    fn cmd_out(&mut self, high: bool) {
        self.host_cmd = Some(high);
    }
    fn cmd_release(&mut self) {
        self.host_cmd = None;
    }
    fn cmd_in(&mut self) -> bool {
        self.card_cmd.or(self.host_cmd).unwrap_or(true)
    }
    fn dat_out(&mut self, high: bool) {
        self.host_dat = Some(high);
    }
    fn dat_release(&mut self) {
        self.host_dat = None;
    }
    fn dat_in(&mut self) -> bool {
        self.card_dat.or(self.host_dat).unwrap_or(true)
    }
    fn half(&mut self, _slow: bool) {}
    fn now_us(&mut self) -> u64 {
        self.clocks
    }
}

fn pattern(seed: u32) -> [u8; BLOCK] {
    let mut b = [0u8; BLOCK];
    let mut x = seed.wrapping_mul(2_654_435_761) | 1;
    for v in b.iter_mut() {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *v = x as u8;
    }
    b
}

fn sdhc() -> SdNative<Sim> {
    SdNative::new(Sim::new(Card::new(8192, true, true)))
}

#[test]
fn init_sdhc() {
    let mut sd = sdhc();
    let info = sd.init(10).expect("init");
    assert!(info.high_capacity && info.v2);
    assert_eq!(info.blocks, 8192);
    assert_eq!(info.manufacturer, 0x1B);
    assert_eq!(&info.oem, b"SM");
    assert_eq!(&info.product, b"EB1QT");
    assert_eq!((info.year, info.month), (2023, 5));
    assert_eq!(sd.pins().card.commands, [0, 8, 55, 41, 55, 41, 55, 41, 55, 41, 2, 3, 9, 7, 55, 6]);
    assert_eq!(sd.pins().card.state, State::Tran);
    sd.status().expect("CMD13");
}

#[test]
fn init_v1_standard_capacity_uses_cmd16_and_byte_addresses() {
    let mut sd = SdNative::new(Sim::new(Card::new(4096, false, false)));
    let info = sd.init(10).expect("init");
    assert!(!info.high_capacity && !info.v2);
    assert_eq!(info.blocks, 4096);
    assert_eq!(*sd.pins().card.commands.last().unwrap(), 16);
    let b = pattern(7);
    sd.write(3000, &[b]).unwrap();
    let mut r = [[0u8; BLOCK]];
    sd.read(3000, &mut r).unwrap();
    assert_eq!(r[0], b);
    assert_eq!(sd.pins().card.store[3000], b);
}

#[test]
fn sdsc_card_in_v2_host_is_not_high_capacity() {
    let mut sd = SdNative::new(Sim::new(Card::new(4096, true, false)));
    let info = sd.init(10).unwrap();
    assert!(info.v2 && !info.high_capacity);
}

#[test]
fn empty_slot() {
    let mut card = Card::new(8192, true, true);
    card.present = false;
    let mut sd = SdNative::new(Sim::new(card));
    sd.begin().expect("begin works with nothing there (CMD0 has no reply, CMD8 none either)");
    assert_eq!(sd.poll_ready(), Err(Error::NoResponse(55)));
    assert!(sd.info().is_none());
    // A probe is cheap: well under a thousand clocks.
    assert!(sd.pins().clocks < 1000, "{}", sd.pins().clocks);
}

#[test]
fn card_never_ready() {
    let mut card = Card::new(8192, true, true);
    card.busy_polls = 1000;
    let mut sd = SdNative::new(Sim::new(card));
    assert_eq!(sd.init(5), Err(Error::NotReady));
}

#[test]
fn single_block_write_then_read() {
    let mut sd = sdhc();
    sd.init(10).unwrap();
    let b = pattern(1);
    sd.write(42, &[b]).unwrap();
    assert!(sd.is_busy(), "card holds DAT0 low after a write");
    assert_eq!(sd.pins().card.store[42], b);
    // The next command waits busy out by itself.
    let mut r = [[0u8; BLOCK]];
    sd.read(42, &mut r).unwrap();
    assert_eq!(r[0], b);
    assert!(!sd.is_busy());
}

#[test]
fn is_busy_clears_after_the_card_finishes() {
    let mut sd = sdhc();
    sd.init(10).unwrap();
    sd.write(5, &[pattern(5)]).unwrap();
    let mut polls = 0;
    while sd.is_busy() {
        polls += 1;
        assert!(polls < 100);
    }
    // 40 busy clocks at 8 clocks per poll.
    assert!((3..=7).contains(&polls), "{polls}");
}

#[test]
fn multi_block_write_and_read() {
    let mut sd = sdhc();
    sd.init(10).unwrap();
    let blocks: Vec<_> = (0..5).map(pattern).collect();
    sd.write(100, &blocks).unwrap();
    assert_eq!(sd.pins().card.writes, 5);
    for (i, b) in blocks.iter().enumerate() {
        assert_eq!(&sd.pins().card.store[100 + i], b);
    }
    assert!(sd.pins().card.commands.ends_with(&[25, 12]));
    let mut r = vec![[0u8; BLOCK]; 5];
    sd.read(100, &mut r).unwrap();
    assert_eq!(r, blocks);
    assert!(sd.pins().card.commands.ends_with(&[18, 12]));
    // Still in step afterwards.
    let mut one = [[0u8; BLOCK]];
    sd.read(102, &mut one).unwrap();
    assert_eq!(one[0], blocks[2]);
}

#[test]
fn data_starting_inside_the_response_is_caught() {
    for nac in [0, 1, 5, 30, 60, 200] {
        let mut card = Card::new(8192, true, true);
        card.nac = nac;
        card.store[9] = pattern(9);
        card.store[10] = pattern(10);
        let mut sd = SdNative::new(Sim::new(card));
        sd.init(10).unwrap();
        let mut r = [[0u8; BLOCK]];
        sd.read(9, &mut r).unwrap_or_else(|e| panic!("nac {nac}: {e:?}"));
        assert_eq!(r[0], pattern(9), "nac {nac}");
        let mut r2 = [[0u8; BLOCK]; 2];
        sd.read(9, &mut r2).unwrap_or_else(|e| panic!("multi nac {nac}: {e:?}"));
        assert_eq!(r2[1], pattern(10), "nac {nac}");
    }
}

#[test]
fn bad_read_crc_is_reported() {
    let mut sd = sdhc();
    sd.init(10).unwrap();
    sd.pins().card.corrupt_read = true;
    let mut r = [[0u8; BLOCK]];
    assert_eq!(sd.read(1, &mut r), Err(Error::DataCrc));
    // And the bus is usable after it.
    sd.pins().card.corrupt_read = false;
    sd.read(1, &mut r).unwrap();
}

#[test]
fn rejected_write_is_reported() {
    let mut sd = sdhc();
    sd.init(10).unwrap();
    sd.pins().card.reject_write_crc = true;
    assert_eq!(sd.write(1, &[pattern(1)]), Err(Error::WriteCrc));
    assert_eq!(sd.pins().card.store[1], [0u8; BLOCK]);
}

#[test]
fn out_of_range_and_uninitialised() {
    let mut sd = sdhc();
    let mut r = [[0u8; BLOCK]];
    assert_eq!(sd.read(0, &mut r), Err(Error::NoCard));
    sd.init(10).unwrap();
    assert_eq!(sd.read(8192, &mut r), Err(Error::OutOfRange));
    let mut two = [[0u8; BLOCK]; 2];
    assert_eq!(sd.read(8191, &mut two), Err(Error::OutOfRange));
    sd.read(8191, &mut r).unwrap();
}

#[test]
fn card_pulled_out_after_init() {
    let mut sd = sdhc();
    sd.init(10).unwrap();
    sd.pins().card.present = false;
    assert_eq!(sd.status(), Err(Error::NoResponse(13)));
    let mut r = [[0u8; BLOCK]];
    assert_eq!(sd.read(0, &mut r), Err(Error::NoResponse(17)));
    assert_eq!(sd.write(0, &[pattern(0)]), Err(Error::NoResponse(24)));
}

#[test]
fn clocks_per_block_are_close_to_the_bit_count() {
    let mut sd = sdhc();
    sd.init(10).unwrap();
    let c0 = sd.pins().clocks;
    sd.write(7, &[pattern(7)]).unwrap();
    sd.wait_busy().unwrap();
    let w = sd.pins().clocks - c0;
    let c0 = sd.pins().clocks;
    let mut r = [[0u8; BLOCK]];
    sd.read(7, &mut r).unwrap();
    let rd = sd.pins().clocks - c0;
    // 48 command + ~56 response + 4113 data bits + framing; busy 40. The
    // read is shorter: its data starts while the response is still coming.
    assert!((4200..4400).contains(&w), "write {w} clocks");
    assert!((4150..4400).contains(&rd), "read {rd} clocks");
}
