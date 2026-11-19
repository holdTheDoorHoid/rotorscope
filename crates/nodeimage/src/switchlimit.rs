//! How often a node may switch images (design WP-13, section 7.1).
//!
//! Every switch erases `rsjob`, writes `otadata` and reboots, so a stream of
//! role changes would be a reboot loop that wears the flash. The node limits
//! itself, whatever the hub asks: a switch is allowed only once the running
//! image has been up for [`min_uptime_s`]`(streak)`, which doubles with
//! every quick switch in a row (60 s, 2 min, 4 min, ... capped at 1 h).
//!
//! `streak` travels in the job record ([`crate::encode_job`]): the switching
//! image writes [`next_streak`], the image it starts reads it back.
//!
//! When the streak resets. The design's words are "streak + 1 if it ran less
//! than 10 minutes, else 0". Read literally, the cap could never be reached:
//! from streak 4 on the wait itself (16 min) is longer than 10 minutes, so
//! every switch at streak 4 would reset to 0, and a steady storm would get
//! about 230 switches a day instead of the design's "about 24 a day at the
//! cap". So the 10 minutes are counted from the moment the switch became
//! allowed: an image that sat [`QUIET_S`] past its own wait without being
//! asked to switch resets the streak; one asked as soon as it may continues
//! it. With that, a storm settles at one switch an hour (24 a day), and a
//! node that is left alone for 10 minutes is back to the 60 s wait.
//!
//! Pure functions, host-tested; the flash and clock glue is
//! `firmware/node-common` (`Images::switch`).

/// Shortest uptime before a switch, with no streak.
pub const BASE_S: u32 = 60;
/// Longest uptime ever required.
pub const CAP_S: u32 = 3600;
/// Up this long past its own wait, an image resets the streak.
pub const QUIET_S: u32 = 600;

/// Uptime the running image needs before it may switch, given the streak
/// it was started with: 60 s x 2^streak, at most 1 h.
pub const fn min_uptime_s(streak: u8) -> u32 {
    // 60 << 6 = 3840 is already past the cap; never shift further.
    let shift = if streak > 6 { 6 } else { streak };
    let t = BASE_S << shift;
    if t > CAP_S { CAP_S } else { t }
}

/// Seconds until a switch is allowed (0: allowed now).
pub const fn wait_s(streak: u8, uptime_s: u32) -> u32 {
    min_uptime_s(streak).saturating_sub(uptime_s)
}

/// Whether a switch is allowed now.
pub const fn allowed(streak: u8, uptime_s: u32) -> bool {
    wait_s(streak, uptime_s) == 0
}

/// The streak the switching image writes into the job record: one more if
/// it switches within [`QUIET_S`] of being allowed to, else 0. Saturates
/// (the wait is capped long before).
pub const fn next_streak(streak: u8, uptime_s: u32) -> u8 {
    if uptime_s < min_uptime_s(streak).saturating_add(QUIET_S) { streak.saturating_add(1) } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_backoff_doubles_and_caps() {
        let want = [60, 120, 240, 480, 960, 1920, 3600, 3600, 3600];
        for (s, &w) in want.iter().enumerate() {
            assert_eq!(min_uptime_s(s as u8), w, "streak {s}");
        }
        assert_eq!(min_uptime_s(u8::MAX), CAP_S);
        assert!(!allowed(0, 59));
        assert!(allowed(0, 60));
        assert_eq!(wait_s(2, 100), 140);
        assert_eq!(wait_s(2, 1000), 0);
        assert_eq!(next_streak(u8::MAX, 0), u8::MAX, "saturates");
    }

    #[test]
    fn streak_resets_after_ten_quiet_minutes() {
        // Switched as soon as allowed: the streak grows.
        assert_eq!(next_streak(0, 60), 1);
        assert_eq!(next_streak(4, 960), 5);
        assert_eq!(next_streak(9, 3600), 10);
        // Up 10 minutes past its own wait: back to 0.
        assert_eq!(next_streak(0, 60 + 599), 1);
        assert_eq!(next_streak(0, 60 + 600), 0);
        assert_eq!(next_streak(6, 3600 + 600), 0);
        assert_eq!(next_streak(6, 3600 + 599), 7);
        // And a fresh start waits the base minute again.
        assert_eq!(min_uptime_s(next_streak(8, 5000)), 60);
    }

    /// The hub (or a forger with a node's key) asks for a switch every 5 s,
    /// forever. Each image switches the moment it is allowed.
    #[test]
    fn storm_settles_at_about_24_switches_a_day() {
        let (mut t, mut streak, mut up) = (0u64, 0u8, 0u32);
        let mut at: std::vec::Vec<u64> = std::vec::Vec::new();
        while t < 3 * 86_400 {
            t += 5;
            up += 5;
            if allowed(streak, up) {
                streak = next_streak(streak, up);
                at.push(t);
                up = 0; // the next image boots
            }
        }
        let day = |d: u64| at.iter().filter(|&&s| s >= d * 86_400 && s < (d + 1) * 86_400).count();
        // First day: the climb (60 s .. 32 min) then hourly.
        assert!(day(0) <= 30, "day 0: {}", day(0));
        assert!((23..=25).contains(&day(1)), "day 1: {}", day(1));
        assert!((23..=25).contains(&day(2)), "day 2: {}", day(2));
        // Never two switches less than a minute apart.
        assert!(at.windows(2).all(|w| w[1] - w[0] >= 60));
    }
}
