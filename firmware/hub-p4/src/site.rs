//! Node positions and muted alarms (`tracker::site`): from the SD card's
//! `ROTORSCP/SITE.TXT` at mount, from the console (`L`, one line), and from
//! the settings page. Every change made here is appended to the file and
//! logged as an evidence Setting event, so it survives a reboot and leaves
//! a record.

use core::fmt::Write;

use tracker::site::{self, SiteLine};

use crate::app::App;
use crate::evlog::Evidence;
use crate::links::GatewayLink;
use crate::sd::Logger;

/// Node positions held (one per relayed node, like the gateway's table).
pub const MAX_PLACED: usize = 16;

#[derive(Default)]
pub struct Site {
    /// Station address -> (lat, lon).
    nodes: heapless::Vec<([u8; 6], f64, f64), MAX_PLACED>,
}

/// Where a change came from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum From {
    /// The card's file at mount: nothing is written back or logged.
    File,
    Console,
    Screen,
}

impl From {
    fn name(self) -> &'static str {
        match self {
            From::File => "file",
            From::Console => "console",
            From::Screen => "screen",
        }
    }
}

impl Site {
    fn place(&mut self, mac: [u8; 6], lat: f64, lon: f64) {
        self.nodes.retain(|n| n.0 != mac);
        if self.nodes.is_full() {
            self.nodes.remove(0);
        }
        let _ = self.nodes.push((mac, lat, lon));
    }

    /// Once a second: tell the tracker where each node it knows stands
    /// (the tracker numbers relayed nodes from 1, in gateway order).
    pub fn apply(&self, app: &mut App, gw: &GatewayLink<'_>) {
        for (i, n) in gw.nodes.iter().enumerate() {
            let node = i as u8 + 1;
            match self.nodes.iter().find(|p| p.0 == n.mac) {
                Some(&(_, lat, lon)) => app.tracker.set_node_position(node, lat, lon),
                None => app.tracker.clear_node_position(node),
            }
        }
    }

    /// Every line of the card's file, in order (later lines win).
    pub fn load(&mut self, text: &[u8], app: &mut App) {
        let (mut ok, mut bad) = (0u32, 0u32);
        for (n, raw) in text.split(|&b| b == b'\n').enumerate() {
            let Ok(line) = core::str::from_utf8(raw) else {
                bad += 1;
                continue;
            };
            match site::parse_line(line) {
                Ok(SiteLine::Nothing) => {}
                // "node all here" needs a live hub position: console/screen only.
                Ok(SiteLine::AllNodesHere) => crate::println!("hub-p4: SITE.TXT line {}: 'node all here' only works on the console", n + 1),
                Ok(l) => {
                    ok += 1;
                    self.apply_line(l, app);
                }
                Err(e) => {
                    bad += 1;
                    crate::println!("hub-p4: SITE.TXT line {} ignored: {e}", n + 1);
                }
            }
        }
        crate::println!(
            "hub-p4: site file: {ok} lines used, {bad} ignored; {} nodes placed, {} alarm mutes",
            self.nodes.len(),
            app.tracker.mutes().len()
        );
    }

    fn apply_line(&mut self, l: SiteLine, app: &mut App) {
        match l {
            SiteLine::NodeAt { mac, lat, lon } => self.place(mac, lat, lon),
            SiteLine::NodeClear { mac } => self.nodes.retain(|n| n.0 != mac),
            SiteLine::Mute(m) => drop(app.tracker.mute(m)),
            SiteLine::UnmuteAll => app.tracker.unmute_all(),
            SiteLine::AllNodesHere | SiteLine::Nothing => {}
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
        let mut text: heapless::String<128> = heapless::String::new();
        let _ = writeln!(text, "{}", line.trim());
        self.apply_line(parsed, app);
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

    /// Settings "Alarms": mute everything sounding now.
    pub fn mute_active(&mut self, app: &mut App, now: u64, sd: &mut Logger, ev: &mut Evidence, from: From) {
        let added = app.tracker.mute_active(now);
        if added.is_empty() {
            return;
        }
        let mut text: heapless::String<1536> = heapless::String::new();
        for m in &added {
            let _ = site::write_mute(&mut text, m);
        }
        self.save(&text, from, sd, ev);
    }

    /// Settings "Alarms" when nothing sounds but rules exist.
    pub fn unmute_all(&mut self, app: &mut App, sd: &mut Logger, ev: &mut Evidence, from: From) {
        app.tracker.unmute_all();
        self.save("unmute all\n", from, sd, ev);
    }

    /// Append to the card and log each line as a Setting event.
    fn save(&self, text: &str, from: From, sd: &mut Logger, ev: &mut Evidence) {
        if from == From::File {
            return;
        }
        let saved = sd.append_site(text);
        for line in text.lines() {
            let mut t: heapless::String<160> = heapless::String::new();
            let _ = write!(t, "site ({}): {line}", from.name());
            ev.event(evidence::EventCode::Setting, t.as_bytes(), &[3]);
            crate::println!("hub-p4: {t}{}", if saved { "" } else { " (no card: not saved)" });
        }
    }
}
