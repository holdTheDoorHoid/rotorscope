//! A simulated hub for the continuity tests: boots in turn, each writing a
//! chain (boot event, continuity record from its counter store, notes and
//! checkpoints) and saving its durable head after every record, as the
//! hub does after each card flush.

use evidence::continuity::{FileCounter, Slots, SlotCounter, BootCounter, STATE_LEN};
use evidence::format::{self, boot, CheckpointReason, EventCode};
use evidence::sign::SoftSigner;
use evidence::writer::{Chain, Context};
use evidence::Signer;

#[derive(Default)]
pub struct Mem {
    pub slot: [Option<Vec<u8>>; 2],
}

impl Slots for Mem {
    type Error = ();
    fn read(&mut self, slot: usize, buf: &mut [u8; STATE_LEN]) -> Result<Option<usize>, ()> {
        Ok(self.slot[slot].as_ref().map(|v| {
            let n = v.len().min(STATE_LEN);
            buf[..n].copy_from_slice(&v[..n]);
            v.len()
        }))
    }
    fn write(&mut self, slot: usize, data: &[u8; STATE_LEN]) -> Result<(), ()> {
        self.slot[slot] = Some(data.to_vec());
        Ok(())
    }
}

pub struct Unit {
    pub store: Mem,
    pub signer: SoftSigner,
    /// Hub ms at which each boot starts (its UTC, if any, follows).
    pub utc_base: Option<u64>,
}

/// One boot's records, and the byte offset after each record.
pub struct Boot {
    pub bytes: Vec<u8>,
    pub ends: Vec<usize>,
    pub boot_id: [u8; 8],
}

impl Boot {
    /// The bytes up to and including record `seq`.
    pub fn through(&self, seq: usize) -> Vec<u8> {
        self.bytes[..self.ends[seq]].to_vec()
    }
}

impl Unit {
    pub fn new() -> Self {
        Self { store: Mem::default(), signer: SoftSigner::public_test(), utc_base: None }
    }

    /// Boot `id`: boot event, continuity, then `notes` notes with a
    /// checkpoint after every third, then `tail` notes after the last
    /// checkpoint (unsigned until the next boot vouches for them).
    pub fn boot(&mut self, id: u8, notes: usize, tail: usize, text: &str) -> Boot {
        let boot_id = [id; 8];
        let mut c = Chain::new(boot_id, [0xF1; 32]);
        let mut counter = FileCounter::new();
        let mut out = Boot { bytes: Vec::new(), ends: Vec::new(), boot_id };
        let mut b = [0u8; format::MAX_RECORD];
        let mut t = 0u64;
        let ctx = |t: u64, base: Option<u64>| Context {
            uptime_ms: t,
            utc_ms: base.map(|u| u + t),
            time_source: if base.is_some() { format::TimeSource::Gps } else { format::TimeSource::None },
            time_age_ms: base.map(|_| 100),
            ..Context::default()
        };
        let keep = |c: &mut Chain, out: &mut Boot, n: usize, b: &[u8], counter: &mut FileCounter, store: &mut Mem| {
            out.bytes.extend_from_slice(&b[..n]);
            out.ends.push(out.bytes.len());
            c.commit();
            // Durable at once (the hub: after each flush).
            let _ = SlotCounter { slots: store, state: counter }.progress(c.seq(), c.head());
        };
        let bd = boot::Boot {
            key_class: self.signer.key_class() as u8,
            pubkey: self.signer.public_key(),
            chip: (1, 3),
            ext: Some(boot::Ext {
                built_unix_s: 1_763_000_000,
                git: [0x66; 20],
                git_state: boot::GIT_CLEAN,
                counter_source: boot::COUNTER_SD_FILE,
                boot_id_method: boot::BOOT_ID_MIXED,
            }),
        };
        let mut data = [0u8; boot::LEN];
        let dn = bd.encode(&mut data);
        let n = c.event(&ctx(t, self.utc_base), EventCode::Boot, b"hub-p4 test; bench: none", &data[..dn], &mut b).unwrap();
        keep(&mut c, &mut out, n, &b, &mut counter, &mut self.store);
        let k = SlotCounter { slots: &mut self.store, state: &mut counter }.begin(boot_id).unwrap().unwrap();
        t += 100;
        let n = c.event(&ctx(t, self.utc_base), EventCode::Continuity, b"boot counter", &k.encode(), &mut b).unwrap();
        keep(&mut c, &mut out, n, &b, &mut counter, &mut self.store);
        for i in 0..notes + tail {
            t += 1_000;
            let n = c.event(&ctx(t, self.utc_base), EventCode::Note, text.as_bytes(), &[], &mut b).unwrap();
            keep(&mut c, &mut out, n, &b, &mut counter, &mut self.store);
            if i < notes && i % 3 == 2 {
                t += 1_000;
                let n = c.checkpoint(&ctx(t, self.utc_base), CheckpointReason::Periodic, &mut self.signer, &mut b).unwrap();
                keep(&mut c, &mut out, n, &b, &mut counter, &mut self.store);
            }
        }
        if let Some(u) = self.utc_base.as_mut() {
            *u += t + 60_000;
        }
        out
    }
}
