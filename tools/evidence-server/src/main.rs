//! evidence-server (see lib.rs for the API and docs/EVIDENCE.md for use).
//!
//! ```text
//! evidence-server serve --data DIR [--listen 127.0.0.1:8650]
//!                 [--tls-cert cert.pem --tls-key key.pem] [--token T]
//!                 [--name NAME] [--open-registration]
//!                 [--stamp [--stamp-tsa URL[,chain=URL][,crl=URL]]...
//!                  [--stamp-every MIN] [--stamp-idle SEC] [--stamp-alarm-hours H]]
//! evidence-server keygen --out FILE
//! ```
//!
//! Outside time-stamping is off unless `--stamp` is given: it sends a
//! SHA-256 fingerprint of each unit's access log (nothing else) to the
//! services listed, by default the built-in ones (`stamp::default_authorities`).
//! Each `--stamp-tsa` replaces the built-in list. See docs/EVIDENCE.md.
//!
//! Registering a unit needs the token. Without `--token` the server refuses
//! to register anything unless `--open-registration` is given (a bench
//! laptop nobody else can reach).
//!
//! Plain HTTP is for the bench on one laptop only: anyone on the path can
//! read or block uploads (they still cannot forge records: the unit's
//! signatures are checked). Use TLS anywhere else.
//!
//! `keygen` makes a development signing key for a hub build
//! (`ROTORSCOPE_DEV_KEY=$(cat FILE) cargo fw` in firmware/hub-p4). It is a
//! software key: not evidence-grade.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use evidence_server::{router, store::Store, App};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("serve") => serve(&args),
        Some("keygen") => keygen(&args),
        _ => {
            eprintln!("usage: evidence-server serve --data DIR [--listen ADDR] [--tls-cert PEM --tls-key PEM] [--token T] [--name NAME] [--open-registration]");
            eprintln!("         [--stamp [--stamp-tsa URL[,chain=URL][,crl=URL]]... [--stamp-every MIN] [--stamp-idle SEC] [--stamp-alarm-hours H]]");
            eprintln!("       evidence-server keygen --out FILE");
            std::process::exit(2);
        }
    }
}

fn keygen(args: &[String]) {
    let Some(out) = arg(args, "--out") else {
        eprintln!("keygen needs --out FILE");
        std::process::exit(2);
    };
    let out = PathBuf::from(out);
    if out.exists() {
        eprintln!("{} exists: not overwriting a key", out.display());
        std::process::exit(2);
    }
    let (secret, public) = loop {
        let mut k = [0u8; 32];
        getrandom::getrandom(&mut k).expect("random");
        if let Some(p) = evidence::sign::public_of(&k) {
            break (k, p);
        }
    };
    let hexs = evidence::verify::hex(&secret);
    {
        use std::io::Write;
        #[cfg(unix)]
        use std::os::unix::fs::OpenOptionsExt;
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        o.mode(0o600);
        let mut f = o.open(&out).expect("create key file");
        writeln!(f, "{hexs}").expect("write key");
    }
    println!("development key written to {} (keep it off git; anyone with it can sign as this unit)", out.display());
    println!("public key  {}", evidence::verify::hex(&public));
    println!("fingerprint {}", std::str::from_utf8(&evidence::fingerprint(&public)).unwrap());
}

fn serve(args: &[String]) {
    let data = arg(args, "--data").unwrap_or_else(|| "evidence-data".into());
    let listen: SocketAddr = arg(args, "--listen").unwrap_or_else(|| "127.0.0.1:8650".into()).parse().expect("--listen ADDR:PORT");
    let store = Store::open(&data).expect("open data directory");
    let tls = match (arg(args, "--tls-cert"), arg(args, "--tls-key")) {
        (Some(c), Some(k)) => Some(tls_config(&c, &k)),
        (None, None) => None,
        _ => panic!("--tls-cert and --tls-key go together"),
    };
    let name = arg(args, "--name").unwrap_or_else(|| format!("{}://{listen}", if tls.is_some() { "https" } else { "http" }));
    let token = arg(args, "--token").or_else(|| std::env::var("EVIDENCE_TOKEN").ok());
    let cfg = stamp_config(args);
    let stamping = cfg.enabled.then(|| evidence_server::stamp::Stamping::new(cfg.clone()));
    let app = Arc::new(App::new(store, token, args.iter().any(|a| a == "--open-registration"), name.clone(), stamping));
    let n = app.lock().devices.len();
    eprintln!("evidence-server: data {data}, {n} devices, listening on {name}{}", if tls.is_none() { " (plain HTTP: bench only, not evidence-grade transport)" } else { "" });
    if app.token.is_none() {
        if app.open_registration {
            eprintln!("evidence-server: WARNING: no --token and --open-registration: anyone who can reach {listen} can register units and read everything");
        } else {
            eprintln!("evidence-server: no --token: registering units is refused (add --token T, or --open-registration for a bench laptop)");
        }
    }
    if cfg.enabled {
        let list: Vec<&str> = cfg.authorities.iter().map(|a| a.url.as_str()).collect();
        eprintln!("evidence-server: outside time-stamping ON: {} (one stamp per unit at most every {} min while uploading, and when uploads stop)", list.join(", "), cfg.every_ms / 60_000);
        eprintln!("evidence-server: it sends only a SHA-256 fingerprint of each unit's access log; the services also see this server's address");
    }
    for w in cfg.warnings() {
        eprintln!("evidence-server: WARNING: {w}");
    }
    if cfg.enabled {
        evidence_server::stamp::spawn_worker(app.clone());
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(listen).await.expect("bind");
        let r = router(app);
        match tls {
            None => axum::serve(listener, r.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap(),
            Some(cfg) => serve_tls(listener, r, cfg).await,
        }
    });
}

/// Every value after each `name`.
fn args_all(args: &[String], name: &str) -> Vec<String> {
    args.windows(2).filter(|w| w[0] == name).map(|w| w[1].clone()).collect()
}

fn stamp_config(args: &[String]) -> evidence_server::stamp::Config {
    use evidence_server::stamp::{Authority, Config};
    if arg(args, "--tsa").is_some() {
        eprintln!("--tsa is replaced by --stamp (automatic, access-log stamps) and --stamp-tsa URL; see docs/EVIDENCE.md");
        std::process::exit(2);
    }
    let mut c = Config { enabled: args.iter().any(|a| a == "--stamp"), ..Config::default() };
    let specs = args_all(args, "--stamp-tsa");
    if !specs.is_empty() {
        c.authorities = specs
            .iter()
            .map(|s| {
                Authority::parse(s).unwrap_or_else(|e| {
                    eprintln!("--stamp-tsa {e}");
                    std::process::exit(2)
                })
            })
            .collect();
        if !c.enabled {
            eprintln!("evidence-server: --stamp-tsa given without --stamp: outside time-stamping stays OFF");
        }
    }
    let num = |name: &str, unit: u64, default: u64| -> u64 {
        match arg(args, name) {
            None => default,
            Some(v) => v.parse::<u64>().ok().filter(|&n| n > 0).map(|n| n.saturating_mul(unit)).unwrap_or_else(|| {
                eprintln!("{name} needs a whole number above 0");
                std::process::exit(2)
            }),
        }
    };
    c.every_ms = num("--stamp-every", 60_000, c.every_ms);
    c.idle_ms = num("--stamp-idle", 1000, c.idle_ms);
    c.alarm_ms = num("--stamp-alarm-hours", 3_600_000, c.alarm_ms);
    c
}

fn tls_config(cert: &str, key: &str) -> Arc<rustls::ServerConfig> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert).expect("read cert").map(|c| c.expect("cert")).collect();
    let key = PrivateKeyDer::from_pem_file(key).expect("read key");
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .expect("certificate and key");
    Arc::new(cfg)
}

async fn serve_tls(listener: tokio::net::TcpListener, r: axum::Router, cfg: Arc<rustls::ServerConfig>) {
    use axum::extract::ConnectInfo;
    use tower::ServiceExt;
    let acceptor = tokio_rustls::TlsAcceptor::from(cfg);
    loop {
        let Ok((tcp, addr)) = listener.accept().await else { continue };
        let acceptor = acceptor.clone();
        let r = r.clone();
        tokio::spawn(async move {
            // A peer that opens a connection and never finishes the
            // handshake must not hold a task and a socket for ever.
            let Ok(Ok(tls)) = tokio::time::timeout(std::time::Duration::from_secs(15), acceptor.accept(tcp)).await else { return };
            let svc = hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
                req.extensions_mut().insert(ConnectInfo(addr));
                r.clone().oneshot(req.map(axum::body::Body::new))
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .timer(hyper_util::rt::TokioTimer::new())
                .header_read_timeout(std::time::Duration::from_secs(30))
                .serve_connection(hyper_util::rt::TokioIo::new(tls), svc)
                .await;
        });
    }
}
