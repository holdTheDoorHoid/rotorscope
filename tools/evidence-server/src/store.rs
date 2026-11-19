//! Write-once storage and the per-device audit chain.
//!
//! ```text
//! <data>/devices/<key-id>/device.json       registration (written once)
//! <data>/devices/<key-id>/batches/000001.bin each upload exactly as received
//! <data>/devices/<key-id>/audit.log          one AuditLine (JSON) per line
//! <data>/devices/<key-id>/timestamps.log     outside time-stamps (JSON lines)
//! ```
//!
//! A batch is checked in two steps: [`check_batch`] on its own (every
//! link and signature; no store state, so the server runs it without
//! holding the store), then [`Device::commit`] against what is stored
//! (overlaps, links to stored neighbours, and for a batch that does not end
//! on a checkpoint, the stored continuity record that covers it).
//!
//! Batch files are created with `create_new` (never overwritten), synced,
//! and made read-only. The audit log is only ever appended. Nothing here
//! deletes or rewrites a file. (On a cloud box, put `<data>` on storage
//! with object lock / WORM retention for the same guarantee against the
//! server's own administrator; see docs/EVIDENCE.md.)

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use evidence::continuity::Continuity;
use evidence::format::{self, Body, EventCode};
use evidence::verify::bundle::{audit_hash, AuditEntry, AuditLine, Batch, Bundle, Device as BDevice, Timestamp, FORMAT};
use evidence::verify::{self, hex, unhex, Input, Level, Options};
use serde::{Deserialize, Serialize};

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub key_id: String,
    pub public_key: String,
    pub name: String,
    pub registered_utc_ms: u64,
}

#[derive(Debug, Clone)]
pub struct BatchMeta {
    pub n: u64,
    pub received_utc_ms: u64,
    pub sha256: String,
    pub path: PathBuf,
}

/// One stored record: its hash and the previous hash it claims.
#[derive(Debug, Clone, Copy)]
struct Stored {
    hash: [u8; 32],
    prev: [u8; 32],
}

/// A stored `continuity` record (event 27): it names the head of the boot
/// before it. Every stored record is covered by a signature (its batch's
/// checkpoint, or, for a tail, a stored continuity record), so a stored one
/// can vouch for an earlier boot's tail.
#[derive(Debug, Clone, Copy)]
struct Cover {
    boot: [u8; 8],
    seq: u64,
    k: Continuity,
}

/// A detection for the map and list.
#[derive(Debug, Clone, Serialize)]
pub struct Point {
    pub boot: String,
    pub seq: u64,
    pub utc_ms: Option<u64>,
    pub time_source: &'static str,
    pub hub_lat: Option<f64>,
    pub hub_lon: Option<f64>,
    pub hub_position: &'static str,
    pub drone_id: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub alt_hae_m: Option<f32>,
    pub operator_lat: Option<f64>,
    pub operator_lon: Option<f64>,
    pub summary: String,
}

pub struct Device {
    pub info: DeviceInfo,
    pub key: [u8; 65],
    dir: PathBuf,
    pub batches: Vec<BatchMeta>,
    chains: HashMap<[u8; 8], BTreeMap<u64, Stored>>,
    covers: Vec<Cover>,
    /// Batches not indexed for the map and list: their file no longer
    /// matches what was received (see `load_problems`).
    unshown: Vec<u64>,
    pub audit: Vec<AuditLine>,
    audit_head: [u8; 32],
    pub timestamps: Vec<Timestamp>,
    pub points: Vec<Point>,
    pub firmware: Vec<String>,
    /// Problems found when loading from disk (should stay empty).
    pub load_problems: Vec<String>,
}

#[derive(Debug)]
pub enum StoreError {
    NotFound,
    Conflict(String),
    Rejected(String),
    Io(String),
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e.to_string())
    }
}

pub struct Store {
    root: PathBuf,
    pub devices: BTreeMap<String, Device>,
}

/// A bundle waiting for its batch files (see [`Device::bundle_plan`]).
pub struct BundlePlan {
    batches: Vec<BatchMeta>,
    bundle: Bundle,
}

impl BundlePlan {
    pub fn build(mut self) -> Result<Bundle, StoreError> {
        for b in &self.batches {
            let bytes = fs::read(&b.path)?;
            self.bundle.batches.push(Batch { n: b.n, received_utc_ms: b.received_utc_ms, sha256: b.sha256.clone(), records: evidence::b64::encode_vec(&bytes) });
        }
        Ok(self.bundle)
    }
}

/// What an accepted upload added.
#[derive(Debug, Clone, Serialize)]
pub struct UploadResult {
    pub batch: u64,
    pub sha256: String,
    pub records: usize,
    pub detections: usize,
    pub chains: Vec<ChainRange>,
    pub notes: Vec<String>,
    /// For a batch that ends unsigned (a boot's tail): the stored record of
    /// a later boot whose signed continuity record names its last record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub covered_by: Option<CoveredBy>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoveredBy {
    pub boot: String,
    pub seq: u64,
}

/// A batch that checked out on its own (see [`check_batch`]).
pub struct Checked {
    boot: [u8; 8],
    first: u64,
    last: u64,
    first_prev: [u8; 32],
    last_hash: [u8; 32],
    ends_on_checkpoint: bool,
    records: usize,
    detections: usize,
    notes: Vec<String>,
}

/// Check a batch on its own against the unit's registered key: every
/// record, link and signature, whole records only, one boot, contiguous.
/// Uses no store state (the server runs it without holding the store).
pub fn check_batch(bytes: &[u8], key: &[u8; 65]) -> Result<Checked, StoreError> {
    if bytes.is_empty() {
        return Err(StoreError::Rejected("empty batch".into()));
    }
    // The key registered for the device is the outside key. A device
    // registered with the published test key is stored like any other
    // (a bench hub with no development key signs with it): the server
    // keeps what arrives and vouches for nothing; evidence-verify refuses
    // the test key in a bundle unless --allow-test-key.
    let opts = Options { gaps_allowed: true, expect_key: Some(*key), allow_test_key: true, ..Options::default() };
    let rep = verify::verify(&[Input::new("batch", bytes.to_vec())], &opts);
    let fails: Vec<String> = rep.findings.iter().filter(|f| f.level == Level::Fail).map(|f| format!("{}: {}", f.at, f.what)).collect();
    if !fails.is_empty() {
        return Err(StoreError::Rejected(format!("batch does not verify: {}", fails.join("; "))));
    }
    // Whole records, one chain, contiguous.
    let mut run: Option<Checked> = None;
    let mut covered = 0usize;
    for (_, off, r) in &rep.records {
        let p = r.parse();
        let c = run.get_or_insert(Checked {
            boot: r.boot,
            first: r.seq,
            last: r.seq,
            first_prev: p.header.prev,
            last_hash: r.hash,
            ends_on_checkpoint: false,
            records: 0,
            detections: 0,
            notes: Vec::new(),
        });
        if r.boot != c.boot {
            return Err(StoreError::Rejected("a batch must hold records of one boot only".into()));
        }
        if c.records > 0 && c.last.checked_add(1) != Some(r.seq) {
            return Err(StoreError::Rejected(format!("batch byte {off}: record #{} does not follow #{} (a batch must be contiguous)", r.seq, c.last)));
        }
        c.last = r.seq;
        c.last_hash = r.hash;
        c.ends_on_checkpoint = r.kind == format::Kind::Checkpoint;
        c.records += 1;
        c.detections += usize::from(r.kind == format::Kind::Detection);
        covered = off + r.bytes.len();
    }
    if covered != bytes.len() {
        return Err(StoreError::Rejected(format!("batch byte {covered}: not a whole record")));
    }
    let mut c = run.ok_or_else(|| StoreError::Rejected("no records".into()))?;
    c.notes = rep.findings.iter().filter(|f| f.level != Level::Fail).map(|f| format!("{} {}: {}", f.level.name(), f.at, f.what)).collect();
    Ok(c)
}

#[derive(Debug, Clone, Serialize)]
pub struct ChainRange {
    pub boot: String,
    pub first: u64,
    pub last: u64,
}

fn write_once(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = OpenOptions::new().write(true).create_new(true).open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    let mut perm = f.metadata()?.permissions();
    perm.set_readonly(true);
    fs::set_permissions(path, perm)?;
    Ok(())
}

fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    let mut f = OpenOptions::new().append(true).create(true).open(path)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()
}

impl Store {
    /// Open (or create) a data directory and re-check everything in it.
    pub fn open(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("devices"))?;
        let mut s = Store { root, devices: BTreeMap::new() };
        let mut ids: Vec<PathBuf> = fs::read_dir(s.root.join("devices"))?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        ids.sort();
        for d in ids {
            match s.load_device(&d) {
                Ok(dev) => {
                    for p in &dev.load_problems {
                        eprintln!("evidence-server: WARNING device {}: {p}", dev.info.key_id);
                    }
                    s.devices.insert(dev.info.key_id.clone(), dev);
                }
                Err(e) => eprintln!("evidence-server: WARNING cannot load {}: {e}", d.display()),
            }
        }
        Ok(s)
    }

    fn load_device(&self, dir: &Path) -> std::io::Result<Device> {
        let info: DeviceInfo = serde_json::from_slice(&fs::read(dir.join("device.json"))?).map_err(std::io::Error::other)?;
        let key: [u8; 65] = unhex(&info.public_key).and_then(|v| v.try_into().ok()).ok_or_else(|| std::io::Error::other("bad public key"))?;
        let mut dev = Device::new(info, key, dir.to_path_buf());
        // Audit chain.
        if let Ok(text) = fs::read_to_string(dir.join("audit.log")) {
            for (i, l) in text.lines().enumerate() {
                match serde_json::from_str::<AuditLine>(l) {
                    Ok(line) => {
                        let h = audit_hash(&dev.audit_head, &line.entry);
                        if hex(&h) != line.hash {
                            dev.load_problems.push(format!("audit.log line {}: hash does not match (edited or reordered)", i + 1));
                        }
                        dev.audit_head = h;
                        dev.audit.push(line);
                    }
                    Err(e) => dev.load_problems.push(format!("audit.log line {}: {e}", i + 1)),
                }
            }
        }
        let uploads: Vec<(u64, String)> = dev
            .audit
            .iter()
            .filter_map(|l| serde_json::from_str::<AuditEntry>(&l.entry).ok())
            .filter(|e| e.action == "upload")
            .map(|e| (e.detail["batch"].as_u64().unwrap_or(0), e.detail["sha256"].as_str().unwrap_or("").to_string()))
            .collect();
        let mut files: Vec<PathBuf> = fs::read_dir(dir.join("batches"))
            .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        files.sort();
        for (i, p) in files.iter().enumerate() {
            let bytes = fs::read(p)?;
            let sha = hex(&evidence::sha256(&bytes));
            let n = p.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse().ok()).unwrap_or(0);
            let matches = uploads.get(i) == Some(&(n, sha.clone()));
            if !matches {
                dev.load_problems.push(format!("batch {} does not match its upload entry in the audit log: not shown on the map or list", p.display()));
                dev.unshown.push(n);
            }
            let received = dev
                .audit
                .iter()
                .filter_map(|l| serde_json::from_str::<AuditEntry>(&l.entry).ok())
                .find(|e| e.action == "upload" && e.detail["batch"].as_u64() == Some(n))
                .map_or(0, |e| e.utc_ms);
            if let Err(e) = dev.index_batch(&bytes, matches) {
                dev.load_problems.push(format!("batch {}: {e}", p.display()));
            }
            dev.batches.push(BatchMeta { n, received_utc_ms: received, sha256: sha, path: p.clone() });
        }
        if uploads.len() != files.len() {
            dev.load_problems.push(format!("audit log has {} uploads, {} batch files on disk", uploads.len(), files.len()));
        }
        if let Ok(text) = fs::read_to_string(dir.join("timestamps.log")) {
            dev.timestamps = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        }
        Ok(dev)
    }

    pub fn register(&mut self, public_key_hex: &str, name: &str, actor: &str) -> Result<DeviceInfo, StoreError> {
        let key: [u8; 65] = unhex(public_key_hex)
            .and_then(|v| v.try_into().ok())
            .filter(|k: &[u8; 65]| k[0] == 4)
            .ok_or_else(|| StoreError::Rejected("public_key must be 130 hex digits (65-byte uncompressed P-256 key)".into()))?;
        if !evidence::sign::key_is_valid(&key) {
            return Err(StoreError::Rejected("public_key is not a point on P-256".into()));
        }
        let key_id = String::from_utf8(evidence::fingerprint(&key).to_vec()).unwrap();
        if self.devices.contains_key(&key_id) {
            return Err(StoreError::Conflict(format!("device {key_id} is already registered")));
        }
        let dir = self.root.join("devices").join(&key_id);
        fs::create_dir_all(dir.join("batches"))?;
        let info = DeviceInfo { key_id: key_id.clone(), public_key: hex(&key), name: name.to_string(), registered_utc_ms: now_ms() };
        write_once(&dir.join("device.json"), serde_json::to_string_pretty(&info).unwrap().as_bytes())?;
        let mut dev = Device::new(info.clone(), key, dir);
        dev.audit(actor, "register", serde_json::json!({ "public_key": info.public_key, "name": name, "key_id": key_id }))?;
        self.devices.insert(key_id, dev);
        Ok(info)
    }

    pub fn device(&self, id: &str) -> Result<&Device, StoreError> {
        self.devices.get(id).ok_or(StoreError::NotFound)
    }

    pub fn device_mut(&mut self, id: &str) -> Result<&mut Device, StoreError> {
        self.devices.get_mut(id).ok_or(StoreError::NotFound)
    }
}

impl Device {
    fn new(info: DeviceInfo, key: [u8; 65], dir: PathBuf) -> Self {
        Device {
            info,
            key,
            dir,
            batches: Vec::new(),
            chains: HashMap::new(),
            covers: Vec::new(),
            unshown: Vec::new(),
            audit: Vec::new(),
            audit_head: [0; 32],
            timestamps: Vec::new(),
            points: Vec::new(),
            firmware: Vec::new(),
            load_problems: Vec::new(),
        }
    }

    /// Append one entry to the audit chain (file first, then memory).
    pub fn audit(&mut self, actor: &str, action: &str, detail: serde_json::Value) -> Result<AuditLine, StoreError> {
        let entry = AuditEntry {
            n: self.audit.len() as u64,
            utc_ms: now_ms(),
            actor: actor.to_string(),
            action: action.to_string(),
            device: self.info.key_id.clone(),
            detail,
        };
        let entry = serde_json::to_string(&entry).unwrap();
        let h = audit_hash(&self.audit_head, &entry);
        let line = AuditLine { entry, hash: hex(&h) };
        append_line(&self.dir.join("audit.log"), &serde_json::to_string(&line).unwrap())?;
        self.audit_head = h;
        self.audit.push(line.clone());
        Ok(line)
    }

    /// Next sequence number the server lacks, per boot.
    pub fn next_seq(&self) -> BTreeMap<String, u64> {
        self.chains.iter().map(|(b, m)| (hex(b), m.keys().next_back().map_or(0, |s| s.saturating_add(1)))).collect()
    }

    /// Stored ranges per boot (gaps visible).
    pub fn ranges(&self) -> BTreeMap<String, Vec<(u64, u64)>> {
        self.chains
            .iter()
            .map(|(b, m)| {
                let mut v: Vec<(u64, u64)> = Vec::new();
                for &s in m.keys() {
                    match v.last_mut() {
                        Some(r) if r.1.checked_add(1) == Some(s) => r.1 = s,
                        _ => v.push((s, s)),
                    }
                }
                (hex(b), v)
            })
            .collect()
    }

    /// Check a batch, then keep it ([`check_batch`] then [`commit`](Self::commit)).
    pub fn upload(&mut self, bytes: &[u8], actor: &str) -> Result<UploadResult, StoreError> {
        let c = check_batch(bytes, &self.key)?;
        self.commit(&c, bytes, actor)
    }

    /// Keep a batch [`check_batch`] passed, if it fits what is stored: no
    /// overlap, links to stored neighbours, and either it ends on a signed
    /// checkpoint or a stored continuity record of a later boot names its
    /// last record exactly (that boot's signature then covers it).
    pub fn commit(&mut self, c: &Checked, bytes: &[u8], actor: &str) -> Result<UploadResult, StoreError> {
        let (boot, first, last) = (&c.boot, c.first, c.last);
        let mut covered_by = None;
        if !c.ends_on_checkpoint {
            let cover = self.covers.iter().find(|v| {
                v.boot != *boot && v.k.has_prev() && v.k.prev_boot == *boot && v.k.prev_records.checked_sub(1) == Some(last) && v.k.prev_head == c.last_hash
            });
            match cover {
                Some(v) => covered_by = Some(CoveredBy { boot: hex(&v.boot), seq: v.seq }),
                None => {
                    let named = self.covers.iter().filter(|v| v.k.has_prev() && v.k.prev_boot == *boot).map(|v| v.k.prev_records).max();
                    return Err(StoreError::Rejected(match named {
                        Some(n) => format!(
                            "a batch must end with a signed checkpoint, or end exactly at the record a later boot's stored continuity record names (#{} of boot {}, with its hash); this one ends unsigned at #{last}",
                            n.saturating_sub(1),
                            hex(boot)
                        ),
                        None => "a batch must end with a signed checkpoint (or, for a boot's last records, upload the next boot's records first: its signed continuity record covers them)".into(),
                    }));
                }
            }
        }
        // `range(first..=last)` panics on an inverted range.
        if first > last {
            return Err(StoreError::Rejected(format!("records #{first}..#{last} are not in order")));
        }
        if let Some(m) = self.chains.get(boot) {
            if m.range(first..=last).next().is_some() {
                let next = m.keys().next_back().map_or(0, |s| s.saturating_add(1));
                return Err(StoreError::Conflict(format!(
                    "records #{first}..#{last} overlap records already stored for boot {}; next new record is #{next}",
                    hex(boot)
                )));
            }
            if first > 0
                && let Some(p) = m.get(&(first - 1))
                && p.hash != c.first_prev
            {
                return Err(StoreError::Rejected(format!("record #{first} does not link to stored record #{}", first - 1)));
            }
            if let Some(after) = last.checked_add(1)
                && let Some(n) = m.get(&after)
                && n.prev != c.last_hash
            {
                return Err(StoreError::Rejected(format!("stored record #{after} does not link to this batch's last record #{last}")));
            }
        }
        // Keep it.
        let n = self.batches.last().map_or(1, |b| b.n + 1);
        let path = self.dir.join("batches").join(format!("{n:06}.bin"));
        write_once(&path, bytes)?;
        let sha = hex(&evidence::sha256(bytes));
        let chains = vec![ChainRange { boot: hex(boot), first, last }];
        let mut detail = serde_json::json!({ "batch": n, "sha256": sha, "bytes": bytes.len(), "records": c.records, "chains": chains });
        if let Some(v) = &covered_by {
            detail["covered_by"] = serde_json::json!(v);
        }
        self.audit(actor, "upload", detail)?;
        self.index_batch(bytes, true).map_err(StoreError::Io)?;
        self.batches.push(BatchMeta { n, received_utc_ms: now_ms(), sha256: sha.clone(), path });
        Ok(UploadResult { batch: n, sha256: sha, records: c.records, detections: c.detections, chains, notes: c.notes.clone(), covered_by })
    }

    /// Add a stored batch's records to the index and, when `show`, to the
    /// map and list. A batch whose file no longer matches what was received
    /// is indexed (so its records are not asked for again) but not shown:
    /// the page must not show as the log what the log does not say.
    fn index_batch(&mut self, bytes: &[u8], show: bool) -> Result<(), String> {
        for (off, r) in format::records(bytes) {
            let r = r.map_err(|e| format!("byte {off}: {e}"))?;
            let h = r.header;
            self.chains.entry(h.boot_id).or_default().insert(h.seq, Stored { hash: r.hash(), prev: h.prev });
            if !show {
                continue;
            }
            let fw = hex(&h.fw_hash);
            if !self.firmware.contains(&fw) {
                self.firmware.push(fw);
            }
            match r.body {
                Body::Detection(d) => self.points.push(point(&r, &d)),
                Body::Event(e) if e.code == EventCode::Continuity as u16 => {
                    if let Some(k) = Continuity::decode(e.data) {
                        self.covers.push(Cover { boot: h.boot_id, seq: h.seq, k });
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Batches kept out of the map and list (changed on disk).
    pub fn unshown_batches(&self) -> Vec<u64> {
        self.unshown.clone()
    }

    pub fn bundle(&self, server: &str) -> Result<Bundle, StoreError> {
        self.bundle_plan(server).build()
    }

    /// Everything a bundle holds except the batch files' contents, so the
    /// files can be read without holding the store.
    pub fn bundle_plan(&self, server: &str) -> BundlePlan {
        BundlePlan { batches: self.batches.clone(), bundle: self.bundle_without_batches(server) }
    }

    fn bundle_without_batches(&self, server: &str) -> Bundle {
        Bundle {
            format: FORMAT.into(),
            created_utc_ms: now_ms(),
            server: server.into(),
            device: BDevice {
                key_id: self.info.key_id.clone(),
                public_key: self.info.public_key.clone(),
                name: self.info.name.clone(),
                registered_utc_ms: self.info.registered_utc_ms,
            },
            batches: Vec::new(),
            audit: self.audit.clone(),
            firmware_hashes: self.firmware.clone(),
            timestamps: self.timestamps.clone(),
            stamping: None,
        }
    }

    /// The newest stored checkpoint: (boot, seq, record hash).
    /// From the newest batch that holds one (a boot's tail, stored after
    /// the next boot's records, holds none).
    pub fn latest_checkpoint(&self) -> Option<([u8; 8], u64, [u8; 32])> {
        self.batches.iter().rev().filter(|b| !self.unshown.contains(&b.n)).find_map(|b| {
            let bytes = fs::read(&b.path).ok()?;
            format::records(&bytes)
                .filter_map(|(_, r)| r.ok())
                .filter(|r| r.header.kind == format::Kind::Checkpoint)
                .last()
                .map(|r| (r.header.boot_id, r.header.seq, r.hash()))
        })
    }

    pub fn add_timestamp(&mut self, t: Timestamp) -> std::io::Result<()> {
        append_line(&self.dir.join("timestamps.log"), &serde_json::to_string(&t).unwrap())?;
        self.timestamps.push(t);
        Ok(())
    }

    /// True once the record exists here (for annotations).
    pub fn has_record(&self, boot: &[u8; 8], seq: u64) -> bool {
        self.chains.get(boot).is_some_and(|m| m.contains_key(&seq))
    }
}

fn point(r: &format::Record<'_>, d: &format::DetectionBody<'_>) -> Point {
    let h = &r.header;
    let mut p = Point {
        boot: hex(&h.boot_id),
        seq: h.seq,
        utc_ms: h.utc_ms,
        time_source: format::TimeSource::from_u8(h.time_source).map_or("?", |t| t.name()),
        hub_lat: (h.pos_source != 0).then(|| h.lat_e7 as f64 / 1e7),
        hub_lon: (h.pos_source != 0).then(|| h.lon_e7 as f64 / 1e7),
        hub_position: format::PosSource::from_u8(h.pos_source).map_or("?", |s| s.name()),
        drone_id: None,
        lat: None,
        lon: None,
        alt_hae_m: None,
        operator_lat: None,
        operator_lon: None,
        summary: String::from_utf8_lossy(d.summary).into_owned(),
    };
    if let Ok((_, _, link::Frame::Detection(det))) = evidence::summary::decode_raw(d.raw_format, d.raw) {
        use rid::message::Message;
        for m in det.messages.iter().filter_map(|m| rid::message::decode(m).ok()) {
            match m {
                Message::BasicId(b) => p.drone_id = b.uas_id.as_str().map(str::to_string),
                Message::Location(l) => {
                    if let Some(pos) = l.position {
                        p.lat = Some(pos.lat);
                        p.lon = Some(pos.lon);
                    }
                    p.alt_hae_m = l.altitude_geodetic;
                }
                Message::System(s) => {
                    if let Some(pos) = s.operator_position {
                        p.operator_lat = Some(pos.lat);
                        p.operator_lon = Some(pos.lon);
                    }
                }
                _ => {}
            }
        }
    }
    p
}
