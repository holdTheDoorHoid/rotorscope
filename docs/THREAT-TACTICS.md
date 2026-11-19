# Rotorscope threat profiling: hostile-drone TTPs and detectable signatures

Research date 2026-11-18. Claims carry a source URL. Lines marked **[inference]** are engineering judgement, not sourced. Most vendor material is marketing; treat vendor numbers as claims.

---

## 1. Use cases and what the flight looks like

### 1.1 Prison contraband delivery
- High volume and repeated: HMP Manchester logged 220 drone sightings in a year; prisoners burned holes in cell windows so drones could deliver straight to the window. https://www.gbnews.com/news/manchester-gangs-smuggle-drugs-prison-windows-drones , https://hmiprisons.justiceinspectorates.gov.uk/news/drones-dropping-drugs-and-weapons-into-high-security-prisons-are-a-threat-to-national-security
- Georgia DOC recorded 384 drone incidents in one year. https://correctionalnews.com/2025/02/03/the-role-of-drones-and-counter-uas-efforts-to-combat-contraband-an-overview-of-the-drone-detection-and-deterrence-symposium-at-the-2025-aca-winter-conference/
- Deliveries go to the yard or to specific windows; operator is outside the perimeter. https://sentrycs.com/the-counter-drone-blog/the-changing-landscape-of-contraband-delivery-in-prisons/
- Staff are told to block the drop zone the moment a drone is detected, so time-to-alert matters more than classification. https://correctionalnews.com/2025/02/03/the-role-of-drones-and-counter-uas-efforts-to-combat-contraband-an-overview-of-the-drone-detection-and-deterrence-symposium-at-the-2025-aca-winter-conference/

Signatures we could detect:
- Short transit from outside to a point inside the fence, then a **low hover beside a building face** (window height), then a straight exit. [inference from window-delivery reports]
- **Altitude dip-and-recover inside a zone**: descend, hold a few seconds, climb. [inference]
- **Operator outside, drone inside** (RID/DroneID operator position). Repeat visits on many nights from operator spots that cluster together.

### 1.2 Stalking, voyeurism, harassment
- Toronto: drone hovering just outside a window; charged with voyeurism and harassment ("watching and besetting"). https://globalnews.ca/news/11284972/drone-charges-toronto
- Orem, Utah: pilot hovered and waited for a light to come on in a home, then flew to that window; pilot operated from a truck. https://www.eastidahonews.com/?p=301858
- Cranston RI: drone at a bathroom window, flown by a neighbour. https://dronedj.com/2023/06/26/rhode-island-police-drone-voyeurism/
- Rockingham County VA: the same drone came back to one victim repeatedly for months; this led to stalking convictions. https://www.police1.com/drones/what-happens-when-a-drone-stalks-you-at-first-nothing-but-a-va-woman-found-a-way-to-fight-back

Signatures:
- **Low-altitude hover near a structure** (<~15 m AGL), often at night. [inference: window height]
- **Repeat visitor**: same RID/DroneID serial, or same operator location, over many days.
- **Operator very close** (neighbour, under ~200 m), or in a vehicle.
- In handheld mode, **the drone follows the user**: its position keeps tracking the user's GPS as they move. [inference]

### 1.3 Pre-burglary casing
- Police in the UK, NY/NJ, Ireland and NZ warned about drones photographing back gardens and grounds that can't be seen from the road. The evidence is mostly from 2015–2019 and partly anecdotal. https://gulfnews.com/world/europe/burglars-using-drones-to-find-soft-targets-police-warn-1.1517629 , https://donegaldaily.com/2016/12/02/garda-issue-warning-as-drones-used-to-case-houses-and-yards
- A former burglar described thieves filming backyards from a car down the street. https://nzherald.co.nz/nz/drones-the-new-tool-in-burglars-arsenal/F56QBTON6Y57DPLYPTE26JARQE

Signatures: slow passes over the rear of a property, short hovers over each of several neighbouring properties in a row ("lawnmower" survey), operator stationary in a vehicle on the street, a visit some days before an incident. [inference]

### 1.4 Critical infrastructure (substations, airports, plants)
- CISA indicators: repeated unauthorised flights, **repeated fly-overs or prolonged low-altitude hovering** (reconnaissance), **probing security by flying into sensitive areas and watching the response**, visible modifications or added items, and FAA violations such as flying above 400 ft AGL. CISA says one indicator alone does not mean illegal activity. https://www.cisa.gov/sites/default/files/2025-08/Suspicious_UAS_Identification_Poster_082025_508.pdf , https://www.cisa.gov/sites/default/files/publications/Responding%20to%20Drone%20Calls%20Guidance%20for%20Emergency%20Communications%20Centers.pdf
- 2020 Pennsylvania substation: a DJI Mavic 2 carried a copper wire on nylon cords, likely meant to short-circuit equipment. **The camera and memory card had been removed** (forensic denial). It was found on a roof next to the substation. https://www.twz.com/43015/likely-drone-attack-on-u-s-power-grid-revealed-in-new-intelligence-report
- Denmark, Sept 2025: drone incursions closed Copenhagen (~4 h), Aalborg (~3 h, about 22:00–01:00) and Billund airports. Officials called it a "systematic" hybrid operation by a professional actor, with **several sites hit over several nights**. Drones at Aalborg flew **with lights on**. https://www.aljazeera.com/news/2025/9/25/denmark-shuts-second-airport-in-a-week-more-unidentified-drones-spotted , https://www.travelextra.ie/denmarks-aalborg-airport-closed-for-three-housers-after-further-drone-sitings/

Signatures: perimeter-following flight (see the "perimeter" intent class in §3), multiple passes, hovering over specific assets, altitude above 120 m, night flights, several drones over hours, and a **probe-and-watch** pattern (cross the fence, retreat, return after the security response). Payload cue: a drone that lands or stays very low near equipment.

### 1.5 Cartel and border counter-surveillance
- Cartels use drones as overhead lookouts to spot agents and troopers, look for crossing points, and map patrol movements and **response times**. https://www.yahoo.com/news/texas-border-officials-using-drones-152830666.html , https://www.foxnews.com/world/cartels-use-drones-scout-border-agents-map-smuggling-routes-expert-warns
- Scale: CBP is cited at more than 42,000 drone flights near the border in FY25 (partisan outlet). https://www.breitbart.com/border/2026/02/15/cbp-cartels-flew-42000-drones-near-u-s-border-in-fy25 . Weaponised use is so far mostly inside Mexico. https://foxbaltimore.com/news/nation-world/border-patrol-agents-warned-cartels-might-attack-with-weaponized-drones-reports-southern-border-security-cbp-arizona-mexican-drug-cartels-trump-illegal-immigration-crackdown

Signatures (relevant to the handheld/moving-user case): **a drone that keeps station over a moving person or vehicle**. Its position follows the user's track with a lag. It stays high (often 60–120 m) and hovers for long periods. The operator is across a boundary or far away. [inference]

### 1.6 Protest and event surveillance
- R&S ARDRONIS has been used at large public events to locate drone pilots, who were then apprehended. https://rohde-schwarz.com/_250881.html
- Signatures: long hover or slow orbit over a crowd, often long dwell with battery swaps (the same serial reappears after about a 5 min gap). [inference]

### 1.7 Weaponised drones (Ukraine, Israel, cartels)
- **Spotter, then striker**: a Mavic-class recon drone finds the target and streams video, and an FPV is launched. Accounts give "a few minutes" and about four minutes of flight to target, and a reported best detect-to-destroy time of about 5 minutes. https://cepa.org/article/suicide-squads-and-the-fight-for-ukraine/ , https://kyivindependent.com/head-first-into-the-future-inside-the-race-to-win-the-drone-war-in-ukraine , https://pravda.com.ua/eng/articles/2024/12/23/7490268
- **FPV speeds**: service FPVs fly at about 150 km/h. https://militarnyi.com/en/news/new-fpv-drones-with-enhanced-speed-of-150-km-h-enter-service-with-ukrainian-defense-forces . Reported strike dives exceed 170 km/h. https://mezha.net/eng/?p=3067472 . An interceptor-class record is 325 km/h. https://euromaidanpress.com/2024/09/16/ukrainian-experimental-interceptor-drone-hits-325-km-h/
- **Fibre-optic FPV**: there is **no RF emission** to detect, it is immune to jamming, and strikes at up to about 50 km are claimed. It is slower than radio FPV, and the operator sits at the end of the cable. Ukraine counters it with **sound** and IR. https://euromaidanpress.com/2025/02/03/russias-fiber-optic-drones-dodge-jamming-but-ukraine-hunts-them-with-infrared-and-sound/ , https://euromaidanpress.com/2025/12/30/ukrainian-drones-fly-50-km-on-fiber-optic-cable-no-radio-no-detection-video/
- **Low-altitude, terrain-following approach**: the Hezbollah drone that hit the Binyamina base (Oct 2024) was lost by Israeli tracking after it **dropped to low altitude**. It was launched **alongside other drones and a rocket barrage** to saturate defences, and its route changed altitude and direction often. Because tracking was lost, **no local warning was issued**. https://businessinsider.nl/hezbollah-evaded-israeli-defenses-with-a-drone-that-killed-4-soldiers-raising-uncomfortable-questions/ , https://www.ynetnews.com/article/sjrod45kje
- Shahed-class terminal dives exceed 300 km/h. This is out of scope for ESP32 nodes but shows that the terminal phase of an attack lasts seconds. https://euromaidanpress.com/2025/06/29/why-cant-ukraine-stop-russias-shahed-drones-anymore/

Signatures:
- **Loiter, then a new fast inbound contact**: a hovering or orbiting recon drone, followed within about 1–10 min by a second, faster contact heading at the same spot. **This pairing is a stronger threat signal than either contact alone.**
- **FPV video carrier rising fast** (5.8 GHz analog/digital, or ELRS link RSSI), with no RID. At 150 km/h, a drone first heard at 1 km arrives in about 24 s, so any approach alert must fire within seconds. [arithmetic]
- **Rotor sound with no RF** points to a fibre-optic or autonomous drone. That mismatch is a threat signal in its own right.
- **Track lost at low altitude near the site** must not clear the alert. Hold it ("last seen inbound"), because that is exactly the failure at Binyamina.
- **Several contacts at once**, or a contact appearing during jamming or GNSS spoofing, suggests saturation or a decoy.

### 1.8 Evasion behaviours across all categories
- An operator launches from one spot, then **moves in a vehicle**. D-Fend flags this as possible deliberate evasion. https://www.unmannedsystemstechnology.com/feature/enhancing-counter-uas-decision-making-through-drone-identification/
- Camera and memory card stripped (PA substation, above). Non-compliant or modified drones broadcast no RID. **The RID or DroneID stream stopping while the RF link stays up, or the serial changing mid-track**, is a strong anomaly. [inference; RID is required at 1 Hz, see §5]
- DJI DroneID carries the pilot location and home point, and DJI says the pilot location can't be switched off (forum anecdotes disagree). https://dji.com/fi/newsroom/news/dji-unveils-technology-to-identify-and-track-airborne-drones , https://dronexl.co/2022/10/17/80000-drone-ids-dji-aeroscope-data-leak

---

## 2. How commercial C-UAS products and guidance frame alerting

- **Dedrone**: user-drawn alert zones, and tiers by distance (low = well outside the perimeter, medium = near the perimeter or approach paths), with response matched to the tier. It markets "detecting their behavior". https://www.dedrone.com/blog/theres-a-drone-at-my-critical-infrastructure-site-now-what , https://dedrone.com/products/software
- Dedrone through the Raven integration has two alert classes: "Alert" (inside a restricted area, high priority) and "Warning" (lower priority, can be disabled). https://support.ravencontrols.com/raven-academy/dedrone-integration
- **DroneShield DroneSentry-C2**: custom zones, threat prioritisation, and a "ThreatAI" rating fused from all sensors plus user input. The Q2-2026 update auto-classifies **Friendly / Neutral / Hostile / Unknown** from serial number and Remote ID, and suppresses background contacts. https://securitybrief.co.uk/story/droneshield-adds-threat-ranking-in-q2-software-update , https://www.droneshield.com/products/dronesentry
- **D-Fend EnforceAir**: polygon or circular protection and alert zones, with a circular default centred on the unit, plus **alert areas that move with the protected asset** (a direct precedent for the "bubble" idea). It prioritises by prevalence, payload capacity and range, and shows a threat level with "possible intentions of the pilot". https://blog.dronestagr.am/2021/10/18/d-fend-updates-enforceair-new-features/ , https://militaryembedded.com/unmanned/counter-uas/d-fend-solutions-upgrades-enforceair2-c-uas-with-user-interface-enhancements-ai-based-mitigation-engine-and-new-disrupted-environments-mode
- **AirSight AirGuard**: analytics for **repeat offenders**, flight times, **drone-to-pilot distance**, max altitude, and breached zones. https://www.unmannedsystemstechnology.com/feature/enhancing-counter-uas-decision-making-through-drone-identification/
- **SkeyDrone**: groups many per-detection alerts into one "threat" with a level, which avoids alert spam. https://www.unmannedairspace.info/uncategorized/skeydrones-drone-radar-moves-from-passive-monitoring-to-active-risk-management/
- **CISA**: judge indicators in context and set procedures to separate routine from suspicious activity. See §1.4 for the indicator list. https://www.waterisac.org/tlpclear-cisa-releases-three-unmanned-aircraft-systems-uas-security-guidance-products

Rule types the industry offers: zones (polygon or circle, fixed or moving), tier by distance, friend/foe lists keyed on serial or RID, operator location, repeat offender history, max altitude, drone-to-pilot distance, and alert aggregation. **I found no public documentation of explicit dwell, orbit or approach-rate rules** in these products; they appear only as marketing language. Rotorscope would be ahead here, not copying.

---

## 3. Academic work on intent and trajectory anomaly (lightweight methods)

- Perrusquía et al. 2024 classify drone trajectories into **mapping (lawnmower), point-to-point, package delivery, perimeter** using a BiLSTM, with an autoencoder for novelty detection. Too heavy for an MCU, but these classes are a useful set of labels. https://doaj.org/article/c94dfbb8ffe34384b5281ab3163e9bcb
- A 2026 follow-up uses six intent classes and lists **hover duration and orbit count** as key features. https://arxiv.org/pdf/2606.11687
- **Maritime loitering (MCU-friendly)**: a score built from course-change rate, speed, heading versus course, **area enclosed by the track**, and **start-to-end distance**, ranked by Isolation Forest. It reports 92% F-score with no training on normal traffic. https://peerj.com/articles/cs-1572
- **Cumulative heading change** over a sliding buffer, compared with a threshold, used as an online turn detector. https://arxiv.org/pdf/1601.06041 . A full orbit is a cumulative heading change of at least 360° with a bounded radius. [inference]
- **CPA/TCPA**: under constant velocity, TCPA = −(r·v)/|v|² and DCPA = |r + v·TCPA|. Alarm when DCPA < D and 0 < TCPA < T, and keep the alarm latched until the condition clears. Use a cheap range gate first. Constant-velocity extrapolation gives false alarms on parallel or curving tracks. https://arxiv.org/pdf/2508.08264 , https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/6249241 , https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/11945560
- Loitering has no standard definition, and benign loiterers exist (pilot boats, survey craft). https://openaccess.thecvf.com/content/WACV2024W/RWS/papers/Nunez_Identifying_Loitering_Behavior_With_Trajectory_Analysis_WACVW_2024_paper.pdf

MCU feature set (all O(1) per update over a ring buffer of about 60–300 RID fixes): radial velocity toward the protected point; DCPA/TCPA; cumulative heading change; bounding-circle radius and track enclosed area (shoelace formula); straightness ratio (net displacement / path length); time inside zone; vertical-speed sign changes. [inference]

---

## 4. Non-positional sensors: inferring "approaching"

- **RSSI**: shadowing is roughly Gaussian in dB, so the distance error is **multiplicative and grows with range**. Single readings are unreliable at long range. https://arxiv.org/pdf/2402.09810 , https://par.nsf.gov/servlets/purl/10517499 . Outdoor ground tests found about 3.4 m mean error to 65 m and 6.7 m to 100 m (ZigBee). https://www.iieta.org/node/21120 . RSS is noisy and affected by multipath, and RF alone doesn't prove the emitter is a drone. https://home.cs.colorado.edu/~rhan/Papers/2019_DRONET_RF-based_Localization.pdf
- Practical consequence [inference]: an absolute RSSI-to-distance mapping is poor, but a **sustained monotonic rise** (for example, the median-filtered slope over 5–10 s exceeding about 6 dB, equal to halving the distance under free-space 20·log10) is a usable "closing" cue. Antenna orientation as the drone banks and yaws can swing RSSI by more than 10 dB, so require persistence. Cross-check several nodes: **a rise on one node with a fall on another** suggests a crossing track, not an approach.
- **Sound**: level drops about 6 dB per doubling of distance. https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/10748434 . The unknown source level (about 70–80 dBA at 1.5 m varies with drone and throttle) turns directly into range error. https://engrxiv.org/preprint/download/6810/11135/9456 . Lower harmonics carry farther. https://arxiv.org/pdf/2606.29589 . Bearing (TDOA arrays) is more reliable than range. https://acta-acustica.edpsciences.org/articles/aacus/abs/2026/01/aacus250134/aacus250134.html
- Consequence [inference]: use the trend in SPL and blade-pass level, not the absolute level. A rise of about 6 dB or more in 5 s plus an increasing blade-pass frequency (throttle up, or Doppler shift on approach) indicates a likely closing drone. Wind and nearby traffic are the main false sources.
- No source quantified the real-world reliability of RSSI or SPL trend approach detection. **This is a field-test item.**

---

## 5. Reference numbers

| Item | Value | Source |
|---|---|---|
| RID broadcast rate | at least 1 message/s (Location message) | https://mavlink.io/en/services/opendroneid.html |
| RID horizontal accuracy | within 100 ft (30 m), 95% | https://gleim.com/aviation/faraim/index.php?fullTextNum=89D |
| RID operator fields | operator lat/lon/alt + type (Dynamic/Fixed) + timestamp | https://cdn.standards.iteh.ai/samples/112932/7efe6944332748d4a3c63213c034ecd2/ASTM-F3586-22.pdf |
| DJI Mini 4 Pro | 12 m/s normal, 16 m/s sport; 5 m/s climb and descent | https://www.dji.com/mini-4-pro/specs |
| Mavic 3 Pro | about 21 m/s (75.6 km/h) | https://pickdrones.com/compare/dji-mini-4-pro-vs-dji-mavic-3-pro/ |
| Military FPV | about 150 km/h cruise; dives over 170 km/h | militarnyi / mezha above |
| FAA ceiling indicator | above 400 ft AGL is a violation indicator | CISA ChemLock / ECC guidance |

Not found within budget: Avata 2 / hobby FPV top speeds (commonly quoted at over 100 km/h, unverified here), and photography versus drop hover altitudes (no published figures; treat as inference).

---

## 6. Rule candidates

Sensor key: **P** = position (RID, DroneID, MAVLink), **R** = RF strength only (FPV video, ELRS/SiK, Wi-Fi), **S** = sound, **M** = multi-node.

| # | Rule | Data | Trigger sketch | Main false alarms |
|---|---|---|---|---|
| 1 | Zone entry (fixed polygon or circle) | P | drone inside zone | neighbours' hobby flights, police or DFR drones, survey work |
| 2 | Moving bubble (radius around handheld GPS) | P (+ hub GPS) | distance to user < R | drones you walk past; the user's own drone (allowlist it) |
| 3 | Operator-in-zone / operator near | P (operator fields) | pilot inside zone, or pilot < X m | crowded parks, events |
| 4 | Drone in, operator out | P | drone in zone and operator outside | legitimate contractors (allowlist) |
| 5 | Approach / beeline | P | radial speed toward user > v, straightness > 0.85, DCPA < R, TCPA < T | delivery drones transiting past (DCPA filters), RTH paths |
| 6 | Fast inbound (FPV-class) | P or R | speed > 25 m/s toward user, or RSSI slope steep and sustained | racing and freestyle sessions, wingers |
| 7 | RF closing (no position) | R, M | median RSSI rise > ~6 dB over 5–10 s on 1 or more nodes, not falling on others | bank-angle and antenna swings, the user walking toward the pilot |
| 8 | Acoustic closing | S | SPL rise and blade-pass rise over 5 s with drone classifier on | wind, traffic, lawn equipment, the user walking toward the source |
| 9 | Sound without RF ("dark drone") | S + R | drone acoustics with no RID, DroneID or RF link | the RF sensor missing a protocol or band; out-of-band links |
| 10 | Loiter / dwell | P | time in zone > N min, or bounding radius < r for > N min | real-estate and inspection photographers, roof surveys, hobby hovers |
| 11 | Orbit / circling | P | cumulative heading change ≥ 360°, radius < ~150 m, centre near an asset or the user | real-estate orbits ("POI" mode), cinematic shots, mapping |
| 12 | Perimeter-following | P | track stays within a band at the fence for > X% of the perimeter | utility line or fence inspection |
| 13 | Low hover near structure | P (alt AGL) | altitude < ~15 m and hover > 10 s within the zone | roof and gutter inspection, landing |
| 14 | Payload-drop profile | P | descend > ~10 m, hold, climb within 60 s, in zone | photographers dropping for a shot, landing aborts |
| 15 | Repeat visitor | P (serial / operator location), R (fingerprint) | same ID or operator cluster seen on ≥ 3 days in a 14-day window | a neighbour who flies daily; delivery routes |
| 16 | Probe-and-watch | P | zone breach, exit, re-entry within 30 min | hobbyists who don't know the boundary |
| 17 | Spotter then striker | P/R/S, M | loiter or orbit contact, then a new fast inbound within 10 min | coincidental second hobbyist |
| 18 | Following the user | P + hub GPS | drone–user distance stays < D while the user moves > 200 m | follow-me selfie drones (the user's own), group rides |
| 19 | Swarm / multiple | any | 3 or more simultaneous contacts, or contacts appearing during jamming or GNSS spoofing | events, light shows, drone-racing meets |
| 20 | Identity anomaly | P + R | RID stops while RF link persists; serial changes mid-track; RID position jumps more than physically possible | weak reception gaps (require a hold-off), multi-drone confusion |
| 21 | Night / dark flight | P + time | flight between local sunset and sunrise in zone (no lights cue available to RF) | licensed night ops, police |
| 22 | Altitude violation | P | > 120 m AGL | RID altitude errors, launches from hilltops |
| 23 | Operator mobile | P (operator track) | operator speed > 5 m/s while the drone is airborne | follow-me from a vehicle, boats |
| 24 | Lost-track hold | any | an inbound contact disappears inside R: hold the alert for N s rather than clearing it | dropout as the drone flies behind cover |

**Scoring approach (suggested)**: give each rule a weight, sum the weights per track, and raise one aggregated alert per track (the SkeyDrone pattern) with hysteresis. Raise the weight of rule combinations (for example 5+9, 10+15, 17) rather than single hits, in line with CISA's "one indicator is not enough". Keep an allowlist keyed on serial for friendly drones (DroneShield Friendly/Neutral/Hostile/Unknown).
