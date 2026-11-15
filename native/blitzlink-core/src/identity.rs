//! 호스트 신원 — 호스트마다 Ed25519 키가 하나 있다(`--key-file`, 씨앗 32바이트를 16진 한 줄). 클라(join)는 `meta` 채널로
//! 무작위 nonce를 보내고(`Probe`), 호스트는 그것에 서명해 공개키와 함께 돌려준다. 클라는 서명을 확인하고 그 **지문**이
//! 중계가 건넨 지문과 같은지 본다(다르면 `badIdentity`). 모드는 지문을 기억했다가 바뀌면 경고한다(SSH known_hosts와 같은 생각).

use std::path::Path;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

/// 클라에게 하는 신원 증명의 머리 — 다른 용도의 서명을 신원 증명으로 돌려쓰지 못하게.
pub const CONTEXT: &[u8] = b"blitzlink/server-identity/v1";
/// 공식 중계에 호스트를 올릴 때 서명하는 글의 머리 — 클라에게 한 증명을 중계 등록으로 돌려쓰지 못하게(그 반대도).
pub const RELAY_CONTEXT: &[u8] = b"blitzlink/relay-host/v1";

/// 호스트가 서명할 바이트 — 머리 + 클라의 nonce.
pub fn challenge(nonce: &[u8; 32]) -> Vec<u8> {
    let mut m = Vec::with_capacity(CONTEXT.len() + 32);
    m.extend_from_slice(CONTEXT);
    m.extend_from_slice(nonce);
    m
}

/// 중계가 준 nonce에 호스트가 서명할 바이트.
pub fn relay_challenge(nonce: &[u8; 32]) -> Vec<u8> {
    let mut m = Vec::with_capacity(RELAY_CONTEXT.len() + 32);
    m.extend_from_slice(RELAY_CONTEXT);
    m.extend_from_slice(nonce);
    m
}

fn verify_message(key: &[u8; 32], message: &[u8], sig: &[u8]) -> bool {
    let Ok(vk) = VerifyingKey::from_bytes(key) else { return false };
    let Ok(sig) = <[u8; 64]>::try_from(sig) else { return false };
    vk.verify(message, &Signature::from_bytes(&sig)).is_ok()
}

/// 서명이 그 공개키로 그 nonce에 한 것이 맞는가(클라용 증명).
pub fn verify(key: &[u8; 32], nonce: &[u8; 32], sig: &[u8]) -> bool {
    verify_message(key, &challenge(nonce), sig)
}

/// 중계 등록 서명이 맞는가(`relay_challenge`).
pub fn verify_relay(key: &[u8; 32], nonce: &[u8; 32], sig: &[u8]) -> bool {
    verify_message(key, &relay_challenge(nonce), sig)
}

const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// 사람이 읽는 지문 — 공개키 SHA-256의 앞 80비트를 base32로, 넷씩 끊어 `ABCD-EFGH-IJKL-MNOP`.
pub fn fingerprint(key: &[u8; 32]) -> String {
    let digest = Sha256::digest(key);
    let mut bits: u128 = 0;
    for &b in &digest[..10] {
        bits = (bits << 8) | b as u128;
    }
    let mut out = String::with_capacity(19);
    for i in 0..16 {
        if i > 0 && i % 4 == 0 {
            out.push('-');
        }
        let idx = (bits >> (75 - i * 5)) & 31;
        out.push(B32[idx as usize] as char);
    }
    out
}

/// 호스트의 서명 키(사이드카만 쓴다).
#[derive(Clone)]
pub struct Identity {
    key: SigningKey,
}

fn seed_from_hex(s: &str) -> Option<[u8; 32]> {
    let v = crate::relay::unhex(s.trim())?;
    <[u8; 32]>::try_from(v).ok()
}

impl Identity {
    /// 파일이 있으면 읽고, 없으면 새로 만들어 쓴다. 두 번째 값은 「새로 만들었다」.
    pub fn load_or_create(path: &Path) -> Result<(Self, bool), String> {
        if let Ok(text) = std::fs::read_to_string(path) {
            let seed = seed_from_hex(&text).ok_or_else(|| format!("{}: not 64 hex characters", path.display()))?;
            return Ok((Self { key: SigningKey::from_bytes(&seed) }, false));
        }
        let seed = crate::random32();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(path, crate::relay::hex(&seed) + "\n").map_err(|e| format!("{}: cannot write ({e})", path.display()))?;
        Ok((Self { key: SigningKey::from_bytes(&seed) }, true))
    }

    /// 파일 없이(시험용).
    pub fn ephemeral() -> Self {
        Self { key: SigningKey::from_bytes(&crate::random32()) }
    }

    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self { key: SigningKey::from_bytes(&seed) }
    }

    pub fn public(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public())
    }

    /// 공식 중계에 올릴 때 — 중계가 준 nonce에(`relay_challenge`, 클라용 증명과 머리가 다르다).
    pub fn relay_sig(&self, nonce: &[u8; 32]) -> Vec<u8> {
        self.key.sign(&relay_challenge(nonce)).to_bytes().to_vec()
    }

    /// 클라의 `Probe`에 — `challenge` 서명.
    pub fn proof(&self, nonce: &[u8; 32]) -> Vec<u8> {
        self.key.sign(&challenge(nonce)).to_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_and_fingerprint() {
        let id = Identity::from_seed([7u8; 32]);
        let key = id.public();
        let nonce = [3u8; 32];
        let sig = id.proof(&nonce);
        assert!(verify(&key, &nonce, &sig));
        assert!(!verify(&key, &[4u8; 32], &sig), "다른 nonce");
        assert!(!verify_relay(&key, &nonce, &sig), "클라용 서명을 중계 등록으로 돌려쓰지 못한다");
        let rsig = id.relay_sig(&nonce);
        assert!(verify_relay(&key, &nonce, &rsig));
        assert!(!verify(&key, &nonce, &rsig));
        assert!(!verify(&key, &nonce, &sig[..63]), "길이");
        let fp = fingerprint(&key);
        assert_eq!(fp.len(), 19);
        assert_eq!(fp.matches('-').count(), 3);
        assert_eq!(fp, id.fingerprint());
        assert_ne!(fp, Identity::from_seed([8u8; 32]).fingerprint());
    }

    #[test]
    fn other_context_signatures_do_not_verify() {
        // 같은 키라도 다른 머리로 한 서명은 통하지 않는다.
        let sk = SigningKey::from_bytes(&[9u8; 32]);
        let nonce = [1u8; 32];
        let mut m = b"other-app/server-identity/v1".to_vec();
        m.extend_from_slice(&nonce);
        let sig = sk.sign(&m).to_bytes();
        assert!(!verify(&sk.verifying_key().to_bytes(), &nonce, &sig));
    }

    #[test]
    fn key_file_round_trip() {
        let dir = std::env::temp_dir().join(format!("blitzlink-id-{}", std::process::id()));
        let path = dir.join("sub").join("host.key");
        let _ = std::fs::remove_dir_all(&dir);
        let (a, created) = Identity::load_or_create(&path).unwrap();
        assert!(created);
        let (b, created) = Identity::load_or_create(&path).unwrap();
        assert!(!created);
        assert_eq!(a.public(), b.public());
        std::fs::write(&path, "nope").unwrap();
        assert!(Identity::load_or_create(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
