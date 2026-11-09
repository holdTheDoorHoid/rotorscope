//! Node USB JSON lines → events for the tracker.
//!
//! The nodes print one JSON object per line with a `"t"` tag. A detection
//! line looks like
//! `{"t":"rid","tr":"WifiBeacon","ch":6,"rssi":-60,"mac":"aa:..","ctr":3,"up_us":..,"msgs":[...]}`
//! where each `msgs` element is a message as serialized by the `rid` crate
//! (`{"BasicId":{...}}`), or, for one the node could not decode, a hex
//! string or `{"error":..,"raw":"hex"}`. Unknown fields and line types are
//! ignored so firmware can add to the format freely.
//!
//! The `rid` types serialize but do not deserialize, so this module rebuilds
//! `Message`s from JSON itself. Enum names are not duplicated here: each is
//! matched by serializing every code with `rid` and comparing, so a name the
//! `rid` crate changes cannot silently drift. If a line carries `"raw"` (an
//! array of 50-hex-char messages, or per message), the raw bytes are decoded
//! with `rid` instead, which is lossless; the decoded form has one ambiguity
//! (see [`fixed_str`]).

use rid::Transport;
use rid::message::*;
use serde::Serialize;
use serde_json::Value;

/// What one input line meant.
#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    Rid(RidLine),
    Hello(Value),
    Stats(Value),
    /// Any other `t` (log, err, boot, cmd...): ignored by the table.
    Other,
    /// Not JSON, or a `rid` line without the fields we need.
    Bad,
}

/// A Remote ID detection from a node.
#[derive(Debug, Clone, PartialEq)]
pub struct RidLine {
    pub transport: Transport,
    pub channel: u8,
    pub rssi: i8,
    pub mac: [u8; 6],
    pub counter: u8,
    pub up_us: Option<u64>,
    pub msgs: Vec<Message>,
    /// Messages present but not decodable.
    pub errors: u32,
}

pub fn line(text: &str) -> Line {
    let Ok(v) = serde_json::from_str::<Value>(text.trim()) else {
        return Line::Bad;
    };
    match v.get("t").and_then(Value::as_str) {
        Some("rid") => rid_line(&v).map_or(Line::Bad, Line::Rid),
        Some("hello") => Line::Hello(v),
        Some("stats") => Line::Stats(v),
        _ => Line::Other,
    }
}

fn rid_line(v: &Value) -> Option<RidLine> {
    let mut out = RidLine {
        transport: parse_enum(v.get("tr")?, transport_from_code)?,
        channel: num(v.get("ch")).unwrap_or(0.0) as u8,
        rssi: num(v.get("rssi")).unwrap_or(0.0) as i8,
        mac: v.get("mac").and_then(Value::as_str).and_then(mac)?,
        counter: num(v.get("ctr")).unwrap_or(0.0) as u8,
        up_us: v.get("up_us").and_then(Value::as_u64),
        msgs: Vec::new(),
        errors: 0,
    };
    // Prefer raw bytes when the node sends them: they are exact.
    if let Some(raws) = v.get("raw").and_then(Value::as_array) {
        for r in raws {
            out.push_raw(r.as_str().and_then(raw_hex));
        }
        return Some(out);
    }
    for m in v
        .get("msgs")
        .and_then(Value::as_array)
        .map_or(&[][..], |a| a)
    {
        match m {
            Value::String(s) => out.push_raw(raw_hex(s)),
            Value::Object(o) if o.contains_key("raw") => {
                out.push_raw(o.get("raw").and_then(Value::as_str).and_then(raw_hex))
            }
            other => match message(other) {
                Some(msg) => out.msgs.push(msg),
                None => out.errors += 1,
            },
        }
    }
    Some(out)
}

impl RidLine {
    fn push_raw(&mut self, raw: Option<rid::RawMessage>) {
        match raw.map(|r| decode(&r)) {
            Some(Ok(m)) => self.msgs.push(m),
            _ => self.errors += 1,
        }
    }
}

// --------------------------------------------------------------- scalars --

fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
}

fn opt_f32(v: &Value, key: &str) -> Option<f32> {
    num(v.get(key)).map(|x| x as f32)
}

fn hex_bytes(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

pub fn raw_hex(s: &str) -> Option<rid::RawMessage> {
    hex_bytes(s)?.try_into().ok()
}

pub fn mac(s: &str) -> Option<[u8; 6]> {
    let bytes = s
        .split([':', '-'])
        .map(|p| u8::from_str_radix(p, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    bytes.try_into().ok()
}

/// Match `v` against how `rid` serializes each code of an enum. Works for
/// unit variants (`"Airborne"`) and the `{"Reserved": n}` escape alike.
fn parse_enum<T: Serialize>(v: &Value, from_code: impl Fn(u8) -> Option<T>) -> Option<T> {
    (0..=255u8).find_map(|c| {
        let t = from_code(c)?;
        (serde_json::to_value(&t).ok()? == *v).then_some(t)
    })
}

fn transport_from_code(c: u8) -> Option<Transport> {
    tracker::TRANSPORTS.get(usize::from(c)).copied()
}

fn field<T: Serialize>(v: &Value, key: &str, from_code: fn(u8) -> T, default: T) -> T {
    v.get(key)
        .and_then(|x| parse_enum(x, |c| Some(from_code(c))))
        .unwrap_or(default)
}

fn lat_lon(v: Option<&Value>) -> Option<LatLon> {
    let v = v?;
    Some(LatLon {
        lat: num(v.get("lat"))?,
        lon: num(v.get("lon"))?,
    })
}

/// Rebuild a fixed text field. `rid` writes clean ASCII as the string and
/// anything else as lowercase hex, so a string alone is ambiguous ("cafe"
/// could be either). `binary` says which the field normally is: Basic IDs of
/// type UTM UUID or session ID are binary; everything else is text by the
/// standard, so it is taken literally.
fn fixed_str<const N: usize>(v: Option<&Value>, binary: bool) -> Option<FixedStr<N>> {
    let s = v?.as_str()?;
    if binary
        && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        && let Some(bytes) = hex_bytes(s)
    {
        return FixedStr::new(&bytes).ok();
    }
    FixedStr::new(s.as_bytes()).ok()
}

// -------------------------------------------------------------- messages --

/// One `rid`-serialized message, e.g. `{"Location":{...}}`. Missing fields
/// take the standard's "unknown" so older or trimmed firmware still parses.
pub fn message(v: &Value) -> Option<Message> {
    let (kind, body) = v.as_object()?.iter().next()?;
    Some(match kind.as_str() {
        "BasicId" => {
            let id_type = field(body, "id_type", IdType::from_code, IdType::None);
            let binary = matches!(id_type, IdType::UtmAssigned | IdType::SpecificSession);
            Message::BasicId(BasicId {
                id_type,
                ua_type: field(body, "ua_type", UaType::from_code, UaType::None),
                uas_id: fixed_str(body.get("uas_id"), binary)?,
            })
        }
        "Location" => Message::Location(Location {
            status: field(body, "status", Status::from_code, Status::Undeclared),
            height_type: parse_enum(body.get("height_type").unwrap_or(&Value::Null), |c| {
                [HeightType::AboveTakeoff, HeightType::AboveGround]
                    .get(usize::from(c))
                    .copied()
            })
            .unwrap_or(HeightType::AboveTakeoff),
            direction: opt_f32(body, "direction"),
            speed_horizontal: opt_f32(body, "speed_horizontal"),
            speed_vertical: opt_f32(body, "speed_vertical"),
            position: lat_lon(body.get("position")),
            altitude_pressure: opt_f32(body, "altitude_pressure"),
            altitude_geodetic: opt_f32(body, "altitude_geodetic"),
            height: opt_f32(body, "height"),
            horizontal_accuracy: field(
                body,
                "horizontal_accuracy",
                HorizontalAccuracy::from_code,
                HorizontalAccuracy::Unknown,
            ),
            vertical_accuracy: field(
                body,
                "vertical_accuracy",
                VerticalAccuracy::from_code,
                VerticalAccuracy::Unknown,
            ),
            baro_accuracy: field(
                body,
                "baro_accuracy",
                VerticalAccuracy::from_code,
                VerticalAccuracy::Unknown,
            ),
            speed_accuracy: field(
                body,
                "speed_accuracy",
                SpeedAccuracy::from_code,
                SpeedAccuracy::Unknown,
            ),
            timestamp: opt_f32(body, "timestamp"),
            timestamp_accuracy: opt_f32(body, "timestamp_accuracy"),
        }),
        "SelfId" => Message::SelfId(SelfId {
            desc_type: field(body, "desc_type", DescType::from_code, DescType::Text),
            description: fixed_str(body.get("description"), false)?,
        }),
        "System" => Message::System(System {
            operator_location_type: field(
                body,
                "operator_location_type",
                OperatorLocationType::from_code,
                OperatorLocationType::Takeoff,
            ),
            classification_type: field(
                body,
                "classification_type",
                ClassificationType::from_code,
                ClassificationType::Undeclared,
            ),
            operator_position: lat_lon(body.get("operator_position")),
            area_count: num(body.get("area_count")).unwrap_or(1.0) as u16,
            area_radius: num(body.get("area_radius")).unwrap_or(0.0) as u16,
            area_ceiling: opt_f32(body, "area_ceiling"),
            area_floor: opt_f32(body, "area_floor"),
            category: field(body, "category", Category::from_code, Category::Undeclared),
            class: field(body, "class", UaClass::from_code, UaClass::Undeclared),
            operator_altitude: opt_f32(body, "operator_altitude"),
            timestamp: body
                .get("timestamp")
                .and_then(Value::as_u64)
                .map(|t| t as u32),
        }),
        "OperatorId" => Message::OperatorId(OperatorId {
            operator_id_type: field(
                body,
                "operator_id_type",
                OperatorIdType::from_code,
                OperatorIdType::OperatorId,
            ),
            operator_id: fixed_str(body.get("operator_id"), false)?,
        }),
        // Authentication pages carry nothing the table shows; count them as
        // messages so they are not reported as errors.
        "Auth" => return auth_placeholder(),
        _ => return None,
    })
}

/// A minimal Auth message: the tracker ignores Auth content.
fn auth_placeholder() -> Option<Message> {
    let mut raw = [0u8; rid::MESSAGE_SIZE];
    raw[0] = (kind::AUTH << 4) | PROTOCOL_VERSION;
    raw[1] = 0x01; // page 1: no page-0 consistency rules apply
    decode(&raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rid::encode;

    fn sample_messages() -> Vec<Message> {
        vec![
            Message::BasicId(BasicId {
                id_type: IdType::SerialNumber,
                ua_type: UaType::HelicopterOrMultirotor,
                uas_id: FixedStr::new(b"1581F5FKD229400D").unwrap(),
            }),
            Message::BasicId(BasicId {
                id_type: IdType::SpecificSession,
                ua_type: UaType::HelicopterOrMultirotor,
                uas_id: FixedStr::new(&[0x01, 0xca, 0xfe, 0x00, 0x42]).unwrap(),
            }),
            Message::Location(Location {
                status: Status::Reserved(9),
                height_type: HeightType::AboveGround,
                direction: Some(271.0),
                speed_horizontal: Some(12.25),
                speed_vertical: Some(-1.5),
                position: Some(LatLon {
                    lat: 39.9525834,
                    lon: -75.1652219,
                }),
                altitude_pressure: None,
                altitude_geodetic: Some(123.5),
                height: Some(40.0),
                horizontal_accuracy: HorizontalAccuracy::M3,
                vertical_accuracy: VerticalAccuracy::M10,
                baro_accuracy: VerticalAccuracy::Unknown,
                speed_accuracy: SpeedAccuracy::Mps0_3,
                timestamp: Some(360.5),
                timestamp_accuracy: None,
            }),
            Message::SelfId(SelfId {
                desc_type: DescType::Text,
                description: FixedStr::new(b"cafe").unwrap(),
            }),
            Message::System(System {
                operator_location_type: OperatorLocationType::LiveGnss,
                classification_type: ClassificationType::Eu,
                operator_position: Some(LatLon {
                    lat: 39.95,
                    lon: -75.17,
                }),
                area_count: 1,
                area_radius: 0,
                area_ceiling: None,
                area_floor: None,
                category: Category::Open,
                class: UaClass::Class1,
                operator_altitude: Some(12.5),
                timestamp: Some(250_000_000),
            }),
            Message::OperatorId(OperatorId {
                operator_id_type: OperatorIdType::OperatorId,
                operator_id: FixedStr::new(b"FIN87astrdge12k8").unwrap(),
            }),
        ]
    }

    /// What the firmware prints (rid's own Serialize) parses back to the
    /// same messages, exactly.
    #[test]
    fn decoded_json_round_trips() {
        let msgs = sample_messages();
        // Normalise through the wire format first, as a node would see them.
        let msgs: Vec<Message> = msgs
            .iter()
            .map(|m| decode(&encode::message(m).unwrap()).unwrap())
            .collect();
        let line = serde_json::json!({
            "t": "rid", "tr": "WifiNan", "ch": 6, "rssi": -71, "mac": "aa:bb:cc:dd:ee:0f",
            "ctr": 9, "up_us": 123456789u64, "future_field": {"x": 1}, "msgs": msgs,
        });
        let Line::Rid(r) = line_of(&line) else {
            panic!()
        };
        assert_eq!(r.transport, Transport::WifiNan);
        assert_eq!(r.mac, [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x0f]);
        assert_eq!(
            (r.channel, r.rssi, r.counter, r.up_us),
            (6, -71, 9, Some(123456789))
        );
        assert_eq!(r.errors, 0);
        assert_eq!(r.msgs, msgs);
    }

    fn line_of(v: &Value) -> Line {
        line(&v.to_string())
    }

    #[test]
    fn raw_forms_and_errors() {
        let msgs = sample_messages();
        let hex = |m: &Message| {
            encode::message(m)
                .unwrap()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        // Per-message hex string, node-ble's error object, and junk.
        let mut bad = [0u8; 25];
        bad[0] = 0x92;
        let bad_hex: String = bad.iter().map(|b| format!("{b:02x}")).collect();
        let v = serde_json::json!({
            "t":"rid","tr":"BleLegacy","rssi":-80,"mac":"01:02:03:04:05:06",
            "msgs":[hex(&msgs[0]), {"error":"UnknownType","raw":bad_hex}, {"Nonsense":{}}],
        });
        let Line::Rid(r) = line_of(&v) else { panic!() };
        assert_eq!(r.msgs, vec![msgs[0].clone()]);
        assert_eq!(r.errors, 2);

        // A top-level "raw" array wins over msgs.
        let v = serde_json::json!({
            "t":"rid","tr":"WifiBeacon","mac":"01:02:03:04:05:06",
            "raw":[hex(&msgs[1]), hex(&msgs[3])], "msgs":[],
        });
        let Line::Rid(r) = line_of(&v) else { panic!() };
        assert_eq!(r.msgs.len(), 2);
        let Message::BasicId(b) = &r.msgs[0] else {
            panic!()
        };
        assert_eq!(b.uas_id.as_bytes(), &[0x01, 0xca, 0xfe, 0x00, 0x42]);
    }

    #[test]
    fn other_lines() {
        assert_eq!(line("not json"), Line::Bad);
        assert_eq!(line(r#"{"t":"rid"}"#), Line::Bad);
        assert_eq!(line(r#"{"t":"log","msg":"hi"}"#), Line::Other);
        assert!(matches!(
            line(r#"{"t":"hello","node":"wifi24","fw":"0.1"}"#),
            Line::Hello(_)
        ));
        assert!(matches!(line(r#"{"t":"stats","rid":3}"#), Line::Stats(_)));
        assert_eq!(
            mac("AA-BB-CC-DD-EE-FF"),
            Some([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff])
        );
        assert_eq!(mac("aa:bb"), None);
    }
}
