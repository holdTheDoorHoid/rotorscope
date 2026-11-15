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
| 5 | Analog FPV / RF scan | Its own built-in sweep |
| 6 | Second DroneID | Splits the bands: the first keeps 5.8 GHz, the second takes the 2.4 GHz list |
| 7 | Second Wi-Fi 5 GHz | Splits: the first parks on 149, the second sweeps 36-48 and 153-165 (100 ms each) |
| 8 | Second Wi-Fi 2.4 GHz | Splits: the first parks on 6, the second sweeps 1-5 and 7-13 (80 ms each) |
| 9 | Second FPV | Splits the 47 FPV channels (bands A, B, E, F, R, L) in half |
| 10 | Second Bluetooth | A duplicate: Bluetooth has no channels to split |

More nodes than lines: each keeps whatever job it has (more Bluetooth and
Wi-Fi nodes just add more chances to hear a packet; a third DroneID, Wi-Fi or
FPV node takes a share of the sweep). The list and every channel table are
at the top of `src/plans.rs`; change them there.

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
