//! Every threshold the rule engine uses, in one table, each with the rule it
//! comes from and why the default is what it is. [`Settings`] holds the
//! values; [`ROWS`] documents them (the hub prints it on request, and the
//! README table is generated from it by the `settings_table` test).
//!
//! "project choice" marks a number the FAA does not give: a noise filter or
//! a grading step. "unsourced" marks a default nobody has published a study
//! for; tune it.

/// One documented setting.
#[derive(Debug, Clone, Copy)]
pub struct Row {
    pub name: &'static str,
    pub default: f32,
    pub unit: &'static str,
    /// The rule the setting serves.
    pub rule: &'static str,
    /// Why this default.
    pub why: &'static str,
}

macro_rules! settings {
    ($( $(#[$doc:meta])* $name:ident: $default:expr, $unit:expr, $rule:expr, $why:expr; )*) => {
        /// Rule-engine thresholds. See the module docs and [`ROWS`].
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct Settings {
            $( $(#[$doc])* pub $name: f32, )*
        }

        impl Default for Settings {
            fn default() -> Self {
                Self { $( $name: $default, )* }
            }
        }

        /// The documentation of every setting, in table order.
        pub const ROWS: &[Row] = &[
            $( Row { name: stringify!($name), default: $default, unit: $unit, rule: $rule, why: $why }, )*
        ];
    };
}

settings! {
    /// Height limit above ground.
    agl_limit_ft: 400.0, "ft", "14 CFR 107.51(b); 49 USC 44809(a)(6)",
        "The rule's own number (recreational flyers: 400 ft in Class G).";
    /// Over the limit (best estimate) this long before a Watch.
    agl_watch_s: 2.0, "s", "107.51(b)",
        "project choice: one or two noisy reports never flag (PLAN: 'a single noisy reading never shouts').";
    /// Beyond the limit by this much *after* subtracting the reported
    /// accuracy, for a Warning.
    agl_warn_margin_ft: 50.0, "ft", "107.51(b)",
        "PLAN.md level table: 'over 450 ft above ground for 30 s' is a Warning.";
    agl_warn_s: 30.0, "s", "107.51(b)", "PLAN.md level table (30 s sustained).";
    /// Lower bound over this, plus another flag (far away, night, no
    /// Remote ID), is an Alert.
    agl_alert_ft: 600.0, "ft", "107.51(b)",
        "PLAN.md level table: 'over 600 ft above ground and 1.5 km from the operator at night'.";
    /// Lower bound over this, alone and sustained, is an Alert.
    agl_alert_alone_ft: 1000.0, "ft", "107.51(b)",
        "project choice: 600 ft over the limit even at the worst accuracy class allowed for a Watch.";
    /// In an urban cell, a height under limit + this stays a Watch
    /// ("high, check for structures"): the structure exception.
    structure_allow_ft: 400.0, "ft", "107.51(b)(1)-(2)",
        "Rule: within a 400 ft radius of a structure, up to 400 ft above its top. The pack has no buildings.";
    /// Vertical accuracy assumed when the drone reports none.
    vert_acc_unknown_m: 45.0, "m", "F3411 accuracy classes",
        "project choice: the F3411 class just coarser than typical GNSS (< 45 m).";
    /// Error allowed for the terrain height (15 arc-second posts, bilinear).
    terrain_err_m: 10.0, "m", "107.51(b)",
        "project choice: 3DEP resampled to ~460 m posts; the Philadelphia region varies by a few metres per post.";
    /// Extra error when the height is above the takeoff point.
    takeoff_height_err_m: 10.0, "m", "107.51(b)",
        "project choice: takeoff and ground under the drone differ (corrected with terrain when the operator is known).";
    /// Error allowed for a pressure altitude used as MSL.
    baro_err_m: 90.0, "m", "107.51(b); 91.119",
        "about 300 ft: pressure altitude is off from MSL by ~1000 ft per inHg; +/-0.3 inHg is ordinary weather.";
    /// Above a facility-map ceiling by at least this (best estimate) for a Watch.
    uasfm_margin_m: 15.0, "m", "107.41 (UAS Facility Map / LAANC)",
        "project choice: keeps a drone sitting on the ground in a 0 ft cell from flagging.";
    /// Lower bound above the ceiling this long for a Warning.
    uasfm_warn_s: 10.0, "s", "107.41", "project choice (PLAN: above the facility-map ceiling is a Warning).";
    /// Inside a prohibited / national-security / active TFR area this long
    /// (and farther than the position accuracy from its edge) for an Alert.
    area_alert_s: 5.0, "s", "91.133; 99.7; 91.137-145; 107.45/107.47",
        "project choice: a few seconds of consistent position before the top level.";
    /// Drone-to-operator distance for a Watch.
    vlos_watch_m: 500.0, "m", "14 CFR 107.31; 49 USC 44809(a)(7)",
        "UK CAA CAP 722: VLOS normally out to 500 m. Field studies put 50 % detection of a Phantom 4 at ~245 m (Applied Ergonomics, 2018) and a Mavic Air at ~307 m (Applied Sciences 10(16):5501, 2020). Sources seen as search snippets only; verify.";
    /// Distance for a Warning (sustained).
    vlos_warn_m: 1000.0, "m", "107.31",
        "unsourced: about three times the measured 50 % detection range above.";
    /// Distance for an Alert (sustained).
    vlos_alert_m: 1500.0, "m", "107.31", "PLAN.md level table example (1.5 km).";
    vlos_warn_s: 30.0, "s", "107.31", "project choice, as the height Warning.";
    /// Terrain this far above the operator-drone sight line blocks it.
    los_block_m: 10.0, "m", "107.31",
        "project choice: the terrain error above, so tile noise never blocks a line.";
    /// Operator eye height above ground when Remote ID gives no altitude.
    eye_height_m: 1.7, "m", "107.31", "typical standing eye height.";
    /// Heard on DJI DroneID with no Remote ID this long: Watch.
    no_rid_s: 10.0, "s", "14 CFR 89.105",
        "project choice: F3411 sends Location at least once a second, so 10 s of silence while DroneID is heard is not a reception gap.";
    no_rid_warn_s: 60.0, "s", "89.105", "project choice.";
    /// Remote ID heard but no operator (control station) position this long.
    op_missing_s: 15.0, "s", "14 CFR 89.305(c); 89.315(c)",
        "project choice: F3411 static messages repeat at least every 3 s; 15 s allows for missed frames.";
    /// A serial-number change stays flagged this long.
    id_change_hold_s: 120.0, "s", "14 CFR 89.305(a)", "project choice.";
    /// Crewed aircraft over a congested area: minimum height.
    ac_congested_ft: 1000.0, "ft", "14 CFR 91.119(b)",
        "The rule (1000 ft above the highest obstacle within 2000 ft). Congested = census urban area, a stand-in.";
    /// Crewed aircraft elsewhere.
    ac_other_ft: 500.0, "ft", "14 CFR 91.119(c)", "The rule (500 ft above the surface).";
    /// No 91.119 flag this close to a public airport (takeoff/landing, 91.119 preamble).
    ac_airport_excl_m: 5556.0, "m", "14 CFR 91.119 (except when necessary for takeoff or landing)",
        "project choice: 3 NM; a 3 degree approach is ~1000 ft up at 3 NM.";
    /// ... and this close to a private airfield or seaplane base.
    ac_private_excl_m: 1852.0, "m", "91.119", "project choice: 1 NM for private strips and seaplane bases (heliports are ignored: helicopters are not checked).";
    ac_warn_s: 30.0, "s", "91.119", "project choice.";
    /// Only positions this fresh are evaluated.
    fresh_ms: 3000.0, "ms", "-", "project choice: as the tracker's 'Remote ID preferred' window.";
}
