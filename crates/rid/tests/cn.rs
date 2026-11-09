//! GB 46750-2025 decoder against core-c's libopendroneidcn.

mod common;

use common::*;
use odid_ref as c;
use rid::cn;

/// Decode with both; assert they agree on acceptance and every field.
fn compare(buf: &[u8]) -> bool {
    let mut d = c::DroneRIDData_t::default();
    let mut ver = 0u8;
    let rc = unsafe { c::CN46750_RID_Decode(buf.as_ptr(), buf.len(), &mut d, &mut ver) };
    let ours = cn::decode(buf);
    let c_ok = rc == c::RID_OK || rc == c::RID_OK_EXTENSION;
    assert_eq!(c_ok, ours.is_ok(), "rc {rc}, ours {ours:?}, {buf:02x?}");
    let Ok(m) = ours else { return false };
    let ctx = format!("{buf:02x?}\nC: {d:?}\nours: {m:?}");
    assert_eq!(m.sub_version, ver, "{ctx}");
    assert_eq!(m.has_extensions, rc == c::RID_OK_EXTENSION, "{ctx}");
    assert_eq!(ours_str(&m.serial), cstr(&d.sn), "{ctx}");
    assert_eq!(ours_str(&m.uin), cstr(&d.uin), "{ctx}");
    assert_eq!(
        m.category.map(|x| x.code() as i32),
        d.has_op_category.then_some(d.op_category),
        "{ctx}"
    );
    assert_eq!(m.class.code() as i32, d.drone_class, "{ctx}");
    assert_eq!(m.gcs_position_type.code() as i32, d.gcs_pos_type, "{ctx}");
    // core-c turns a per-axis invalid code into 0.0; we drop the whole position.
    let pos = |p: Option<rid::message::LatLon>, lat: f64, lon: f64| match p {
        Some(p) => assert!(p.lat == lat && p.lon == lon, "{ctx}"),
        None => {
            let invalid_axis = lat == 0.0 || lon == 0.0;
            let out_of_range = !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon);
            assert!(invalid_axis || out_of_range, "{ctx}");
        }
    };
    pos(m.gcs_position, d.gcs_lat, d.gcs_lon);
    pos(m.position, d.drone_lat, d.drone_lon);
    let alt =
        |ours: Option<f32>, c: f32, unknown: f32| assert_eq!(ours.unwrap_or(unknown), c, "{ctx}");
    alt(m.gcs_altitude, d.gcs_alt, -1000.0);
    alt(m.altitude_geodetic, d.drone_alt, -1000.0);
    alt(m.track, d.track_angle, -1.0);
    alt(m.ground_speed, d.ground_speed, -1.0);
    if d.has_rel_alt {
        alt(m.height_above_takeoff, d.rel_alt, -9000.0);
    } else {
        assert!(m.height_above_takeoff.is_none());
    }
    if d.has_v_speed {
        alt(m.speed_vertical, d.vertical_speed, 0.0);
    } else {
        assert!(m.speed_vertical.is_none());
    }
    if d.has_pressure_alt {
        alt(m.altitude_pressure, d.pressure_alt, -1000.0);
    } else {
        assert!(m.altitude_pressure.is_none());
    }
    assert_eq!(m.status.code() as i32, d.op_status, "{ctx}");
    assert_eq!(m.coordinate_system.code() as i32, d.coord_system, "{ctx}");
    assert_eq!(m.horizontal_accuracy.code(), d.horizontal_accuracy, "{ctx}");
    assert_eq!(m.vertical_accuracy.code(), d.vertical_accuracy, "{ctx}");
    assert_eq!(m.speed_accuracy.code(), d.speed_accuracy, "{ctx}");
    assert_eq!(m.timestamp_ms.unwrap_or(0), d.timestamp_ms, "{ctx}");
    assert_eq!(m.timestamp_accuracy, d.timestamp_accuracy, "{ctx}");
    true
}

fn gen_data(r: &mut Rng) -> c::DroneRIDData_t {
    let mut d = c::DroneRIDData_t::default();
    let mut sn = r.text(20);
    if sn.is_empty() {
        sn.push(b'A');
    }
    set_cstr(&mut d.sn, &sn);
    let mut uin = r.text(8);
    if uin.is_empty() {
        uin.push(b'1');
    }
    set_cstr(&mut d.uin, &uin);
    d.has_op_category = r.chance(2);
    d.op_category = r.range(0, 15) as i32;
    d.drone_class = r.range(0, 15) as i32;
    d.gcs_pos_type = r.range(0, 15) as i32;
    d.gcs_lat = r.f64_in(-90.0, 90.0);
    d.gcs_lon = r.f64_in(-180.0, 180.0);
    d.gcs_alt = r.f32_in(-1000.0, 31000.0);
    d.drone_lat = r.f64_in(-90.0, 90.0);
    d.drone_lon = r.f64_in(-180.0, 180.0);
    d.track_angle = if r.chance(8) {
        -1.0
    } else {
        r.f32_in(0.0, 359.9)
    };
    d.ground_speed = if r.chance(8) {
        -1.0
    } else {
        r.f32_in(0.0, 300.0)
    };
    d.has_rel_alt = r.chance(2);
    d.rel_alt = r.f32_in(-9000.0, 20000.0);
    d.has_v_speed = r.chance(2);
    d.vertical_speed = r.f32_in(-63.0, 63.0);
    d.drone_alt = r.f32_in(-1000.0, 31000.0);
    d.has_pressure_alt = r.chance(2);
    d.pressure_alt = r.f32_in(-1000.0, 31000.0);
    d.op_status = r.range(0, 15) as i32;
    d.coord_system = r.range(0, 15) as i32;
    d.horizontal_accuracy = r.range(0, 15) as u8;
    d.vertical_accuracy = r.range(0, 15) as u8;
    d.speed_accuracy = r.range(0, 15) as u8;
    d.timestamp_ms = r.u64() & 0xFFFF_FFFF_FFFF;
    d.timestamp_accuracy = r.range(0, 15) as u8;
    d
}

#[test]
fn core_c_encoded_packets_decode_alike() {
    let mut r = Rng::new(300);
    for _ in 0..50_000 {
        let d = gen_data(&mut r);
        let mut buf = [0u8; 160];
        let mut len = 0usize;
        assert_eq!(
            unsafe { c::CN46750_RID_Encode(&d, buf.as_mut_ptr(), buf.len(), &mut len) },
            c::RID_OK
        );
        assert!(compare(&buf[..len]));
        // Trailing bytes are ignored by both; find() locates the packet in noise.
        let noise_len = r.below(20);
        let mut noisy = r
            .bytes(noise_len)
            .into_iter()
            .map(|b| b & 0x7F)
            .collect::<Vec<_>>();
        let at = noisy.len();
        noisy.extend_from_slice(&buf[..len]);
        noisy.extend_from_slice(&[0xAA; 5]);
        assert!(compare(&noisy[at..]));
        assert_eq!(cn::find(&noisy), Some(&buf[..len]));
    }
}

/// Structured random packets: valid header, random flag chains (often with
/// the mandatory bits set) and random data, sometimes truncated.
#[test]
fn random_packets_agree_with_core_c() {
    let mut r = Rng::new(301);
    let mut accepted = 0;
    for _ in 0..300_000 {
        let n_flags = 1 + r.below(5);
        let mut flags: Vec<u8> = (0..n_flags).map(|_| r.u8() | 0x01).collect();
        if r.chance(2) {
            flags[0] |= 0xDE;
            if n_flags > 1 {
                flags[1] |= 0xE4;
            }
            if n_flags > 2 {
                flags[2] |= 0xFE;
            }
        }
        if !r.chance(10) {
            *flags.last_mut().unwrap() &= 0xFE;
        }
        let data_len = if r.chance(20) {
            r.u8()
        } else {
            r.range(40, 120) as u8
        };
        let mut buf = vec![
            0xFF,
            if r.chance(20) {
                r.u8()
            } else {
                0x20 | (r.u8() & 0x1F)
            },
            data_len,
        ];
        buf.extend_from_slice(&flags);
        for _ in 0..data_len {
            // Mostly printable so the ASCII checks pass now and then.
            buf.push(if r.chance(4) {
                r.u8()
            } else {
                r.range(0, 0x7E) as u8 & if r.chance(3) { 0 } else { 0xFF }
            });
        }
        if r.chance(2) {
            // Mutate a valid packet instead, so acceptance paths get exercised too.
            let d = gen_data(&mut r);
            let mut enc = [0u8; 160];
            let mut len = 0usize;
            unsafe { c::CN46750_RID_Encode(&d, enc.as_mut_ptr(), enc.len(), &mut len) };
            buf = enc[..len].to_vec();
            for _ in 0..r.below(3) {
                let at = r.below(buf.len());
                match r.below(4) {
                    0 => buf[at] = r.u8(),
                    1 => buf[at] ^= 1 << r.below(8),
                    2 => buf[2] = buf[2].wrapping_add(r.range(-3, 3) as u8),
                    _ => buf.extend_from_slice(&[0xFF, 0xFF]),
                }
            }
        }
        let cut = if r.chance(5) {
            r.below(buf.len() + 1)
        } else {
            buf.len()
        };
        if compare(&buf[..cut]) {
            accepted += 1;
        }
        // find() agrees with core-c's scanner.
        let mut off = 0usize;
        let mut len = 0usize;
        let found = unsafe { c::CN46750_FindPacket(buf.as_ptr(), cut, &mut off, &mut len) };
        assert_eq!(cn::find(&buf[..cut]), found.then(|| &buf[off..off + len]));
    }
    assert!(accepted > 30_000, "{accepted}");
}

#[test]
fn random_bytes_never_panic() {
    let mut r = Rng::new(302);
    for _ in 0..1_000_000 {
        let len = r.below(300);
        let mut b = r.bytes(len);
        if let Some(x) = b.first_mut() {
            *x = 0xFF;
        }
        if let Some(x) = b.get_mut(1) {
            *x = 0x20 | (*x & 0x1F);
        }
        let _ = cn::decode(&b);
        let _ = cn::find(&b);
    }
}
