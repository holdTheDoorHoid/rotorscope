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
//! Not built yet: OpenTimestamps (a second kind of [`Stamper`]) and yearly
//! re-stamping of old tokens (docs/EVIDENCE.md).

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
        }
    }
}

/// What a stamp shows, in the words the page and start-up use.
pub const WHAT_IT_SHOWS: &str = "A stamp shows that the unit's access log, with every upload and view in it, existed by the service's time. It does not show when the records were written: they reach the server only when uploaded.";

impl Config {
    /// Services on different hosts. (Whether two hosts share an operator is
    /// for the server's operator to check.)
    pub fn independent(&self) -> usize {
        self.authorities.iter().map(|a| url_host(&a.url)).collect::<HashSet<_>>().len()
    }

    /// What is wrong with the set-up, for start-up and the page.
    pub fn warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        if !self.enabled {
            w.push("Outside time-stamping is OFF: nothing but this server's word shows when a log existed. The server's operator turns it on with --stamp (it sends a SHA-256 fingerprint of each unit's access log to the services listed).".into());
        } else if self.independent() < 2 {
            w.push(format!(
                "Outside time-stamping uses {} independent service(s): fewer than two, so one service failing, closing or misbehaving leaves no other witness. Add another with --stamp-tsa URL (docs/EVIDENCE.md lists candidates).",
                self.independent()
            ));
        }
        w
    }
}

/// A stamp received and checked against what was sent.
#[derive(Debug, Clone)]
pub struct Stamped {
    /// The reply exactly as received.
    pub token: Vec<u8>,
    pub nonce: [u8; 8],
    pub gen_time_ms: u64,
    pub requested_utc_ms: u64,
    pub certs: Option<(Vec<u8>, String)>,
    pub revocation: Option<(Vec<u8>, String)>,
    pub evidence_note: Option<String>,
}

/// A time-stamping service. RFC 3161 now; OpenTimestamps would be another.
pub trait Stamper: Send + Sync {
    /// The service's URL (stored with each stamp).
    fn authority(&self) -> &str;
    fn kind(&self) -> &'static str {
        "rfc3161"
    }
    /// Stamp `imprint`; `Err` for anything that is not a stamp of it.
    fn stamp(&self, imprint: &[u8; 32]) -> Result<Stamped, String>;
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
        Ok(Stamped { token: reply, nonce, gen_time_ms, requested_utc_ms, certs, revocation, evidence_note: (!notes.is_empty()).then(|| notes.join("; ")) })
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
}

pub struct Stamping {
    pub config: Config,
    stampers: Vec<Box<dyn Stamper>>,
    units: Mutex<HashMap<String, UnitState>>,
    /// Requests run here, one at a time per service.
    busy: Mutex<()>,
}

impl Stamping {
    /// RFC 3161 stampers for the configured services.
    pub fn new(config: Config) -> Self {
        let stampers = config.authorities.iter().map(|a| Box::new(Rfc3161::new(a.clone(), config.timeout)) as Box<dyn Stamper>).collect();
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
        for a in self.authorities() {
            let newest = d.newest_stamp(&a).map(|t| {
                let gt = evidence::b64::decode_vec(&t.token).and_then(|b| evidence::tsp::tst_info(&b).ok().map(|i| i.gen_time_ms));
                serde_json::json!({ "audit_n": t.audit_n, "gen_time_ms": gt, "queued_utc_ms": t.queued_utc_ms, "delay_ms": gt.zip(t.queued_utc_ms).map(|(g, q)| g.saturating_sub(q)) })
            });
            let fails: Vec<&StampFailure> = d.stamp_failures.iter().filter(|f| f.authority == a).collect();
            let since_ok = d.newest_stamp(&a).and_then(|t| t.requested_utc_ms).unwrap_or(0);
            let recent: Vec<&&StampFailure> = fails.iter().filter(|f| f.utc_ms >= since_ok).collect();
            per.push(serde_json::json!({
                "authority": a,
                "newest": newest,
                "outstanding": d.outstanding_for(&a).map(|q| serde_json::json!({ "audit_n": q.audit_n, "queued_utc_ms": q.queued_utc_ms })),
                "failures": fails.len(),
                "failures_since_last_stamp": recent.len(),
                "last_failure": fails.last().map(|f| serde_json::json!({ "utc_ms": f.utc_ms, "reason": f.reason })),
            }));
        }
        serde_json::json!({ "enabled": self.config.enabled, "services": per, "stale": self.stale(d, now) })
    }

    /// An upload has waited longer than [`Config::alarm_ms`] for any stamp.
    pub fn stale(&self, d: &Device, now: u64) -> bool {
        if !self.config.enabled {
            return false;
        }
        let Some((n, t)) = d.newest_upload() else { return false };
        let covered = d.timestamps.iter().any(|s| s.audit_n.is_some_and(|a| a >= n));
        !covered && now.saturating_sub(t) > self.config.alarm_ms
    }

    /// What a bundle says about stamping for this unit.
    pub fn report(&self, d: &Device) -> bundle::Stamping {
        let authorities = self.authorities();
        let outstanding = authorities
            .iter()
            .filter_map(|a| d.outstanding_for(a).map(|q| Outstanding { authority: a.clone(), audit_n: q.audit_n, queued_utc_ms: q.queued_utc_ms }))
            .collect();
        bundle::Stamping { enabled: self.config.enabled, authorities, outstanding, failures: d.stamp_failures.clone() }
    }
}

/// One job: a unit's outstanding stamp for one service.
struct Job {
    stamper: usize,
    unit: String,
    q: Queued,
    imprint: [u8; 32],
}

/// One round of the worker at time `now`: queue what is due, ask each
/// service for what it owes (without holding the store), keep the results.
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
    // 2. What each service owes, if its back-off allows.
    let mut jobs = Vec::new();
    {
        let s = app.lock();
        let units = st.units();
        for (id, d) in &s.devices {
            for (k, sp) in st.stampers.iter().enumerate() {
                let Some(q) = d.outstanding_for(sp.authority()) else { continue };
                if units.get(id).and_then(|u| u.backoff.get(sp.authority())).is_some_and(|b| !b.ready(now)) {
                    continue;
                }
                // The queued entry must be the access log's own entry.
                let ok = usize::try_from(q.audit_n).ok().and_then(|n| d.audit.get(n)).is_some_and(|l| l.hash == q.audit_hash);
                let Some(h) = unhex(&q.audit_hash).and_then(|v| <[u8; 32]>::try_from(v).ok()).filter(|_| ok) else {
                    eprintln!("evidence-server: WARNING {id}: queued time-stamp for audit entry #{} does not match the access log; not sent", q.audit_n);
                    continue;
                };
                jobs.push(Job { stamper: k, unit: id.clone(), q: q.clone(), imprint: bundle::audit_imprint(&d.info.key_id, q.audit_n, &h) });
            }
        }
    }
    // 3. Ask, each service on its own thread, its jobs one after another.
    // Every job gets exactly one result: the threads are joined before
    // anything is kept, a panic in a stamper becomes that job's failure, and
    // a job with no result (a thread that died outside a stamp) is a failure
    // too. Nothing is dropped and nothing is recorded twice.
    let mut slots: Vec<Option<Result<Stamped, String>>> = (0..jobs.len()).map(|_| None).collect();
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
                        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sp.stamp(&j.imprint)))
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
    let results: Vec<(Job, Result<Stamped, String>)> =
        jobs.into_iter().zip(slots).map(|(j, r)| (j, r.unwrap_or_else(|| Err("no result from the stamping thread".into())))).collect();
    // 4. Keep.
    for (j, r) in results {
        let sp = &st.stampers[j.stamper];
        let authority = sp.authority().to_string();
        let mut s = app.lock();
        let Ok(d) = s.device_mut(&j.unit) else { continue };
        let ok = match r {
            Ok(x) => {
                let b64 = |v: &Vec<u8>| evidence::b64::encode_vec(v);
                let t = Timestamp {
                    kind: sp.kind().into(),
                    authority: authority.clone(),
                    imprint: hex(&j.imprint),
                    covers: bundle::COVERS_AUDIT_HEAD.into(),
                    audit_n: Some(j.q.audit_n),
                    audit_hash: Some(j.q.audit_hash.clone()),
                    queued_utc_ms: Some(j.q.queued_utc_ms),
                    requested_utc_ms: Some(x.requested_utc_ms),
                    nonce: Some(hex(&x.nonce)),
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
            Err(reason) => {
                let reason: String = reason.chars().filter(|c| !c.is_control()).take(300).collect();
                if let Err(e) = d.record_stamp_failure(StampFailure { authority: authority.clone(), audit_n: j.q.audit_n, utc_ms: now, reason }) {
                    eprintln!("evidence-server: cannot record a failed time-stamp for {}: {e:?}", j.unit);
                }
                false
            }
        };
        drop(s);
        let mut units = st.units();
        let b = units.entry(j.unit).or_default().backoff.entry(authority).or_default();
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
