# coverage: who listens to what

The hub runs this every 2 seconds. It looks at the node table (which nodes
are there, what each can do, what each is doing) and decides one job per
node. The hub then sends each node its job (`link::Command::Assign`).

## The ladder

Jobs are handed out from the top of this list down, one node per line. A
node only gets a job it says it can do (the `can` list in its Status), and a
node that is a gateway never gets one.

| # | Job | What the node does |
|---|---|---|
| 1 | Wi-Fi 2.4 GHz Remote ID | Channel 6 most of the time (150 ms of every 200), short visits to 1 and 11 |
| 2 | Bluetooth Remote ID | Its own built-in plan |
| 3 | Wi-Fi 5 GHz | Channel 149 most of the time, short visits to 153, 157, 161, 165 and 36, 40, 44, 48 |
| 4 | DJI DroneID | 5756.5, 5776.5, 5796.5 MHz, then 2414.5, 2429.5, 2444.5, 2459.5, 2399.5 MHz, 700 ms on each (a DroneID burst comes every ~640 ms, so one dwell always catches one) |
| 5 | Analog FPV / RF scan | Its own built-in sweep (2.4 GHz analog video channels, then every 5.8 GHz channel it reaches) |
| 6 | Second DroneID | Splits the bands: the first keeps 5.8 GHz, the second takes the 2.4 GHz list |
| 7 | Second Wi-Fi 5 GHz | Splits: the first parks on 149, the second sweeps 36-48, 153-165 and 169-177 (100 ms each) |
| 8 | Second Wi-Fi 2.4 GHz | Splits: the first parks on 6, the second sweeps 1-5 and 7-14 (80 ms each) |
| 9 | Second FPV | Splits the 46 FPV channels a C5 reaches (15 at 2.4 GHz, 31 at 5.8 GHz) in half |
| 10 | Second Bluetooth | A duplicate: Bluetooth has no channels to split |

More nodes than lines: each keeps whatever job it has (more Bluetooth and
Wi-Fi nodes just add more chances to hear a packet; a third DroneID, Wi-Fi or
FPV node takes a share of the sweep). The list and every channel table are
at the top of `src/plans.rs`; change them there.

**Aircraft (aero) nodes.** An LR2021 aero node (docs/AERO-NODE.md) is not on
the ladder: it can do nothing else and nothing else can do its job. It always
gets the aircraft job. Alone, it takes turns: 978 MHz UAT for 10 s, FLARM
(US hopping) for 10 s, OGN trackers (US hopping) for 5 s, and also 1090 MHz
for 10 s when the hub has no ADSBee hearing 1090 already (the hub says so,
`Coordinator::aero`). Two or more aero nodes split that list, so each mode
can get a radio of its own; a node with one mode stays on it. The hub's
console command `a` can pin every aero node to one mode instead (until
reboot). Tables: `AERO_*` in `src/plans.rs`.

**Scouting.** Whatever its plan, every Wi-Fi node leaves it for 105 ms every
3 s to visit one channel of its band(s) the plan skips, round robin
(`plans::scout_channel`): 2.4 GHz 1-14, 5 GHz 36-177 including the DFS
channels and 169-177. Nothing can hide on a channel the plan skips (a drone's
network, an evil twin, a beacon flood); it costs 3.5% of air time. The node
does this on its own; the hub's plans do not change.

## Keeping things calm

* **Nobody moves without a reason.** Among all the ways to fill the most
  important lines, the coordinator picks the one that changes the fewest
  nodes' jobs. Changing a job means a reboot of a few seconds on that node.
* **One job change per minute per node, at most.** A node that just changed
  job keeps it for 60 s whatever happens.
* **A node is gone after 15 s of silence.** Its line goes to someone else.
  If it comes back, it is put back where it was if that is still useful.
* **Plan numbers change only when the plan changes.** The number is made
  from the plan itself, so it also survives a hub restart.
* **Same table in, same answer out**, whatever order the nodes are listed in.

## ESP-NOW nodes and channel 6

A node that reports through the ESP-NOW gateway can only send while it is
on 2.4 GHz channel 6 (the gateway's channel). So every Wi-Fi plan for such a
node gets a 40 ms visit to channel 6 at least once a second (`plans.rs`,
`ESPNOW_MAX_AWAY_MS`). A 5 GHz node therefore leaves 5 GHz for a moment
once a second; node-wifi switches band per hop for that (branch `node-roles`).

The raw-capture jobs (DroneID, FPV) do not get that visit in their plan:
their plan is radio frequencies, not channels. A relayed DroneID or FPV node
must itself retune briefly to channel 6 when it has something to send. That
is also node-side work, not built yet.

## What it does not do (yet)

* It does not know where nodes are, so two nodes in the same room count as
  much as two nodes at opposite ends of a field.
* It does not watch whether a job finds anything; the plans are fixed.
