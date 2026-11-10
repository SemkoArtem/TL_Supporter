//! Updates. Only a lead PC reaches GitHub, and only on request. Employee PCs receive the new
//! version over the LAN. Every file must carry a valid release signature.

use crate::node::Node;
use crate::proto;
use crate::secure;
use crate::util::{data_dir, APP_VERSION};
use anyhow::{anyhow, bail, Result};
use std::path::PathBuf;
use std::sync::Arc;

const RELEASE_PUBLIC: &[u8] = include_bytes!("../assets/release_public.bin");
const MAX_SIZE: u64 = 200 * 1024 * 1024;

pub fn update_dir() -> PathBuf {
    let d = data_dir().join("update");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn signed_message(version: &str, exe: &[u8]) -> Vec<u8> {
    let mut m = b"tlsupporter-update:".to_vec();
    m.extend_from_slice(version.as_bytes());
    m.push(b':');
    m.extend_from_slice(&secure::sha256(exe));
    m
}

pub fn verify_release(version: &str, exe: &[u8], sig: &[u8]) -> bool {
    RELEASE_PUBLIC.len() == 32 && secure::verify(RELEASE_PUBLIC, &signed_message(version, exe), sig)
}

/// `1.2.10` is newer than `1.2.9`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> { v.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    parse(candidate) > parse(current)
}

/// Developer tool: create the signing key pair.
pub fn generate_release_key(private_path: &str, public_path: &str) -> Result<()> {
    let (private, public) = secure::generate_authority()?;
    std::fs::write(private_path, private)?;
    std::fs::write(public_path, public)?;
    Ok(())
}

/// Developer tool: sign a build, producing `<exe>.sig` for the GitHub release.
pub fn sign_release(exe_path: &str, private_path: &str, version: &str) -> Result<()> {
    let exe = std::fs::read(exe_path)?;
    let private = std::fs::read(private_path)?;
    let sig = secure::sign(&private, &signed_message(version, &exe))?;
    std::fs::write(format!("{exe_path}.sig"), sig)?;
    Ok(())
}

fn store_pending(version: &str, exe: &[u8], sig: &[u8]) -> Result<()> {
    let dir = update_dir();
    std::fs::write(dir.join("new.tmp"), exe)?;
    std::fs::rename(dir.join("new.tmp"), dir.join("new.exe"))?;
    std::fs::write(dir.join("new.sig"), sig)?;
    std::fs::write(dir.join("new.ver"), version)?;
    Ok(())
}

/// Called by the service. Returns the verified file itself: the folder is writable by every
/// user, so whatever gets installed must be exactly the bytes that were checked.
pub fn pending_update() -> Option<Vec<u8>> {
    let dir = update_dir();
    let exe = std::fs::read(dir.join("new.exe")).ok()?;
    let sig = std::fs::read(dir.join("new.sig")).ok()?;
    let version = std::fs::read_to_string(dir.join("new.ver")).ok()?;
    if verify_release(&version, &exe, &sig) && is_newer(&version, APP_VERSION) {
        Some(exe)
    } else {
        let _ = std::fs::remove_file(dir.join("new.exe"));
        None
    }
}

fn github_latest(repo: &str) -> Result<(String, Vec<u8>, Vec<u8>)> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(120)).build();
    let info: serde_json::Value = agent.get(&url).set("User-Agent", "TLSupporter-updater").call()?.into_json()?;
    let tag = info["tag_name"].as_str().ok_or_else(|| anyhow!("у репозиторії немає випусків"))?.trim_start_matches('v').to_string();
    if !is_newer(&tag, APP_VERSION) {
        return Ok((tag, Vec::new(), Vec::new()));
    }
    let asset = |name: &str| -> Result<String> {
        info["assets"]
            .as_array()
            .and_then(|a| a.iter().find(|x| x["name"] == name))
            .and_then(|x| x["browser_download_url"].as_str())
            .map(String::from)
            .ok_or_else(|| anyhow!("у випуску немає файлу {name}"))
    };
    let download = |u: String| -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let reader = agent.get(&u).set("User-Agent", "TLSupporter-updater").call()?.into_reader();
        std::io::Read::read_to_end(&mut std::io::Read::take(reader, MAX_SIZE), &mut out)?;
        Ok(out)
    };
    let exe = download(asset("TLSupporter.exe")?)?;
    let sig = download(asset("TLSupporter.exe.sig")?)?;
    Ok((tag, exe, sig))
}

fn send_update(node: &Arc<Node>, version: &str, exe: Arc<Vec<u8>>, sig: Arc<Vec<u8>>) -> usize {
    let peers: Vec<_> = node.peers.lock().values().cloned().collect();
    let count = peers.len();
    for p in peers {
        let (exe, sig, version) = (exe.clone(), sig.clone(), version.to_string());
        node.runtime.spawn(async move {
            let result: Result<()> = async {
                let mut s = p.conn.open_uni().await?;
                let v = version.as_bytes();
                let mut header = vec![proto::UNI_UPDATE];
                header.extend_from_slice(&(v.len() as u16).to_le_bytes());
                header.extend_from_slice(v);
                header.extend_from_slice(&sig);
                header.extend_from_slice(&(exe.len() as u64).to_le_bytes());
                s.write_all(&header).await?;
                s.write_all(&exe).await?;
                s.finish()?;
                s.stopped().await?;
                Ok(())
            }
            .await;
            if let Err(e) = result {
                crate::logi!("update send to {} failed: {e}", p.id);
            }
        });
    }
    count
}

/// Download from GitHub, check the signature and hand the update to this PC and all connected PCs.
pub fn check_and_distribute(node: &Arc<Node>) -> String {
    let repo = node.state.read().directory.policy.update_repo.clone();
    if repo.is_empty() {
        return crate::i18n::tr("Спочатку вкажи репозиторій.").into();
    }
    match github_latest(&repo) {
        Err(e) => format!("{}: {e}", crate::i18n::tr("Не вдалося перевірити")),
        Ok((tag, exe, _)) if exe.is_empty() => format!("{} ({tag}).", crate::i18n::tr("Встановлено останню версію")),
        Ok((tag, exe, sig)) => {
            if !verify_release(&tag, &exe, &sig) {
                return crate::i18n::tr("Підпис оновлення недійсний, оновлення відхилено.").into();
            }
            if let Err(e) = store_pending(&tag, &exe, &sig) {
                return format!("{}: {e}", crate::i18n::tr("Не вдалося зберегти"));
            }
            let n = send_update(node, &tag, Arc::new(exe), Arc::new(sig));
            format!("{tag}. {} {n}", crate::i18n::tr("Завантажено і розіслано, ПК:"))
        }
    }
}

/// Receive an update from a lead over the LAN.
pub async fn receive_update(mut s: quinn::RecvStream) -> Result<()> {
    let mut len = [0u8; 2];
    s.read_exact(&mut len).await?;
    let mut version = vec![0u8; u16::from_le_bytes(len) as usize];
    s.read_exact(&mut version).await?;
    let version = String::from_utf8(version)?;
    let mut sig = vec![0u8; 64];
    s.read_exact(&mut sig).await?;
    let mut size = [0u8; 8];
    s.read_exact(&mut size).await?;
    let size = u64::from_le_bytes(size);
    if size > MAX_SIZE {
        bail!("update too large");
    }
    let mut exe = vec![0u8; size as usize];
    s.read_exact(&mut exe).await?;
    if !is_newer(&version, APP_VERSION) {
        return Ok(());
    }
    if !verify_release(&version, &exe, &sig) {
        bail!("bad update signature");
    }
    store_pending(&version, &exe, &sig)?;
    crate::logi!("update {version} received");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("1.2.10", "1.2.9"));
        assert!(is_newer("v2.0.0", "1.9.9"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("0.9.0", "1.0.0"));
    }

    #[test]
    fn signature_covers_version_and_file() {
        let (private, public) = secure::generate_authority().unwrap();
        let sig = secure::sign(&private, &signed_message("1.0.0", b"exe")).unwrap();
        assert!(secure::verify(&public, &signed_message("1.0.0", b"exe"), &sig));
        assert!(!secure::verify(&public, &signed_message("1.0.1", b"exe"), &sig));
        assert!(!secure::verify(&public, &signed_message("1.0.0", b"evil"), &sig));
    }
}
