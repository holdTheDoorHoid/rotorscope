//! Words for flags: the log line, CSV columns, JSON and the screen.
//!
//! Every sentence says "possible violation" and never "illegal": the hub
//! cannot see waivers, LAANC, COAs or recreational status.

use core::fmt::{self, Write};

use airspace::Pack;
use airspace::format::src;

use crate::{Entry, Flag, Key, Rule};

/// Civil date (y, m, d) from Unix seconds.
pub fn civil_date(unix_s: u32) -> (i32, u32, u32) {
    let z = i64::from(unix_s / 86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + i64::from(m <= 2)) as i32;
    (y, m, d)
}

/// "FAA ADDS Class_Airspace 2026-10-30", "Remote ID", "computed", "ADS-B".
pub struct SourceText<'p, 'a>(pub u8, pub Option<&'p Pack<'a>>);

impl fmt::Display for SourceText<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            src::BROADCAST => f.write_str("drone broadcast"),
            src::COMPUTED => f.write_str("computed on hub"),
            src::ADSB => f.write_str("ADS-B"),
            id => match self.1.and_then(|p| p.source(id)) {
                Some(s) => {
                    let (y, m, d) = civil_date(s.date);
                    write!(f, "{} {y:04}-{m:02}-{d:02}", s.name)
                }
                None => write!(f, "source {id}"),
            },
        }
    }
}

/// Level name.
pub fn level_name(level: u8) -> &'static str {
    match level {
        0 => "Note",
        1 => "Watch",
        2 => "Warning",
        _ => "Alert",
    }
}

/// Text without commas or quotes, safe in a CSV field and JSON string.
struct Clean<'s>(&'s str);

impl fmt::Display for Clean<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in self.0.chars() {
            f.write_char(match c {
                ',' | '"' | '\\' | ';' => ' ',
                c if c.is_control() => ' ',
                c => c,
            })?;
        }
        Ok(())
    }
}

/// The evidence part of a flag ("152 m above ground ...").
pub struct Evidence<'f>(pub &'f Flag);

impl fmt::Display for Evidence<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let fl = self.0;
        let e = fl.ev;
        let ft = |m: f32| m * airspace::FT_PER_M as f32;
        match fl.rule {
            Rule::Height => write!(
                f,
                "{:.0} ft above ground +/-{:.0} ft ({}) limit {:.0} ft",
                ft(e[0]),
                ft(e[1]),
                Clean(&fl.name),
                ft(e[2])
            ),
            Rule::FacilityMap => write!(
                f,
                "{:.0} ft above ground +/-{:.0} ft over facility-map ceiling {:.0} ft",
                ft(e[0]),
                ft(e[2]),
                e[1]
            ),
            Rule::Controlled => write!(
                f,
                "inside {} ({}) often authorized (LAANC)",
                fl.area.map_or("", |a| a.name()),
                Clean(&fl.name)
            ),
            Rule::Area => {
                write!(
                    f,
                    "inside {} ({}) {:.0} m from edge",
                    fl.area.map_or("", |a| a.name()),
                    Clean(&fl.name),
                    e[0]
                )?;
                if fl.area == Some(airspace::AreaKind::Stadium) {
                    f.write_str(" restricted during events only")?;
                }
                if fl.no_clock {
                    f.write_str(" time unknown")?;
                }
                Ok(())
            }
            Rule::NoRemoteId => f.write_str("DJI DroneID heard but no Remote ID"),
            Rule::NoOperator => f.write_str("Remote ID without operator position"),
            Rule::SerialChanged => write!(
                f,
                "serial number changed {} time(s) mid-flight",
                e[0] as u32
            ),
            Rule::LowAircraft => write!(
                f,
                "{:.0} ft above ground +/-{:.0} ft minimum {:.0} ft ({})",
                ft(e[0]),
                ft(e[2]),
                e[1],
                Clean(&fl.name)
            ),
            Rule::Vlos => {
                let kind = match e[1] as u8 {
                    0 => "takeoff point",
                    1 => "live operator GNSS",
                    _ => "fixed operator point",
                };
                write!(f, "{:.0} m from operator ({kind}) +/-{:.0} m", e[0], e[2])
            }
            Rule::VlosTerrain => write!(
                f,
                "terrain {:.0} m above the operator's sight line at {:.0} m",
                e[0],
                e[1] * e[2]
            ),
            Rule::Night => {
                if e[1] > 0.5 {
                    write!(
                        f,
                        "night (sun {:.1} deg) lighting not visible to radio",
                        e[0]
                    )
                } else {
                    write!(f, "civil twilight (sun {:.1} deg) lighting required", e[0])
                }
            }
        }
    }
}

/// One full sentence: "Warning: possible violation of 14 CFR 107.51(b) ...:
/// <evidence>, 34 s [source date]".
pub fn write_flag(w: &mut impl Write, fl: &Flag, pack: Option<&Pack<'_>>) -> fmt::Result {
    let what = if fl.level == 0 {
        "note"
    } else {
        "possible violation"
    };
    write!(
        w,
        "{} {}: {what} of {}: {} for {:.0} s [{}]",
        level_name(fl.level),
        fl.rule.code(),
        fl.rule.citation(fl.area),
        Evidence(fl),
        fl.secs,
        SourceText(fl.src, pack)
    )
}

/// The two CSV columns the P4 hub appends: `flag_level` (empty when no
/// flag) and `flags` (every flag, `; `-separated, no commas or quotes).
pub fn write_csv_columns(
    w: &mut impl Write,
    e: Option<&Entry>,
    pack: Option<&Pack<'_>>,
) -> fmt::Result {
    let Some(e) = e.filter(|e| !e.flags.is_empty()) else {
        return w.write_str(",,");
    };
    write!(w, ",{},", e.level().unwrap_or(0))?;
    for (i, fl) in e.flags.iter().enumerate() {
        if i > 0 {
            w.write_str("; ")?;
        }
        write!(
            w,
            "L{} {} {} {:.0}s [{}]",
            fl.level,
            fl.rule.code(),
            Evidence(fl),
            fl.secs,
            Clean(&Src(fl.src, pack).to_short())
        )?;
    }
    Ok(())
}

/// The CSV header columns appended by [`write_csv_columns`].
pub const CSV_HEADER_SUFFIX: &str = ",flag_level,flags";

struct Src<'p, 'a>(u8, Option<&'p Pack<'a>>);

impl Src<'_, '_> {
    fn to_short(&self) -> heapless::String<48> {
        let mut s = heapless::String::new();
        let _ = write!(s, "{}", SourceText(self.0, self.1));
        s
    }
}

/// A JSON line for the USB stream and the web head:
/// `{"t":"flags","track":7,"level":2,"flags":[{"rule":"HEIGHT","level":2,"area":"Class B","name":"...","ev":[..],"secs":34,"src":"..."}],"worst":2,"max_agl_m":152.0}`
/// (`"icao":"a1b2c3"` instead of `"track"` for an aircraft).
pub fn write_json(w: &mut impl Write, e: &Entry, pack: Option<&Pack<'_>>) -> fmt::Result {
    w.write_str("{\"t\":\"flags\",")?;
    match e.key {
        Key::Drone(id) => write!(w, "\"track\":{}", id.0)?,
        Key::Aircraft(icao) => write!(w, "\"icao\":\"{icao:06x}\"")?,
    }
    write!(
        w,
        ",\"level\":{},\"flags\":[",
        e.level().map_or(-1, i32::from)
    )?;
    for (i, fl) in e.flags.iter().enumerate() {
        if i > 0 {
            w.write_char(',')?;
        }
        write!(
            w,
            "{{\"rule\":\"{}\",\"level\":{},\"cite\":\"{}\"",
            fl.rule.code(),
            fl.level,
            fl.rule.citation(fl.area)
        )?;
        if let Some(a) = fl.area {
            write!(w, ",\"area\":\"{}\"", a.name())?;
        }
        write!(
            w,
            ",\"name\":\"{}\",\"ev\":[{:.1},{:.1},{:.2}],\"secs\":{:.0},\"src\":\"{}\",\"text\":\"{}\"}}",
            Clean(&fl.name),
            fl.ev[0],
            fl.ev[1],
            fl.ev[2],
            fl.secs,
            Clean(&Src(fl.src, pack).to_short()),
            Evidence(fl)
        )?;
    }
    let sm = &e.summary;
    write!(w, "],\"worst\":{}", sm.worst_level)?;
    if let Some(r) = sm.worst_rule {
        write!(w, ",\"worst_rule\":\"{}\"", r.code())?;
    }
    if sm.has_agl {
        write!(w, ",\"max_agl_m\":{:.1}", sm.max_agl_m)?;
    }
    w.write_str("}\n")
}

/// Short text for a screen row: "HEIGHT 520ft" etc. (at most ~20 chars).
pub fn screen_text(fl: &Flag) -> heapless::String<24> {
    let mut s = heapless::String::new();
    let ft = |m: f32| m * airspace::FT_PER_M as f32;
    let e = fl.ev;
    let _ = match fl.rule {
        Rule::Height => write!(s, "{:.0}ft AGL", ft(e[0])),
        Rule::FacilityMap => write!(s, "{:.0}>{:.0}ft LAANC", ft(e[0]), e[1]),
        Rule::Controlled | Rule::Area => write!(s, "{}", fl.area.map_or("area", |a| a.name())),
        Rule::NoRemoteId => write!(s, "no Remote ID"),
        Rule::NoOperator => write!(s, "no operator pos"),
        Rule::SerialChanged => write!(s, "serial changed"),
        Rule::LowAircraft => write!(s, "{:.0}ft<{:.0} 91.119", ft(e[0]), e[1]),
        Rule::Vlos => write!(s, "{:.0}m from pilot", e[0]),
        Rule::VlosTerrain => write!(s, "hill blocks view"),
        Rule::Night => write!(s, "{}", if e[1] > 0.5 { "night" } else { "twilight" }),
    };
    s
}

/// The one number a compact report carries for a flag (`link::FlagItem`):
/// height above ground or distance in metres, terrain over the sight line,
/// sun elevation, number of serial changes; 0 for the rest.
pub fn item_value(fl: &Flag) -> f32 {
    match fl.rule {
        Rule::Height | Rule::FacilityMap | Rule::LowAircraft | Rule::Vlos | Rule::VlosTerrain | Rule::Night | Rule::SerialChanged => fl.ev[0],
        _ => 0.0,
    }
}

/// A compact flag as words, for a board that only has the compact report
/// (the web head's phone page and CoT remark): "Warning HEIGHT 920 ft AGL
/// (14 CFR 107.51(b) / 49 USC 44809(a)(6))".
pub fn item_text(rule: u8, area: u8, level: u8, value: f32) -> heapless::String<120> {
    let mut s = heapless::String::new();
    let Some(r) = Rule::from_u8(rule) else {
        let _ = write!(s, "{} rule {rule}", level_name(level));
        return s;
    };
    let area = airspace::AreaKind::from_u8(area);
    let ft = value * airspace::FT_PER_M as f32;
    let _ = write!(s, "{} {} ", level_name(level), r.code());
    let _ = match r {
        Rule::Height => write!(s, "{ft:.0} ft AGL"),
        Rule::FacilityMap => write!(s, "{ft:.0} ft AGL over facility-map ceiling"),
        Rule::LowAircraft => write!(s, "{ft:.0} ft above ground"),
        Rule::Controlled | Rule::Area => write!(s, "inside {}", area.map_or("area", |a| a.name())),
        Rule::Vlos => write!(s, "{value:.0} m from operator"),
        Rule::VlosTerrain => write!(s, "terrain {value:.0} m over sight line"),
        Rule::Night => write!(s, "sun {value:.1} deg"),
        Rule::SerialChanged => write!(s, "serial changed {}x", value as u32),
        Rule::NoRemoteId => write!(s, "no Remote ID"),
        Rule::NoOperator => write!(s, "no operator position"),
    };
    let _ = write!(s, " ({})", r.citation(area));
    s
}
