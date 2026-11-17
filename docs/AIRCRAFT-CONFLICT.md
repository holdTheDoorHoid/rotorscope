# Drone near a crewed aircraft

## In plain language

Rotorscope tracks drones and crewed aircraft (planes, helicopters, gliders)
side by side. Once a second the hub compares every drone with every aircraft
and warns when they are close, or when they are on course to pass close
within the next minute. A medevac or police helicopter coming low over a park
where a drone is flying is the case this is for.

There are three levels:

| Level | When |
|---|---|
| **Warning** | Within 300 m and roughly the same height now, or predicted to pass within 300 m in the next 20 s |
| **Caution** | Within 1 km and roughly the same height now, or predicted to pass within 600 m in the next 45 s |
| **Advisory** | Within 3 km and within about 1,000 ft of height |

"Roughly the same height" means within 150 m (about 500 ft).

What you see:
- At Caution or Warning the status bar shows **AIRCRAFT** in a box. It takes
  priority over JAMMED and ATTACK.
- The health page gets a **NEAR** line saying how far the drone is from which
  aircraft and the height difference.
- Each pair is logged once per level in the evidence log (code 22,
  `aircraft_conflict`) and printed on the console as `AIRCRAFT ...`. If the
  pair gets more serious it is logged again, and if it is still close five
  minutes later it is logged again too.
- The 1 Hz JSON stream has one `{"t":"conflict"}` line per pair, and the hub
  line has a `"conflicts"` count.

It is a prompt to look up, not a finding of fault. A drone pilot must give
way to crewed aircraft (14 CFR 107.37), but the hub cannot see what either
pilot can see.

## What it ignores

- Aircraft on the ground, aircraft that are themselves drones (ADS-B category
  "UAV"), ground vehicles and obstacles.
- Drones or aircraft not heard recently (stale).
- Drones that have not sent a position.

## How heights are compared

Drones and aircraft can report two kinds of height:
- **GPS height**, measured against the WGS-84 ellipsoid;
- **pressure altitude**, from a barometer set to standard pressure.

The two differ with the weather, by up to a few hundred feet. So:

1. **Same kind on both sides:** the two heights are compared directly.
2. **Drone gives pressure only, aircraft gives both:** the aircraft's own
   difference between its GPS and pressure heights is the local offset. The
   hub uses that offset to convert the drone's height, then compares.
3. **Otherwise ("mixed"):** the hub compares one kind with the other and
   widens the 150 m margin by 90 m.
4. **No height on one side:** the pair is judged on distance alone and never
   goes above Caution.

## How prediction works

- Each side's last position is moved forward by its speed and direction for
  the time since it was heard, up to 10 s.
- The hub then works out the closest point of approach over the next 60 s,
  assuming both keep going in a straight line.
- If the drone or aircraft sent no speed, the hub treats it as standing
  still. Hovering drones often send no speed.

The idea comes from SoftRF's traffic warnings (`TrafficHelper.cpp`, GPLv3,
Linar Yusupov). SoftRF only warns when one is heading within 10 degrees of
the other. Rotorscope uses the closest-approach calculation instead, because
a hovering drone has no heading.

Code: `crates/tracker/src/conflict.rs`, tests in `tests_conflict.rs`. The
thresholds are named constants at the top of the file.
