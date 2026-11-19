# Evidence log: what it does, what it proves, what it does not yet

PLAN.md Phase 13, first part (bench-safe). Status 2026-11-15: working on the
P4 bench hub with a **development key**. Not yet evidence-grade.

**Whether a court accepts any of this is the court's decision. Have a lawyer
review the method and these documents before relying on them.**

## What this log proves today, and what it does not

The goal is a log that can stand up in court. Today it is well short of
that.

**It proves:** the records have not changed since they were signed, by
whoever holds the signing key. Today that key can be read out of the
unit's flash, and it is also in the key file and the firmware file on the
laptop that built the firmware.

**It does not prove:**

- **which unit made the log.** Anyone with the key (see above) can write a
  log that checks out exactly like this unit's. A log also names its own key,
  so a log checked only against the key it names proves nothing about who
  made it: the verifier says **VERIFIED** only when you give it the unit's
  key from outside the log (`--key` or `--keys`, see "Who signed" below).
- **that the log is complete.** Since WP-14a each boot records a boot
  counter and how far the boot before it had reached the card, so a boot
  missing *between* the ones supplied, or an older boot's log cut short
  after the next boot started, now fails. But the counter is kept on the SD
  card, which whoever holds it can rewrite or wipe: the newest boot's last
  records, the newest card files deleted, or a wiped card still leave no
  trace. The verifier says so on every run, naming the newest boot counter
  it saw.
- **that the times are true.** Times come from the unit's own GPS receiver
  and clock; nothing outside the unit vouches for them. An outside
  time-stamp (when the server's operator has turned them on, below) only
  shows the records existed by the time of the stamp, which is after they
  were uploaded. Since WP-12 the hub only writes GPS time
  it trusts, and its clock follows GPS by no more than a crystal could drift,
  but a spoofer present from the very first boot, or walking the time by
  under about 1.7-3.5 s a day, is not caught (docs/GNSS-INTEGRITY.md).
  The verifier checks what it can without an outside reference (WP-14a):
  UTC before the firmware's build date **fails** (the hub never writes such
  time); a jump of UTC against the hub's own uptime with no clock event, a
  drift beyond the hub's own steering, and a boot whose time starts before
  the previous boot's ended are **noted** (they cast doubt on the time, not
  on the records). A log from a hub that never had GPS time verifies as
  before, its records marked "no UTC".

**Still missing before it could be court-grade:**

1. a signing key held in hardware, which software cannot read (design
   below, nothing burned yet);
2. a counter that cannot be set back (a secure element: the SD card
   counter of WP-14a is behind the same interface and in the same format),
   so that missing newest boots show to a checker who reads the unit's
   current counter;
3. outside time-stamping turned on, with two independent services (built
   in WP-14c and automatic once on, but off by default, and only one free
   service qualifies as a built-in default; see "Outside time-stamps");
4. reproducible firmware builds, plus secure boot, so the firmware hash in
   each record can be matched to published source and only that firmware
   can run.

## In plain words

Every time the hub receives a detection from a node, it writes a record of
exactly what arrived (the raw bytes, so anyone can decode them again), what
the hub made of it, which node heard it and how strongly, where the hub was
and how good its GPS fix was, what time it was and where that time came
from, and which firmware was running. It also writes a record for anything
that could matter later: power-on, GPS gained or lost or jumping, the clock
being set or corrected, the SD card going in or out, nodes appearing or
going quiet, the airspace data version, settings changes, operator notes.

Each record contains a fingerprint (SHA-256) of the record before it. Change
one letter anywhere, remove a record from the middle or swap two, and every
fingerprint after that point stops matching. Once a minute the hub **signs**
the latest fingerprint with its key. A signature covers everything before
it. Removing records from the **end** changes nothing that comes before, so
it does not show (see above).

The records go to the SD card in their own folder (`ROTORSCP/EVIDENCE/`),
separate from the everyday CSV log, and stream out of the USB port. A laptop
program passes them to our server, which checks every fingerprint and
signature as they arrive, refuses anything that does not check out, and
keeps what it accepts in files it never changes. The server also keeps its
own fingerprinted log of who looked at, exported or annotated what.

An **export bundle** is one file with everything: the records exactly as
uploaded, the unit's public key, the firmware fingerprints, the server's
access log for them, and any outside time-stamps. The **verifier**
(`evidence-verify`, open source, runs on any laptop) checks all of it and
says, for any problem it can see, which file, record and byte. With
`--check-timestamps` it also checks each time-stamp's signature, using the
`openssl` tool (see "Outside time-stamps").

## Who signed: give the verifier the key

Every log names its own key, in the boot event and in every checkpoint.
Anyone can generate a key, write records naming it, and set the key class
byte to "hardware key": the signatures will check against the key the log
names. So:

- `evidence-verify` says **VERIFIED** only when the signing key comes from
  outside the log: `--key <130 hex digits>`, or `--keys <file>` with one key
  per line (hex, then a label; `#` starts a comment). Take the key from the
  unit itself when it is set up (the hub prints `hub-p4: evidence public
  key ...` at boot) and write its fingerprint on the custody record. Do not
  take it from the log or from an export bundle: a bundle's device key is
  only the server's record of what was registered.
- Without a key the verifier still checks everything else, but its verdict
  is **SELF-CONSISTENT, SIGNER NOT CHECKED: anyone could have made this
  log** (exit code 4).
- The key class is printed as what the log **claims** ("log claims: ...").
  It is a byte the signer writes and proves nothing by itself.
- A log signed with the **published test key** (whose secret is in this
  repository) is a failure: anyone can produce one. Only the project's own
  tests accept it, with `--allow-test-key`.
- A log whose boot event names bench features (the hub writes
  `...; bench: fakedrone,faulttest`, or `bench: none`; docs/BENCH.md "Bench
  builds") gets a NOTE saying a bench build wrote it: such a build can fake
  detections, position or time. The note does not change the verdict.

## What is protected now

Each item below assumes the verifier was given the unit's key from outside
the log (see "Who signed").

- **Changing, reordering or adding records, and removing records from the
  middle** after the hub wrote them: caught, on the card, in an upload, on
  the server's disk and in a bundle (host tests, and on the bench
  2026-11-15; see "Tested"). **Cutting records off the end is not caught**
  when the cut falls at a checkpoint: what is left is a shorter, valid log.
  A cut between checkpoints shows only as records "not covered by a
  signature".
- **Changing the server's access log**: caught (it is chained the same way).
  The access log is not signed, so whoever runs the server can rewrite it
  whole; signing it is later work.
- **A batch altered on the way to the server**: refused on arrival.
- **A boot's last records on the server** (WP-14b): a batch must end on a
  signed checkpoint, except a boot's tail (its records after its last
  checkpoint), which the server takes once the next boot's records are
  stored and only if that boot's continuity record names the tail's last
  record by number and hash; that boot's signature then covers it. The
  uploader sends tails by itself after the signed batches. A tail that no
  stored continuity record names, or that runs past the record named, is
  refused. So an export of several boots shows no gap at each boot's end,
  except the newest boot's (nothing names it yet).
- **The server under load and against hostile peers** (WP-14b): an upload
  is checked (every link and signature) without holding the server's
  store, a few at a time, so a large or slow upload does not hold up other
  requests; two uploads of the same records at once keep one and refuse
  the other as an overlap. Size caps: 16 MiB per upload, 64 KiB for other
  request bodies, a unit's name 100 characters (no control characters), a
  note 4000 characters, a time-stamp reply 256 KiB; TLS handshakes time
  out after 15 s, request headers after 30 s. The uploader reads console
  lines with a length cap (and keeps a line split by a pause, which it used
  to drop), holds at most 512 MiB of records, reads server replies up to
  1 MiB and prints server text without control characters.
- **The page** (WP-14b): its script, style and map library (Leaflet 1.9.4,
  BSD-2-Clause, kept in `tools/evidence-server/src/static/`, byte-identical
  to the published release, checked by a test against the SRI hashes
  leafletjs.com publishes) are served by the server itself, under a
  Content-Security-Policy that allows no inline script and no script from
  anywhere else. Map tiles still come from tile.openstreetmap.org (images
  only: they can show a wrong map, not change the records or the list;
  they also tell OpenStreetMap which area is being viewed). The page warns
  when a unit signs with the published test key or when a stored file
  failed its check at start-up; such a file's detections are not shown.
  The page is a view; the export bundle checked with `evidence-verify` is
  the evidence.
- **A boot left out, or an older boot's log cut short** (WP-14a): each
  boot's `continuity` record names its boot counter and the previous boot's
  record count and last record. Given both boots, a log cut back to a
  checkpoint after the next boot started fails, and a boot missing between
  two supplied ones fails (a gap with `--allow-gaps`); the earliest boot
  supplied says which boot came before it. A previous boot's last records
  after its own last checkpoint are covered by the next boot's signature.
  The counter is on the SD card: see "What is NOT protected yet".
- **An earlier card file shortened or extended** after the hub moved on:
  each new card file names the previous file's exact length and last
  record, so this shows, provided the newer file is there. **A missing file
  between two supplied files** is a failure unless `--allow-gaps` is given;
  with it, it is reported as a gap that may hide removed records. **The
  newest files deleted** do not show at all: nothing later names them, and
  the hub will reuse their numbers.
- **Signature malleability**: an ECDSA signature has two valid forms; the
  hub writes only the "low-S" form and the verifier refuses the other, so
  two genuine copies of a log cannot be made to differ in their signatures.
  Logs written by firmware built before 2026-11-18 have the other form on
  about half their checkpoints and now fail those checks; rebuild the hub
  firmware.
- **Attacks that come and go** (WP-14a): every time an attack or jamming
  alarm is raised it gets a record, and every time it ends a `threat_ended`
  record with what it totalled (how long, how many frames, how strong), so
  an alarm tripped briefly and then used for a real attack minutes later
  shows as two episodes. An alarm that flaps (raised three times within a
  minute) is held as one episode until it has been quiet for a minute, so
  it cannot flood the log; the records say when that happens and how many
  raises the episode held. A re-raised alarm counts as not yet seen on the
  screen, so "Mute seen alarms" does not silence it.
- **Which build and which airspace data** (WP-14a): the boot event names
  the build date, the source commit and whether the source had changes
  ("-dirty"); the verifier notes a changed or unknown source. The firmware
  hash still names the exact image. The `pack` event carries the airspace
  pack's SHA-256.
- **No GPS**: records made without GPS time say "time source: none" and
  carry only time since power-on; records without a GPS position say so. The
  verifier counts and prints them. A typed-in position is marked "manual".
- **Untrusted GPS** (WP-12, 2026-11-18): when the hub suspects its own GPS is
  spoofed and has nothing trusted to hold (a spoofer on since power-up), or the
  GPS time is before the firmware's build date or before the latest time the
  hub remembers trusting, it records **no** UTC, with time source
  "untrusted" (code 4) instead of "none", and no position. Only time the hub
  trusted (ten clean epochs in a row, steered by at most 20 ppm against its
  own clock: docs/GNSS-INTEGRITY.md "Time") is ever written as GPS time; held
  time is "free running" with its age. The verifier counts "untrusted"
  records separately.
- **Outside time-stamps** (WP-14c): when the server's operator turns them
  on, the server has public RFC 3161 time-stamp services sign a fingerprint
  of each unit's access log after uploads, automatically, showing the whole
  log existed by then. Only the fingerprint leaves the server. Off unless
  the server is started with `--stamp`. Details in "Outside time-stamps".

## What is NOT protected yet (be plain about this in any report)

1. **Development key.** The signing key was made on a laptop and built into
   the firmware. (The hub signs only through one interface, so a hardware
   key in a secure element can replace it without changing the log format;
   that chip is not chosen yet.) Anyone with the firmware file, the laptop, or the unit
   (reading its flash) can sign records that look like this unit's. Every
   checkpoint says "DEVELOPMENT KEY, NOT EVIDENCE-GRADE", and the verifier
   shows the class the log claims. So today the signatures show the records
   **have not changed since they were signed**; they do not show **only this
   unit** could have signed them. A class byte saying "hardware" would not
   show that either: it is written by whoever signs.
2. **No secure boot or flash encryption.** Someone with the unit could load
   firmware that writes false records. The firmware fingerprint in each
   record shows *which* firmware ran, but without secure boot nothing stops
   a different one from running.
3. **The end of the log.** Records after the newest signature are chained
   but not yet signed; the verifier reports them as "not covered by a
   signature" (the next boot's signed continuity record covers them once it
   exists). Cutting the **newest** boot's log back to a checkpoint, or
   deleting the newest card files, leaves no trace. The boot counter
   (WP-14a) lives in two state files on the SD card: whoever holds the card
   can rewrite them, delete them (the next boot then says the card held no
   counter and starts at 1) or wipe the card. So boots after the newest one
   supplied do not show unless you know the unit's current counter (the hub
   prints `evidence boot counter N` on its console at each card mount; a
   secure-element counter would make that number trustworthy).
4. **Time-stamps: off by default, one built-in service, no renewal.**
   Automatic stamping is built (WP-14c) but stays off until the server's
   operator turns it on. Only one free service (FreeTSA) says on its own
   pages that anyone may use it, so it is the only built-in one: with it
   alone the server warns that it has fewer than two independent services.
   A stamp shows the log existed by the service's time, which is after the
   upload, not when records were written. Old tokens are not re-stamped yet
   (see "Not built yet" under "Outside time-stamps").
5. **The re-decode shares our decoder.** The verifier decodes the raw bytes
   again, but with the same `rid` code the hub uses, so it shows the summary
   matches the bytes, not that our decoding is right. The raw Remote ID
   messages are standard ASTM F3411 messages, so the other side's expert can
   decode them with an independent decoder (e.g. opendroneid-core-c). A
   fully independent decoder in the verifier is future work.
6. **Laptop link.** The P4 has no network; a laptop carries the records.
   A laptop that only uploads does not have the key, so it cannot forge
   records, but it could withhold some: the server shows a gap in the
   middle, not records withheld from the end. The laptop that **built** the
   firmware does hold the key (the key file and the firmware ELF it is built
   into) and can sign records that check out like the unit's.
7. **Bench transport.** Plain HTTP on one laptop is fine for the bench. Use
   the server's TLS option anywhere else.
8. **Access control on the server is a shared token**, and the name in the
   access log is whatever the person typed. Real accounts are later work.
9. **Over-the-air frames.** The node keeps the Remote ID messages, not the
   whole Wi-Fi or Bluetooth packet they came in.

## Outside time-stamps (WP-14c)

**What a stamp shows.** That the unit's access log on the server, and so
every batch uploaded and every view, export and note logged up to that
entry, existed exactly as it is by the time a service outside the owner's
control vouches for. Nothing in it can have been changed, added or removed
after that time without the stamp failing. It does **not** show when the
records were written: they reach the server only when uploaded (minutes
with the live uploader, days if a card is read later). The verifier and the
page show the delay between the upload and the stamp, and between the
newest GPS time in the records and the stamp, and say plainly that the
stamp is not proof of when the records were written.

**When.** After an upload is stored and answered, the server marks the
unit. At most once every 10 minutes per unit while uploads continue, and
once more 2 minutes after they stop, it queues a stamp of the access log's
newest entry. Each service is asked on its own (one being down or slow does
not hold up another); a failed attempt is retried after 1 minute, doubling
to at most 1 hour; a service is sent at most one request every 2 seconds
(FreeTSA allows one per second per address). An upload never waits for a
service and never fails because of one. A server restart picks the queue up
again (`stamp-queue.log`), including an upload stored just before it stopped.

**What leaves the server.** Per stamp, one HTTP POST to the service's URL:

```text
POST /tsr HTTP/1.1
Host: <the service>
User-Agent: rfc3161-client
Accept: */*
Content-Type: application/timestamp-query
Accept-Encoding: identity
Content-Length: 69

<69 bytes: DER TimeStampReq = SHA-256 algorithm id, the 32-byte imprint,
 an 8-byte random nonce, certReq true>
```

The imprint is a SHA-256 over the unit's key id, the entry number and that
entry's hash (EVIDENCE-FORMAT.md, "Export bundle"): it names nothing and
cannot be reversed. No cookies, no referrer, no proxy settings taken from the
environment, no redirects followed. When a certificate chain or revocation
list URL is configured, the server also fetches those files (plain GETs with
the same headers), at most once an hour per service. The service sees this
server's IP address and the times of the requests. A test checks the exact
bytes sent, and passes both in a build of the server alone and in a
workspace-wide build (where another crate turns on the HTTP client's gzip
support, which used to add `Accept-Encoding: gzip`; the headers are now set
explicitly, and a compressed reply is refused in either build). One case
remains build-dependent: a reply both compressed and chunked is unpacked
by a workspace-wide build instead of refused; it must still be a stamp of
exactly the imprint and nonce sent.

**What is kept.** A reply is kept only if it is a granted stamp whose
imprint and nonce are exactly the ones sent; anything else (refused,
unreachable, timed out after 30 s, larger than 256 KiB, compressed, of
unstated length, a redirect, another imprint, another or no nonce, not a
time-stamp reply at all) is recorded as
a failure with its reason and never stored as a stamp. A stamp is a line in
`timestamps.log` with the service's reply as received, the nonce, when it
was queued and sent, and the chain and revocation list fetched then; plus
an audit entry naming the token's SHA-256, so removing a stamp shows.
Export bundles carry all of it, plus the server's list of outstanding and
failed stamps (bundle format 2).

**Turning it on** (the server's operator; it sends fingerprints to third
parties, so it is never on by default):

```text
evidence-server serve --data DIR --token T --stamp \
  --stamp-tsa 'https://freetsa.org/tsr,chain=https://freetsa.org/files/cacert.pem,crl=https://www.freetsa.org/crl/root_ca.crl' \
  --stamp-tsa '<a second service, of a different operator>'
```

- `--stamp` alone uses the built-in list (FreeTSA only) and warns, at
  start-up and on the page, that fewer than two independent services are
  configured.
- Each `--stamp-tsa URL[,chain=URL][,crl=URL]` replaces the built-in list,
  so list every service you want, FreeTSA included. Any RFC 3161 URL works.
  Two services count as independent when their host names differ; whether
  they share an operator is for you to check.
- `--stamp-every MIN` (default 10), `--stamp-idle SEC` (default 120),
  `--stamp-alarm-hours H` (default 24: the page and the server's log warn
  when an upload has waited that long for any stamp).
- `POST /api/devices/{id}/timestamp` asks for a stamp at the next round
  (about a second) instead of waiting for the 10 minutes. The old `--tsa`
  option (one manual stamp of a chain head) is gone; bundles holding such
  stamps still verify.

**Time-stamp services** (checked 2026-11-19 on the operators' own pages;
the server never contacted any of them during development or tests). A
service is built in only if its operator says free general use is allowed.

| service | operator | URL | free general use stated by the operator? | rate limit stated | hashes | certificate chain | built in |
|---|---|---|---|---|---|---|---|
| FreeTSA | an individual (the site names a contact, no company) | `https://freetsa.org/tsr` | **yes**: "Timestamps issued by FreeTSA may be used for commercial purposes." and "FreeTSA provides a publicly accessible timestamping service." ([freetsa.org/freetsa.html](https://freetsa.org/freetsa.html)); best effort, no SLA | "a maximum of one timestamping request per second per IP address" (same page) | SHA-1, -224, -256, -384, -512 ([freetsa.org](https://freetsa.org/)) | root `https://freetsa.org/files/cacert.pem`, TSA `https://freetsa.org/files/tsa.crt`, CRL `https://www.freetsa.org/crl/root_ca.crl`, OCSP `http://www.freetsa.org:2560` | **yes** |
| DigiCert | DigiCert, Inc. | `http://timestamp.digicert.com` | not stated: the page gives the URL and chain only ([knowledge.digicert.com](https://knowledge.digicert.com/general-information/rfc3161-compliant-time-stamp-authority-server)) | none stated | not stated (SHA-256/384/512 responder certificates listed) | on the same page | no: works technically; terms not confirmed |
| Sectigo | Sectigo | `http://timestamp.sectigo.com` | not stated ([sectigo.com](https://www.sectigo.com/resource-library/time-stamping-server)) | asks scripts for "a delay of 15 seconds or more" between requests (same page) | SHA-384 signing by default; request hash chosen by the request | not found | no: works technically; terms not confirmed |
| Sigstore public TSA | the Sigstore community (OpenSSF) | `https://timestamp.sigstore.dev/api/v1/timestamp` | not stated; Sigstore's docs list it among public TSAs ([docs.sigstore.dev](https://docs.sigstore.dev/cosign/verifying/timestamps/)) | none stated | SHA-256/384/512 per its software's documentation | self-signed root; publication point not found | no: works technically; terms not confirmed |
| Certum | Asseco Data Systems S.A. | `http://time.certum.pl` | no: its TSA policy (Polish) treats free use as for code signing (Authenticode) and warns of claims against users without an agreement | none stated | not stated | CRLs and OCSP at `crl.certum.pl`, `ocsp.certum.pl` | no: do not enable without an agreement |
| Apple | Apple Inc. | (third-party lists only) | no: its time-stamp CPS says its users are internal to Apple | - | - | - | no |
| Microsoft | Microsoft | `http://timestamp.acs.microsoft.com` | no: for its Artifact Signing subscribers | - | - | - | no |
| GlobalSign, DFN, SSL.com, Entrust | the CAs | various | not confirmed on operator pages (DFN: a university page says non-commercial use within DFN only; SSL.com: by arrangement; Entrust: no public endpoint found) | - | - | - | no |

The owner chose two independent free services. Only one qualifies today,
so the server ships with one and says so. To have two, the owner can ask
DigiCert or Sectigo (or another operator) for written permission, or use a
paid or EU-qualified provider as the second (docs/EVIDENCE-TIMESTAMPING-DESIGN.md).

**Checking stamps** (`evidence-verify`): for every stamp in a bundle it
recomputes the imprint from the bundle's own access log (a stamp naming an
entry that is missing or was changed, a token over another hash, or a stamp
the access log records but the bundle lacks, **fails**) and says, e.g.:

```text
NOTE  time-stamp 0 (https://freetsa.org/tsr): the access log through entry #2
      (registration, 2 uploaded batches and every view, export and note logged
      before it) existed by 2026-11-19T01:12:36.000Z, by https://freetsa.org/tsr's
      clock (its signature is checked only with --check-timestamps). Stamped
      4 min after the newest upload it covers was stored (server clock); 2 h 5 min
      after the newest GPS time in the records it covers. A stamp shows the log
      existed by then; it does not show when the records in it were written
```

It also lists the server's outstanding and failed stamps and warns when
fewer than two independent services were used. With
`--check-timestamps DIR --tsa-ca ROOT.pem` (one `--tsa-ca` per service,
the root obtained from the service itself) it writes each token, chain and
revocation list to `DIR` and runs
`openssl ts -verify -in DIR/stamp-N.tsr -digest <imprint> -CAfile ROOT.pem -attime <the token's time>`
on each: a signature that does not check is a **failure**. Without
`--tsa-ca` it checks against the chain the server stored and says that
proves less (the server supplied it). Without `openssl` it prints the
commands to run elsewhere. The stored revocation list is written out but
not checked automatically (`openssl crl -in DIR/stamp-N.crl -noout -text`).
No Rust code checks token signatures (no new dependencies); the reader in
`crates/evidence/src/tsp.rs` only takes out the time, imprint and nonce.

**Not built yet, and what that means.**

- **Re-stamping (archive time-stamps, RFC 4998).** A token stays checkable
  only while its service's certificate chain, the revocation status at
  stamping time and its algorithms (SHA-256, the service's signature
  algorithm) can be trusted. The server keeps the chain and a revocation
  list fetched at stamping time when their URLs are configured (built in
  for FreeTSA; it fetches a CRL, not an OCSP response), so a token can be
  checked at its own time after the certificate expires (FreeTSA's current
  one runs to 2040). But if a service's key is ever compromised, or its
  algorithms weaken, tokens taken before then lose their value unless they
  were stamped again, by a fresh service, before it happened. Nothing does
  that yet; the `Stamper` trait in `tools/evidence-server/src/stamp.rs` is
  where a yearly re-stamp would go.
- **OpenTimestamps** (a Bitcoin-anchored stamp with no certificate
  authority): not built; it would be a second kind of `Stamper`.
- Stamps are taken at the server, after upload: the hub has no network.

## Privacy

Remote ID includes the pilot's location: personal data. The console stream
is on by default (it carries what the console JSON already did); the
**server upload is the owner's choice** and nothing uploads unless someone
runs the uploader. Open FPV link identifiers stay out of the summary text
(project policy) but are inside the raw bytes, as they must be for the
record to be complete. **Owner decision 2026-11-15: always keep them.** The
evidence log stores open-link frames' raw bytes in full, whatever the
`show-link-ids` setting; that setting only controls what screens, the phone
page, CSV, CoT and the summary text show. So anyone with the evidence log or
an export bundle can read link IDs; treat both as sensitive. Retention limits are still to be built
(PLAN Phase 13 item 6).

## Using it on the bench

On the hub's USB console (115200 baud):

| key | does |
|---|---|
| `n` then text, Enter | operator note into the log |
| `k` | signed checkpoint now |
| `r` | re-send the current card file over USB |
| `u` | console stream on/off (logged as a setting change) |
| `e` | "stop": shutdown event + signed checkpoint, flushed to the card. Remove the card after the next `hub-p4: evidence ...` line shows `queued` small and `flushes` up by one |

Laptop (from the repository):

```text
# once: a development key for this unit (keep it off git)
target/release/evidence-server keygen --out ~/p4-dev.evkey
cd firmware/hub-p4 && ROTORSCOPE_DEV_KEY=$(cat ~/p4-dev.evkey) cargo fw   # then flash as usual
# without ROTORSCOPE_DEV_KEY the hub uses the published test key (class "public test"),
#   and its screen says "TEST KEY: log not evidence" for as long as it runs (a dev key:
#   "DEV KEY: not evidence-grade"); the hello line names the class too
# a ROTORSCOPE_DEV_KEY that is set but not exactly 64 hex digits (or is zero) stops the
#   build (it used to fall back to the test key silently); the value is never printed

# Registering a unit needs the token: without --token the server refuses it.
# (--open-registration lifts that, for a bench laptop nobody else can reach.)
export EVIDENCE_TOKEN=<a long random string>   # the server and the uploader both read it
target/release/evidence-server serve --data ~/evidence-data          # http://127.0.0.1:8650
target/release/evidence-upload --server http://127.0.0.1:8650 \
    --serial /dev/serial/by-id/usb-1a86_USB_Serial-if00-port0 --replay --register "bench P4"
#  check that the fingerprint it registers matches the one the hub prints at boot
#  ("hub-p4: evidence key ... fingerprint xxxx-xxxx-xxxx-xxxx")
curl -H "Authorization: Bearer $EVIDENCE_TOKEN" -o bundle.json http://127.0.0.1:8650/api/devices/<fingerprint>/export
# the key as the hub printed it at boot ("hub-p4: evidence public key ..."):
target/release/evidence-verify --key <130 hex digits> bundle.json
# or straight from the card in a card reader:
target/release/evidence-verify --key <130 hex digits> /media/<card>/ROTORSCP/EVIDENCE/
# or with a file of known units, one per line: <hex> <label>
target/release/evidence-verify --keys ~/rotorscope-units.keys bundle.json
```

With a token set, every call except an upload needs it (the page asks for
it). The map and list page is at the server's address. Every view, export and
note is written to the access log with the name you type.

Verifier verdicts and exit codes:

| exit | verdict | meaning |
|---:|---|---|
| 0 | VERIFIED | every record links, every signature checks against a key you supplied, a signature covers every record checked |
| 1 | FAILED | the log is not what its signer wrote, or it is signed by a key you did not supply or by the published test key, or a file is missing (without `--allow-gaps`); it names the file, byte, chain and record |
| 2 | (could not run) | bad arguments, unreadable input or keys file |
| 3 | SIGNER CHECKED, WITH GAPS | the signer is a key you supplied and nothing contradicts the log, but some records are not covered by a signature, or (with `--allow-gaps`) are missing; missing records may have been removed |
| 4 | SELF-CONSISTENT, SIGNER NOT CHECKED | no key was supplied: anyone could have made this log |

Every run also prints: "NOT PROVEN: that the log is complete."

## Tested

- Host: `cargo test -p evidence -p evidence-server -p evidence-verify`:
  a log forged from scratch with a made-up "hardware" key (not VERIFIED
  without an outside key; VERIFIED with the forger's key passed; FAILED with
  another key; the same for a bundle), the published test key refused unless
  allowed, a missing middle file failing by default, a checkpoint signature
  turned into its high-S form failing, the verifier's exit codes and verdict
  words; editing any of
  a spread of bytes in every record, deleting any record, swapping any two
  neighbours, cutting a record, rewriting the chain after an edit, files
  shortened or extended after the next file opened, missing files, batches
  altered in transit, re-uploads, gaps filled later, bundles with a record
  edited / deleted / truncated, a batch dropped or reordered, audit entries
  edited / dropped / reordered, the key swapped, a stored batch altered on
  the server's disk; the no-GPS marking. WP-14b (`tests/hardening.rs`,
  `lines`, `plan`, `tsa` unit tests): a bad upload refused while the store
  is held by something else, six simultaneous uploads of the same records
  (one kept), the page's policy and the vendored library's hashes, body
  and field caps, a tail taken only once covered and only up to the named
  record (and not for another unit), a changed batch file kept off the
  map, the newest checkpoint found past a tail, a console line with no end,
  a line split by a timeout, batches split at checkpoints under a size cap,
  an endless time-stamp reply. WP-14c (`crates/evidence/tests/stamps.rs`,
  `tools/evidence-server/tests/stamping.rs`,
  `tools/evidence-verify/tests/timestamps.rs`, fake services on 127.0.0.1
  and canned tokens from two throwaway openssl test authorities, never a
  real service): the exact bytes and headers sent; replies that are
  refused, garbage, over another imprint, with another or no nonce,
  oversized, a 500, a redirect (not followed) or too slow all recorded as
  failures and never kept as stamps; two services independent with their
  own back-off; an upload answered at once while a service hangs; the queue
  re-read after a restart; the coalescing rule with a fake clock; the export
  carrying stamps, outstanding and failed ones; the checker failing a cut or
  edited access log, a token over another hash, a removed stamp, a broken
  token signature and an untrusted root; version 1 bundles still verify;
  the DER reader on malformed input and 5000 random mutations of a token;
  a three-boot continuity chain (fixed in WP-14c: the checker now follows
  the boots' continuity records, not their ids). End to end with the binaries: a two-boot
  card through `evidence-upload` -> server -> export -> `evidence-verify`:
  VERIFIED, 0 gaps, boot 1's tail sent as its own batch; the 2026-11-18
  bench logs (`final-replay`, `spoofboot-stream`) upload and verify, and
  `spoofboot-stream`'s two records after its last checkpoint stay on the
  laptop (no later boot names them).
- Bench, 2026-11-15, P4 CrowPanel (chip v1.3) with the `fakedrone` build,
  two real nodes via the gateway, no GPS attached: detections went hub ->
  USB -> laptop uploader -> server -> export bundle -> verifier: **VERIFIED**
  (255 records, 2 boots, 5 signed checkpoints). That run used the verifier
  before 2026-11-18, which checked the signatures against the device key
  named in the bundle itself; it showed the records were self-consistent
  and unchanged since signing, not who signed them. Copies of the bundle with a
  digit of a drone latitude changed, a record removed, two records swapped,
  an access-log name changed, or the last checkpoint removed: all
  **FAILED**, each naming the batch and record. Console replay of the card
  file gave the same bytes as the live stream. Over TLS (self-signed bench
  certificate) and with a DigiCert RFC 3161 time-stamp (`openssl ts -verify`:
  OK).

Cost on the P4: about 400 bytes per detection record (base64 on the console
about 550 bytes); building one takes up to 1.8 ms; signing a checkpoint
33 ms once a minute (software P-256 at 360 MHz); SD steps for the evidence
file at most about 18 ms (a flush, same as the CSV's); opening the evidence
file at mount about 50 ms (400 ms the very first time, when the folder is
created). Records queue in 256 KB of PSRAM (about 10 minutes of one
detection a second with no card). Main loop rate unchanged (about 520-550
passes a second with the fake drone).

## The hardware key (design; nothing burned)

**Do not do any of this without the owner's go-ahead for that specific
unit.** eFuses cannot be unburned. Never on the bench boards.

- **P4 (the evidence writer).** The ECDSA peripheral signs with a P-256 key
  held in an eFuse key block whose purpose is set to ECDSA_KEY and which is
  read-protected: software can ask for signatures but can never read the
  key. Plan: at first setup the firmware draws 256 random bits from the
  hardware RNG, burns them into a free key block with ECDSA_KEY purpose and
  read protection (so the key never exists outside the chip), then asks the
  peripheral for the public key ("export public key" mode) and prints it
  and its fingerprint for registration. A `HwSigner` implementing the
  `Signer` trait (`crates/evidence/src/sign.rs`) feeds the peripheral the
  SHA-256 digest it already computes; key class becomes 2 and nothing else
  in the format changes. To check first, on this pre-v3 (v1.3) silicon:
  that the ECDSA peripheral and its export mode work as documented; esp-hal
  has no P4 ECDSA driver yet (we would write a register-level one).
- **S3 / C5.** The S3's digital-signature (DS) peripheral is RSA only, so an
  S3 signer would need a new signature algorithm code (a format version 2).
  The C5 has an ECDSA peripheral like the P4's. Neither writes evidence
  today; the P4 does.
- **Secure boot v2 and flash encryption** on kit units: needs our own signed
  bootloader in place of Elecrow's, a signing key kept offline, and burning
  the secure-boot and encryption eFuses. With both, only our signed firmware
  runs and the flash cannot be read, which is what makes the firmware
  fingerprint in each record meaningful. Per-unit owner permission.

## Uploading through the web head (later)

The P4 has no network; the S3 web head has Wi-Fi station mode but almost no
free RAM (about 1 KB of stack margin with the web head features), so a TLS
upload client does not fit on it. Options, best first:

1. **A C5 as an uplink node** (384 KB RAM): the P4 sends batches to it over
   a UART or ESP-NOW, it uploads over TLS in station mode. Cleanest.
2. **The S3 as a pass-through**: a new link frame (protocol 7, chunked, since
   `MAX_FRAME` is 320 bytes) carries batches to the S3, which offers them on
   its phone page; the phone or laptop on the hotspot uploads them. No new
   client or buffers on the S3; the hub's signatures protect the content.
3. Plain-HTTP upload from the S3 (signed batches keep integrity but not
   confidentiality): only if 1 and 2 fail.

## Custody record (template)

One sheet per hand-over of the unit or its SD card. Keep with the export.

```text
Unit: ______________  key fingerprint: xxxx-xxxx-xxxx-xxxx  (printed at boot)
Firmware SHA-256 (first 16): ________________  SD card serial/label: ________

Date/time (UTC) | From (name, signature) | To (name, signature) | Item (unit / SD card / bundle file)
                |                        |                      | Condition / seal no.
----------------+------------------------+----------------------+------------------------------------
                |                        |                      |

Exports made:
Date/time (UTC) | By | Bundle file name | Bundle SHA-256 (from verifier or X-Bundle-SHA256) | Verifier result
```

## Methods document (outline, to write with the lawyer)

1. Purpose and scope: what the unit detects (Remote ID broadcasts, DJI
   DroneID, Wi-Fi drone networks, open FPV links) and what it does not.
2. Equipment: hub and node hardware, firmware versions and fingerprints,
   antennas, GPS receiver, the server and verifier versions.
3. How a detection is made: radio reception, node decoding, the link to the
   hub, what is stored (this document and EVIDENCE-FORMAT.md).
4. Time and position: GPS, PPS, free-running clock, how each record says
   which it used; known uncertainty of each.
5. Integrity: hash chain, signed checkpoints, key class, card-file linking,
   server checks, access log, outside time-stamps.
6. Known error rates and limits: bench and field test results (detection
   rates, false matches, range), "a detection shows a broadcast was received,
   not who was flying", spoofable broadcasts, development-key caveats while
   they apply.
7. Handling: setup, registration, custody record, export and verification
   steps, retention.
8. Validation: how to reproduce the tests; the verifier's own test suite.
9. Versions and change log.
