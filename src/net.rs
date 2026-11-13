//! Encrypted QUIC transport authenticated by device keys, plus LAN discovery.

use crate::secure::{self, Identity};
use anyhow::{anyhow, Result};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

const ALPN: &[u8] = b"tlsupporter/1";

/// Accepts any Ed25519 certificate at the TLS layer. The peer's key is then checked against the
/// team directory, which is what actually decides who may connect.
#[derive(Debug)]
struct KeyVerifier {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl KeyVerifier {
    fn check(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }
}

impl ServerCertVerifier for KeyVerifier {
    fn verify_server_cert(&self, _: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is not used".into()))
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.check(m, c, d)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

impl ClientCertVerifier for KeyVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }
    fn verify_client_cert(&self, _: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: UnixTime) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is not used".into()))
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.check(m, c, d)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

fn transport() -> Arc<quinn::TransportConfig> {
    let mut t = quinn::TransportConfig::default();
    t.keep_alive_interval(Some(Duration::from_secs(4)));
    t.max_idle_timeout(Some(Duration::from_secs(15).try_into().unwrap()));
    t.max_concurrent_uni_streams(32u32.into());
    t.max_concurrent_bidi_streams(8u32.into());
    t.datagram_receive_buffer_size(Some(1 << 20));
    t.datagram_send_buffer_size(1 << 20);
    t.stream_receive_window((8u32 << 20).into());
    t.receive_window((32u32 << 20).into());
    Arc::new(t)
}

pub fn create_endpoint(identity: &Identity, port: u16) -> Result<quinn::Endpoint> {
    let pkcs8 = PrivatePkcs8KeyDer::from(identity.pkcs8.clone());
    let key_pair = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(&pkcs8, &rcgen::PKCS_ED25519)?;
    let cert = rcgen::CertificateParams::new(vec!["tlsupporter".to_string()])?.self_signed(&key_pair)?;
    let chain = vec![CertificateDer::from(cert.der().to_vec())];
    let key = PrivateKeyDer::Pkcs8(pkcs8);
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(KeyVerifier { provider: provider.clone() });

    let mut server = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(verifier.clone())
        .with_single_cert(chain.clone(), key.clone_key())?;
    server.alpn_protocols = vec![ALPN.to_vec()];
    let mut server_cfg = quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server)?));
    server_cfg.transport_config(transport());

    let mut client = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(chain, key)?;
    client.alpn_protocols = vec![ALPN.to_vec()];
    let mut client_cfg = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(client)?));
    client_cfg.transport_config(transport());

    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
    let mut endpoint = quinn::Endpoint::server(server_cfg, addr)?;
    endpoint.set_default_client_config(client_cfg);
    Ok(endpoint)
}

/// Raw public key of the connected peer.
pub fn peer_public_key(conn: &quinn::Connection) -> Result<Vec<u8>> {
    let identity = conn.peer_identity().ok_or_else(|| anyhow!("peer sent no certificate"))?;
    let certs = identity
        .downcast::<Vec<CertificateDer<'static>>>()
        .map_err(|_| anyhow!("unexpected peer identity"))?;
    let der = certs.first().ok_or_else(|| anyhow!("empty certificate chain"))?;
    public_key_from_cert(der.as_ref()).ok_or_else(|| anyhow!("not an Ed25519 certificate"))
}

/// One DER element: its tag, its content and whatever follows it.
fn der_next(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let count = (first & 0x7F) as usize;
        if count == 0 || count > 4 || rest.len() < count {
            return None;
        }
        let (bytes, rest) = rest.split_at(count);
        (bytes.iter().fold(0usize, |value, b| value << 8 | *b as usize), rest)
    };
    if rest.len() < len {
        return None;
    }
    let (value, rest) = rest.split_at(len);
    Some((tag, value, rest))
}

/// Reads the certificate field by field down to the key TLS has verified. Searching for the
/// key's bytes would find a copy planted earlier in the certificate, such as in its name.
pub fn public_key_from_cert(der: &[u8]) -> Option<Vec<u8>> {
    const ED25519_KEY_INFO: [u8; 10] = [0x30, 0x05, 0x06, 0x03, 0x2B, 0x65, 0x70, 0x03, 0x21, 0x00];
    let Some((0x30, cert, _)) = der_next(der) else { return None };
    let Some((0x30, mut tbs, _)) = der_next(cert) else { return None };
    let (first, _, after) = der_next(tbs)?;
    if first == 0xA0 {
        tbs = after; // the optional version
    }
    // Serial number, signature algorithm, issuer, validity and subject come before the key.
    for expected in [0x02u8, 0x30, 0x30, 0x30, 0x30] {
        let (tag, _, after) = der_next(tbs)?;
        if tag != expected {
            return None;
        }
        tbs = after;
    }
    let Some((0x30, info, _)) = der_next(tbs) else { return None };
    let key = info.strip_prefix(&ED25519_KEY_INFO)?;
    (key.len() == 32).then(|| key.to_vec())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Disc {
    Probe { tag: String, id: String, port: u16, mac: Vec<u8> },
    Pong { tag: String, id: String, name: String, public_b64: String, port: u16, proof_b64: String, mac: Vec<u8> },
    JoinReq { code_tag: String, id: String, public_b64: String, name: String, mac: Vec<u8> },
    JoinOk { code_tag: String, id: String, sealed: Vec<u8> },
    SecurePong { tag: String, id: String, port: u16, mac: Vec<u8> },
}

pub fn team_tag(team_id: &str) -> String {
    secure::b64(&secure::sha256(team_id.as_bytes())[..8])
}

pub fn probe_mac(key: &[u8], tag: &str, id: &str, port: u16) -> Vec<u8> {
    secure::hmac_sign(key, &[b"probe", tag.as_bytes(), id.as_bytes(), &port.to_le_bytes()])
}

pub fn pong_mac(key: &[u8], tag: &str, id: &str, name: &str, public_b64: &str, port: u16, proof: &str) -> Vec<u8> {
    secure::hmac_sign(key, &[b"pong", tag.as_bytes(), id.as_bytes(), name.as_bytes(), public_b64.as_bytes(), &port.to_le_bytes(), proof.as_bytes()])
}

pub fn secure_pong_mac(key: &[u8], tag: &str, id: &str, port: u16) -> Vec<u8> {
    secure::hmac_sign(key, &[b"secure-pong", tag.as_bytes(), id.as_bytes(), &port.to_le_bytes()])
}

pub fn code_tag(code: &str) -> String {
    secure::b64(&secure::sha256(format!("tag:{}", normalize_code(code)).as_bytes())[..8])
}

pub fn normalize_code(code: &str) -> String {
    code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

pub fn join_mac(code: &str, id: &str, public_b64: &str, name: &str) -> Vec<u8> {
    secure::hmac_sign(normalize_code(code).as_bytes(), &[b"join", id.as_bytes(), public_b64.as_bytes(), name.as_bytes()])
}

pub fn encode_disc(d: &Disc) -> Vec<u8> {
    rmp_serde::to_vec_named(d).unwrap_or_default()
}

pub fn decode_disc(b: &[u8]) -> Option<Disc> {
    rmp_serde::from_slice(b).ok()
}

/// Windows turns a "port unreachable" reply to an earlier packet into an error on the next
/// receive. A PC that was off could then stall the wait for an answer, so it is switched off.
pub fn ignore_unreachable(socket: &impl std::os::windows::io::AsRawSocket) {
    use windows::Win32::Networking::WinSock::{WSAIoctl, SIO_UDP_CONNRESET, SOCKET};
    let off: u32 = 0;
    let mut returned = 0u32;
    unsafe {
        let _ = WSAIoctl(
            SOCKET(socket.as_raw_socket() as usize),
            SIO_UDP_CONNRESET,
            Some(&off as *const u32 as *const std::ffi::c_void),
            std::mem::size_of::<u32>() as u32,
            None,
            0,
            &mut returned,
            None,
            None,
        );
    }
}

/// Broadcast addresses of all active IPv4 interfaces, without loopback.
pub fn broadcast_targets(port: u16) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    if let Ok(ifs) = if_addrs::get_if_addrs() {
        for i in ifs {
            if i.is_loopback() {
                continue;
            }
            if let if_addrs::IfAddr::V4(v4) = i.addr {
                if let Some(b) = v4.broadcast {
                    out.push(SocketAddr::new(IpAddr::V4(b), port));
                }
            }
        }
    }
    out.push(SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), port));
    out.sort();
    out.dedup();
    out
}

/// Only private LAN addresses are ever accepted, so the program cannot reach the internet by accident.
pub fn is_local_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_key_is_read_from_certificate() {
        let pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
        let cert = rcgen::CertificateParams::new(vec!["x".to_string()]).unwrap().self_signed(&pair).unwrap();
        assert_eq!(public_key_from_cert(cert.der()).unwrap(), pair.public_key_raw());
    }

    #[test]
    fn a_key_planted_in_the_name_is_not_taken_for_the_real_one() {
        let pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["x".to_string()]).unwrap();
        let mut planted = String::from("\u{6}\u{3}+ep\u{3}!\0");
        planted.push_str(&"A".repeat(32));
        params.distinguished_name.push(rcgen::DnType::CommonName, planted);
        let cert = params.self_signed(&pair).unwrap();
        assert_eq!(public_key_from_cert(cert.der()).unwrap(), pair.public_key_raw());
    }

    #[test]
    fn broken_certificates_are_refused() {
        assert!(public_key_from_cert(&[]).is_none());
        assert!(public_key_from_cert(&[0x30, 0x84, 0xFF, 0xFF, 0xFF, 0xFF]).is_none());
        assert!(public_key_from_cert(&[0x30, 0x02, 0x30, 0x00]).is_none());
    }

    #[test]
    fn codes_ignore_formatting() {
        assert_eq!(code_tag("abcd-efgh"), code_tag("ABCDEFGH"));
        assert_eq!(join_mac("abcd-efgh", "i", "p", "n"), join_mac("ABCDEFGH", "i", "p", "n"));
    }

    #[test]
    fn only_lan_addresses_pass() {
        assert!(is_local_address("192.168.1.5".parse().unwrap()));
        assert!(is_local_address("10.0.0.1".parse().unwrap()));
        assert!(!is_local_address("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn discovery_packet_roundtrip() {
        let d = Disc::Probe { tag: "t".into(), id: "i".into(), port: 1, mac: vec![1, 2] };
        assert_eq!(decode_disc(&encode_disc(&d)), Some(d));
        assert_eq!(decode_disc(b"garbage"), None);
    }
}
