//! `meta` 채널(신뢰·순서, 클라가 SDP에서 만든다)의 글 — JSON 텍스트 하나 = 메시지 하나, 태그 `t`.
//!
//! - 클라 → 호스트 `probe{nonce}` — 무작위 32바이트(16진).
//! - 호스트 → 클라 `info{key, sig, info}` — `sig`는 `identity::challenge(nonce)`에 한 서명, `info`는 모드가 준 서버 정보(모드 목록
//!   포함, 사이드카는 손대지 않는다). 모드가 정보를 바꾸면 `info{info}`(증명 없이)를 다시 보낸다.
//! - 클라 → 호스트 `ping{n}`, 호스트 → 클라 `pong{n}` — 살아 있는지·왕복 시간.
//!
//! 클라는 서명을 확인하고 `fingerprint(key)`가 중계가 건넨 지문과 같은지 본다(다르면 `badIdentity`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `info` 하나의 상한(모드 목록이 길어도 이 안에서).
pub const MAX_META: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum MetaMsg {
    Probe {
        #[serde(with = "blitzlink_core::relay::hex32")]
        nonce: [u8; 32],
    },
    Info {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sig: Option<String>,
        info: Value,
    },
    Ping {
        n: u64,
    },
    Pong {
        n: u64,
    },
}

impl MetaMsg {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("meta")
    }

    pub fn parse(b: &[u8]) -> Option<MetaMsg> {
        if b.len() > MAX_META {
            return None;
        }
        serde_json::from_slice(b).ok()
    }
}

/// 클라가 받은 `info`의 증명을 확인한다 — 맞으면 지문.
pub fn verify_info(key: Option<&str>, sig: Option<&str>, nonce: &[u8; 32]) -> Option<String> {
    let key: [u8; 32] = blitzlink_core::relay::unhex(key?)?.try_into().ok()?;
    let sig = blitzlink_core::relay::unhex(sig?)?;
    blitzlink_core::identity::verify(&key, nonce, &sig).then(|| blitzlink_core::identity::fingerprint(&key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use blitzlink_core::identity::Identity;
    use blitzlink_core::relay::hex;

    #[test]
    fn probe_and_info() {
        let id = Identity::ephemeral();
        let nonce = [9u8; 32];
        let p = MetaMsg::Probe { nonce };
        assert_eq!(MetaMsg::parse(p.to_json().as_bytes()), Some(p));
        let info = MetaMsg::Info { key: Some(hex(&id.public())), sig: Some(hex(&id.proof(&nonce))), info: serde_json::json!({"mods": []}) };
        let MetaMsg::Info { key, sig, .. } = MetaMsg::parse(info.to_json().as_bytes()).unwrap() else { panic!() };
        assert_eq!(verify_info(key.as_deref(), sig.as_deref(), &nonce), Some(id.fingerprint()));
        assert_eq!(verify_info(key.as_deref(), sig.as_deref(), &[1u8; 32]), None, "다른 nonce");
        assert_eq!(verify_info(None, sig.as_deref(), &nonce), None);
        assert!(MetaMsg::parse(b"junk").is_none());
    }
}
