//! CA lokal + sertifikat per situs yang dibuat saat dibutuhkan (SNI).
//!
//! CA dibatasi (name constraints) hanya untuk `.<tld>` dan `localhost`, jadi
//! walaupun file kuncinya bocor, CA ini tidak bisa dipakai memalsukan situs lain.

use rcgen::{
    BasicConstraints, Certificate, CertificateParams, CidrSubnet, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, GeneralSubtree, IsCa, KeyPair, KeyUsagePurpose, NameConstraints,
    SanType,
};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use time::{Duration, OffsetDateTime};

pub struct Ca {
    cert: Certificate,
    key: KeyPair,
}

pub fn cert_dir(home: &Path) -> PathBuf {
    home.join("certs")
}

pub fn ca_crt_path(home: &Path) -> PathBuf {
    cert_dir(home).join("herdlite-ca.crt")
}

fn ca_params(cn: &str, tld: &str) -> CertificateParams {
    let mut p = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, cn);
    dn.push(DnType::OrganizationName, "herdlite");
    p.distinguished_name = dn;
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
    // Hanya untuk sertifikat server web (bukan email/S-MIME atau code signing).
    p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    p.name_constraints = Some(NameConstraints {
        permitted_subtrees: vec![
            GeneralSubtree::DnsName(tld.to_string()),
            GeneralSubtree::DnsName("localhost".into()),
        ],
        // Semua alamat IP dilarang, supaya CA ini tidak bisa dipakai untuk https://192.168.x.x dll.
        excluded_subtrees: vec![
            GeneralSubtree::IpAddress(CidrSubnet::V4([0; 4], [0; 4])),
            GeneralSubtree::IpAddress(CidrSubnet::V6([0; 16], [0; 16])),
        ],
    });
    let now = OffsetDateTime::now_utc();
    p.not_before = now - Duration::days(1);
    p.not_after = now + Duration::days(3650);
    p
}

/// Nama (CN) CA untuk komputer ini.
pub fn ca_name() -> String {
    let pc = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into());
    format!("herdlite Local CA ({pc})")
}

impl Ca {
    /// Muat CA dari `<home>\certs`, atau buat baru kalau belum ada. Return (ca, baru_dibuat).
    pub fn load_or_create(home: &Path, tld: &str) -> Result<(Ca, bool), String> {
        let dir = cert_dir(home);
        let crt = ca_crt_path(home);
        let keyp = dir.join("herdlite-ca.key");
        let meta = dir.join("herdlite-ca.json");
        if crt.is_file() && keyp.is_file() && meta.is_file() {
            let key_pem = fs::read_to_string(&keyp).map_err(|e| e.to_string())?;
            let key = KeyPair::from_pem(&key_pem).map_err(|e| format!("kunci CA rusak: {e}"))?;
            let m: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&meta).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            let cn = m["cn"].as_str().unwrap_or("herdlite Local CA").to_string();
            let ctld = m["tld"].as_str().unwrap_or(tld).to_string();
            // Bangun ulang objek CA dengan DN + kunci yang sama untuk menandatangani sertifikat situs.
            let cert = ca_params(&cn, &ctld).self_signed(&key).map_err(|e| e.to_string())?;
            return Ok((Ca { cert, key }, false));
        }
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let cn = ca_name();
        let key = KeyPair::generate().map_err(|e| e.to_string())?;
        let cert = ca_params(&cn, tld).self_signed(&key).map_err(|e| e.to_string())?;
        fs::write(&crt, cert.pem()).map_err(|e| e.to_string())?;
        fs::write(&keyp, key.serialize_pem()).map_err(|e| e.to_string())?;
        fs::write(&meta, serde_json::json!({ "cn": cn, "tld": tld }).to_string()).map_err(|e| e.to_string())?;
        Ok((Ca { cert, key }, true))
    }

    /// Terbitkan sertifikat untuk host (plus wildcard satu level). Return (cert DER, key PKCS#8 DER).
    pub fn issue(&self, host: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
        let mut p = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, host);
        p.distinguished_name = dn;
        let mut sans = vec![SanType::DnsName(host.to_string().try_into().map_err(|e| format!("{e}"))?)];
        if host != "localhost" {
            if let Ok(w) = format!("*.{host}").try_into() {
                sans.push(SanType::DnsName(w));
            }
        }
        p.subject_alt_names = sans;
        p.is_ca = IsCa::ExplicitNoCa;
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::KeyEncipherment];
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        p.use_authority_key_identifier_extension = true;
        let now = OffsetDateTime::now_utc();
        p.not_before = now - Duration::days(1);
        p.not_after = now + Duration::days(390);
        let mut serial = [0u8; 16];
        let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut serial);
        serial[0] &= 0x7f;
        p.serial_number = Some(serial.to_vec().into());
        let key = KeyPair::generate().map_err(|e| e.to_string())?;
        let cert = p.signed_by(&key, &self.cert, &self.key).map_err(|e| e.to_string())?;
        Ok((cert.der().to_vec(), key.serialize_der()))
    }
}

pub struct Resolver {
    ca: Ca,
    tld: String,
    provider: Arc<CryptoProvider>,
    cache: Mutex<HashMap<String, Arc<CertifiedKey>>>,
}

/// Batas jumlah sertifikat di memori (mencegah halaman web memaksa ribuan nama).
const CACHE_MAX: usize = 256;

impl std::fmt::Debug for Resolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Resolver")
    }
}

impl ResolvesServerCert for Resolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let mut name = hello.server_name().unwrap_or("localhost").to_ascii_lowercase();
        // Hanya terbitkan untuk *.<tld> dan localhost; nama lain dapat sertifikat localhost.
        if name != "localhost" && !name.ends_with(&format!(".{}", self.tld)) {
            name = "localhost".into();
        }
        if let Some(c) = self.cache.lock().ok()?.get(&name) {
            return Some(c.clone());
        }
        let (cert, key) = self.ca.issue(&name).ok()?;
        let signing = self
            .provider
            .key_provider
            .load_private_key(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)))
            .ok()?;
        let ck = Arc::new(CertifiedKey::new(vec![CertificateDer::from(cert)], signing));
        let mut cache = self.cache.lock().ok()?;
        if cache.len() >= CACHE_MAX {
            cache.clear();
        }
        cache.insert(name, ck.clone());
        Some(ck)
    }
}

pub fn server_config(ca: Ca, tld: &str) -> Result<rustls::ServerConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let resolver = Resolver { ca, tld: tld.to_string(), provider: provider.clone(), cache: Mutex::new(HashMap::new()) };
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(resolver));
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(cfg)
}
