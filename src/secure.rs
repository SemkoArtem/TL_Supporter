//! Device identity and cryptographic helpers.

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ring::{aead, digest, hmac, pbkdf2, rand::SecureRandom, signature};
use std::num::NonZeroU32;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_LOCAL_MACHINE, CRYPT_INTEGER_BLOB,
};

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    ring::rand::SystemRandom::new().fill(&mut b).expect("system rng");
    b
}

pub fn b64(data: &[u8]) -> String {
    B64.encode(data)
}

pub fn unb64(s: &str) -> Result<Vec<u8>> {
    Ok(B64.decode(s)?)
}

pub fn sha256(data: &[u8]) -> Vec<u8> {
    digest::digest(&digest::SHA256, data).as_ref().to_vec()
}

/// Compares secrets such as message codes in a time that does not depend on where they differ.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

const ID_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

fn encode_id(data: &[u8], chars: usize) -> String {
    let mut out = String::new();
    let mut acc = 0u32;
    let mut bits = 0;
    let mut bytes = data.iter();
    while out.chars().filter(|c| *c != '-').count() < chars {
        if bits < 5 {
            acc = (acc << 8) | *bytes.next().unwrap_or(&0) as u32;
            bits += 8;
        }
        bits -= 5;
        let v = (acc >> bits) & 31;
        if !out.is_empty() && out.chars().filter(|c| *c != '-').count() % 4 == 0 {
            out.push('-');
        }
        out.push(ID_ALPHABET[v as usize] as char);
    }
    out
}

/// Readable device key code derived from the public key, e.g. `K7Q2-9XTM-AB3D-77PQ`.
pub fn device_id(public_key: &[u8]) -> String {
    encode_id(&sha256(public_key), 16)
}

pub fn short_id(id: &str) -> u32 {
    let h = sha256(id.as_bytes());
    u32::from_le_bytes([h[0], h[1], h[2], h[3]])
}

pub fn generate_join_code() -> String {
    encode_id(&random_bytes::<8>(), 8)
}

/// Encrypt with DPAPI so the blob can only be opened on this PC.
pub fn dpapi_protect(data: &[u8]) -> Result<Vec<u8>> {
    dpapi_seal(data, CRYPTPROTECT_LOCAL_MACHINE)
}

/// Encrypt so that only this Windows account can open the blob.
pub fn dpapi_protect_user(data: &[u8]) -> Result<Vec<u8>> {
    dpapi_seal(data, 0)
}

fn dpapi_seal(data: &[u8], flags: u32) -> Result<Vec<u8>> {
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut output = CRYPT_INTEGER_BLOB::default();
        CryptProtectData(&input, None, None, None, None, flags, &mut output)?;
        let v = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(output.pbData as *mut _));
        Ok(v)
    }
}

pub fn dpapi_unprotect(data: &[u8]) -> Result<Vec<u8>> {
    dpapi_open(data, CRYPTPROTECT_LOCAL_MACHINE)
}

pub fn dpapi_unprotect_user(data: &[u8]) -> Result<Vec<u8>> {
    dpapi_open(data, 0)
}

fn dpapi_open(data: &[u8], flags: u32) -> Result<Vec<u8>> {
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut output = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(&input, None, None, None, None, flags, &mut output)?;
        let v = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(output.pbData as *mut _));
        Ok(v)
    }
}

/// Key pair of this PC.
#[derive(Clone)]
pub struct Identity {
    pub pkcs8: Vec<u8>,
    pub public: Vec<u8>,
    pub id: String,
}

impl Identity {
    pub fn load_or_create() -> Result<Self> {
        let path = crate::util::data_dir().join("device.key");
        let pkcs8 = match std::fs::read(&path) {
            Ok(blob) => dpapi_unprotect(&blob).context("device key belongs to another PC")?,
            Err(_) => {
                let doc = signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
                    .map_err(|_| anyhow!("key generation failed"))?;
                let bytes = doc.as_ref().to_vec();
                std::fs::write(&path, dpapi_protect(&bytes)?)?;
                bytes
            }
        };
        Self::from_pkcs8(pkcs8)
    }

    /// For the SYSTEM process, which must not create files in the folder open to every user.
    pub fn load_existing() -> Result<Self> {
        let blob = std::fs::read(crate::util::data_dir().join("device.key"))?;
        Self::from_pkcs8(dpapi_unprotect(&blob).context("device key belongs to another PC")?)
    }

    fn from_pkcs8(pkcs8: Vec<u8>) -> Result<Self> {
        let pair = signature::Ed25519KeyPair::from_pkcs8(&pkcs8).map_err(|_| anyhow!("bad device key"))?;
        use signature::KeyPair;
        let public = pair.public_key().as_ref().to_vec();
        let id = device_id(&public);
        Ok(Self { pkcs8, public, id })
    }
}

pub fn generate_authority() -> Result<(Vec<u8>, Vec<u8>)> {
    let doc = signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
        .map_err(|_| anyhow!("key generation failed"))?;
    let pair = signature::Ed25519KeyPair::from_pkcs8(doc.as_ref()).map_err(|_| anyhow!("bad key"))?;
    use signature::KeyPair;
    Ok((doc.as_ref().to_vec(), pair.public_key().as_ref().to_vec()))
}

/// The public half of a private key.
pub fn public_of(pkcs8: &[u8]) -> Option<Vec<u8>> {
    use signature::KeyPair;
    let pair = signature::Ed25519KeyPair::from_pkcs8(pkcs8).ok()?;
    Some(pair.public_key().as_ref().to_vec())
}

pub fn sign(pkcs8: &[u8], msg: &[u8]) -> Result<Vec<u8>> {
    let pair = signature::Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| anyhow!("bad key"))?;
    Ok(pair.sign(msg).as_ref().to_vec())
}

pub fn verify(public: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    signature::UnparsedPublicKey::new(&signature::ED25519, public).verify(msg, sig).is_ok()
}

pub fn hmac_sign(key: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    let k = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mut ctx = hmac::Context::with_key(&k);
    for p in parts {
        ctx.update(p);
    }
    ctx.sign().as_ref().to_vec()
}

pub fn hmac_verify(key: &[u8], parts: &[&[u8]], tag: &[u8]) -> bool {
    let k = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mut joined = Vec::new();
    for p in parts {
        joined.extend_from_slice(p);
    }
    hmac::verify(&k, &joined, tag).is_ok()
}

const PBKDF_ROUNDS: u32 = 150_000;

fn derive(password: &str, salt: &[u8], out: &mut [u8]) {
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(PBKDF_ROUNDS).unwrap(),
        salt,
        password.as_bytes(),
        out,
    );
}

/// Returns `salt:hash`, or an empty string when no password is set.
pub fn hash_password(password: &str) -> String {
    if password.is_empty() {
        return String::new();
    }
    let salt = random_bytes::<16>();
    let mut out = [0u8; 32];
    derive(password, &salt, &mut out);
    format!("{}:{}", b64(&salt), b64(&out))
}

pub fn check_password(stored: &str, password: &str) -> bool {
    if stored.is_empty() {
        return true;
    }
    let Some((s, h)) = stored.split_once(':') else { return false };
    let (Ok(salt), Ok(hash)) = (unb64(s), unb64(h)) else { return false };
    pbkdf2::verify(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(PBKDF_ROUNDS).unwrap(),
        &salt,
        password.as_bytes(),
        &hash,
    )
    .is_ok()
}

fn aead_seal(key: &[u8; 32], plain: &[u8]) -> Result<Vec<u8>> {
    let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key).map_err(|_| anyhow!("key"))?;
    let sealing = aead::LessSafeKey::new(unbound);
    let nonce = random_bytes::<12>();
    let mut buf = plain.to_vec();
    sealing
        .seal_in_place_append_tag(aead::Nonce::assume_unique_for_key(nonce), aead::Aad::empty(), &mut buf)
        .map_err(|_| anyhow!("encrypt failed"))?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&buf);
    Ok(out)
}

fn aead_open(key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < 12 + 16 {
        return Err(anyhow!("data too short"));
    }
    let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key).map_err(|_| anyhow!("key"))?;
    let opening = aead::LessSafeKey::new(unbound);
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&blob[..12]);
    let mut buf = blob[12..].to_vec();
    let plain = opening
        .open_in_place(aead::Nonce::assume_unique_for_key(nonce), aead::Aad::empty(), &mut buf)
        .map_err(|_| anyhow!("wrong password or damaged data"))?;
    Ok(plain.to_vec())
}

/// Seal with a human password. Output is `salt || nonce || ciphertext`.
pub fn seal_with_password(password: &str, plain: &[u8]) -> Result<Vec<u8>> {
    let salt = random_bytes::<16>();
    let mut key = [0u8; 32];
    derive(password, &salt, &mut key);
    let mut out = salt.to_vec();
    out.extend_from_slice(&aead_seal(&key, plain)?);
    Ok(out)
}

pub fn open_with_password(password: &str, blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < 16 {
        return Err(anyhow!("data too short"));
    }
    let mut key = [0u8; 32];
    derive(password, &blob[..16], &mut key);
    aead_open(&key, &blob[16..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_roundtrip() {
        let stored = hash_password("секрет");
        assert!(check_password(&stored, "секрет"));
        assert!(!check_password(&stored, "інший"));
        assert!(check_password("", "any"));
    }

    #[test]
    fn seal_roundtrip() {
        let blob = seal_with_password("pw", b"hello").unwrap();
        assert_eq!(open_with_password("pw", &blob).unwrap(), b"hello");
        assert!(open_with_password("no", &blob).is_err());
    }

    #[test]
    fn dpapi_roundtrip() {
        let blob = dpapi_protect(b"device").unwrap();
        assert_eq!(dpapi_unprotect(&blob).unwrap(), b"device");
    }

    #[test]
    fn user_dpapi_roundtrip() {
        let blob = dpapi_protect_user(b"password").unwrap();
        assert_eq!(dpapi_unprotect_user(&blob).unwrap(), b"password");
        assert!(dpapi_unprotect_user(b"forged").is_err());
    }

    #[test]
    fn signature_roundtrip() {
        let (private, public) = generate_authority().unwrap();
        let s = sign(&private, b"m").unwrap();
        assert!(verify(&public, b"m", &s));
        assert!(!verify(&public, b"x", &s));
    }

    #[test]
    fn id_format() {
        let id = device_id(b"abc");
        assert_eq!(id.len(), 19);
        assert_eq!(id.matches('-').count(), 3);
    }
}
