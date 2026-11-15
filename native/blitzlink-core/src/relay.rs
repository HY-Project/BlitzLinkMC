//! 공식 중계(`blitzlink-relay`)와 주고받는 메시지의 정본 — 중계와 사이드카(`blitzlink-tunnel`)가 **이 한 벌**을 쓴다.
//!
//! 중계는 **SDP만** 건넨다(마크 데이터는 WebRTC DataChannel로 호스트와 클라가 직접 — `docs/en/RELAY.md`).
//!  1. 누구든 붙으면 중계가 `Hello{nonce}`를 먼저 보낸다.
//!  2. 호스트(`tunnel host`) → `Host{key, sig, listing, public, code?}` — `sig`는 nonce에 한 서명(`identity::relay_challenge`).
//!     중계가 확인하고 **참가 코드**(`MCJE-7K3Q`)를 준다(`Hosted`). 호스트는 인원이 바뀌면 `Update`.
//!  3. 클라(`tunnel join`) → `Join{code, sdp}` → 중계가 호스트에 `Offer{session, sdp}` → 호스트 `Answer{session, sdp, turn?}`
//!     → 중계가 클라에 `Answer{sdp, fingerprint, listing, turn?}`. 클라는 그 뒤 소켓을 닫고 DataChannel `meta`로 `Probe`부터 한다.
//!     호스트의 답에는 **내장 TURN**(`TurnInfo` — MC 포트의 TCP, 시간제한 자격)이 실릴 수 있다. UDP가 막힌 망의 클라는 첫 연결이
//!     늦으면 그 TURN만 쓰는 연결로 한 번 더 `Join`한다.
//!
//! 모드 목록 본체는 여기 싣지 않는다 — P2P `meta` 채널로 간다(hyproj 부하 최소화, 메시지 16 KB 한도).
//! 글은 JSON(텍스트 프레임 하나 = 메시지 하나), 태그는 `t`.

use serde::{Deserialize, Serialize};

/// 중계 프로토콜 판.
pub const RELAY_PROTOCOL: u32 = 1;
/// 메시지 하나의 상한(바이트). SDP 하나가 2~4 KB.
pub const MAX_MESSAGE: usize = 16 * 1024;
pub const MAX_SDP: usize = 12 * 1024;
/// 참가 코드의 머리(`MCJE-7K3Q`의 `MCJE`). 코드는 `MCJE-` + 네 글자.
pub const CODE_PREFIX: &str = "MCJE";
/// 참가 코드 글자 — 헷갈리는 0·1·O·I·L·U를 뺀 30자. 네 글자면 81만 가지.
pub const CODE_ALPHABET: &[u8; 30] = b"23456789ABCDEFGHJKMNPQRSTVWXYZ";
pub const CODE_LEN: usize = 4;

/// 로더 이름 — 이 밖의 것은 `vanilla`로 본다(`bukkit`은 Spigot·Paper와 그 갈래 플러그인 판).
pub const LOADERS: [&str; 4] = ["vanilla", "neoforge", "fabric", "bukkit"];

/// 공개 목록·참가 화면에 보이는 서버 요약. 모드(Java)가 만들어 사이드카에 `listing`으로 주고, 중계가 `sanitized()`로 다듬는다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Listing {
    pub name: String,
    pub motd: String,
    /// `26.3` 꼴.
    pub mc_version: String,
    /// MC 프로토콜 번호(핸드셰이크의 것).
    pub protocol: u32,
    /// `vanilla` · `neoforge` · `fabric` · `bukkit`.
    pub loader: String,
    /// `21.3.4` · `0.17.2` · `Paper 26.3-251` 꼴.
    pub loader_version: String,
    pub mod_count: u16,
    /// 모드 목록의 해시(16진 16자) — 클라가 캐시와 비교한다. 목록 본체는 `meta` 채널로.
    pub mods_hash: String,
    pub players: u32,
    pub max_players: u32,
    pub online_mode: bool,
}

impl Default for Listing {
    fn default() -> Self {
        Listing {
            name: String::new(),
            motd: String::new(),
            mc_version: String::new(),
            protocol: 0,
            loader: "vanilla".into(),
            loader_version: String::new(),
            mod_count: 0,
            mods_hash: String::new(),
            players: 0,
            max_players: 20,
            online_mode: true,
        }
    }
}

fn clip(s: &str, n: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(n).collect::<String>().trim().to_string()
}

impl Listing {
    /// 길이·범위를 바로잡는다(중계가 받은 그대로 남에게 보여 주지 않게).
    pub fn sanitized(&self) -> Listing {
        let max_players = self.max_players.clamp(1, 100_000);
        let loader = self.loader.trim().to_ascii_lowercase();
        let hash = self.mods_hash.trim().to_ascii_lowercase();
        Listing {
            name: clip(&self.name, 48),
            motd: clip(&self.motd, 160),
            mc_version: clip(&self.mc_version, 16),
            protocol: self.protocol,
            loader: if LOADERS.contains(&loader.as_str()) { loader } else { "vanilla".into() },
            loader_version: clip(&self.loader_version, 24),
            mod_count: self.mod_count,
            mods_hash: if hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()) { hash } else { String::new() },
            players: self.players.min(max_players),
            max_players,
            online_mode: self.online_mode,
        }
    }
}

/// 호스트 내장 TURN의 접속 정보(RTCIceServer 꼴) — `turn:IP:포트?transport=tcp`, 자격은 호스트가 HMAC으로 확인한다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInfo {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
}

impl TurnInfo {
    /// 중계가 클라에 건네기 전에 다듬는다 — `turn:` 주소 넷까지, 글 길이 상한. 쓸 것이 없으면 None.
    pub fn sanitized(&self) -> Option<TurnInfo> {
        let ok = |s: &str| s.len() <= 128 && !s.chars().any(|c| c.is_control() || c.is_whitespace());
        let urls: Vec<String> = self.urls.iter().filter(|u| u.starts_with("turn:") && ok(u)).take(4).cloned().collect();
        (!urls.is_empty() && ok(&self.username) && ok(&self.credential) && !self.username.is_empty())
            .then(|| TurnInfo { urls, username: self.username.clone(), credential: self.credential.clone() })
    }
}

/// 공개 목록의 한 줄(`GET /servers`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicServer {
    pub code: String,
    pub fingerprint: String,
    #[serde(flatten)]
    pub listing: Listing,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerList {
    pub relay: u32,
    pub servers: Vec<PublicServer>,
}

/// 중계가 거절하는 까닭 — 클라 문구는 `blitzlink.relay.error.<이름>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RelayError {
    /// 그런 참가 코드가 없다(서버가 닫혔거나 잘못 적음).
    NoSuchCode,
    /// 호스트가 제때 답하지 않았다.
    HostTimeout,
    /// 호스트가 받지 않았다(가득 참·닫는 중).
    Refused,
    /// 너무 잦다.
    RateLimited,
    /// 중계가 가득 찼다.
    Full,
    /// 서명이 틀렸다(호스트).
    BadSignature,
    /// 중계 프로토콜 판이 다르다.
    Version,
    /// 풀 수 없는 메시지.
    BadMessage,
}

impl RelayError {
    pub fn key(self) -> &'static str {
        match self {
            RelayError::NoSuchCode => "noSuchCode",
            RelayError::HostTimeout => "hostTimeout",
            RelayError::Refused => "refused",
            RelayError::RateLimited => "rateLimited",
            RelayError::Full => "full",
            RelayError::BadSignature => "badSignature",
            RelayError::Version => "version",
            RelayError::BadMessage => "badMessage",
        }
    }
}

/// 중계로 가는 메시지(호스트와 클라 모두). 첫 메시지가 `Host`면 호스트, `Join`이면 클라다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ToRelay {
    Host {
        relay: u32,
        #[serde(with = "hex32")]
        key: [u8; 32],
        #[serde(with = "hexvec")]
        sig: Vec<u8>,
        listing: Listing,
        /// 공개 목록에 올린다(아니면 코드를 아는 사람만).
        public: bool,
        /// 지난번에 받은 코드 — 비어 있거나 같은 키가 쓰던 것이면 그대로 준다.
        code: Option<String>,
    },
    Update {
        listing: Listing,
        /// 공개 여부도 바꿀 수 있다(없으면 그대로).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        public: Option<bool>,
    },
    Answer {
        session: u64,
        sdp: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<TurnInfo>,
    },
    Refuse {
        session: u64,
    },
    Join {
        relay: u32,
        code: String,
        sdp: String,
    },
}

/// 중계가 보내는 메시지.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum FromRelay {
    Hello {
        relay: u32,
        #[serde(with = "hex32")]
        nonce: [u8; 32],
    },
    /// (호스트에게) 참가 코드가 생겼다.
    Hosted { code: String, fingerprint: String },
    /// (호스트에게) 클라 하나가 들어오려 한다.
    Offer { session: u64, sdp: String },
    /// (클라에게) 호스트의 답. 지문은 중계가 서명으로 확인한 것 — 클라는 `meta` 채널의 `Probe`로 한 번 더 확인한다.
    Answer {
        sdp: String,
        fingerprint: String,
        listing: Listing,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<TurnInfo>,
    },
    Error { reason: RelayError },
}

impl ToRelay {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("relay message")
    }

    pub fn from_json(s: &str) -> Result<Self, String> {
        if s.len() > MAX_MESSAGE {
            return Err("too large".into());
        }
        serde_json::from_str(s).map_err(|e| e.to_string())
    }
}

impl FromRelay {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("relay message")
    }

    pub fn from_json(s: &str) -> Result<Self, String> {
        if s.len() > MAX_MESSAGE {
            return Err("too large".into());
        }
        serde_json::from_str(s).map_err(|e| e.to_string())
    }
}

/// 무작위 비트 → 참가 코드(`MCJE-7K3Q`). 난수는 부르는 쪽이(여기는 IO 없음).
pub fn code_from_bits(mut bits: u64) -> String {
    let mut s = String::with_capacity(CODE_PREFIX.len() + 1 + CODE_LEN);
    s.push_str(CODE_PREFIX);
    s.push('-');
    for _ in 0..CODE_LEN {
        s.push(CODE_ALPHABET[(bits % CODE_ALPHABET.len() as u64) as usize] as char);
        bits /= CODE_ALPHABET.len() as u64;
    }
    s
}

/// 사람이 적은 참가 코드를 바로잡는다 — 대소문자·공백·하이픈 무시, 머리(`MCJE`)는 없어도 된다.
/// 코드 글자(`CODE_ALPHABET`)가 아닌 것이 섞이면 코드가 아니다(`None` — 주소로 읽는다).
pub fn normalize_code(input: &str) -> Option<String> {
    let s: String = input.chars().filter(|c| !c.is_whitespace() && *c != '-').collect::<String>().to_ascii_uppercase();
    let body = s.strip_prefix(CODE_PREFIX).unwrap_or(&s);
    if body.len() != CODE_LEN || !body.bytes().all(|b| CODE_ALPHABET.contains(&b)) {
        return None;
    }
    Some(format!("{CODE_PREFIX}-{body}"))
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

pub mod hex32 {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::hex(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        super::unhex(&s).and_then(|v| <[u8; 32]>::try_from(v).ok()).ok_or_else(|| D::Error::custom("hex 32"))
    }
}

pub mod hexvec {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::hex(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        if s.len() > 256 {
            return Err(D::Error::custom("hex too long"));
        }
        super::unhex(&s).ok_or_else(|| D::Error::custom("hex"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip() {
        for bits in [0u64, 1, 29, 30, 12345, 809_999, u64::MAX] {
            let c = code_from_bits(bits);
            assert_eq!(c.len(), 9);
            assert!(c.starts_with("MCJE-"), "{c}");
            assert_eq!(normalize_code(&c).as_deref(), Some(c.as_str()));
        }
        assert_eq!(normalize_code(" mcje-7k3q ").as_deref(), Some("MCJE-7K3Q"));
        assert_eq!(normalize_code("7k3q").as_deref(), Some("MCJE-7K3Q"));
        assert_eq!(normalize_code("MCJE7K3Q").as_deref(), Some("MCJE-7K3Q"));
        assert_eq!(normalize_code("mc je - 7k 3q").as_deref(), Some("MCJE-7K3Q"));
        assert_eq!(normalize_code("MCJE-7K3O"), None, "O는 없는 글자");
        assert_eq!(normalize_code("MCJE-7K3"), None);
        assert_eq!(normalize_code("ABCD-7K3Q"), None, "다른 머리");
        assert_eq!(normalize_code("127.0.0.1"), None);
        assert_eq!(normalize_code("example.com"), None);
    }

    #[test]
    fn messages_round_trip() {
        let m = ToRelay::Host {
            relay: RELAY_PROTOCOL,
            key: [7; 32],
            sig: vec![1; 64],
            listing: Listing { name: "A".into(), max_players: 64, loader: "fabric".into(), ..Default::default() },
            public: true,
            code: None,
        };
        let j = m.to_json();
        assert!(j.contains("\"t\":\"host\""), "{j}");
        assert!(j.contains("\"maxPlayers\":64"), "{j}");
        assert!(j.contains("\"mcVersion\""), "{j}");
        assert!(j.contains("\"onlineMode\":true"), "{j}");
        assert_eq!(ToRelay::from_json(&j).unwrap(), m);
        let a = FromRelay::Error { reason: RelayError::NoSuchCode };
        assert_eq!(a.to_json(), r#"{"t":"error","reason":"noSuchCode"}"#);
        assert_eq!(FromRelay::from_json(&a.to_json()).unwrap(), a);
        assert!(ToRelay::from_json(&"x".repeat(MAX_MESSAGE + 1)).is_err());
        let u = ToRelay::Update { listing: Listing::default(), public: None };
        assert!(!u.to_json().contains("public"));
        assert_eq!(ToRelay::from_json(r#"{"t":"update","listing":{}}"#).unwrap(), u);
    }

    #[test]
    fn turn_info_is_optional_and_clipped() {
        let old = r#"{"t":"answer","session":3,"sdp":"v=0"}"#;
        assert_eq!(ToRelay::from_json(old).unwrap(), ToRelay::Answer { session: 3, sdp: "v=0".into(), turn: None });
        let t = TurnInfo {
            urls: vec!["turn:1.2.3.4:25565?transport=tcp".into(), "http://evil".into(), "turn:a b".into()],
            username: "1700000000:ab".into(),
            credential: "x".into(),
        };
        assert_eq!(t.sanitized().unwrap().urls, vec!["turn:1.2.3.4:25565?transport=tcp".to_string()]);
        assert!(TurnInfo { urls: vec!["stun:x".into()], ..t.clone() }.sanitized().is_none());
        assert!(TurnInfo { username: String::new(), ..t.clone() }.sanitized().is_none());
        let m = ToRelay::Answer { session: 1, sdp: "v=0".into(), turn: Some(t) };
        assert_eq!(ToRelay::from_json(&m.to_json()).unwrap(), m);
    }

    #[test]
    fn listing_is_clipped() {
        let l = Listing {
            name: "가".repeat(100),
            motd: "a\u{7}b".into(),
            mc_version: "26.3-very-long-version-string".into(),
            loader: "Forge".into(),
            mods_hash: "ABCDEF0123456789".into(),
            players: 200_000,
            max_players: 500_000,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(l.name.chars().count(), 48);
        assert_eq!(l.motd, "ab");
        assert_eq!(l.mc_version.chars().count(), 16);
        assert_eq!(l.loader, "vanilla", "모르는 로더");
        assert_eq!(l.mods_hash, "abcdef0123456789");
        assert_eq!(l.max_players, 100_000);
        assert_eq!(l.players, 100_000);
        assert_eq!(Listing { mods_hash: "xyz".into(), loader: "NeoForge".into(), ..Default::default() }.sanitized().mods_hash, "");
        assert_eq!(Listing { loader: " NeoForge ".into(), ..Default::default() }.sanitized().loader, "neoforge");
        let b = Listing { loader: "Bukkit".into(), loader_version: "Paper 26.3-251".into(), ..Default::default() }.sanitized();
        assert_eq!((b.loader.as_str(), b.loader_version.as_str()), ("bukkit", "Paper 26.3-251"));
    }
}
