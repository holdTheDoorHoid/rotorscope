//! The hotspot's DHCP pool, sized so edge-dhcp 0.8 can reuse expired leases.
//!
//! edge-dhcp's `Server<_, N>` keeps leases in an `N`-entry map. Handing out
//! an address (`available()`) first looks for one in the range that is not
//! in the map; only when *every* address in the range is in the map does it
//! look for an expired lease, remove it and offer that address. A `Request`
//! then stores the lease (`add_lease`), which fails when the map is full.
//!
//! The hub used to give 192.168.4.50-200 (151 addresses) with an 8-entry
//! map: after eight distinct phones the map was full, but 143 addresses
//! were still "not in the map", so the reclaim branch never ran. The ninth
//! phone was offered a fresh address, its Request failed to store, and it
//! got a NAK, until the hub was rebooted.
//!
//! With exactly `LEASES` addresses in the range, a full map means every
//! address is in it, so the reclaim branch runs and the oldest expired
//! lease is reused. Shorter leases make one free sooner after a phone
//! leaves (a phone renews at half the lease while it stays).

/// Lease table size (edge-dhcp's `N`). MAX_CLIENTS (4) phones joined at once
/// plus four that left recently.
pub const LEASES: usize = 8;
/// First host number handed out: 192.168.4.50.
pub const FIRST: u8 = 50;
/// Last host number handed out: `FIRST + LEASES - 1` = 192.168.4.57. The
/// range must hold exactly `LEASES` addresses (see the module docs).
pub const LAST: u8 = FIRST + LEASES as u8 - 1;
/// Lease length. Ten minutes: a phone that left without releasing frees its
/// address within ten minutes; one that stays renews every five.
pub const LEASE_SECS: u32 = 600;

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::Cell;
    use core::net::Ipv4Addr;
    use edge_dhcp::server::{Server, ServerOptions};
    use edge_dhcp::{DhcpOption, MessageType, Options, Packet};

    const IP: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);

    fn mac(n: u8) -> [u8; 6] {
        [0x02, 0, 0, 0, 0, n]
    }

    /// What the server answered: (message type, address).
    fn reply_of(p: &Packet<'_>) -> (Option<MessageType>, Ipv4Addr) {
        let t = p.options.iter().find_map(|o| if let DhcpOption::MessageType(t) = o { Some(t) } else { None });
        (t, p.yiaddr)
    }

    /// One phone joining: DISCOVER, then REQUEST for the offered address.
    /// Returns the ACKed address, or `None` (no offer, or a NAK).
    fn join<F: FnMut() -> u64, const N: usize>(server: &mut Server<F, N>, opts: &ServerOptions<'_>, m: [u8; 6]) -> Option<Ipv4Addr> {
        let mut obuf = Options::buf();
        let discover = Packet::new_request(m, 1, 0, None, true, Options::discover(None, &mut obuf));
        let mut rbuf = Options::buf();
        let offered = reply_of(&server.handle_request(&mut rbuf, opts, &discover)?);
        assert_eq!(offered.0, Some(MessageType::Offer));
        let mut obuf = Options::buf();
        let request = Packet::new_request(m, 2, 0, None, true, Options::request(offered.1, &mut obuf));
        let mut rbuf = Options::buf();
        let acked = reply_of(&server.handle_request(&mut rbuf, opts, &request)?);
        (acked.0 == Some(MessageType::Ack)).then_some(acked.1)
    }

    fn options(gw: &mut [Ipv4Addr; 1]) -> ServerOptions<'_> {
        let mut o = ServerOptions::new(IP, Some(gw));
        o.lease_duration_secs = LEASE_SECS;
        o
    }

    /// The hub's server: same type, table size, range and lease as
    /// `hotspot::dhcp` in the firmware.
    fn hub_server(clock: &Cell<u64>) -> Server<impl FnMut() -> u64 + '_, LEASES> {
        let mut s = Server::<_, LEASES>::new(move || clock.get(), IP);
        s.range_start = Ipv4Addr::new(192, 168, 4, FIRST);
        s.range_end = Ipv4Addr::new(192, 168, 4, LAST);
        s
    }

    #[test]
    fn range_holds_exactly_the_table() {
        assert_eq!(usize::from(LAST - FIRST) + 1, LEASES);
    }

    #[test]
    fn pool_recovers_after_more_phones_than_the_table_holds() {
        let clock = Cell::new(1_000);
        let mut gw = [Ipv4Addr::UNSPECIFIED];
        let opts = options(&mut gw);
        let mut server = hub_server(&clock);
        let mut seen = std::vec::Vec::new();
        for n in 0..LEASES as u8 {
            let a = join(&mut server, &opts, mac(n)).expect("served while the table has room");
            assert!(!seen.contains(&a));
            seen.push(a);
        }
        // Table full, nothing expired: the next phone must wait (no NAK
        // loop; simply no offer).
        assert_eq!(join(&mut server, &opts, mac(100)), None);
        // Past the lease: phone 9, 10, ... are served by reusing expired leases.
        clock.set(clock.get() + u64::from(LEASE_SECS) + 1);
        for n in 100..100 + 2 * LEASES as u8 {
            let a = join(&mut server, &opts, mac(n)).expect("served after the leases expired");
            assert!(a.octets()[3] >= FIRST && a.octets()[3] <= LAST);
            if n == 100 + LEASES as u8 - 1 {
                // All reused; let them expire again for the next round.
                clock.set(clock.get() + u64::from(LEASE_SECS) + 1);
            }
        }
    }

    /// The old configuration (edge-dhcp's default .50-.200 range, 8 leases,
    /// one hour): the ninth distinct phone is refused even hours later. Kept
    /// as a record of the defect the pool size fixes.
    #[test]
    fn old_configuration_never_recovers() {
        let clock = Cell::new(1_000);
        let mut gw = [Ipv4Addr::UNSPECIFIED];
        let mut opts = options(&mut gw);
        opts.lease_duration_secs = 3600;
        let mut server = Server::<_, 8>::new(|| clock.get(), IP); // .50-.200
        for n in 0..8 {
            assert!(join(&mut server, &opts, mac(n)).is_some());
        }
        clock.set(clock.get() + 10 * 3600);
        assert_eq!(join(&mut server, &opts, mac(100)), None, "ninth phone NAKed until reboot");
    }
}
