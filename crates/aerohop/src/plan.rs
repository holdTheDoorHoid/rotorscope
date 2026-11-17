//! OGN / FLARM frequency plans and the hop channel, ported from the OGN
//! tracker's `freqplan.h` (class `FreqPlan`, as carried in SoftRF
//! `libraries/OGN/freqplan.h`, GPLv3). Only the FLARM ("Legacy") / OGNTP
//! branch of `setPlan` is ported: that is the one that hops.

/// A frequency plan region. The numbers are SoftRF's `RF_BAND_*`, which the
/// link protocol uses as well (`link::AeroMode::FlarmHop::band`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Band {
    /// 868.2 / 868.4 MHz, no hopping (slot picks one of two channels).
    Eu = 1,
    /// 902.2-927.8 MHz, 65 channels of 400 kHz, hopping.
    Us = 2,
    /// Australia and South America: 917.0-926.2 MHz, 24 channels, hopping.
    Au = 3,
    /// New Zealand: 869.25 MHz, single channel.
    Nz = 4,
    /// Russia: 868.8 MHz, single channel.
    Ru = 5,
    /// China: 470.1 MHz, single channel.
    Cn = 6,
    /// PilotAware UK: deprecated in SoftRF, treated as Europe's frequencies
    /// (but with its own plan number, which matters to `getChannel`).
    Uk = 7,
    /// India: 866.0 MHz, single channel.
    In = 8,
    /// Israel: 916.2 MHz, single channel.
    Il = 9,
    /// South Korea: 920.9 MHz, single channel.
    Kr = 10,
}

impl Band {
    pub const ALL: [Band; 10] = [Band::Eu, Band::Us, Band::Au, Band::Nz, Band::Ru, Band::Cn, Band::Uk, Band::In, Band::Il, Band::Kr];

    /// From SoftRF's `RF_BAND_*` number. 0 ("auto", deprecated) is Europe,
    /// as in SoftRF; 11 (reserved, 2.4 GHz) and above are refused.
    pub const fn from_number(n: u8) -> Option<Band> {
        Some(match n {
            0 | 1 => Band::Eu,
            2 => Band::Us,
            3 => Band::Au,
            4 => Band::Nz,
            5 => Band::Ru,
            6 => Band::Cn,
            7 => Band::Uk,
            8 => Band::In,
            9 => Band::Il,
            10 => Band::Kr,
            _ => return None,
        })
    }

    pub const fn name(self) -> &'static str {
        match self {
            Band::Eu => "Europe/Africa",
            Band::Us => "USA/Canada",
            Band::Au => "Australia/South America",
            Band::Nz => "New Zealand",
            Band::Ru => "Russia",
            Band::Cn => "China",
            Band::Uk => "PilotAware (UK)",
            Band::In => "India",
            Band::Il => "Israel",
            Band::Kr => "South Korea",
        }
    }

    /// The FLARM / OGNTP plan for this band (`setPlan(Plan, RF_PROTOCOL_LEGACY)`).
    pub const fn plan(self) -> FreqPlan {
        let (base_hz, sep_hz, channels) = match self {
            Band::Us => (902_200_000, 400_000, 65),
            Band::Au => (917_000_000, 400_000, 24),
            Band::Nz => (869_250_000, 200_000, 1),
            Band::Ru => (868_800_000, 200_000, 1),
            Band::Cn => (470_100_000, 200_000, 1),
            Band::In => (866_000_000, 200_000, 1),
            Band::Il => (916_200_000, 200_000, 1),
            Band::Kr => (920_900_000, 200_000, 1),
            Band::Eu | Band::Uk => (868_200_000, 200_000, 2),
        };
        FreqPlan { plan: self as u8, base_hz, sep_hz, channels }
    }
}

/// One plan: `BaseFreq`, `ChanSepar`, `Channels` and the plan number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreqPlan {
    /// SoftRF's plan number (`Plan`); `getChannel` hops when it is 2 or more.
    pub plan: u8,
    pub base_hz: u32,
    pub sep_hz: u32,
    pub channels: u8,
}

/// The OGN tracker's `FreqHopHash`, bit for bit (all arithmetic on 32-bit
/// unsigned values, wrapping).
pub const fn freq_hop_hash(time: u32) -> u32 {
    let mut t = (time << 15).wrapping_add(!time);
    t ^= t >> 12;
    t = t.wrapping_add(t << 2);
    t ^= t >> 4;
    t = t.wrapping_mul(2057);
    t ^ (t >> 16)
}

impl FreqPlan {
    /// `getChannel(Time, Slot, OGN)`: the channel for UTC second `time`
    /// (Unix seconds) and `slot` (0 or 1). `ogn` false = FLARM's channel.
    pub const fn channel(&self, time: u32, slot: u8, ogn: bool) -> u8 {
        if self.channels <= 1 {
            return 0;
        }
        let slot = slot & 1;
        if self.plan >= 2 {
            // `uint8_t Channel = FreqHopHash((Time<<1)+Slot) % Channels;`
            let mut channel = (freq_hop_hash((time << 1).wrapping_add(slot as u32)) % self.channels as u32) as u8;
            if ogn {
                if slot != 0 {
                    // 2nd slot: FLARM's channel of the 1st slot, unless that
                    // is where FLARM is now; then the next one up.
                    let channel2 = (freq_hop_hash(time << 1) % self.channels as u32) as u8;
                    if channel2 == channel {
                        channel += 1;
                        if channel >= self.channels {
                            channel -= 2;
                        }
                    } else {
                        channel = channel2;
                    }
                } else {
                    // 1st slot: one above FLARM's (one below at the top).
                    channel += 1;
                    if channel >= self.channels {
                        channel -= 2;
                    }
                }
            }
            return channel;
        }
        // Europe: 0 or 1.
        slot ^ (ogn as u8)
    }

    /// `getChanFrequency`.
    pub const fn channel_hz(&self, channel: u8) -> u32 {
        self.base_hz + self.sep_hz * channel as u32
    }

    /// `getFrequency(Time, Slot, OGN)`.
    pub const fn frequency_hz(&self, time: u32, slot: u8, ogn: bool) -> u32 {
        self.channel_hz(self.channel(time, slot, ogn))
    }

    /// Whether this plan hops at all (more than one channel to choose from
    /// by time). With one channel there is nothing to follow.
    pub const fn hops(&self) -> bool {
        self.channels > 1
    }

    /// The channel to park on when UTC is unknown. Any channel is as good
    /// as another over time (the hash spreads evenly); the middle one is
    /// chosen so that a wrong-band antenna loses least.
    pub const fn park_channel(&self) -> u8 {
        self.channels / 2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values worked by hand from the C (and cross-checked with an
    /// independent script written for this port, not the SoftRF build).
    #[test]
    fn hash_known_values() {
        // FreqHopHash(0): (0<<15) + ~0 = 0xFFFFFFFF; ^>>12 = 0xFFF00000;
        // += <<2 = 0xFFB00000; ^>>4 = 0xF04B0000; *2057 = 0xCAA30000
        // (mod 2^32); ^>>16 = 0xCAA3CAA3.
        assert_eq!(freq_hop_hash(0), 0xCAA3_CAA3);
        assert_eq!(freq_hop_hash(1), 0x12D6_0BF6);
        assert_eq!(freq_hop_hash(2), 0x25AC_1FE5);
        assert_eq!(freq_hop_hash(1000), 0x4F23_8DF7);
        assert_eq!(freq_hop_hash(1_700_000_000), 0x70CD_3FE5);
        assert_eq!(freq_hop_hash(u32::MAX), 0xBD55_FC18);
    }

    #[test]
    fn hand_worked_steps_of_hash_zero() {
        let mut t: u32 = 0u32.wrapping_add(!0u32);
        assert_eq!(t, 0xFFFF_FFFF);
        t ^= t >> 12;
        assert_eq!(t, 0xFFF0_0000);
        t = t.wrapping_add(t << 2);
        assert_eq!(t, 0xFFB0_0000);
        t ^= t >> 4;
        assert_eq!(t, 0xF04B_0000);
        t = t.wrapping_mul(2057);
        assert_eq!(t, 0xCAA3_0000);
    }

    /// `[flarm slot0, ogn slot0, flarm slot1, ogn slot1]` for the US and
    /// Australian plans. US, time 0: hash(0) % 65 = 0xCAA3CAA3 % 65 = 20,
    /// so FLARM slot 0 is channel 20 (910.2 MHz) and OGN slot 0 is 21;
    /// hash(1) % 65 = 4 is FLARM slot 1, and OGN slot 1 takes FLARM's slot-0
    /// channel 20.
    #[test]
    fn channels_known_values() {
        let us = Band::Us.plan();
        let au = Band::Au.plan();
        let row = |p: &FreqPlan, t: u32| [p.channel(t, 0, false), p.channel(t, 0, true), p.channel(t, 1, false), p.channel(t, 1, true)];
        assert_eq!(0xCAA3_CAA3u32 % 65, 20);
        assert_eq!(row(&us, 0), [20, 21, 4, 20]);
        assert_eq!(row(&us, 1), [34, 35, 14, 34]);
        assert_eq!(row(&us, 2), [28, 29, 26, 28]);
        assert_eq!(row(&us, 1000), [34, 35, 35, 34]);
        assert_eq!(row(&us, 1_700_000_000), [64, 63, 61, 64]);
        assert_eq!(row(&us, 1_763_337_600), [36, 37, 35, 36]);
        assert_eq!(row(&us, u32::MAX), [52, 53, 45, 52]);
        assert_eq!(row(&au, 0), [19, 20, 6, 19]);
        assert_eq!(row(&au, 1_700_000_000), [22, 23, 6, 22]);
        assert_eq!(us.frequency_hz(0, 0, false), 910_200_000);
        assert_eq!(us.channel_hz(64), 927_800_000);
    }

    /// Time 1000, slot 1: FLARM's slot-1 channel (35) equals OGN's
    /// would-be pick (FLARM's slot-0 channel is 34, so no clash): OGN takes 34.
    /// Time 1700000000, slot 0: FLARM is on the top channel 64, so OGN goes
    /// one below (63) instead of off the end.
    #[test]
    fn ogn_edge_rules() {
        let us = Band::Us.plan();
        assert_eq!(us.channel(1_700_000_000, 0, false), 64);
        assert_eq!(us.channel(1_700_000_000, 0, true), 63);
        // A second where FLARM's two slots coincide: OGN slot 1 moves up one.
        let t = (0..100_000u32).find(|&t| us.channel(t, 0, false) == us.channel(t, 1, false)).unwrap();
        let f = us.channel(t, 1, false);
        let expect = if f + 1 >= 65 { f - 1 } else { f + 1 };
        assert_eq!(us.channel(t, 1, true), expect);
    }

    #[test]
    fn ogn_never_on_flarms_channel_in_the_same_slot() {
        for p in [Band::Us.plan(), Band::Au.plan()] {
            for t in 1_763_337_600..1_763_337_600 + 86_400 {
                for s in 0..2 {
                    assert_ne!(p.channel(t, s, false), p.channel(t, s, true), "t {t} s {s}");
                }
            }
        }
    }

    #[test]
    fn us_plan_spreads_over_all_65_channels() {
        let us = Band::Us.plan();
        let mut n = [0u32; 65];
        for t in 1_763_337_600..1_763_337_600 + 86_400 {
            for s in 0..2 {
                n[us.channel(t, s, false) as usize] += 1;
            }
        }
        // 172800 picks over 65 channels: 2658 each on average; the
        // independent script finds 2549..2798.
        assert_eq!(*n.iter().min().unwrap(), 2549);
        assert_eq!(*n.iter().max().unwrap(), 2798);
    }

    #[test]
    fn europe_and_single_channel_plans() {
        let eu = Band::Eu.plan();
        assert_eq!([eu.channel(5, 0, false), eu.channel(5, 1, false), eu.channel(5, 0, true), eu.channel(5, 1, true)], [0, 1, 1, 0]);
        assert_eq!(eu.frequency_hz(5, 1, false), 868_400_000);
        assert!(!Band::Nz.plan().hops());
        assert_eq!(Band::Nz.plan().frequency_hz(123, 1, true), 869_250_000);
        // UK: Europe's frequencies but plan number 7, so the C hashes.
        let uk = Band::Uk.plan();
        assert_eq!(uk.channel(0, 0, false), (0xCAA3_CAA3u32 % 2) as u8);
        assert_eq!(Band::from_number(0), Some(Band::Eu));
        assert_eq!(Band::from_number(11), None);
        for b in Band::ALL {
            assert_eq!(Band::from_number(b as u8), Some(b));
            let p = b.plan();
            assert!(p.channel_hz(p.channels - 1) < 1_000_000_000, "{}", b.name());
        }
    }
}
