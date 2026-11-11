//! `--ble ADAPTER`: Remote ID scanning with a laptop Bluetooth adapter,
//! through BlueZ over the system D-Bus. No root and no raw HCI socket:
//! discovery is an ordinary BlueZ client request, the same one
//! `bluetoothctl scan on` makes.
//!
//! One caveat for a receive-only project: BlueZ discovery is an *active*
//! LE scan, so the controller sends scan requests to scannable advertisers
//! nearby. Remote ID broadcasts are non-scannable, so a drone never gets
//! one, and nothing is ever connected or paired. A truly passive scan
//! would need BlueZ's advertisement-monitor API, which reports each device
//! once rather than every advert.
//!
//! The discovery filter asks for LE only with `DuplicateData` on, so BlueZ
//! reports every advert whose content changed instead of one per device.
//! Each Remote ID advert carries a new counter, so each one shows up as a
//! `ServiceData` property change on its `org.bluez.Device1` object. RSSI is
//! a separate property that BlueZ only signals when it changes, so the last
//! value per device is kept and attached to the next advert.
//!
//! What a BT4 adapter can hear: legacy advertising only. BT5 extended
//! adverts and Long Range (coded PHY) need a BT5 controller, and BlueZ hands
//! those over the same way, so they would decode here too.

use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type as MsgType;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{MatchRule, fdo};

use crate::ble;
use crate::input::{Clock, Event};
use crate::parse::Line;

const BLUEZ: &str = "org.bluez";

/// What one D-Bus property update says about a device.
#[derive(Debug, Default, PartialEq)]
pub struct Update {
    pub rssi: Option<i16>,
    /// The Remote ID service data (app code, counter, payload), if present.
    pub rid: Option<Vec<u8>>,
    /// The update carried advertising content of any kind (RSSI, service or
    /// manufacturer data, TX power), i.e. BlueZ just heard an advert.
    pub advert: bool,
}

/// Strip variant wrappers (`v` inside `a{sv}`).
fn inner<'a>(v: &'a Value<'a>) -> &'a Value<'a> {
    match v {
        Value::Value(b) => inner(b),
        other => other,
    }
}

fn bytes(v: &Value) -> Option<Vec<u8>> {
    match inner(v) {
        Value::Array(a) => a
            .inner()
            .iter()
            .map(|x| match inner(x) {
                Value::U8(b) => Some(*b),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

/// Read an `org.bluez.Device1` property map (from `PropertiesChanged` or
/// `InterfacesAdded`).
pub fn device_update(props: &HashMap<String, OwnedValue>) -> Update {
    let mut u = Update::default();
    for (k, v) in props {
        match k.as_str() {
            "RSSI" => {
                if let Value::I16(r) = inner(v) {
                    u.rssi = Some(*r);
                    u.advert = true;
                }
            }
            "ServiceData" => {
                u.advert = true;
                if let Value::Dict(d) = inner(v) {
                    u.rid = d.iter().find_map(|(uuid, data)| match inner(uuid) {
                        Value::Str(s) if ble::is_rid_uuid(s) => bytes(data),
                        _ => None,
                    });
                }
            }
            "ManufacturerData" | "TxPower" | "AdvertisingData" => u.advert = true,
            _ => {}
        }
    }
    u
}

/// Turn a D-Bus error into something a person can act on.
fn explain(adapter: &str, e: &zbus::Error, known: &[String]) -> String {
    let name = match e {
        zbus::Error::MethodError(name, _, _) => name.as_str().to_string(),
        zbus::Error::FDO(f) => match **f {
            fdo::Error::ServiceUnknown(_) => "org.freedesktop.DBus.Error.ServiceUnknown".into(),
            fdo::Error::UnknownObject(_) => "org.freedesktop.DBus.Error.UnknownObject".into(),
            fdo::Error::UnknownMethod(_) => "org.freedesktop.DBus.Error.UnknownMethod".into(),
            fdo::Error::InvalidArgs(_) => "org.freedesktop.DBus.Error.InvalidArgs".into(),
            _ => String::new(),
        },
        _ => String::new(),
    };
    match name.as_str() {
        "org.freedesktop.DBus.Error.ServiceUnknown" => {
            "BlueZ is not running (no org.bluez on the system bus); start the bluetooth service"
                .into()
        }
        "org.freedesktop.DBus.Error.UnknownObject"
        | "org.freedesktop.DBus.Error.UnknownMethod"
        | "org.freedesktop.DBus.Error.InvalidArgs" => format!(
            "no Bluetooth adapter named {adapter} (BlueZ has: {})",
            if known.is_empty() {
                "none".to_string()
            } else {
                known.join(", ")
            }
        ),
        "org.bluez.Error.NotReady" => powered_off(adapter),
        "org.freedesktop.DBus.Error.AccessDenied" => format!(
            "BlueZ refused access to {adapter}: {e} (the D-Bus policy for org.bluez may \
             limit it to some users or groups)"
        ),
        _ => format!("{adapter}: {e}"),
    }
}

fn powered_off(adapter: &str) -> String {
    format!(
        "{adapter} is powered off. Turn it on with `bluetoothctl`, then \
         `select <its address>` and `power on` (if it stays off, check `rfkill list`)"
    )
}

/// Adapter names BlueZ knows, for error messages.
pub fn adapters(conn: &Connection) -> Vec<String> {
    let Ok(p) = Proxy::new(conn, BLUEZ, "/", "org.freedesktop.DBus.ObjectManager") else {
        return Vec::new();
    };
    type Objects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;
    let Ok(objs) = p.call::<_, _, Objects>("GetManagedObjects", &()) else {
        return Vec::new();
    };
    let mut v: Vec<String> = objs
        .iter()
        .filter(|(_, ifs)| ifs.contains_key("org.bluez.Adapter1"))
        .filter_map(|(path, ifs)| {
            let name = path.as_str().rsplit('/').next()?;
            let addr = ifs["org.bluez.Adapter1"]
                .get("Address")
                .and_then(|a| match inner(a) {
                    Value::Str(s) => Some(s.to_string()),
                    _ => None,
                })
                .unwrap_or_default();
            Some(format!("{name} {addr}"))
        })
        .collect();
    v.sort();
    v
}

/// Open the system bus and check that `adapter` exists and is powered.
/// Returns the bus, the adapter's object path and its address.
pub fn open_adapter(adapter: &str) -> Result<(Connection, String, String), String> {
    if adapter.is_empty() || !adapter.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(format!("{adapter:?} is not an adapter name like hci0"));
    }
    let conn = Connection::system().map_err(|e| format!("cannot reach the system D-Bus: {e}"))?;
    let path = format!("/org/bluez/{adapter}");
    let props = Proxy::new(
        &conn,
        BLUEZ,
        path.clone(),
        "org.freedesktop.DBus.Properties",
    )
    .map_err(|e| e.to_string())?;
    let get = |name: &str| -> Result<OwnedValue, String> {
        props
            .call("Get", &("org.bluez.Adapter1", name))
            .map_err(|e| explain(adapter, &e, &adapters(&conn)))
    };
    let powered = matches!(*get("Powered")?, Value::Bool(true));
    if !powered {
        return Err(powered_off(adapter));
    }
    let addr = match &*get("Address")? {
        Value::Str(s) => s.to_string(),
        _ => String::new(),
    };
    Ok((conn, path, addr))
}

/// Start passive LE discovery on `adapter` and feed Remote ID adverts to
/// the viewer as node `node`. Runs until `stop_after` (if any) or until the
/// adapter goes away. Fails up front with a readable message if BlueZ, the
/// adapter or power is missing.
pub fn spawn_scan(
    node: u8,
    adapter: &str,
    stop_after: Option<Duration>,
    start: Instant,
    tx: Sender<Event>,
) -> Result<String, String> {
    let (conn, path, addr) = open_adapter(adapter)?;
    let err = |e: zbus::Error| explain(adapter, &e, &adapters(&conn));

    // Subscribe before discovery starts so no early advert is missed.
    let changed = MatchRule::builder()
        .msg_type(MsgType::Signal)
        .sender(BLUEZ)
        .and_then(|b| b.interface("org.freedesktop.DBus.Properties"))
        .and_then(|b| b.member("PropertiesChanged"))
        .and_then(|b| b.path_namespace(path.clone()))
        .map_err(|e| e.to_string())?
        .build();
    let added = MatchRule::builder()
        .msg_type(MsgType::Signal)
        .sender(BLUEZ)
        .and_then(|b| b.interface("org.freedesktop.DBus.ObjectManager"))
        .and_then(|b| b.member("InterfacesAdded"))
        .map_err(|e| e.to_string())?
        .build();
    let changed = MessageIterator::for_match_rule(changed, &conn, Some(4096)).map_err(err)?;
    let added = MessageIterator::for_match_rule(added, &conn, Some(1024)).map_err(err)?;

    let a = Proxy::new(&conn, BLUEZ, path.clone(), "org.bluez.Adapter1").map_err(err)?;
    let filter: HashMap<&str, Value> = HashMap::from([
        ("Transport", Value::from("le")),
        ("DuplicateData", Value::from(true)),
    ]);
    a.call_method("SetDiscoveryFilter", &(filter,))
        .map_err(err)?;
    match a.call_method("StartDiscovery", &()) {
        Ok(_) => {}
        Err(zbus::Error::MethodError(n, _, _)) if n.as_str() == "org.bluez.Error.InProgress" => {}
        Err(e) => return Err(err(e)),
    }

    // Both signal streams feed one worker, which owns the per-device state.
    enum Raw {
        Device(String, HashMap<String, OwnedValue>),
        AdapterOff,
    }
    let (raw_tx, raw_rx) = mpsc::channel::<Raw>();
    {
        let raw_tx = raw_tx.clone();
        let adapter_path = path.clone();
        std::thread::spawn(move || {
            for m in changed {
                let Ok(m) = m else { continue };
                let p = m.header().path().map(|p| p.to_string()).unwrap_or_default();
                type Body = (String, HashMap<String, OwnedValue>, Vec<String>);
                let Ok((iface, props, _)) = m.body().deserialize::<Body>() else {
                    continue;
                };
                let msg = match iface.as_str() {
                    "org.bluez.Device1" => Raw::Device(p, props),
                    "org.bluez.Adapter1"
                        if p == adapter_path
                            && props
                                .get("Powered")
                                .is_some_and(|v| matches!(inner(v), Value::Bool(false))) =>
                    {
                        Raw::AdapterOff
                    }
                    _ => continue,
                };
                if raw_tx.send(msg).is_err() {
                    return;
                }
            }
        });
    }
    {
        let prefix = format!("{path}/");
        std::thread::spawn(move || {
            for m in added {
                let Ok(m) = m else { continue };
                type Body = (
                    OwnedObjectPath,
                    HashMap<String, HashMap<String, OwnedValue>>,
                );
                let Ok((p, mut ifs)) = m.body().deserialize::<Body>() else {
                    continue;
                };
                if !p.as_str().starts_with(&prefix) {
                    continue;
                }
                if let Some(props) = ifs.remove("org.bluez.Device1")
                    && raw_tx
                        .send(Raw::Device(p.as_str().to_string(), props))
                        .is_err()
                {
                    return;
                }
            }
        });
    }

    let hello = serde_json::json!({
        "t": "hello", "node": "ble", "via": "bluez", "adapter": adapter, "addr": addr,
        "scan": "BlueZ LE discovery (active), duplicates on",
    });
    let source = format!("BLE {adapter} {addr}");
    std::thread::spawn(move || {
        let clock = Clock(start);
        let began = Instant::now();
        let send = |line: Line| {
            tx.send(Event::Line {
                node,
                line,
                at_ms: clock.ms(),
            })
            .is_ok()
        };
        if !send(Line::Hello(hello)) {
            return;
        }
        let mut rssi: HashMap<String, i16> = HashMap::new();
        let (mut adv, mut rid, mut tot_adv, mut tot_rid) = (0u64, 0u64, 0u64, 0u64);
        let mut window = Instant::now();
        let why = loop {
            if stop_after.is_some_and(|d| began.elapsed() >= d) {
                break "scan time over".to_string();
            }
            match raw_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Raw::Device(p, props)) => {
                    let u = device_update(&props);
                    if u.advert {
                        adv += 1;
                    }
                    if let Some(r) = u.rssi {
                        rssi.insert(p.clone(), r);
                    }
                    if let (Some(data), Some(mac)) = (u.rid, ble::mac_from_path(&p)) {
                        let r = rssi.get(&p).copied().unwrap_or(-127);
                        if let Some(l) = ble::rid_line(mac, r, &data) {
                            rid += 1;
                            if !send(Line::Rid(l)) {
                                return;
                            }
                        }
                    }
                }
                Ok(Raw::AdapterOff) => break "adapter powered off".to_string(),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break "D-Bus connection lost".to_string(),
            }
            let w = window.elapsed();
            if w >= Duration::from_secs(1) {
                tot_adv += adv;
                tot_rid += rid;
                let stats = serde_json::json!({
                    "t": "stats", "adv": adv, "rid": rid, "win_ms": w.as_millis() as u64,
                    "devices": rssi.len(), "tot_adv": tot_adv, "tot_rid": tot_rid,
                });
                (adv, rid) = (0, 0);
                window = Instant::now();
                if !send(Line::Stats(stats)) {
                    return;
                }
            }
        };
        let _ = a.call_method("StopDiscovery", &());
        let _ = tx.send(Event::End { node, why });
    });
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(v: Value<'static>) -> OwnedValue {
        OwnedValue::try_from(v).unwrap()
    }

    fn service_data(uuid: &str, data: Vec<u8>) -> OwnedValue {
        let m: HashMap<String, Value> = HashMap::from([(uuid.to_string(), Value::from(data))]);
        owned(Value::from(m))
    }

    #[test]
    fn rid_service_data_and_rssi() {
        let props = HashMap::from([
            ("RSSI".to_string(), owned(Value::I16(-63))),
            (
                "ServiceData".to_string(),
                service_data(ble::RID_UUID, vec![0x0D, 5, 0x02]),
            ),
        ]);
        let u = device_update(&props);
        assert_eq!(
            u,
            Update {
                rssi: Some(-63),
                rid: Some(vec![0x0D, 5, 0x02]),
                advert: true
            }
        );
    }

    #[test]
    fn other_service_data_is_an_advert_but_not_rid() {
        let props = HashMap::from([(
            "ServiceData".to_string(),
            service_data("0000feaa-0000-1000-8000-00805f9b34fb", vec![1, 2, 3]),
        )]);
        let u = device_update(&props);
        assert!(u.advert);
        assert_eq!(u.rid, None);
    }

    #[test]
    fn non_advert_properties_are_ignored() {
        let props = HashMap::from([
            ("Connected".to_string(), owned(Value::Bool(false))),
            ("Alias".to_string(), owned(Value::from("x"))),
        ]);
        assert_eq!(device_update(&props), Update::default());
    }

    #[test]
    fn wrong_types_do_not_panic() {
        let props = HashMap::from([
            ("RSSI".to_string(), owned(Value::from("loud"))),
            ("ServiceData".to_string(), owned(Value::U32(7))),
        ]);
        let u = device_update(&props);
        assert_eq!((u.rssi, u.rid), (None, None));
    }

    #[test]
    fn full_advert_through_to_messages() {
        let raw = rid::encode::message(&rid::message::Message::OperatorId(
            rid::message::OperatorId {
                operator_id_type: rid::message::OperatorIdType::OperatorId,
                operator_id: rid::message::FixedStr::new(b"TEST-OPERATOR").unwrap(),
            },
        ))
        .unwrap();
        let mut data = vec![0x0D, 0x22];
        data.extend_from_slice(&raw);
        let props = HashMap::from([(
            "ServiceData".to_string(),
            service_data(&ble::RID_UUID.to_uppercase(), data),
        )]);
        let u = device_update(&props);
        let l = ble::rid_line([0xAA; 6], -70, &u.rid.unwrap()).unwrap();
        assert_eq!(l.counter, 0x22);
        assert_eq!(l.msgs.len(), 1);
    }
}
