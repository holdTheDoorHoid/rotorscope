//! The BlueZ side: serve an `org.bluez.LEAdvertisement1` object, register
//! it, and change its `ServiceData` in place (a `PropertiesChanged` signal;
//! bluetoothd re-reads the property and updates the controller).
//!
//! BlueZ removes the advertisement by itself if this process dies, because
//! it watches the D-Bus client, so a crash never leaves the radio sending.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use zbus::blocking::{Connection, Proxy, connection};
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::payload::{self, RID_UUID, SERVICE_DATA_LEN};
use crate::{Options, rid_tx};

const BLUEZ: &str = "org.bluez";
const PATH: &str = "/org/rotorscope/ridbeacon0";

static STOP: AtomicBool = AtomicBool::new(false);
static RELEASED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn service_data(data: &[u8; SERVICE_DATA_LEN]) -> HashMap<String, OwnedValue> {
    let v = OwnedValue::try_from(Value::from(data.to_vec())).expect("byte array has no fds");
    HashMap::from([(RID_UUID.to_string(), v)])
}

/// The advertisement object. Two flavours, because a D-Bus property is
/// either there or not: `TxPower` is only offered when the adapter says it
/// can honour it (BT5 extended advertising); BT4 controllers transmit at a
/// fixed advertising power the host cannot change.
macro_rules! advert {
    ($name:ident $(, $tx:ident)?) => {
        pub struct $name {
            data: [u8; SERVICE_DATA_LEN],
            #[allow(dead_code)]
            tx_power: i16,
        }

        #[zbus::interface(name = "org.bluez.LEAdvertisement1")]
        impl $name {
            fn release(&self) {
                RELEASED.store(true, Ordering::SeqCst);
            }
            /// Non-connectable, non-scannable (ADV_NONCONN_IND).
            #[zbus(property, name = "Type")]
            fn kind(&self) -> String {
                "broadcast".into()
            }
            #[zbus(property)]
            fn service_data(&self) -> HashMap<String, OwnedValue> {
                service_data(&self.data)
            }
            // No `Discoverable` property, deliberately: on BlueZ 5.72 even
            // `false` makes the kernel reserve 3 bytes for Flags, and the
            // 31-byte Remote ID advert is then refused as too long.
            /// Nothing extra (name, tx-power, appearance) in the advert.
            #[zbus(property)]
            fn includes(&self) -> Vec<String> {
                Vec::new()
            }
            /// 100 ms, the minimum for non-connectable legacy adverts.
            #[zbus(property)]
            fn min_interval(&self) -> u32 {
                100
            }
            #[zbus(property)]
            fn max_interval(&self) -> u32 {
                100
            }
            $(
                #[zbus(property)]
                fn $tx(&self) -> i16 {
                    self.tx_power
                }
            )?
        }
    };
}
advert!(Advert);
advert!(AdvertTxPower, tx_power);

fn now_s() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn err_name(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(n, _, _) => n.as_str().to_string(),
        _ => String::new(),
    }
}

/// Open the bus, check the adapter, print what BlueZ says it can do.
fn adapter(o: &Options) -> Result<(Connection, String, Option<i16>), String> {
    let a = &o.adapter;
    if a.is_empty() || !a.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(format!("{a:?} is not an adapter name like hci0"));
    }
    let conn = Connection::system().map_err(|e| format!("cannot reach the system D-Bus: {e}"))?;
    let path = format!("/org/bluez/{a}");
    let props = Proxy::new(
        &conn,
        BLUEZ,
        path.clone(),
        "org.freedesktop.DBus.Properties",
    )
    .map_err(|e| e.to_string())?;
    let get = |iface: &str, name: &str| -> Result<OwnedValue, zbus::Error> {
        props.call("Get", &(iface, name))
    };
    match get("org.bluez.Adapter1", "Powered") {
        Ok(v) if matches!(*v, Value::Bool(true)) => {}
        Ok(_) => {
            return Err(format!(
                "{a} is powered off. Turn it on with `bluetoothctl`, then `select <its address>` \
                 and `power on`"
            ));
        }
        Err(e) if err_name(&e).starts_with("org.freedesktop.DBus.Error.ServiceUnknown") => {
            return Err("BlueZ is not running (no org.bluez on the system bus)".into());
        }
        Err(e) => return Err(format!("no usable Bluetooth adapter named {a}: {e}")),
    }
    let addr = match get("org.bluez.Adapter1", "Address").as_deref() {
        Ok(Value::Str(s)) => s.to_string(),
        _ => "?".into(),
    };
    let m = "org.bluez.LEAdvertisingManager1";
    let instances = match get(m, "SupportedInstances").as_deref() {
        Ok(Value::U8(n)) => *n,
        Err(e) => {
            return Err(format!(
                "{a} ({addr}) offers no LE advertising through BlueZ: {e}"
            ));
        }
        _ => 0,
    };
    let features: Vec<String> = match get(m, "SupportedFeatures").as_deref() {
        Ok(Value::Array(v)) => v
            .inner()
            .iter()
            .filter_map(|x| match x {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let mut caps: HashMap<String, String> = HashMap::new();
    let mut min_tx = None;
    if let Ok(Value::Dict(d)) = get(m, "SupportedCapabilities").as_deref() {
        for (k, v) in d.iter() {
            let (Value::Str(k), v) = (k, v) else { continue };
            let v = match v {
                Value::Value(b) => &**b,
                other => other,
            };
            if k.as_str() == "MinTxPower"
                && let Value::I16(p) = v
            {
                min_tx = Some(*p);
            }
            caps.insert(k.to_string(), format!("{v}"));
        }
    }
    eprintln!(
        "adapter {a} {addr}: free advert instances {instances}, features {features:?}, \
         capabilities {caps:?}"
    );
    if instances == 0 {
        return Err(format!(
            "{a}: no free advertising instance (another program is advertising?)"
        ));
    }
    let tx = if features.iter().any(|f| f == "CanSetTxPower") {
        let p = min_tx.unwrap_or(-127);
        eprintln!("TX power: asking for the minimum, {p} dBm");
        Some(p)
    } else {
        eprintln!(
            "TX power: not settable on {a} (no CanSetTxPower; a BT4 controller sends adverts \
             at its fixed advertising power)"
        );
        None
    };
    Ok((conn, path, tx))
}

pub fn run(o: &Options) -> Result<(), String> {
    let lat = std::env::var("RID_TEST_LAT").ok();
    let lon = std::env::var("RID_TEST_LON").ok();
    let centre = payload::centre(lat.as_deref(), lon.as_deref());
    if !centre.from_env_ok {
        eprintln!("RID_TEST_LAT/RID_TEST_LON unreadable or out of range; using the default centre");
    }
    // Self-check with the receivers' decoder before anything is sent.
    let t0 = now_s();
    let msgs = rid_tx::messages(&centre, &crate::flight::at(&centre, t0), t0)
        .map_err(|e| format!("encoding: {e:?}"))?;
    rid_tx::self_check(&msgs).map_err(|e| format!("self-check: {e:?}"))?;

    let (system, adapter_path, tx_power) = adapter(o)?;
    drop(system);

    let first = payload::frame(&centre, t0, 0, 0).map_err(|e| format!("encoding: {e:?}"))?;
    let builder = connection::Builder::system().map_err(|e| e.to_string())?;
    let conn = match tx_power {
        Some(p) => builder.serve_at(
            PATH,
            AdvertTxPower {
                data: first.data,
                tx_power: p,
            },
        ),
        None => builder.serve_at(
            PATH,
            Advert {
                data: first.data,
                tx_power: 0,
            },
        ),
    }
    .and_then(|b| b.build())
    .map_err(|e| format!("D-Bus: {e}"))?;

    // SAFETY: installing a handler that only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }

    let mgr = Proxy::new(
        &conn,
        BLUEZ,
        adapter_path,
        "org.bluez.LEAdvertisingManager1",
    )
    .map_err(|e| e.to_string())?;
    let path = ObjectPath::try_from(PATH).expect("valid path");
    let opts: HashMap<&str, Value> = HashMap::new();
    if let Err(e) = mgr.call_method("RegisterAdvertisement", &(&path, opts)) {
        let why = match err_name(&e).as_str() {
            "org.bluez.Error.InvalidLength" | "org.bluez.Error.Failed" => {
                " (if bluetoothd logs \"Advertising data too long\", something reserved room \
                 for a Flags field and the 31-byte advert has none to spare; an LE-only \
                 adapter with BR/EDR disabled does that)"
            }
            "org.bluez.Error.NotPermitted" => " (no advertising instance free)",
            _ => "",
        };
        return Err(format!("BlueZ refused the advertisement: {e}{why}"));
    }
    eprintln!(
        "advertising on {}: message changes every {} ms; counter increments per change",
        o.adapter, o.interval_ms
    );

    let start = Instant::now();
    let step_every = Duration::from_millis(o.interval_ms);
    let mut next = start + step_every;
    let mut step: usize = 0;
    let mut counter: u8 = 0;
    let mut sent_this_second = 0u32;
    let mut last_report = Instant::now();
    let result = loop {
        if STOP.load(Ordering::SeqCst) {
            break Ok("stopped".to_string());
        }
        if RELEASED.load(Ordering::SeqCst) {
            break Err("BlueZ released the advertisement (adapter powered off?)".to_string());
        }
        if o.secs
            .is_some_and(|s| start.elapsed() >= Duration::from_secs(s))
        {
            break Ok("time up".to_string());
        }
        let now = Instant::now();
        if now < next {
            std::thread::sleep((next - now).min(Duration::from_millis(50)));
            continue;
        }
        next += step_every;
        step = step.wrapping_add(1);
        counter = counter.wrapping_add(1);
        let t = now_s();
        let f = match payload::frame(&centre, t, counter, step) {
            Ok(f) => f,
            Err(e) => break Err(format!("encoding: {e:?}")),
        };
        let emitted = match tx_power {
            Some(_) => update::<AdvertTxPower>(&conn, f.data, |a, d| a.data = d),
            None => update::<Advert>(&conn, f.data, |a, d| a.data = d),
        };
        if let Err(e) = emitted {
            break Err(format!("updating the advert: {e}"));
        }
        sent_this_second += 1;
        if last_report.elapsed() >= Duration::from_secs(1) {
            println!(
                "{{\"t\":\"tx\",\"up\":{},\"ble4\":{},\"ctr\":{},\"msg\":\"{}\",\"lat\":{:.6},\"lon\":{:.6},\"hdg\":{:.0}}}",
                start.elapsed().as_secs(),
                sent_this_second,
                counter,
                f.kind,
                f.fix.lat,
                f.fix.lon,
                f.fix.heading_deg
            );
            sent_this_second = 0;
            last_report = Instant::now();
        }
    };
    let opts_path = ObjectPath::try_from(PATH).expect("valid path");
    let unreg = mgr.call_method("UnregisterAdvertisement", &(&opts_path,));
    match (&result, unreg) {
        (_, Ok(_)) => eprintln!("advertisement withdrawn"),
        (Err(_), Err(_)) => {}
        (Ok(_), Err(e)) => eprintln!("UnregisterAdvertisement: {e}"),
    }
    result.map(|why| eprintln!("{why}"))
}

/// Set the new service data and tell BlueZ it changed.
fn update<I>(
    conn: &Connection,
    data: [u8; SERVICE_DATA_LEN],
    set: impl Fn(&mut I, [u8; SERVICE_DATA_LEN]),
) -> zbus::Result<()>
where
    I: zbus::object_server::Interface + ServiceDataChanged,
{
    let iface = conn.object_server().interface::<_, I>(PATH)?;
    set(&mut iface.get_mut(), data);
    let guard = iface.get();
    zbus::block_on(guard.changed(iface.signal_emitter()))
}

/// Lets [`update`] emit the property-changed signal for either flavour.
pub trait ServiceDataChanged {
    fn changed(
        &self,
        emitter: &zbus::object_server::SignalEmitter<'_>,
    ) -> impl std::future::Future<Output = zbus::Result<()>>;
}

impl ServiceDataChanged for Advert {
    fn changed(
        &self,
        emitter: &zbus::object_server::SignalEmitter<'_>,
    ) -> impl std::future::Future<Output = zbus::Result<()>> {
        self.service_data_changed(emitter)
    }
}

impl ServiceDataChanged for AdvertTxPower {
    fn changed(
        &self,
        emitter: &zbus::object_server::SignalEmitter<'_>,
    ) -> impl std::future::Future<Output = zbus::Result<()>> {
        self.service_data_changed(emitter)
    }
}
