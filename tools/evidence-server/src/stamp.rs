//! Automatic outside time-stamping of each unit's access log (WP-14c;
//! design: docs/EVIDENCE-TIMESTAMPING-DESIGN.md, use: docs/EVIDENCE.md).
//!
//! **Off unless the server's operator turns it on** (`--stamp`): it sends a
//! fingerprint of each unit's access log to third-party services.
//!
//! After an upload is stored and answered, the unit is marked; a worker
//! queues a stamp of the access log's head (the newest entry, which
//! commits to every batch, view, export and note before it) at most once
//! per [`Config::every_ms`] per unit, plus once when uploads stop for
//! [`Config::idle_ms`]. Each configured service is asked independently
//! (one failing does not hold up another), with a back-off from one minute
//! doubling to one hour. Uploads never wait for, or fail because of, a
//! service. The queue survives a restart (`stamp-queue.log`).
//!
//! **What leaves the server**: one DER TimeStampReq per stamp, holding the
//! SHA-256 [`audit_imprint`](evidence::verify::bundle::audit_imprint) and a
//! random nonce, over HTTP with a generic user agent and no cookies, no
//! referrer, no redirects. Nothing names the unit, its owner or its site.
//! The service also sees this server's IP address and the time.
//!
//! **What is kept**: a reply only if it is a granted stamp of exactly the
//! imprint and nonce sent (`evidence::tsp::check_reply`); anything else is
//! a failure line, never a stamp. Each stamp is a line in `timestamps.log`
//! plus an audit entry; with it, the service's published certificate chain
//! and revocation list as fetched then (when configured), for checking it
//! years later.
//!
//! **OpenTimestamps** (WP-14d, `--stamp-ots`): a second, independent kind
//! of [`Stamper`] ([`crate::ots`]). The same imprint goes, salted, to one or
//! more calendars; the pending proof each returns is kept at once (a
//! promise, shown as such), and every round of the worker also asks the
//! calendars for completed (Bitcoin-anchored) forms of waiting proofs, at
//! most once per [`Config::ots_upgrade_every_ms`] per proof, keeping each as
//! a further line ([`upgrade`]); nothing is rewritten.
//!
//! Not built yet: yearly re-stamping of old tokens (docs/EVIDENCE.md).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use evidence::verify::bundle::{self, Outstanding, StampFailure, Timestamp};
use evidence::verify::{hex, unhex, url_host};

use crate::store::{now_ms, Device, Queued, Store};
use crate::tsa;

/// One RFC 3161 service: its URL and, optionally, where it publishes its
/// certificate chain and a revocation list (fetched and kept with each
/// stamp).
#[derive(Debug, Clone, PartialEq)]
pub struct Authority {
    pub url: String,
    pub chain_url: Option<String>,
    pub crl_url: Option<String>,
}

impl Authority {
    /// `URL[,chain=URL][,crl=URL]`.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut parts = spec.split(',').map(str::trim);
        let url = parts.next().unwrap_or("").to_string();
        let web = |u: &str| u.starts_with("https://") || u.starts_with("http://");
        if !web(&url) || url_host(&url).is_empty() {
            return Err(format!("{spec:?}: a time-stamp service is an http:// or https:// URL"));
        }
        let mut a = Authority { url, chain_url: None, crl_url: None };
        for p in parts {
            match p.split_once('=') {
                Some(("chain", u)) if web(u) => a.chain_url = Some(u.to_string()),
                Some(("crl", u)) if web(u) => a.crl_url = Some(u.to_string()),
                _ => return Err(format!("{spec:?}: after the URL, only chain=URL and crl=URL")),
            }
        }
        Ok(a)
    }
}

/// The built-in services: only those whose operator's own pages say free
/// general use is allowed (docs/EVIDENCE.md, "Time-stamp services"). Today
/// that is one, FreeTSA, so the built-in set is **not** two independent
/// services: add another with `--stamp-tsa` (the docs list candidates whose
/// terms are not confirmed).
pub fn default_authorities() -> Vec<Authority> {
    vec![Authority {
        url: "https://freetsa.org/tsr".into(),
        chain_url: Some("https://freetsa.org/files/cacert.pem".into()),
        crl_url: Some("https://www.freetsa.org/crl/root_ca.crl".into()),
    }]
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Set only by the operator (`--stamp`).
    pub enabled: bool,
    pub authorities: Vec<Authority>,
    /// At most one stamp per unit this often while uploads continue.
    pub every_ms: u64,
    /// And one when no upload has come for this long.
    pub idle_ms: u64,
    /// Warn when an upload has waited this long for any stamp.
    pub alarm_ms: u64,
    /// Whole-exchange deadline per request.
    pub timeout: Duration,
    /// Least time between two requests to one service (FreeTSA allows one
    /// per second per address).
    pub min_gap_ms: u64,
    /// OpenTimestamps on (`--stamp-ots`; needs `enabled` too).
    pub ots: bool,
    /// Its calendars (`--stamp-ots-calendar`, else [`crate::ots::default_calendars`]).
    pub ots_calendars: Vec<String>,
    /// Ask a calendar for a waiting proof's completed form at most this often.
    pub ots_upgrade_every_ms: u64,
    /// Stop asking after this long (a calendar that has not anchored a
    /// proof in a week has most likely lost it); the page says so.
    pub ots_give_up_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: false,
            authorities: default_authorities(),
            every_ms: 10 * 60_000,
            idle_ms: 2 * 60_000,
            alarm_ms: 24 * 3_600_000,
            timeout: Duration::from_secs(30),
            min_gap_ms: 2_000,
            ots: false,
            ots_calendars: crate::ots::default_calendars(),
            ots_upgrade_every_ms: 30 * 60_000,
            ots_give_up_ms: 7 * 24 * 3_600_000,
        }
    }
}

/// What a stamp shows, in the words the page and start-up use.
pub const WHAT_IT_SHOWS: &str = "A stamp shows that the unit's access log, with every upload and view in it, existed by the service's time. It does not show when the records were written: they reach the server only when uploaded.";

/// The two kinds, as two independent witnesses, for the page.
pub const WITNESSES: &str = "Two kinds of witness, independent of each other: an RFC 3161 service signs \"this log existed by T\" with its own clock and certificate, within seconds; an OpenTimestamps proof puts the log's fingerprint into a Bitcoin block, so it shows \"this log existed by the block's time\", with no certificate authority. Either one holds if the other fails.";

/// What an OpenTimestamps proof shows and when, for the page.
pub const OTS_SHOWS: &str = "OpenTimestamps: a proof is first PENDING, only a calendar's promise, and shows no time. Calendars put it into a Bitcoin block after some hours (they batch submissions; the delay is theirs, not ours). The time it then proves is that block's time, set by its miner: coarse, tens of minutes to a couple of hours, and never the second of the upload. The completed proof is checked offline with evidence-verify and a block header the person checking gets from a source they trust.";

impl Config {
    /// Services on different hosts. (Whether two hosts share an operator is
    /// for the server's operator to check.)
    /// OpenTimestamps on, with any calendar, counts once (its anchor is the
    /// Bitcoin chain, whichever calendar carried the proof).
    pub fn independent(&self) -> usize {
        let rfc: Vec<String> = self.authorities.iter().map(|a| a.url.clone()).collect();
        evidence::verify::independent_witnesses(&rfc, &self.active_calendars())
    }

    /// The OpenTimestamps calendars in use: none unless it is on.
    pub fn active_calendars(&self) -> Vec<String> {
        if self.enabled && self.ots { self.ots_calendars.clone() } else { Vec::new() }
    }

    /// RFC 3161 services on different hosts.
    pub fn rfc3161_hosts(&self) -> usize {
        self.authorities.iter().map(|a| url_host(&a.url)).collect::<HashSet<_>>().len()
    }

    /// What is wrong with the set-up, for start-up and the page.
    pub fn warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        if !self.enabled {
            w.push("Outside time-stamping is OFF: nothing but this server's word shows when a log existed. The server's operator turns it on with --stamp (it sends a SHA-256 fingerprint of each unit's access log to the services listed).".into());
        } else if self.independent() < 2 {
            w.push(format!(
                "Outside time-stamping uses {} independent witness(es): fewer than two, so one service failing, closing or misbehaving leaves no other witness. Turn on OpenTimestamps with --stamp-ots (a Bitcoin-anchored second witness), or add another RFC 3161 service with --stamp-tsa URL (docs/EVIDENCE.md lists candidates).",
                self.independent()
            ));
        }
        if !self.enabled && self.ots {
            w.push("--stamp-ots given without --stamp: OpenTimestamps stays OFF too.".into());
        }
        w
    }
}

/// A stamp received and checked against what was sent.
#[derive(Debug, Clone)]
pub struct Stamped {
    /// RFC 3161: the reply exactly as received. OpenTimestamps: the pending
    /// proof (`.ots`).
    pub token: Vec<u8>,
    /// RFC 3161: the request's nonce. (OpenTimestamps keeps its salt inside
    /// the proof.)
    pub nonce: Option<Vec<u8>>,
    /// RFC 3161: the authority's time. None for a pending proof.
    pub gen_time_ms: Option<u64>,
    pub requested_utc_ms: u64,
    pub certs: Option<(Vec<u8>, String)>,
    pub revocation: Option<(Vec<u8>, String)>,
    pub evidence_note: Option<String>,
}

/// A time-stamping service: RFC 3161 ([`Rfc3161`]) or an OpenTimestamps
/// calendar ([`crate::ots::Calendar`]).
pub trait Stamper: Send + Sync {
    /// The service's URL (stored with each stamp).
    fn authority(&self) -> &str;
    /// `bundle::KIND_RFC3161` or `bundle::KIND_OTS`.
    fn kind(&self) -> &'static str {
        bundle::KIND_RFC3161
    }
    /// Stamp `imprint`; `Err` for anything that is not a stamp of it.
    fn stamp(&self, imprint: &[u8; 32]) -> Result<Stamped, String>;
    /// OpenTimestamps: the completed form of a pending proof this service
    /// gave, if it has one now (`Ok(None)`: not yet). Others: never asked.
    fn upgrade(&self, _token: &[u8]) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }
}

/// The largest certificate chain or revocation list kept.
pub const MAX_EVIDENCE: usize = 256 << 10;
/// How long a fetched chain and list are reused (one fetch per hour per
/// service, not one per stamp).
const EVIDENCE_TTL_MS: u64 = 3_600_000;

type Fetched = Result<Vec<u8>, String>;
/// When fetched, the chain, the list.
type Cached = (u64, Option<Fetched>, Option<Fetched>);

pub struct Rfc3161 {
    auth: Authority,
    agent: ureq::Agent,
    cache: Mutex<Option<Cached>>,
}

impl Rfc3161 {
    pub fn new(auth: Authority, timeout: Duration) -> Self {
        Rfc3161 { auth, agent: tsa::agent(timeout), cache: Mutex::new(None) }
    }

    fn evidence(&self) -> (Option<Fetched>, Option<Fetched>) {
        let mut c = self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = now_ms();
        if let Some((at, certs, crl)) = c.as_ref()
            && now.saturating_sub(*at) < EVIDENCE_TTL_MS
        {
            return (certs.clone(), crl.clone());
        }
        let get = |u: &Option<String>| u.as_ref().map(|u| tsa::get_capped(&self.agent, u, MAX_EVIDENCE));
        let (certs, crl) = (get(&self.auth.chain_url), get(&self.auth.crl_url));
        *c = Some((now, certs.clone(), crl.clone()));
        (certs, crl)
    }
}

impl Stamper for Rfc3161 {
    fn authority(&self) -> &str {
        &self.auth.url
    }

    fn stamp(&self, imprint: &[u8; 32]) -> Result<Stamped, String> {
        let mut random = [0u8; 8];
        getrandom::getrandom(&mut random).map_err(|e| e.to_string())?;
        let nonce = tsa::nonce(random);
        let requested_utc_ms = now_ms();
        let reply = tsa::post_query(&self.agent, &self.auth.url, &tsa::request_der(imprint, nonce))?;
        let gen_time_ms = evidence::tsp::check_reply(&reply, imprint, &nonce).map_err(|e| e.to_string())?.gen_time_ms;
        let (certs, crl) = self.evidence();
        let mut notes = Vec::new();
        let mut keep = |f: Option<Fetched>, url: &Option<String>, what: &str| match (f, url) {
            (Some(Ok(b)), Some(u)) => Some((b, u.clone())),
            (Some(Err(e)), Some(u)) => {
                notes.push(format!("{what} from {u} not fetched: {e}"));
                None
            }
            _ => None,
        };
        let certs = keep(certs, &self.auth.chain_url, "certificate chain");
        let revocation = keep(crl, &self.auth.crl_url, "revocation list");
        Ok(Stamped { token: reply, nonce: Some(nonce.to_vec()), gen_time_ms: Some(gen_time_ms), requested_utc_ms, certs, revocation, evidence_note: (!notes.is_empty()).then(|| notes.join("; ")) })
    }
}

/// When to queue a unit's next stamp (pure: tested with a fake clock).
#[derive(Debug, Default, Clone)]
pub struct Coalesce {
    pub last_queued: Option<u64>,
    /// The first upload not yet covered by a queued stamp.
    pub pending_since: Option<u64>,
    pub last_upload: Option<u64>,
    /// Asked for by hand: queue at the next tick.
    pub forced: bool,
}

impl Coalesce {
    pub fn upload(&mut self, now: u64) {
        self.last_upload = Some(now);
        self.pending_since.get_or_insert(now);
    }

    pub fn force(&mut self, now: u64) {
        self.forced = true;
        self.pending_since.get_or_insert(now);
    }

    /// Queue now? The first upload at once; then at most one per `every`
    /// while uploads go on, and one once they stop for `idle`.
    pub fn due(&self, now: u64, every: u64, idle: u64) -> bool {
        if self.pending_since.is_none() {
            return false;
        }
        if self.forced {
            return true;
        }
        match self.last_queued {
            None => true,
            Some(q) => now >= q.saturating_add(every) || self.last_upload.is_some_and(|u| now >= u.saturating_add(idle)),
        }
    }

    pub fn queued(&mut self, now: u64) {
        self.last_queued = Some(now);
        self.pending_since = None;
        self.forced = false;
    }
}

/// Retry timing per unit and service: one minute, doubling, at most an hour.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Backoff {
    pub next_ms: u64,
    pub delay_ms: u64,
}

impl Backoff {
    pub const FIRST_MS: u64 = 60_000;
    pub const MAX_MS: u64 = 3_600_000;

    pub fn ready(&self, now: u64) -> bool {
        now >= self.next_ms
    }

    pub fn failed(&mut self, now: u64) {
        self.delay_ms = if self.delay_ms == 0 { Self::FIRST_MS } else { self.delay_ms.saturating_mul(2).min(Self::MAX_MS) };
        self.next_ms = now.saturating_add(self.delay_ms);
    }
}

#[derive(Default)]
struct UnitState {
    coalesce: Coalesce,
    backoff: HashMap<String, Backoff>,
    alarmed: bool,
    /// OpenTimestamps: when to next ask for a waiting proof's completed
    /// form, by the SHA-256 of its token (hex). Not kept over a restart: a
    /// restarted server asks at once.
    upgrade_next: HashMap<String, u64>,
}

pub struct Stamping {
    pub config: Config,
    stampers: Vec<Box<dyn Stamper>>,
    units: Mutex<HashMap<String, UnitState>>,
    /// Requests run here, one at a time per service.
    busy: Mutex<()>,
}

impl Stamping {
    /// RFC 3161 stampers for the configured services, and OpenTimestamps
    /// calendars when that is on.
    pub fn new(config: Config) -> Self {
        let mut stampers: Vec<Box<dyn Stamper>> = config.authorities.iter().map(|a| Box::new(Rfc3161::new(a.clone(), config.timeout)) as Box<dyn Stamper>).collect();
        for c in config.active_calendars() {
            stampers.push(Box::new(crate::ots::Calendar::new(c, config.timeout)));
        }
        Self::with_stampers(config, stampers)
    }

    pub fn with_stampers(config: Config, stampers: Vec<Box<dyn Stamper>>) -> Self {
        Stamping { config, stampers, units: Mutex::new(HashMap::new()), busy: Mutex::new(()) }
    }

    pub fn authorities(&self) -> Vec<String> {
        self.stampers.iter().map(|s| s.authority().to_string()).collect()
    }

    fn units(&self) -> std::sync::MutexGuard<'_, HashMap<String, UnitState>> {
        self.units.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Pick up where a previous run left off: when the last stamp was
    /// queued, and an upload stored since then that still needs one.
    pub fn restore(&self, store: &Store) {
        let mut u = self.units();
        for (id, d) in &store.devices {
            let s = u.entry(id.clone()).or_default();
            s.coalesce.last_queued = d.queue.last().map(|q| q.queued_utc_ms);
            if let Some((_, t)) = d.unqueued_upload() {
                s.coalesce.pending_since = Some(t);
                s.coalesce.last_upload = Some(t);
            }
        }
    }

    /// An upload was stored (called after the store is released; only marks
    /// the unit, never waits).
    pub fn on_upload(&self, id: &str, now: u64) {
        if self.config.enabled {
            self.units().entry(id.to_string()).or_default().coalesce.upload(now);
        }
    }

    /// Stamp at the next tick, whatever the coalescing rule says.
    pub fn stamp_soon(&self, id: &str, now: u64) {
        self.units().entry(id.to_string()).or_default().coalesce.force(now);
    }

    /// For the page: this unit's stamps, backlog, failures and alarm.
    pub fn status(&self, d: &Device, now: u64) -> serde_json::Value {
        let mut per = Vec::new();
        for sp in &self.stampers {
            let a = sp.authority().to_string();
            let kind = sp.kind();
            let mut v = serde_json::json!({ "authority": a, "kind": kind });
            if kind == bundle::KIND_OTS {
                let lines: Vec<&Timestamp> = d.timestamps.iter().filter(|t| t.kind == kind && t.authority == a && t.audit_n.is_some()).collect();
                let anchored = lines.iter().filter_map(|t| token_of(t).and_then(|b| crate::ots::anchors(&b)).filter(|h| !h.is_empty()).map(|h| (t, h))).max_by_key(|(t, _)| t.audit_n);
                v["newest"] = anchored.map_or(serde_json::Value::Null, |(t, h)| {
                    serde_json::json!({ "audit_n": t.audit_n, "state": "anchored", "heights": h, "received_utc_ms": t.requested_utc_ms, "queued_utc_ms": t.queued_utc_ms })
                });
                let waiting = waiting_proofs(d, &a);
                let given_up = waiting.iter().filter(|w| self.given_up(&d.timestamps[w.0], now)).count();
                let oldest = waiting.iter().filter_map(|w| d.timestamps[w.0].requested_utc_ms).min();
                v["pending"] = serde_json::json!({ "count": waiting.len(), "oldest_utc_ms": oldest, "given_up": given_up, "newest_audit_n": waiting.iter().filter_map(|w| d.timestamps[w.0].audit_n).max() });
            } else {
                v["newest"] = d.newest_stamp(&a).map_or(serde_json::Value::Null, |t| {
                    let gt = evidence::b64::decode_vec(&t.token).and_then(|b| evidence::tsp::tst_info(&b).ok().map(|i| i.gen_time_ms));
                    serde_json::json!({ "audit_n": t.audit_n, "gen_time_ms": gt, "queued_utc_ms": t.queued_utc_ms, "delay_ms": gt.zip(t.queued_utc_ms).map(|(g, q)| g.saturating_sub(q)) })
                });
            }
            let fails: Vec<&StampFailure> = d.stamp_failures.iter().filter(|f| f.authority == a).collect();
            let since_ok = d.newest_stamp(&a).and_then(|t| t.requested_utc_ms).unwrap_or(0);
            let recent: Vec<&&StampFailure> = fails.iter().filter(|f| f.utc_ms >= since_ok).collect();
            v["outstanding"] = d.outstanding_for(&a).map_or(serde_json::Value::Null, |q| serde_json::json!({ "audit_n": q.audit_n, "queued_utc_ms": q.queued_utc_ms }));
            v["failures"] = fails.len().into();
            v["failures_since_last_stamp"] = recent.len().into();
            v["last_failure"] = fails.last().map_or(serde_json::Value::Null, |f| serde_json::json!({ "utc_ms": f.utc_ms, "reason": f.reason }));
            per.push(v);
        }
        serde_json::json!({ "enabled": self.config.enabled, "services": per, "stale": self.stale(d, now) })
    }

    /// A waiting OpenTimestamps proof no longer asked about.
    fn given_up(&self, t: &Timestamp, now: u64) -> bool {
        t.requested_utc_ms.is_some_and(|r| now.saturating_sub(r) > self.config.ots_give_up_ms)
    }

    /// An upload has waited longer than [`Config::alarm_ms`] for any stamp.
    /// A pending OpenTimestamps proof does not count: it is a promise.
    pub fn stale(&self, d: &Device, now: u64) -> bool {
        if !self.config.enabled {
            return false;
        }
        let Some((n, t)) = d.newest_upload() else { return false };
        let covered = d.timestamps.iter().any(|s| s.audit_n.is_some_and(|a| a >= n) && (s.kind != bundle::KIND_OTS || token_of(s).and_then(|b| crate::ots::anchors(&b)).is_some_and(|h| !h.is_empty())));
        !covered && now.saturating_sub(t) > self.config.alarm_ms
    }

    /// What a bundle says about stamping for this unit.
    pub fn report(&self, d: &Device) -> bundle::Stamping {
        let of = |kind: &str| -> Vec<String> { self.stampers.iter().filter(|s| (s.kind() == bundle::KIND_OTS) == (kind == bundle::KIND_OTS)).map(|s| s.authority().to_string()).collect() };
        let outstanding = self
            .authorities()
            .iter()
            .filter_map(|a| d.outstanding_for(a).map(|q| Outstanding { authority: a.clone(), audit_n: q.audit_n, queued_utc_ms: q.queued_utc_ms }))
            .collect();
        bundle::Stamping { enabled: self.config.enabled, authorities: of(bundle::KIND_RFC3161), ots_calendars: of(bundle::KIND_OTS), outstanding, failures: d.stamp_failures.clone() }
    }
}

fn token_of(t: &Timestamp) -> Option<Vec<u8>> {
    evidence::b64::decode_vec(&t.token)
}

/// OpenTimestamps proofs from `authority` still waiting to be completed:
/// (line in `timestamps`, SHA-256 of its token, the token). Not anchored,
/// and no later line names it as completed.
pub fn waiting_proofs(d: &Device, authority: &str) -> Vec<(usize, String, Vec<u8>)> {
    let done: HashSet<&str> = d.timestamps.iter().filter_map(|t| t.upgrade_of.as_deref()).collect();
    d.timestamps
        .iter()
        .enumerate()
        .filter(|(_, t)| t.kind == bundle::KIND_OTS && t.authority == authority)
        .filter_map(|(i, t)| {
            let tok = token_of(t)?;
            let sha = hex(&evidence::sha256(&tok));
            let pending = crate::ots::anchors(&tok).is_some_and(|h| h.is_empty());
            (pending && !done.contains(sha.as_str())).then_some((i, sha, tok))
        })
        .collect()
}

/// At most this many waiting proofs per unit and calendar are asked about
/// in one round.
pub const MAX_UPGRADES_PER_TICK: usize = 20;

enum What {
    Stamp,
    /// Ask for the completed form of the proof on this line.
    Upgrade { line: usize, sha: String, token: Vec<u8> },
}

enum Outcome {
    Stamped(Stamped),
    Upgraded(Vec<u8>),
    NotYet,
}

/// One job: a unit's outstanding stamp for one service, or a waiting proof
/// to complete.
struct Job {
    stamper: usize,
    unit: String,
    q: Queued,
    imprint: [u8; 32],
    what: What,
}

/// One round of the worker at time `now`: queue what is due, ask each
/// service for what it owes and each calendar for completed proofs (without
/// holding the store), keep the results.
pub fn tick(app: &crate::App, now: u64) {
    let Some(st) = &app.stamping else { return };
    if !st.config.enabled {
        return;
    }
    let _one = st.busy.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // 1. Queue.
    let due: Vec<String> = st.units().iter().filter(|(_, u)| u.coalesce.due(now, st.config.every_ms, st.config.idle_ms)).map(|(id, _)| id.clone()).collect();
    for id in due {
        let r = app.lock().device_mut(&id).and_then(|d| d.queue_stamp(now));
        match r {
            Ok(_) => st.units().entry(id).or_default().coalesce.queued(now),
            Err(e) => eprintln!("evidence-server: cannot queue a time-stamp for {id}: {e:?}"),
        }
    }
    // 2. What each service owes, if its back-off allows; and which waiting
    // proofs each calendar is due to be asked about.
    let mut jobs = Vec::new();
    {
        let s = app.lock();
        let units = st.units();
        for (id, d) in &s.devices {
            for (k, sp) in st.stampers.iter().enumerate() {
                let unit = units.get(id);
                if unit.and_then(|u| u.backoff.get(sp.authority())).is_some_and(|b| !b.ready(now)) {
                    continue;
                }
                if let Some(q) = d.outstanding_for(sp.authority()) {
                    // The queued entry must be the access log's own entry.
                    let ok = usize::try_from(q.audit_n).ok().and_then(|n| d.audit.get(n)).is_some_and(|l| l.hash == q.audit_hash);
                    match unhex(&q.audit_hash).and_then(|v| <[u8; 32]>::try_from(v).ok()).filter(|_| ok) {
                        Some(h) => jobs.push(Job { stamper: k, unit: id.clone(), q: q.clone(), imprint: bundle::audit_imprint(&d.info.key_id, q.audit_n, &h), what: What::Stamp }),
                        None => eprintln!("evidence-server: WARNING {id}: queued time-stamp for audit entry #{} does not match the access log; not sent", q.audit_n),
                    }
                }
                if sp.kind() != bundle::KIND_OTS {
                    continue;
                }
                let mut asked = 0;
                for (line, sha, token) in waiting_proofs(d, sp.authority()) {
                    let t = &d.timestamps[line];
                    let next = unit.and_then(|u| u.upgrade_next.get(&sha)).copied().unwrap_or(0);
                    if st.given_up(t, now) || now < next || asked >= MAX_UPGRADES_PER_TICK {
                        continue;
                    }
                    asked += 1;
                    let q = Queued { audit_n: t.audit_n.unwrap_or(0), audit_hash: t.audit_hash.clone().unwrap_or_default(), queued_utc_ms: t.queued_utc_ms.unwrap_or(0) };
                    jobs.push(Job { stamper: k, unit: id.clone(), q, imprint: [0; 32], what: What::Upgrade { line, sha, token } });
                }
            }
        }
    }
    // 3. Ask, each service on its own thread, its jobs one after another.
    // Every job gets exactly one result: the threads are joined before
    // anything is kept, a panic in a stamper becomes that job's failure, and
    // a job with no result (a thread that died outside a stamp) is a failure
    // too. Nothing is dropped and nothing is recorded twice.
    let mut slots: Vec<Option<Result<Outcome, String>>> = (0..jobs.len()).map(|_| None).collect();
    std::thread::scope(|sc| {
        let handles: Vec<_> = (0..st.stampers.len())
            .map(|k| {
                let mine: Vec<(usize, &Job)> = jobs.iter().enumerate().filter(|(_, j)| j.stamper == k).collect();
                let sp = &st.stampers[k];
                let gap = st.config.min_gap_ms;
                sc.spawn(move || {
                    let mut out = Vec::new();
                    for (n, (i, j)) in mine.into_iter().enumerate() {
                        if n > 0 && gap > 0 {
                            std::thread::sleep(Duration::from_millis(gap));
                        }
                        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &j.what {
                            What::Stamp => sp.stamp(&j.imprint).map(Outcome::Stamped),
                            What::Upgrade { token, .. } => sp.upgrade(token).map(|o| o.map_or(Outcome::NotYet, Outcome::Upgraded)),
                        }))
                        .unwrap_or_else(|_| Err("the stamping code failed (panic); nothing kept".into()));
                        out.push((i, r));
                    }
                    out
                })
            })
            .collect();
        for h in handles {
            for (i, r) in h.join().unwrap_or_default() {
                slots[i] = Some(r);
            }
        }
    });
    let results: Vec<(Job, Result<Outcome, String>)> =
        jobs.into_iter().zip(slots).map(|(j, r)| (j, r.unwrap_or_else(|| Err("no result from the stamping thread".into())))).collect();
    // 4. Keep.
    for (j, r) in results {
        let sp = &st.stampers[j.stamper];
        let authority = sp.authority().to_string();
        let mut s = app.lock();
        let Ok(d) = s.device_mut(&j.unit) else { continue };
        // When to ask next about a waiting proof (None: not one any more).
        let mut ask_again: Option<(String, Option<u64>)> = None;
        let ok = match r {
            Ok(Outcome::Stamped(x)) => {
                let b64 = |v: &Vec<u8>| evidence::b64::encode_vec(v);
                if sp.kind() == bundle::KIND_OTS {
                    ask_again = Some((hex(&evidence::sha256(&x.token)), Some(now.saturating_add(st.config.ots_upgrade_every_ms))));
                }
                let t = Timestamp {
                    kind: sp.kind().into(),
                    authority: authority.clone(),
                    imprint: hex(&j.imprint),
                    covers: bundle::COVERS_AUDIT_HEAD.into(),
                    audit_n: Some(j.q.audit_n),
                    audit_hash: Some(j.q.audit_hash.clone()),
                    queued_utc_ms: Some(j.q.queued_utc_ms),
                    requested_utc_ms: Some(x.requested_utc_ms),
                    nonce: x.nonce.as_deref().map(hex),
                    token: b64(&x.token),
                    certs: x.certs.as_ref().map(|c| b64(&c.0)),
                    certs_from: x.certs.map(|c| c.1),
                    revocation: x.revocation.as_ref().map(|c| b64(&c.0)),
                    revocation_from: x.revocation.map(|c| c.1),
                    evidence_note: x.evidence_note,
                    ..Timestamp::default()
                };
                match d.record_stamp(t, x.gen_time_ms) {
                    Ok(()) => true,
                    Err(e) => {
                        eprintln!("evidence-server: cannot keep a time-stamp for {}: {e:?}", j.unit);
                        false
                    }
                }
            }
            Ok(Outcome::Upgraded(token)) => {
                let What::Upgrade { line, sha, .. } = &j.what else { continue };
                // The line is still the one asked about (the log only grows).
                let Some(old) = d.timestamps.get(*line).filter(|t| token_of(t).is_some_and(|b| hex(&evidence::sha256(&b)) == *sha)).cloned() else { continue };
                ask_again = Some((sha.clone(), None));
                let t = Timestamp { token: evidence::b64::encode_vec(&token), requested_utc_ms: Some(now), upgrade_of: Some(sha.clone()), nonce: None, evidence_note: None, ..old };
                match d.record_stamp(t, None) {
                    Ok(()) => true,
                    Err(e) => {
                        eprintln!("evidence-server: cannot keep a completed proof for {}: {e:?}", j.unit);
                        false
                    }
                }
            }
            Ok(Outcome::NotYet) => {
                if let What::Upgrade { sha, .. } = &j.what {
                    ask_again = Some((sha.clone(), Some(now.saturating_add(st.config.ots_upgrade_every_ms))));
                }
                true
            }
            Err(reason) => {
                let reason: String = reason.chars().filter(|c| !c.is_control()).take(300).collect();
                let reason = match &j.what {
                    What::Stamp => reason,
                    What::Upgrade { sha, .. } => {
                        ask_again = Some((sha.clone(), Some(now.saturating_add(st.config.ots_upgrade_every_ms))));
                        format!("completing the proof: {reason}")
                    }
                };
                if let Err(e) = d.record_stamp_failure(StampFailure { authority: authority.clone(), audit_n: j.q.audit_n, utc_ms: now, reason }) {
                    eprintln!("evidence-server: cannot record a failed time-stamp for {}: {e:?}", j.unit);
                }
                false
            }
        };
        drop(s);
        let mut units = st.units();
        let u = units.entry(j.unit).or_default();
        match ask_again {
            Some((sha, Some(at))) => {
                u.upgrade_next.insert(sha, at);
            }
            Some((sha, None)) => {
                u.upgrade_next.remove(&sha);
            }
            None => {}
        }
        let b = u.backoff.entry(authority).or_default();
        if ok { *b = Backoff::default() } else { b.failed(now) }
    }
    // 5. Alarm, once per unit until it clears.
    let s = app.lock();
    let mut units = st.units();
    for (id, d) in &s.devices {
        let stale = st.stale(d, now);
        let u = units.entry(id.clone()).or_default();
        if stale && !u.alarmed {
            eprintln!("evidence-server: WARNING {id}: an upload has waited more than {} h for any outside time-stamp", st.config.alarm_ms / 3_600_000);
        }
        u.alarmed = stale;
    }
}

/// Run [`tick`] every second on its own thread.
pub fn spawn_worker(app: std::sync::Arc<crate::App>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("time-stamper".into())
        .spawn(move || {
            loop {
                tick(&app, now_ms());
                std::thread::sleep(Duration::from_secs(1));
            }
        })
        .expect("start the time-stamping thread")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_specs() {
        let a = Authority::parse("https://tsa.example/tsr, chain=https://tsa.example/ca.pem ,crl=http://tsa.example/x.crl").unwrap();
        assert_eq!(a.url, "https://tsa.example/tsr");
        assert_eq!(a.chain_url.as_deref(), Some("https://tsa.example/ca.pem"));
        assert_eq!(a.crl_url.as_deref(), Some("http://tsa.example/x.crl"));
        for bad in ["", "ftp://x/y", "https://", "https://x/tsr,chain=file:///etc/passwd", "https://x/tsr,ocsp=https://x"] {
            assert!(Authority::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn opentimestamps_counts_as_one_more_witness_only_when_on() {
        // The built-in set: FreeTSA only, so one witness and a warning.
        let mut c = Config { enabled: true, ..Config::default() };
        assert_eq!(c.independent(), 1);
        assert!(c.warnings()[0].contains("--stamp-ots"), "{:?}", c.warnings());
        assert!(c.active_calendars().is_empty(), "OpenTimestamps is off unless asked for");
        // With OpenTimestamps on: two, whatever the number of calendars.
        c.ots = true;
        assert_eq!(c.ots_calendars.len(), 2);
        assert_eq!(c.independent(), 2);
        assert!(c.warnings().is_empty(), "{:?}", c.warnings());
        c.ots_calendars.truncate(1);
        assert_eq!(c.independent(), 2);
        // On but with no calendar at all: it adds nothing.
        c.ots_calendars.clear();
        assert_eq!(c.independent(), 1);
        assert!(!c.warnings().is_empty());
        // Stamping off: OpenTimestamps is off too, and says so.
        let off = Config { enabled: false, ots: true, ..Config::default() };
        assert!(off.active_calendars().is_empty());
        assert_eq!(off.independent(), 1);
        let w = off.warnings();
        assert!(w[0].contains("OFF") && w[1].contains("--stamp-ots given without --stamp"), "{w:?}");
        // Only RFC 3161 services: two hosts are two witnesses without it.
        let two = Config { enabled: true, authorities: vec![Authority::parse("https://a.test/tsr").unwrap(), Authority::parse("https://b.test/tsr").unwrap()], ..Config::default() };
        assert_eq!(two.independent(), 2);
        assert!(two.warnings().is_empty());
    }

    #[test]
    fn independence_counts_hosts() {
        let mut c = Config { enabled: true, ..Config::default() };
        assert_eq!(c.independent(), 1);
        assert!(c.warnings()[0].contains("fewer than two"), "{:?}", c.warnings());
        c.authorities.push(Authority::parse("https://FREETSA.org:443/other").unwrap());
        assert_eq!(c.independent(), 1);
        c.authorities.push(Authority::parse("http://tsa.example/tsr").unwrap());
        assert_eq!(c.independent(), 2);
        assert!(c.warnings().is_empty());
        assert!(Config::default().warnings()[0].contains("OFF"));
    }

    #[test]
    fn coalescing_with_a_fake_clock() {
        let (every, idle) = (600_000, 120_000);
        let mut c = Coalesce::default();
        assert!(!c.due(0, every, idle), "nothing uploaded");
        c.upload(1_000);
        assert!(c.due(1_000, every, idle), "the first upload is stamped at once");
        c.queued(1_000);
        // The live uploader posts every 15 s for 21 minutes.
        let mut queued = vec![1_000];
        let mut t = 1_000;
        while t < 1_000 + 21 * 60_000 {
            t += 15_000;
            c.upload(t);
            if c.due(t, every, idle) {
                c.queued(t);
                queued.push(t);
            }
        }
        assert_eq!(queued, vec![1_000, 601_000, 1_201_000], "at most one per ten minutes while uploads go on");
        // Uploads stop: one more two minutes after the last.
        let last = t;
        assert!(!c.due(last + idle - 1, every, idle));
        assert!(c.due(last + idle, every, idle));
        c.queued(last + idle);
        assert!(!c.due(last + 10 * every, every, idle), "nothing new: no more stamps");
        // By hand.
        c.force(last + idle + 5);
        assert!(c.due(last + idle + 5, every, idle));
    }

    #[test]
    fn backoff_doubles_to_an_hour() {
        let mut b = Backoff::default();
        assert!(b.ready(0));
        let mut delays = Vec::new();
        for _ in 0..9 {
            b.failed(0);
            delays.push(b.delay_ms / 60_000);
        }
        assert_eq!(delays, vec![1, 2, 4, 8, 16, 32, 60, 60, 60]);
        assert!(!b.ready(59 * 60_000) && b.ready(60 * 60_000));
    }
}
