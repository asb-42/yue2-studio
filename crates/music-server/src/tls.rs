//! Self-signed HTTPS for the LAN.
//!
//! Browsers grant `AudioWorklet` (the piano-roll editor's synth, the
//! visualiser pop-out feed) only in secure contexts, and a headless studio
//! has no public name to get a real certificate for. So the service mints its
//! own server certificate on first use: `YUE_TLS_PORT` set serves the whole
//! studio — UI and API — over TLS on that port alongside plain HTTP, with the
//! certificate kept in the data root (`tls/`) and its SHA-256 fingerprint
//! printed to the log, so the browser's one click-through is checkable.
//! Authentication does not change: off-machine browsers still need network
//! access with its key.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// `YUE_TLS_PORT` set: the HTTPS port. Unset or empty: HTTP only.
pub fn tls_port() -> Option<u16> {
    std::env::var("YUE_TLS_PORT").ok().filter(|value| !value.trim().is_empty())?.parse().ok()
}

fn tls_dir(data_root: &Path) -> PathBuf {
    data_root.join("tls")
}

fn cert_path(data_root: &Path) -> PathBuf {
    tls_dir(data_root).join("cert.pem")
}

fn key_path(data_root: &Path) -> PathBuf {
    tls_dir(data_root).join("key.pem")
}

fn fingerprint_path(data_root: &Path) -> PathBuf {
    tls_dir(data_root).join("fingerprint.txt")
}

/// The certificate and key: PEM on disk for humans, DER in memory for
/// rustls, with the fingerprint. Minting also writes the fingerprint file the
/// log prints on later boots.
pub fn ensure_self_signed(data_root: &Path) -> Result<(Vec<u8>, Vec<u8>, String)> {
    if let (Ok(cert), Ok(key), Ok(print)) =
        (std::fs::read(cert_path(data_root)), std::fs::read(key_path(data_root)), std::fs::read_to_string(fingerprint_path(data_root)))
    {
        if !cert.is_empty() && !key.is_empty() && !print.trim().is_empty() {
            let (cert_der, key_der) = der_of(&cert, &key)?;
            return Ok((cert_der, key_der, print.trim().to_string()));
        }
    }
    let minted = generate()?;
    let print = fingerprint(&minted.cert_der);
    std::fs::create_dir_all(tls_dir(data_root)).with_context(|| format!("create {}", tls_dir(data_root).display()))?;
    std::fs::write(cert_path(data_root), &minted.cert_pem).with_context(|| format!("write {}", cert_path(data_root).display()))?;
    std::fs::write(key_path(data_root), &minted.key_pem).with_context(|| format!("write {}", key_path(data_root).display()))?;
    // The key alone lets anyone on the machine impersonate the studio to its
    // browsers; it is readable only by its owner.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(key_path(data_root), std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::write(fingerprint_path(data_root), &print)?;
    Ok((minted.cert_der, minted.key_der, print))
}

/// Minted material in both shapes.
struct Minted {
    cert_pem: Vec<u8>,
    key_pem: Vec<u8>,
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
}

/// PEM back to DER on reload, without a parser dependency: the files hold
/// exactly what the generator wrote.
fn der_of(cert_pem: &[u8], key_pem: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    use base64::Engine as _;
    fn body(pem: &[u8]) -> Result<Vec<u8>> {
        let text = String::from_utf8_lossy(pem);
        let body: String = text.lines().filter(|line| !line.starts_with("-----")).collect();
        base64::engine::general_purpose::STANDARD.decode(&body).context("the PEM is not base64")
    }
    Ok((body(cert_pem)?, body(key_pem)?))
}

/// A ten-year server certificate for localhost, loopback, and this machine's
/// LAN addresses, so the click-through names the address actually opened.
fn generate() -> Result<Minted> {
    use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
    use std::net::IpAddr;

    let mut distinguished = DistinguishedName::new();
    distinguished.push(DnType::CommonName, "YuE2 Studio (self-signed)");
    // The constructor only takes DNS names; loopback and LAN IPs join as
    // typed entries afterwards.
    let mut params = CertificateParams::new(vec!["localhost".to_string()]).context("prepare the certificate parameters")?;
    params.distinguished_name = distinguished;
    let mut ips = vec!["127.0.0.1".to_string(), "::1".to_string()];
    ips.extend(lan_addresses());
    ips.sort();
    ips.dedup();
    for address in ips {
        if let Ok(ip) = address.parse::<IpAddr>() {
            params.subject_alt_names.push(SanType::IpAddress(ip));
        }
    }
    params.not_before = time::OffsetDateTime::now_utc();
    params.not_after = params.not_before.checked_add(time::Duration::days(365 * 10)).context("ten years fit the calendar")?;
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).context("generate the key")?;
    let certificate = params.self_signed(&key).context("self-sign the certificate")?;
    Ok(Minted {
        cert_pem: certificate.pem().into_bytes(),
        key_pem: key.serialize_pem().into_bytes(),
        cert_der: certificate.der().to_vec(),
        key_der: key.serialize_der(),
    })
}

/// This machine's LAN addresses, the ones browsers on the network open:
/// the address the default route would send from, without sending anything.
fn lan_addresses() -> Vec<String> {
    let mut found = Vec::new();
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("192.168.0.1:9").is_ok() {
            if let Ok(address) = socket.local_addr() {
                if !address.ip().is_loopback() && !address.ip().is_unspecified() {
                    found.push(address.ip().to_string());
                }
            }
        }
    }
    found
}

/// The SHA-256 fingerprint over the certificate DER, the way browsers show
/// it: uppercase hex pairs separated by colons.
fn fingerprint(cert_der: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(cert_der);
    digest.iter().map(|byte| format!("{byte:02X}")).collect::<Vec<_>>().join(":")
}

/// Serve the studio over TLS with the minted certificate. HTTP/1.1 only:
/// the studio streams server-sent events, which HTTP/2 intermediaries buffer.
pub async fn serve_tls(app: axum::Router, address: std::net::SocketAddr, data_root: &Path) -> Result<()> {
    let (cert, key, print) = ensure_self_signed(data_root)?;
    {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }
    let mut server = rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(
        vec![rustls::pki_types::CertificateDer::from(cert)],
        rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(key)),
    )?;
    server.alpn_protocols = vec![b"http/1.1".to_vec()];
    let config = axum_server::tls_rustls::RustlsConfig::from_config(std::sync::Arc::new(server));
    println!("https on https://{} (fingerprint SHA-256: {print})", address);
    println!("check the fingerprint matches once, then accept: the page is a secure context and the editor works");
    let handle = axum_server::Handle::new();
    tokio::spawn({
        let handle = handle.clone();
        async move {
            let _ = tokio::signal::ctrl_c().await;
            handle.graceful_shutdown(None);
        }
    });
    axum_server::bind_rustls(address, config)
        .handle(handle)
        .serve(app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tls_port_is_opt_in_and_must_parse() {
        unsafe { std::env::remove_var("YUE_TLS_PORT") };
        assert_eq!(tls_port(), None);
        unsafe { std::env::set_var("YUE_TLS_PORT", "8792") };
        assert_eq!(tls_port(), Some(8792));
        unsafe { std::env::set_var("YUE_TLS_PORT", "nope") };
        assert_eq!(tls_port(), None);
        unsafe { std::env::remove_var("YUE_TLS_PORT") };
    }

    #[test]
    fn a_minted_certificate_covers_localhost_and_parses_as_pem() {
        let root = std::env::temp_dir().join(format!("tls-{}", uuid::Uuid::now_v7()));
        let (cert, key, print) = ensure_self_signed(&root).unwrap();
        // humans get PEM on disk, rustls gets DER in memory
        let text = std::fs::read_to_string(cert_path(&root)).unwrap();
        assert!(text.contains("-----BEGIN CERTIFICATE-----"));
        assert!(std::fs::read_to_string(key_path(&root)).unwrap().contains("PRIVATE KEY"));
        assert!(!cert.is_empty() && !key.is_empty());
        // the fingerprint the log will print: 32 colon-separated hex pairs
        let pairs: Vec<&str> = print.split(':').collect();
        assert_eq!(pairs.len(), 32);
        assert!(pairs.iter().all(|pair| pair.len() == 2 && pair.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase())));
        // minting twice keeps the certificate instead of rotating it
        let (again, _, same) = ensure_self_signed(&root).unwrap();
        assert_eq!(cert, again);
        assert_eq!(print, same);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(key_path(&root)).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&root).ok();
    }
}
