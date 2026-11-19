//! The boot id (WP-14a, HB-16): eight bytes that name one boot's chain.
//!
//! It must differ between boots, or two boots' records would claim to be one
//! chain. On the ESP32-P4 the hardware random number generator is only
//! guaranteed to be truly random while a radio or an ADC feeds it noise
//! (Espressif's documentation; esp-hal lists the P4's true-RNG mode as not
//! supported), and the P4 has no radio. Unproven entropy is not trusted
//! alone: the hub mixes several independent inputs with SHA-256
//! ([`Mixer`]), each labelled and length-prefixed, and takes the first 8
//! bytes. Any one input that differs between boots makes the id differ.
//!
//! Inputs the hub uses (boot event data byte 98 = 1, "mixed"): 64 bytes from
//! the hardware RNG read in 16 pieces, the CPU cycle counter and the system
//! timer sampled between the pieces (timing jitter), the time from power-on
//! to here (it includes PSRAM, I2C and panel start-up, whose timing varies),
//! the reset cause, and the chip's unique ID and base MAC (so two units
//! never share ids even if every other input repeated). The boot counter
//! (`continuity`) is read later, at the first card mount, so it cannot be an
//! input; it numbers the boots instead.

/// Domain separation: changes if the construction ever changes.
const DOMAIN: &[u8] = b"rotorscope boot id v1";

/// Collects the inputs. Each `add` is labelled and length-prefixed, so no
/// two different sequences of inputs hash alike.
pub struct Mixer(crate::Sha256Stream);

impl Default for Mixer {
    fn default() -> Self {
        Self::new()
    }
}

impl Mixer {
    pub fn new() -> Self {
        let mut h = crate::Sha256Stream::new();
        h.update(&(DOMAIN.len() as u32).to_le_bytes());
        h.update(DOMAIN);
        Self(h)
    }

    pub fn add(&mut self, label: &[u8], data: &[u8]) {
        self.0.update(&(label.len() as u32).to_le_bytes());
        self.0.update(label);
        self.0.update(&(data.len() as u32).to_le_bytes());
        self.0.update(data);
    }

    pub fn add_u64(&mut self, label: &[u8], v: u64) {
        self.add(label, &v.to_le_bytes());
    }

    /// The boot id.
    pub fn finish(self) -> [u8; 8] {
        let h = self.0.finish();
        h[..8].try_into().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(inputs: &[(&[u8], &[u8])]) -> [u8; 8] {
        let mut m = Mixer::new();
        for (l, d) in inputs {
            m.add(l, d);
        }
        m.finish()
    }

    #[test]
    fn same_inputs_same_id_and_any_change_differs() {
        let base: [(&[u8], &[u8]); 3] = [(b"rng", &[1, 2, 3, 4]), (b"jitter", &[9; 8]), (b"unit", &[7; 16])];
        assert_eq!(id(&base), id(&base));
        // A repeating RNG (pseudo-random from the same state every boot) is
        // rescued by any other input that moves.
        let mut moved = base;
        moved[1] = (b"jitter", &[9, 9, 9, 9, 9, 9, 9, 8]);
        assert_ne!(id(&base), id(&moved));
        // Two units with identical everything else differ by their unit id.
        let mut other = base;
        other[2] = (b"unit", &[6; 16]);
        assert_ne!(id(&base), id(&other));
    }

    #[test]
    fn inputs_are_not_ambiguous() {
        // Moving a byte from one input to the next must change the id.
        assert_ne!(id(&[(b"a", &[1, 2]), (b"b", &[3])]), id(&[(b"a", &[1]), (b"b", &[2, 3])]));
        assert_ne!(id(&[(b"ab", &[1])]), id(&[(b"a", &[b'b', 1])]));
    }
}
