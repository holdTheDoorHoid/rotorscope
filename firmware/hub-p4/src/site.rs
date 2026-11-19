//! Node positions and muted alarms (`tracker::site`): from the SD card's
//! `ROTORSCP/SITE.TXT` at mount, from the console (`L`, one line), and from
//! the settings page. A change typed on the console or made on the screen
//! ("Nodes at hub") is appended to the file and logged as an evidence
//! Setting event, so it survives a reboot and leaves a record.
//!
//! **Loading the file is logged too** (WP-11b): a swapped or edited card
//! must not silence alarms or move the node positions the spoof checks
//! use without a trace. Every load writes one Setting event with the
//! file's SHA-256 and what it sets, then one per mute rule and node
//! position that differs from what the hub was using (at most
//! `tracker::site::MAX_CHANGE_EVENTS`, then one event counting the rest).
//!
//! **The screen's Mute / Snooze** silences specific alarms for a time
//! (`tracker::threats`): logged, never written to the file, gone at reboot.
//! Snooze also snoozes the alerts of drones at Warning or above (threat
//! levels, `tracker::behaviour`), for the same five minutes.
//!
//! **Places to protect** (`area`, `bubble`, `friend` lines) live in the
//! tracker (`Tracker::places`), like the mute rules. A load logs each
//! change to them (a friend lowers alarms, so a swapped card adding one
//! must leave a trace). `area here R` (console only) becomes an ordinary
//! `area here circle LAT LON R` line at the hub's position before it is
//! applied and saved.

use core::fmt::Write;

use tracker::site::{self, SiteLine, SiteState};

use crate::app::App;
use crate::evlog::Evidence;
use crate::links::GatewayLink;
use crate::sd::Logger;

/// What the hub keeps of the site file itself: node positions, the
/// placed home and the control-link switches. The mute rules and the
/// places live in the tracker, so this is not a whole `SiteState` (which
/// would put ~3.5 KB of mutes and places a second time on the main stack).
#[derive(Default)]
struct Kept {
    nodes: heapless::Vec<([u8; 6], f64, f64), { site::MAX_PLACED }>,
    control_ids: Option<bool>,
    control_content: Option<bool>,
    home: Option<(f64, f64, Option<f64>)>,
}

impl Kept {
    fn place(&mut self, mac: [u8; 6], lat: f64, lon: f64) {
        self.nodes.retain(|n| n.0 != mac);
        if self.nodes.is_full() {
            self.nodes.remove(0);
        }
        let _ = self.nodes.push((mac, lat, lon));
    }
}

#[derive(Default)]
pub struct Site {
    /// Node positions, home, switches (the mute rules and the places to
    /// protect live in the tracker).
    st: Kept,
    /// The placed hub home changed (file load or an operator line): the
    /// main loop hands it to the GPS checks (`take_home`). `Some(None)`:
    /// cleared.
    home_changed: Option<Option<(f64, f64, Option<f64>)>>,
}

impl Site {
    /// The control-link opt-ins (read IDs, decode content): what the file
    /// and the operator's lines last said; off when never said.
    pub fn control(&self) -> (bool, bool) {
        (self.st.control_ids.unwrap_or(false), self.st.control_content.unwrap_or(false))
    }

    /// The placed hub home (`home` line), if it changed since the last call.
    pub fn take_home(&mut self) -> Option<Option<(f64, f64, Option<f64>)>> {
        self.home_changed.take()
    }
}

/// Where a change came from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum From {
    Console,
    Screen,
}

/// What the card's file was, as read at mount (`sd.rs`).
pub struct FileFacts {
    pub sha256: [u8; 32],
    pub bytes: u32,
    pub lines: u32,
}

impl From {
    fn name(self) -> &'static str {
        match self {
            From::Console => "console",
            From::Screen => "screen",
        }
    }
}

impl Site {
    fn place(&mut self, mac: [u8; 6], lat: f64, lon: f64) {
        self.st.place(mac, lat, lon);
    }

    /// Once a second: tell the tracker where each node it knows stands
    /// (the tracker numbers relayed nodes from 1, in gateway order).
    pub fn apply(&self, app: &mut App, gw: &GatewayLink<'_>) {
        for (i, n) in gw.nodes.iter().enumerate() {
            let node = i as u8 + 1;
            match self.st.nodes.iter().find(|p| p.0 == n.mac) {
                Some(&(_, lat, lon)) => app.tracker.set_node_position(node, lat, lon),
                None => app.tracker.clear_node_position(node),
            }
        }
    }

    /// What the card's file said at mount: logged (hash, summary, every
    /// change from what the hub was using), then applied.
    #[inline(never)]
    pub fn load(&mut self, st: &SiteState, facts: &FileFacts, app: &mut App, ev: &mut Evidence, now: u64) {
        let sum = site::LoadSummary { st, sha256: &facts.sha256, bytes: facts.bytes, lines: facts.lines };
        let mut t: heapless::String<384> = heapless::String::new();
        let _ = write!(t, "{sum}");
        ev.event(evidence::EventCode::Setting, t.as_bytes(), &sum.data());
        crate::println!("hub-p4: {t}");
        let mut logged = 0usize;
        site::changes_since(app.tracker.mutes(), &self.st.nodes, st, |c| {
            if logged < site::MAX_CHANGE_EVENTS {
                let mut t: heapless::String<256> = heapless::String::new();
                let _ = site::write_change(&mut t, &c);
                ev.event(evidence::EventCode::Setting, t.as_bytes(), &[3]);
                crate::println!("hub-p4: {t}");
            }
            logged += 1;
        });
        // Areas, the bubble and friends: each change, within the same bound.
        site::place_changes(app.tracker.places(), &st.places, |c| {
            if logged < site::MAX_CHANGE_EVENTS {
                let mut t: heapless::String<256> = heapless::String::new();
                let _ = site::write_place_change(&mut t, &c);
                ev.event(evidence::EventCode::Setting, t.as_bytes(), &[3]);
                crate::println!("hub-p4: {t}");
            }
            logged += 1;
        });
        let n = logged;
        if n > site::MAX_CHANGE_EVENTS {
            let mut t: heapless::String<96> = heapless::String::new();
            let _ = write!(t, "site file changed: {} more changes not logged one by one", n - site::MAX_CHANGE_EVENTS);
            ev.event(evidence::EventCode::Setting, t.as_bytes(), &[3]);
            crate::println!("hub-p4: {t}");
        }
        app.tracker.set_places(&st.places);
        let refused = app.tracker.set_site_mutes(&st.mutes, now);
        if refused > 0 {
            crate::println!("hub-p4: site file: {refused} mute rules refused by the tracker");
        }
        if st.home != self.st.home {
            let mut t: heapless::String<128> = heapless::String::new();
            let _ = match st.home {
                Some((lat, lon, _)) => write!(t, "site file changed: hub home placed at {lat:.7} {lon:.7}; GPS spoofing checks use it"),
                None => write!(t, "site file changed: placed hub home cleared"),
            };
            ev.event(evidence::EventCode::Setting, t.as_bytes(), &[3]);
            crate::println!("hub-p4: {t}");
            self.home_changed = Some(st.home);
        }
        self.st.home = st.home;
        self.st.nodes = st.nodes.clone();
        self.st.control_ids = st.control_ids;
        self.st.control_content = st.control_content;
    }

    fn apply_line(&mut self, l: SiteLine, app: &mut App, now: u64) {
        match l {
            SiteLine::NodeAt { mac, lat, lon } => self.place(mac, lat, lon),
            SiteLine::NodeClear { mac } => self.st.nodes.retain(|n| n.0 != mac),
            SiteLine::Mute(m) => {
                if let Err(e) = app.tracker.mute(m, now) {
                    crate::println!("hub-p4: site line refused: {e}");
                }
            }
            SiteLine::UnmuteAll => app.tracker.unmute_all(now),
            SiteLine::Control { what: site::ControlSetting::Ids, on } => self.st.control_ids = Some(on),
            SiteLine::Control { what: site::ControlSetting::Content, on } => self.st.control_content = Some(on),
            SiteLine::Home { lat, lon, alt } => {
                self.st.home = Some((lat, lon, alt));
                self.home_changed = Some(self.st.home);
            }
            SiteLine::HomeClear => {
                self.st.home = None;
                self.home_changed = Some(None);
            }
            l @ (SiteLine::AreaShape { .. }
            | SiteLine::AreaCeiling { .. }
            | SiteLine::AreaClear { .. }
            | SiteLine::Bubble { .. }
            | SiteLine::Friend(_)
            | SiteLine::FriendClear { .. }) => {
                site::apply_place(app.tracker.places_mut(), &l);
            }
            SiteLine::AllNodesHere | SiteLine::AreaHere { .. } | SiteLine::Nothing => {}
        }
    }

    /// One line typed by the operator (console `L`): apply, save, log.
    pub fn operator_line(
        &mut self,
        line: &str,
        app: &mut App,
        gw: &GatewayLink<'_>,
        own: Option<(f64, f64)>,
        sd: &mut Logger,
        ev: &mut Evidence,
        now: u64,
    ) {
        let parsed = match site::parse_line(line) {
            Ok(SiteLine::Nothing) => return,
            Ok(l) => l,
            Err(e) => {
                crate::println!("hub-p4: site line refused: {e}");
                return;
            }
        };
        if parsed == SiteLine::AllNodesHere {
            return self.all_nodes_here(app, gw, own, sd, ev, From::Console);
        }
        if let Err(e) = site::check_room(app.tracker.places(), &parsed) {
            crate::println!("hub-p4: site line refused: {e}");
            return;
        }
        let mut text: heapless::String<128> = heapless::String::new();
        let parsed = match parsed {
            // A circle round the hub now, saved with its centre.
            SiteLine::AreaHere { radius_m } => {
                let Some((lat, lon)) = own else {
                    crate::println!("hub-p4: area here: the hub has no position yet (GPS fix or manual)");
                    return;
                };
                let shape = tracker::behaviour::Shape::Circle { lat, lon, radius_m };
                let _ = site::write_area(&mut text, "here", &shape);
                SiteLine::AreaShape { name: tracker::behaviour::Name::try_from("here").unwrap_or_default(), shape }
            }
            l => {
                let _ = writeln!(text, "{}", line.trim());
                l
            }
        };
        self.apply_line(parsed, app, now);
        self.save(&text, From::Console, sd, ev);
    }

    /// Settings "Nodes at hub": every node the hub knows stands where the
    /// hub is (a wired or bench kit).
    pub fn all_nodes_here(
        &mut self,
        app: &mut App,
        gw: &GatewayLink<'_>,
        own: Option<(f64, f64)>,
        sd: &mut Logger,
        ev: &mut Evidence,
        from: From,
    ) {
        let Some((lat, lon)) = own else {
            crate::println!("hub-p4: nodes at hub: the hub has no position yet (GPS fix or manual)");
            return;
        };
        let mut text: heapless::String<1024> = heapless::String::new();
        for n in gw.nodes.iter() {
            self.place(n.mac, lat, lon);
            let _ = site::write_node(&mut text, &n.mac, lat, lon);
        }
        if text.is_empty() {
            crate::println!("hub-p4: nodes at hub: no nodes heard yet");
            return;
        }
        self.apply(app, gw);
        self.save(&text, from, sd, ev);
    }

    /// Settings "Alarms": mute (nuisance kinds, 2 h) or snooze (safety
    /// kinds, 5 min) the alarms on the screen the operator was looking at
    /// (`seen_ms`). One Setting event per rule; nothing goes to the card.
    #[inline(never)]
    pub fn silence_seen(&mut self, app: &mut App, seen_ms: u64, snooze: bool, now: u64, ev: &mut Evidence) {
        if snooze {
            // Drones at Warning or above: their level alerts, five minutes.
            for id in app.tracker.snooze_levels(now) {
                let mut t: heapless::String<160> = heapless::String::new();
                let _ = write!(t, "screen: snooze 5 min: threat level alerts of drone track {}", id.0);
                if let Some(d) = app.tracker.get(id).and_then(|d| d.primary_id()).and_then(|u| u.id.as_str()) {
                    // The ID arrived over the air: shown cleaned.
                    let _ = write!(t, " ({})", link::printable::Printable(d));
                }
                let _ = write!(t, "; level still shown and logged; ends at hub uptime {} s", (now + tracker::behaviour::SNOOZE_MS) / 1000);
                ev.event(evidence::EventCode::Setting, t.as_bytes(), &[3]);
                crate::println!("hub-p4: {t}");
            }
        }
        let added = if snooze { app.tracker.snooze_seen(seen_ms, now) } else { app.tracker.mute_seen(seen_ms, now) };
        for m in &added {
            let mut t: heapless::String<256> = heapless::String::new();
            let _ = t.push_str("screen: ");
            let _ = site::write_mute_text(&mut t, m, now);
            ev.event(evidence::EventCode::Setting, t.as_bytes(), &[3, m.kind as u8]);
            crate::println!("hub-p4: {t}");
        }
    }

    /// Settings "Alarms" when nothing sounds: drop the operator's mutes and
    /// snoozes (the site file's stay; `unmute all` on the console drops
    /// those too).
    pub fn unmute_operator(&mut self, app: &mut App, now: u64, ev: &mut Evidence) {
        app.tracker.unmute_operator(now);
        let site = app.tracker.mutes().len();
        let mut t: heapless::String<96> = heapless::String::new();
        let _ = write!(t, "screen: unmute: the operator's mutes and snoozes dropped; {site} site-file mutes stay");
        ev.event(evidence::EventCode::Setting, t.as_bytes(), &[3]);
        crate::println!("hub-p4: {t}");
    }

    /// Append to the card and log each line as a Setting event.
    fn save(&self, text: &str, from: From, sd: &mut Logger, ev: &mut Evidence) {
        let saved = sd.append_site(text);
        for line in text.lines() {
            let mut t: heapless::String<160> = heapless::String::new();
            let _ = write!(t, "site ({}): {line}", from.name());
            ev.event(evidence::EventCode::Setting, t.as_bytes(), &[3]);
            crate::println!("hub-p4: {t}{}", if saved { "" } else { " (no card: not saved)" });
        }
    }
}
