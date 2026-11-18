//! evidence-server (see lib.rs for the API and docs/EVIDENCE.md for use).
//!
//! ```text
//! evidence-server serve --data DIR [--listen 127.0.0.1:8650]
//!                 [--tls-cert cert.pem --tls-key key.pem] [--token T]
//!                 [--tsa URL] [--name NAME] [--open-registration]
//! evidence-server keygen --out FILE
//! ```
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
use std::sync::{Arc, Mutex};

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
            eprintln!("usage: evidence-server serve --data DIR [--listen ADDR] [--tls-cert PEM --tls-key PEM] [--token T] [--tsa URL] [--name NAME] [--open-registration]");
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
    let app = Arc::new(App { store: Mutex::new(store), open_registration: args.iter().any(|a| a == "--open-registration"), token: arg(args, "--token").or_else(|| std::env::var("EVIDENCE_TOKEN").ok()), server_name: name.clone(), tsa_url: arg(args, "--tsa") });
    let n = app.lock().devices.len();
    eprintln!("evidence-server: data {data}, {n} devices, listening on {name}{}", if tls.is_none() { " (plain HTTP: bench only, not evidence-grade transport)" } else { "" });
    if app.token.is_none() {
        if app.open_registration {
            eprintln!("evidence-server: WARNING: no --token and --open-registration: anyone who can reach {listen} can register units and read everything");
        } else {
            eprintln!("evidence-server: no --token: registering units is refused (add --token T, or --open-registration for a bench laptop)");
        }
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
            let Ok(tls) = acceptor.accept(tcp).await else { return };
            let svc = hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
                req.extensions_mut().insert(ConnectInfo(addr));
                r.clone().oneshot(req.map(axum::body::Body::new))
            });
            let _ = hyper::server::conn::http1::Builder::new().serve_connection(hyper_util::rt::TokioIo::new(tls), svc).await;
        });
    }
}
