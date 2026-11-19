//! What the uploader sends, decided from the records it holds (testable
//! without a server or a hub).
//!
//! - **Signed batches**: from the first record the server lacks, runs that
//!   end on a signed checkpoint, each at most a size limit.
//! - **A boot's tail**: records after a boot's last checkpoint are sent once
//!   a later boot's `continuity` record (which that boot signs) names their
//!   last record by number and hash; the server takes them only then.

use std::collections::BTreeMap;

use evidence::continuity::Continuity;
use evidence::format::{self, Body, EventCode, Kind};

/// Records held, per boot, by sequence number.
pub type Chains = BTreeMap<[u8; 8], BTreeMap<u64, Vec<u8>>>;

/// Records the uploader will hold in memory at most (a hub streaming for
/// days, or a peer sending endless records, stops being collected here).
pub const MAX_HELD_BYTES: usize = 512 << 20;

fn is_checkpoint(r: &[u8]) -> bool {
    format::parse(r).is_ok_and(|p| p.header.kind == Kind::Checkpoint)
}

/// Runs `(first, last)` from `start`, contiguous in `recs`, each ending on a
/// checkpoint and at most `max_bytes` long (one checkpoint-to-checkpoint
/// stretch longer than that is sent alone; the server will refuse it).
pub fn signed_batches(recs: &BTreeMap<u64, Vec<u8>>, start: u64, max_bytes: usize) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut first = start;
    let mut bytes = 0usize;
    let mut last_cp: Option<u64> = None;
    let mut bytes_at_cp = 0usize;
    let mut s = start;
    while let Some(r) = recs.get(&s) {
        bytes += r.len();
        if is_checkpoint(r) {
            if bytes > max_bytes
                && let Some(cp) = last_cp.filter(|&cp| cp >= first)
            {
                out.push((first, cp));
                first = cp + 1;
                bytes -= bytes_at_cp;
            }
            last_cp = Some(s);
            bytes_at_cp = bytes;
        }
        match s.checked_add(1) {
            Some(n) => s = n,
            None => break,
        }
    }
    if let Some(cp) = last_cp.filter(|&cp| cp >= first) {
        out.push((first, cp));
    }
    out
}

/// The last record of `boot` that a held `continuity` record of another
/// boot names, with the hash it names: the furthest one if several do.
pub fn covered_through(chains: &Chains, boot: &[u8; 8]) -> Option<(u64, [u8; 32])> {
    let mut best: Option<(u64, [u8; 32])> = None;
    for (b, recs) in chains {
        if b == boot {
            continue;
        }
        for r in recs.values() {
            let Ok(p) = format::parse(r) else { continue };
            let Body::Event(e) = p.body else { continue };
            if e.code != EventCode::Continuity as u16 {
                continue;
            }
            let Some(k) = Continuity::decode(e.data) else { continue };
            if !k.has_prev() || k.prev_boot != *boot || k.prev_records == 0 {
                continue;
            }
            let last = k.prev_records - 1;
            if best.is_none_or(|(l, _)| last > l) {
                best = Some((last, k.prev_head));
            }
        }
    }
    best
}

/// The tail to send for one boot: `start..=last` when a continuity record
/// covers through `last`, every record in between is held, and the held
/// record `last` is the one named. `None` when there is nothing to send.
pub fn tail(recs: &BTreeMap<u64, Vec<u8>>, start: u64, cover: Option<(u64, [u8; 32])>) -> Option<(u64, u64)> {
    let (last, head) = cover?;
    if last < start {
        return None;
    }
    if (start..=last).any(|s| !recs.contains_key(&s)) {
        return None;
    }
    (evidence::sha256(&recs[&last]) == head).then_some((start, last))
}

#[cfg(test)]
mod tests {
    use super::*;
    use evidence::format::CheckpointReason;
    use evidence::sign::SoftSigner;
    use evidence::writer::{Chain, Context};

    /// One boot: `n` notes with a checkpoint after every `every`, then
    /// `tail` notes; optionally a continuity record naming another boot.
    fn boot(id: u8, n: usize, every: usize, tail: usize, names: Option<([u8; 8], u64, [u8; 32])>) -> BTreeMap<u64, Vec<u8>> {
        let mut c = Chain::new([id; 8], [0; 32]);
        let mut s = SoftSigner::public_test();
        let mut b = [0u8; format::MAX_RECORD];
        let mut out = BTreeMap::new();
        let ctx = Context::default();
        let mut keep = |c: &mut Chain, k: usize, b: &[u8]| {
            out.insert(c.seq(), b[..k].to_vec());
            c.commit();
        };
        if let Some((pb, pn, ph)) = names {
            let k = Continuity { source: 1, status: 0, counter: 2, prev_boot: pb, prev_records: pn, prev_head: ph };
            let m = c.event(&ctx, EventCode::Continuity, b"boot counter", &k.encode(), &mut b).unwrap();
            keep(&mut c, m, &b);
        }
        for i in 0..n + tail {
            let m = c.event(&ctx, EventCode::Note, &[b'x'; 200], &[], &mut b).unwrap();
            keep(&mut c, m, &b);
            if i < n && i % every == every - 1 {
                let m = c.checkpoint(&ctx, CheckpointReason::Periodic, &mut s, &mut b).unwrap();
                keep(&mut c, m, &b);
            }
        }
        out
    }

    #[test]
    fn batches_end_on_checkpoints_and_respect_the_size_cap() {
        // Notes #0..#2, checkpoint #3, notes #4..#6, checkpoint #7, ...
        let recs = boot(1, 12, 3, 2, None);
        let last_cp = recs.iter().filter(|(_, r)| is_checkpoint(r)).map(|(s, _)| *s).max().unwrap();
        assert_eq!(signed_batches(&recs, 0, usize::MAX), vec![(0, last_cp)]);
        // Small cap: one checkpoint stretch per batch.
        let small = signed_batches(&recs, 0, 2000);
        assert_eq!(small.first(), Some(&(0, 3)));
        assert_eq!(small.last().unwrap().1, last_cp);
        for w in small.windows(2) {
            assert_eq!(w[0].1 + 1, w[1].0);
        }
        for (a, b) in &small {
            assert!(is_checkpoint(&recs[b]));
            let n: usize = (*a..=*b).map(|s| recs[&s].len()).sum();
            assert!(n <= 2000, "{a}..{b}: {n}");
        }
        // From the middle, and nothing after the last checkpoint.
        assert_eq!(signed_batches(&recs, 4, usize::MAX), vec![(4, last_cp)]);
        assert!(signed_batches(&recs, last_cp + 1, usize::MAX).is_empty());
        // A hole stops the run.
        let mut holed = recs.clone();
        holed.remove(&5);
        assert_eq!(signed_batches(&holed, 0, usize::MAX), vec![(0, 3)]);
    }

    #[test]
    fn a_tail_is_sent_only_when_a_continuity_record_names_it() {
        let a = boot(1, 6, 3, 2, None);
        let last = *a.keys().next_back().unwrap();
        let head = evidence::sha256(&a[&last]);
        let mut chains: Chains = BTreeMap::new();
        chains.insert([1; 8], a.clone());
        assert_eq!(covered_through(&chains, &[1; 8]), None);
        assert_eq!(tail(&a, 8, covered_through(&chains, &[1; 8])), None);

        chains.insert([2; 8], boot(2, 3, 3, 0, Some(([1; 8], last + 1, head))));
        let cover = covered_through(&chains, &[1; 8]);
        assert_eq!(cover, Some((last, head)));
        assert_eq!(tail(&a, 8, cover), Some((8, last)));
        // Already sent, or a record missing in between: nothing.
        assert_eq!(tail(&a, last + 1, cover), None);
        let mut holed = a.clone();
        holed.remove(&(last - 1));
        assert_eq!(tail(&holed, 8, cover), None);
        // A held record that is not the one named: nothing.
        assert_eq!(tail(&a, 8, Some((last, [9; 32]))), None);
        // A boot's own records never cover it.
        let mut own: Chains = BTreeMap::new();
        own.insert([1; 8], boot(1, 3, 3, 0, Some(([1; 8], 2, [0; 32]))));
        assert_eq!(covered_through(&own, &[1; 8]), None);
    }
}
