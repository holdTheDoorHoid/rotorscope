# Outside time-stamping of evidence: design note

Status: written for the owner to choose from (WP-14b, 2026-11-18).
**Owner's choice:** two independent free RFC 3161 services, stamped
automatically; no paid provider, no public transparency log;
OpenTimestamps left for later. **Built in WP-14c (2026-11-19):** steps 1 to
6 of the implementation outline below (access-log imprint and bundle format
2, queue, worker with pluggable services, chain and revocation list kept,
verifier with `--check-timestamps`, page and alarm), off until the server's
operator turns it on with `--stamp`. Not built then: step 7 (OpenTimestamps) and
step 8 (re-stamping). **Built in WP-14d (2026-11-18):** step 7, OpenTimestamps
as the second, independent anchor (`--stamp-ots`; an in-house client, no
crate), checked offline against block headers the checker supplies
(docs/EVIDENCE.md, "OpenTimestamps"). Only one free service (FreeTSA) states on its own
pages that general use is allowed, so the server ships with one and warns
that it has fewer than two. How to use it, the services checked and their
terms: docs/EVIDENCE.md, "Outside time-stamps". The manual `--tsa` option
described below as "today" is gone.

## Why

The hub signs its records, but the signing key is the owner's. A signature
shows the records have not changed since they were signed; it cannot answer
"the owner made the whole log up afterwards". A time-stamp from a party the
owner does not control can: it shows that this exact log (by its hash)
existed by a time that party vouches for. The closer that time is to the
events, the less room there was to make anything up.

## What it proves, and what it does not

The P4 hub has no network. Its records reach the server through a laptop
(`evidence-upload`), so stamping happens at the server, after upload. A
stamp therefore proves:

- **The log existed, exactly as stored, by time T.** Any record, batch,
  boot or access-log entry covered by the stamped hash cannot have been
  written, changed or removed after T without the hash no longer matching.

It does **not** prove:

- **That the records were written when they say.** Between an event and its
  upload the card and the unit are in the owner's hands. T is only as close
  to the event as the upload: minutes with the live uploader attached,
  days if the card is read later. The gap is shown (below), not hidden.
- **That nothing was left out.** A stamp covers what was uploaded. Boots or
  records never uploaded leave no trace (see docs/EVIDENCE.md, "NOT
  PROVEN").
- **Who made the records.** That is the signing key's job (and today it is
  a development key; docs/EVIDENCE.md, "What is NOT protected yet").

**How the boot counter narrows it.** Each boot's signed `continuity` record
carries a boot counter and names the previous boot's last record (WP-14a).
A stamp at T over boot *N*'s records therefore also fixes boots 1..*N*-1 as
they were when boot *N* began: an alternative history for those boots
produced after T would not match the stamped chain. Today the counter lives
on the SD card, so whoever holds the card can reset it; it narrows what can
be rewritten *after* a stamp, not before.

**How a secure element would narrow it further (planned).** A monotonic
counter that cannot go back, read and signed by every boot, means each
counter value is used once. Once boot *N* is stamped, no second, different
log claiming to be boot *N* (or any earlier boot) can be produced later
without the duplicate counter showing; and a checker who reads the unit's
current counter knows how many boots exist in total, so withheld boots
show. The stamp then fixes "by T"; the counter fixes "these and only these
boots".

**A lower bound too (optional).** A stamp gives "not after T". The hub could
also record a value nobody could know in advance (for example a public
randomness beacon such as drand, or a recent Bitcoin block hash, typed or
sent to the hub over the console as a note) to give "not before T0" for
every record after it. This needs the laptop or a phone to fetch the value;
it is cheap and independent of the stamping service, but it is outside this
note's scope and has not been looked into further.

## What gets stamped, and when

### The unit stamped: the server's access-log head

Two natural candidates:

1. **A chain head**: one boot's newest checkpoint (what `--tsa` stamps
   today: `head_imprint(boot, seq, hash)`). It covers that one boot only.
2. **The access-log (audit) head after an upload** (recommended). The audit
   log is hash-chained and every accepted upload adds an entry naming the
   batch's number and SHA-256. Its head hash after the upload therefore
   covers every batch stored for that unit, every boot, and every view,
   export and note recorded so far, in one hash. One stamp per upload covers
   everything; nothing per boot is needed.

The imprint sent to the authority is
`SHA-256("rotorscope audit head v1" || device key id || n || audit hash)`,
where *n* is the audit entry number. Only this hash leaves the owner's
network: no record contents, no positions, no device names. (Some services
also see the server's IP address and the time of the request.)

### How often

- After each accepted upload, coalesced per unit: at most one stamp per
  unit every 10 minutes (configurable), plus one when an upload session
  ends (no new upload for 2 minutes). The live uploader posts every 15 s,
  so stamping each batch would be needlessly chatty.
- A stamp request is queued **after** the upload is stored and answered.
  The upload never waits for, or fails because of, the stamping service.

### Stored beside the log, carried in bundles

- A queue file `<data>/devices/<id>/stamp-queue.log` (append-only JSON
  lines): what is waiting (audit entry *n*, its hash, when queued).
- Results in `timestamps.log` (exists today; append-only), one line per
  stamp per service, with new fields: `kind` (`rfc3161`, `ots`, `rekor`),
  `authority`, `covers` (`audit-head` or `chain-head`), `audit_n`,
  `audit_hash`, `imprint`, `queued_utc_ms` (= when the upload was stored),
  `token` (base64, as received), and for RFC 3161 the authority's
  certificate chain and a revocation response (OCSP or CRL) fetched at the
  same time (for checking years later, below). An OpenTimestamps proof that
  is later upgraded gets a **new** line naming the same imprint; nothing is
  rewritten.
- Each stamp also gets an audit entry ("timestamp", with the token's
  SHA-256), so removing a stamp from `timestamps.log` shows.
- Export bundles already carry `timestamps`; the new fields go there. The
  bundle format string is bumped so an older verifier refuses rather than
  ignores them.

### When the service is unreachable

- The queue keeps the request; retries back off (1 min, doubling, capped at
  1 h) and survive a server restart (the queue file is re-read).
- If a stamp is finally taken late, the record shows it: `queued_utc_ms`
  versus the authority's time. The verifier prints the delay for every
  stamp ("stamped 3 h 12 min after upload"), and the page shows units with
  stamps outstanding.
- A configurable alarm (log line, page banner) when a unit's newest stamp
  is older than its newest upload by more than a day.

### What the verifier checks later

1. The imprint is recomputed from the bundle's own audit chain: audit
   entry *n* exists and hashes to `audit_hash`; the imprint matches.
2. The token is checked against the authority (signature, certificate chain
   to a root the checker trusts, the authority's certificate valid at the
   token's time, not revoked then; for OpenTimestamps, the Bitcoin block
   header; for a log, the inclusion proof against the log's signed
   checkpoint).
3. Reported: the authority's time, the delay after upload, and the delay
   after the newest GPS-timed record it covers.

## The options

### A. RFC 3161 time-stamp authorities

How it works: the server sends a hash; the authority returns a signed
token (a CMS SignedData) holding the hash and its own time. Standard since
2001 ([RFC 3161](https://rfc-editor.org/rfc/rfc3161)); checked with common
tools (`openssl ts -verify`), and by many signature and archive products.

**Free public authorities** (no account):

| service | operator | notes |
|---|---|---|
| FreeTSA (`https://freetsa.org/tsr`) | an individual; the site names a contact address, no company ([freetsa.org](https://freetsa.org/)) | free; no SLA stated; no rate limits stated; current signing certificate (ECC P-384) for stamps from 16 March 2026, valid to February 2040; publishes its CA, OCSP and CRL; its older certificate expired in March 2026 |
| DigiCert (`http://timestamp.digicert.com`) | DigiCert | widely used for code signing; listed among public TSAs by Sigstore's docs; **I could not confirm its terms of use for this purpose** |
| Sigstore (`timestamp.sigstore.dev`) | the Sigstore community (Linux Foundation project) | default TSA of Sigstore's `cosign`; its root is self-signed ("sigstore-tsa-selfsigned") ([docs](https://docs.sigstore.dev/cosign/verifying/timestamps/)); no SLA found for the TSA itself |
| Sectigo, GlobalSign, Apple, Entrust and others | the CAs | appear on community lists of RFC 3161 endpoints; **terms not confirmed** |

The server's existing `--tsa` was tested with DigiCert's service (docs/EVIDENCE.md).

**Paid and EU-qualified authorities.** Under the EU eIDAS regulation, a
*qualified* electronic time stamp (from a provider on the EU Trusted List
for that service) enjoys a legal presumption of the accuracy of its date
and time and of the integrity of the data it covers (Article 41(2)); any
electronic time stamp, qualified or not, may not be refused as evidence
solely for being electronic (Article 41(1)) ([Regulation 910/2014, Art. 41](https://www.legislation.gov.uk/eur/2014/910/article/41/data.pdf)).
Qualified providers sell stamps per unit or by subscription; I did not find
reliable current prices to quote. In the United States there is no
comparable statutory presumption that I found; electronic records are
authenticated under the Federal Rules of Evidence, where Rules 902(13) and
902(14) (in force since 1 December 2017) allow a certification by a
qualified person, including one based on hash values, instead of a live
witness ([Foley summary](https://www.foley.com/insights/publications/2017/12/new-federal-rules-of-evidence-90213-and-90214/)).
An expert on either side will recognise an RFC 3161 token and can check it
with standard tools.

**Lifetime.** A token stays checkable as long as the authority's signing
certificate, its chain and its revocation status at the time can be shown,
and the algorithms stay strong. Certificates expire (FreeTSA's previous one
did in 2026), so store the chain and an OCSP/CRL response at stamping time,
and renew before algorithms weaken: the standard way is an *archive time
stamp* that stamps the old token and its evidence again
([RFC 4998, Evidence Record Syntax](https://www.rfc-editor.org/rfc/rfc4998.html)).
A yearly re-stamp of all tokens with a fresh authority keeps the chain of
proof unbroken.

**Availability.** A single free service can disappear or change terms
without notice; using two independent ones at once removes most of that
risk.

**Offline, years later.** Needs the token, the chain, the revocation
response stored with it, and a trusted root (the checker's own copy, e.g.
from the authority's website or an OS store). No network.

**Software.** Requests are already built by hand in `tsa.rs` (no crate).
Checking a token:
- simplest: the system tool `openssl ts -verify -in token.tsr -digest
  <imprint> -CAfile ca.pem -untrusted tsa.crt` (OpenSSL 3.0.13 is on the
  bench laptop). No new Rust dependency; the verifier can either call it or
  print the command.
- in Rust: RustCrypto's `x509-tsp` (RFC 3161 types; Apache-2.0 OR MIT;
  0.1.0, June 2023, the only release) with `cms` and `der` (the lock file
  already has `der` 0.7 and `const-oid`, not `cms` or `x509-cert`), plus
  certificate-chain checking and P-384/RSA signature crates. That is a
  sizeable, young dependency set; a minimal DER reader for the token's time
  and imprint (no signature check) is small enough to write here without a
  crate.

### B. OpenTimestamps

How it works: the server sends a hash to public *calendar* servers, which
aggregate many hashes into one and commit it to a Bitcoin transaction. The
proof is first *pending* (it holds the calendars' promise); after the
transaction is in a block, an *upgrade* step fetches the path to the block
header and the proof becomes complete and independent of the calendars
([overview](https://en.wikipedia.org/wiki/OpenTimestamps)).

- **Operator**: volunteers run the calendars; Bitcoin is the anchor. No one
  party vouches for the time; the proof is "this hash is in block *h*", and
  block *h*'s time is agreed by the network.
- **Cost**: free.
- **What leaves the network**: a hash (as far as I know the reference client also adds a
  random nonce before sending).
- **Delay until final**: hours in practice (calendar batching plus block
  confirmation); third-party sources give figures from about an hour to
  several hours ([one IETF draft](https://datatracker.ietf.org/doc/html/draft-fassbender-scitt-time-anchor-02));
  I found no official figure. Block times can differ from wall-clock time
  by up to about two hours, so the proof's precision is hours, not seconds.
- **Availability**: submit to two or more calendars run by different
  people; a pending proof needs its calendar to still be up at upgrade time.
- **Offline, years later**: a complete proof needs only the Bitcoin block
  header at that height, which is replicated everywhere (a checker can use
  their own node, or several independent sources). No certificates expire.
- **Standing with courts**: I found **no** binding court decision accepting
  OpenTimestamps proofs specifically. China's Hangzhou Internet Court
  accepted blockchain-stored evidence in 2018; in the US, a proposal to the
  federal Advisory Committee on Evidence Rules for a presumption of
  authenticity for Bitcoin-timestamped evidence was reported, not adopted;
  vendor accounts of US cases could not be checked
  ([search summary sources](https://www.tftc.io/bitcoin-open-timestamps-proof-of-authenticity-bryan-jacoutot),
  [OriginStamp](https://originstamp.com/en/blog/reader/blockchain-timestamp)).
  An expert would need to explain it; it is strongest as a second, independent anchor.
- **Software**: the `opentimestamps` Rust crate (0.2.0; docs.rs lists MIT OR
  Apache-2.0, lib.rs shows CC0: **check the version's Cargo.toml**) parses
  and serialises proofs but, as far as I could tell, does not talk to
  calendars or check against Bitcoin by itself; the reference client is
  Python (`ots`), which this project avoids. A small HTTP client for the
  calendar API on top of the crate, and a header source for checking, would
  be needed.

### C. Transparency-log witnessing (Sigstore Rekor)

How it works: the server signs the imprint with its own key and adds an
entry to a public, append-only Merkle log; the log returns an inclusion
proof against its signed checkpoint. Anyone can see the entry and that the
log never removes it.

- **Operator**: the Sigstore community's public instance, with a 99.5%
  availability objective and a volunteer on-call rotation
  ([GA announcement](https://blog.sigstore.dev/sigstore-ga-ddd6ba67894d/),
  [Rekor overview](https://docs.sigstore.dev/rekor/overview/)). Free.
- **Time**: Rekor v2 no longer puts a time in its entries; clients get time
  from a separate RFC 3161 authority ([Rekor v2 GA](https://blog.sigstore.dev/rekor-v2-ga/)).
  So the log adds *public visibility and non-removal*, not time; it needs
  option A for the time anyway.
- **What leaves the network**: the imprint, a signature and the server's
  public key, **published to everyone, permanently**. No record contents,
  but a public trail that this server stamped something at that time.
- **Availability and permanence**: v2 log shards rotate about every six
  months; clients must read the current one from Sigstore's TUF
  repository; old shards stay readable as far as I could tell, but **I
  could not confirm a retention promise**.
- **Offline, years later**: the inclusion proof and the log's signed
  checkpoint and public key, kept with the bundle.
- **Standing with courts**: none known; it is built for software supply
  chains.
- **Software**: the `sigstore` Rust crate (Apache-2.0) is large, or the Go
  tools (`cosign`, `rekor-cli`) as system tools. Heavier than A or B for
  what it adds here.

### D. Two at once

RFC 3161 from two unrelated authorities (fast, recognised, two operators
would have to collude or fail together), optionally plus OpenTimestamps
(no certificate authority at all, final after hours). Each stamp is an
independent line in `timestamps.log`; the verifier reports each; one
failing does not void another.

### Comparison

| | RFC 3161 free | RFC 3161 qualified (EU) | OpenTimestamps | Rekor |
|---|---|---|---|---|
| who vouches | one CA each | a supervised provider | Bitcoin consensus | the log operator (and anyone watching) |
| cost | free | paid | free | free |
| leaves the network | a hash | a hash | a hash (salted) | a hash, a signature, the server's key, publicly |
| time to proof | seconds | seconds | hours | seconds (time from a TSA) |
| recognition | standard, explainable | legal presumption in the EU | no known ruling | none known |
| years later | needs stored chain + revocation; renew | same | needs a block header only | needs stored proof + log key |
| new software | none to request; `openssl` or a young crate to check | same | a crate plus a small client | a large crate or Go tools |

## Recommendation

**Automatic RFC 3161 stamps of the access-log head after each upload, from
two independent free authorities (FreeTSA and DigiCert, or Sigstore's), with
the chain and revocation response stored at stamping time.** It is quick,
standard, explainable to a court and needs no new crates to request and
store (checking can use `openssl ts` at first).

**Add OpenTimestamps as a third, independent anchor** if the owner wants a
proof that relies on no certificate authority and never expires; it costs
only some code and a periodic upgrade job.

**Fallback**: if both authorities are down, uploads carry on, requests
queue, and the delay is recorded and shown; if a free service closes, swap
in another (any RFC 3161 URL works) or a paid one.

**For a case in the EU**, or if a lawyer asks for the strongest standard
form, use a qualified provider from the EU Trusted List (paid) as one of
the two.

## Decisions that are the owner's

1. Which services: two free RFC 3161 authorities (which ones), and whether
   to add OpenTimestamps, a qualified (paid) provider, or both.
2. Paid or free; if paid, a budget (stamps per month: about one per unit
   per ten minutes of uploading, plus one per session).
3. How often: the 10-minute coalescing default, or every upload.
4. Retention: how long the server keeps logs, stamps and the stored
   certificate chains; whether to re-stamp yearly (archive time stamps).
5. Whether any stamp may be public (Rekor makes a permanent public entry;
   RFC 3161 and OpenTimestamps send only a hash to the service).
6. Whether to talk to a lawyer about what form a court in the owner's
   jurisdiction would expect, before relying on it.

## Implementation outline (each step testable without network)

1. **Imprint and record format.** `audit_imprint(key_id, n, hash)` in
   `crates/evidence` (bundle module); new `Timestamp` fields; bundle
   format bump. Tests: imprint is stable; the verifier recomputes it from a
   bundle's audit chain; a stamp naming a missing or edited audit entry
   fails.
2. **Queue.** `stamp-queue.log` in the store; enqueue after a stored upload
   (outside the store lock); coalescing rule. Tests: an upload never waits
   for stamping; restart re-reads the queue; coalescing timing with a fake
   clock.
3. **Worker with pluggable services.** A trait (`Stamper`) with an RFC 3161
   implementation (existing `tsa.rs`, capped reply) and a fake for tests;
   backoff; one result line and one audit entry per stamp. Tests against a
   local fake authority (a canned DER reply) on 127.0.0.1: success,
   refusal, garbage, timeout, oversize; both services independent.
4. **Evidence for later.** Fetch and store the authority's chain and an
   OCSP/CRL response with each stamp; a minimal DER reader that pulls the
   token's time and imprint (no signature check). Tests with canned
   tokens from a test CA made by `openssl` at build time of the fixtures
   (checked in, not fetched).
5. **Verifier.** Imprint match, time and delays reported; with
   `--check-timestamps`, run `openssl ts -verify` per token if present
   (or print the command). Tests with the canned fixtures; tampered token
   fails.
6. **Page and alarms.** Outstanding stamps per unit; stale-stamp warning.
7. **OpenTimestamps (if chosen).** Calendar client (submit, upgrade) as a
   second `Stamper`; periodic upgrade job appending upgraded proofs;
   verifier checks a complete proof against block headers the checker
   supplies. Tests with canned proofs.
8. **Re-stamping (if chosen).** Yearly archive stamp over all tokens
   (RFC 4998 style), stored as further lines.

## What I could not confirm

- DigiCert's, Sectigo's and other CAs' terms for free use of their RFC 3161
  endpoints, and any rate limits (only third-party lists name them).
- Any SLA for FreeTSA or for Sigstore's time-stamp authority.
- Current prices of qualified (eIDAS) time-stamps.
- An official OpenTimestamps figure for the time until a proof is complete;
  which calendars are run by whom today.
- Any court decision specifically accepting OpenTimestamps or Rekor
  evidence; vendor case reports could not be verified.
- The `opentimestamps` crate's licence for its current version (sources
  disagree) and exactly what it implements.
- Rekor's retention of retired v2 shards.
- Whether US courts have applied FRE 902(13)/(14) to time-stamp tokens
  specifically (a 2019 commentary found no published decisions on those
  rules at all; newer ones were not checked).

## Sources

- RFC 3161, Time-Stamp Protocol: https://rfc-editor.org/rfc/rfc3161
- RFC 4998, Evidence Record Syntax: https://www.rfc-editor.org/rfc/rfc4998.html
- FreeTSA: https://freetsa.org/
- Sigstore, verifying timestamps (public TSAs): https://docs.sigstore.dev/cosign/verifying/timestamps/
- Sigstore GA (SLO): https://blog.sigstore.dev/sigstore-ga-ddd6ba67894d/
- Rekor overview: https://docs.sigstore.dev/rekor/overview/
- Rekor v2 GA: https://blog.sigstore.dev/rekor-v2-ga/
- sigstore/timestamp-authority: https://www.github.com/sigstore/timestamp-authority
- eIDAS Article 41 (Regulation 910/2014, as published on legislation.gov.uk): https://www.legislation.gov.uk/eur/2014/910/article/41/data.pdf
- FRE 902(13)/(14), summary: https://www.foley.com/insights/publications/2017/12/new-federal-rules-of-evidence-90213-and-90214/
- OpenTimestamps: https://en.wikipedia.org/wiki/OpenTimestamps
- Bitcoin-anchored temporal proof (IETF draft): https://datatracker.ietf.org/doc/html/draft-fassbender-scitt-time-anchor-02
- Bitcoin time-stamps and evidence (advocacy): https://www.tftc.io/bitcoin-open-timestamps-proof-of-authenticity-bryan-jacoutot
- OriginStamp on blockchain vs RFC 3161 standing (vendor): https://originstamp.com/en/blog/reader/blockchain-timestamp
- x509-tsp crate: https://docs.rs/x509-tsp/latest/x509_tsp/
- opentimestamps crate: https://docs.rs/opentimestamps/latest/opentimestamps/all.html
