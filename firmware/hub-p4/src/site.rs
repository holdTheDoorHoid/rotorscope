//! Node positions and muted alarms (`tracker::site`): from the SD card's
//! `ROTORSCP/SITE.TXT` at mount, from the console (`L`, one line), and from
//! the settings page. Every change made here is appended to the file and
//! logged as an evidence Setting event, so it survives a reboot and leaves
//! a record.

use core::fmt::Write;

use tracker::site::{self, SiteLine, SiteState};

use crate::app::App;
use crate::evlog::Evidence;
use crate::links::GatewayLink;
use crate::sd::Logger;

#[derive(Default)]
pub struct Site {
    /// Node positions (the mute rules live in the tracker).
    st: SiteState,
    /// What last set `audio clips`: the main loop logs a change made by the
    /// file; one typed by the operator is logged by [`Site::save`] already.
    audio_from: From,
}

impl Site {
    /// The control-link opt-ins (read IDs, decode content): what the file
    /// and the operator's lines last said; off when never said.
    pub fn control(&self) -> (bool, bool) {
        (self.st.control_ids.unwrap_or(false), self.st.control_content.unwrap_or(false))
    }

    /// `audio clips on|off`: off unless the file or the operator said on
    /// (owner decision 2026-11-18).
    pub fn audio_clips(&self) -> bool {
        self.st.audio_clips_on()
    }

    /// What last set `audio clips`.
    pub fn audio_from(&self) -> From {
        self.audio_from
    }
}

/// Where a change came from.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum From {
    /// The card's file at mount: nothing is written back or logged.
    #[default]
    File,
    Console,
    Screen,
}

impl From {
    pub fn name(self) -> &'static str {
        match self {
            From::File => "file",
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

    /// What the card's file said at mount.
    pub fn load(&mut self, st: SiteState, app: &mut App) {
        app.tracker.unmute_all();
        for m in &st.mutes {
            app.tracker.mute(m.clone());
        }
        crate::println!(
            "hub-p4: site file: {} lines used, {} ignored; {} nodes placed, {} alarm mutes; control links: IDs {}, content {}; audio clips {}",
            st.used,
            st.ignored,
            st.nodes.len(),
            st.mutes.len(),
            if st.control_ids == Some(true) { "on" } else { "off" },
            if st.control_content == Some(true) { "on" } else { "off" },
            if st.audio_clips_on() { "on" } else { "off" },
        );
        self.audio_from = From::File;
        self.st.nodes = st.nodes;
        self.st.control_ids = st.control_ids;
        self.st.control_content = st.control_content;
        self.st.audio_clips = st.audio_clips;
    }

    fn apply_line(&mut self, l: SiteLine, app: &mut App) {
        match l {
            SiteLine::NodeAt { mac, lat, lon } => self.place(mac, lat, lon),
            SiteLine::NodeClear { mac } => self.st.nodes.retain(|n| n.0 != mac),
            SiteLine::Mute(m) => drop(app.tracker.mute(m)),
            SiteLine::UnmuteAll => app.tracker.unmute_all(),
            l @ (SiteLine::Control { .. } | SiteLine::AudioClips(_)) => self.st.apply(l),
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
        if let SiteLine::AudioClips(_) = parsed {
            self.audio_from = From::Console;
        }
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
