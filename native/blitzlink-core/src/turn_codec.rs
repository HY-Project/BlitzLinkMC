//! TURN 메시지 codec(`Msg`·`Build`·`channel_data`·`frame_len`·`long_term_key`·HMAC). 호스트의 내장 TURN 서버(`blitzlink-tunnel`의 `turn_server`)와 클라의 TURN 클라(`turn_client`)가 같은 codec을 쓴다.
//!
//! RFC 8656(TURN)·RFC 8489(STUN)의 작은 한 벌, TCP 위만. 장기 자격: 시간제한 사용자명(`<만료 unix초>:<무작위>`),
//! 비밀번호 = base64(HMAC-SHA1(호스트 비밀, 사용자명)), 열쇠 = MD5(user:realm:pass). IO 없음.

use std::net::SocketAddr;

use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use md5::{Digest, Md5};
use sha1::Sha1;

use crate::stun::{self, FINGERPRINT, FINGERPRINT_XOR, MAGIC};

pub const REALM: &str = "blitzlink";

pub const ALLOCATE: u16 = 0x003;
pub const REFRESH: u16 = 0x004;
pub const SEND: u16 = 0x006;
pub const DATA: u16 = 0x007;
pub const CREATE_PERMISSION: u16 = 0x008;
pub const CHANNEL_BIND: u16 = 0x009;

pub const CLASS_REQUEST: u16 = 0x0000;
pub const CLASS_INDICATION: u16 = 0x0010;
pub const CLASS_SUCCESS: u16 = 0x0100;
pub const CLASS_ERROR: u16 = 0x0110;

pub const A_USERNAME: u16 = 0x0006;
pub const A_INTEGRITY: u16 = 0x0008;
pub const A_ERROR: u16 = 0x0009;
pub const A_CHANNEL: u16 = 0x000C;
pub const A_LIFETIME: u16 = 0x000D;
pub const A_PEER: u16 = 0x0012;
pub const A_DATA: u16 = 0x0013;
pub const A_REALM: u16 = 0x0014;
pub const A_NONCE: u16 = 0x0015;
pub const A_RELAYED: u16 = 0x0016;
pub const A_TRANSPORT: u16 = 0x0019;
pub const A_SOFTWARE: u16 = 0x8022;

/// REQUESTED-TRANSPORT의 값 — UDP(17).
pub const TRANSPORT_UDP: [u8; 4] = [17, 0, 0, 0];

/// 장기 자격 열쇠 — MD5(user:realm:pass).
pub fn long_term_key(user: &str, pass: &str) -> [u8; 16] {
    let mut h = Md5::new();
    h.update(format!("{user}:{REALM}:{pass}").as_bytes());
    h.finalize().into()
}

pub fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(key).expect("hmac");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// 시간제한 비밀번호 — base64(HMAC-SHA1(비밀, 사용자명)).
pub fn password(secret: &[u8], user: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(hmac_sha1(secret, user.as_bytes()))
}

/// 사용자명 `<만료 unix초>:<태그>`와 그 비밀번호.
pub fn credentials(secret: &[u8], expiry_unix: u64, tag: &str) -> (String, String) {
    let user = format!("{expiry_unix}:{tag}");
    let pass = password(secret, &user);
    (user, pass)
}

/// 사용자명이 그 비밀로 만든 꼴이고 (`check_expiry`면) 아직 만료 전이면 열쇠.
pub fn key_for(secret: &[u8], user: &str, now_unix: u64, check_expiry: bool) -> Option<[u8; 16]> {
    let expiry: u64 = user.split(':').next()?.parse().ok()?;
    if check_expiry && expiry < now_unix {
        return None;
    }
    Some(long_term_key(user, &password(secret, user)))
}

/// 받은 STUN 메시지 하나(속성은 처음 나온 것만 `get`, 모두는 `all`).
pub struct Msg<'a> {
    pub method: u16,
    pub class: u16,
    pub tx: [u8; 12],
    raw: &'a [u8],
    attrs: Vec<(u16, usize, &'a [u8])>,
}

impl<'a> Msg<'a> {
    pub fn parse(raw: &'a [u8]) -> Option<Msg<'a>> {
        let (ty, len, tx) = stun::header(raw)?;
        let mut attrs = Vec::new();
        let mut at = 20;
        while at + 4 <= 20 + len {
            let aty = u16::from_be_bytes([raw[at], raw[at + 1]]);
            let alen = u16::from_be_bytes([raw[at + 2], raw[at + 3]]) as usize;
            let v = raw.get(at + 4..at + 4 + alen)?;
            attrs.push((aty, at, v));
            at += 4 + alen.div_ceil(4) * 4;
        }
        Some(Msg { method: (ty & 0x000F) | ((ty & 0x00E0) >> 1) | ((ty & 0x3E00) >> 2), class: ty & 0x0110, tx, raw, attrs })
    }

    pub fn get(&self, ty: u16) -> Option<&'a [u8]> {
        self.attrs.iter().find(|(t, _, _)| *t == ty).map(|(_, _, v)| *v)
    }

    pub fn all(&self, ty: u16) -> impl Iterator<Item = &'a [u8]> + '_ {
        self.attrs.iter().filter(move |(t, _, _)| *t == ty).map(|(_, _, v)| *v)
    }

    pub fn text(&self, ty: u16) -> Option<&'a str> {
        std::str::from_utf8(self.get(ty)?).ok()
    }

    /// XOR-…-ADDRESS 속성의 값을 주소로.
    pub fn xaddr(&self, v: &[u8]) -> Option<SocketAddr> {
        stun::read_address(v, &self.tx, true)
    }

    /// ERROR-CODE(있으면).
    pub fn error_code(&self) -> Option<u16> {
        self.get(A_ERROR).filter(|v| v.len() >= 4).map(|v| (v[2] & 0x07) as u16 * 100 + v[3] as u16)
    }

    /// MESSAGE-INTEGRITY가 이 열쇠로 맞는가(그 뒤의 속성은 FINGERPRINT뿐이어야 한다).
    pub fn integrity_ok(&self, key: &[u8]) -> bool {
        let Some(&(_, at, v)) = self.attrs.iter().find(|(t, _, _)| *t == A_INTEGRITY) else { return false };
        if v.len() != 20 {
            return false;
        }
        let mut head = self.raw[..at].to_vec();
        head[2..4].copy_from_slice(&((at + 24 - 20) as u16).to_be_bytes());
        hmac_sha1(key, &head) == v
    }
}

/// 보낼 STUN 메시지를 쌓는다.
pub struct Build {
    b: Vec<u8>,
    tx: [u8; 12],
}

impl Build {
    pub fn new(method: u16, class: u16, tx: [u8; 12]) -> Build {
        let ty = (method & 0x000F) | ((method & 0x0070) << 1) | ((method & 0x0F80) << 2) | class;
        let mut b = Vec::with_capacity(128);
        b.extend_from_slice(&ty.to_be_bytes());
        b.extend_from_slice(&[0, 0]);
        b.extend_from_slice(&MAGIC.to_be_bytes());
        b.extend_from_slice(&tx);
        Build { b, tx }
    }

    pub fn attr(mut self, ty: u16, v: &[u8]) -> Build {
        self.b.extend_from_slice(&ty.to_be_bytes());
        self.b.extend_from_slice(&(v.len() as u16).to_be_bytes());
        self.b.extend_from_slice(v);
        self.b.resize(self.b.len() + (4 - v.len() % 4) % 4, 0);
        self
    }

    pub fn addr(self, ty: u16, a: SocketAddr) -> Build {
        let v = stun::xor_address(a, &self.tx);
        self.attr(ty, &v)
    }

    fn set_len(&mut self, extra: usize) {
        let len = (self.b.len() - 20 + extra) as u16;
        self.b[2..4].copy_from_slice(&len.to_be_bytes());
    }

    /// 끝맺는다 — 열쇠가 있으면 MESSAGE-INTEGRITY, 늘 FINGERPRINT.
    pub fn finish(mut self, key: Option<&[u8]>) -> Vec<u8> {
        if let Some(k) = key {
            self.set_len(24);
            let mac = hmac_sha1(k, &self.b);
            self = self.attr(A_INTEGRITY, &mac);
        }
        self.set_len(8);
        let crc = stun::crc32(&self.b) ^ FINGERPRINT_XOR;
        self.attr(FINGERPRINT, &crc.to_be_bytes()).b
    }
}

/// 오류 응답 한 통(ERROR-CODE + 덧붙일 속성).
pub fn error(method: u16, tx: [u8; 12], code: u16, reason: &str, extra: impl FnOnce(Build) -> Build) -> Vec<u8> {
    let mut v = vec![0, 0, (code / 100) as u8, (code % 100) as u8];
    v.extend_from_slice(reason.as_bytes());
    extra(Build::new(method, CLASS_ERROR, tx).attr(A_ERROR, &v)).finish(None)
}

/// ChannelData 한 통 — TCP 위에서는 4바이트로 맞춘다(RFC 8656 12.5).
pub fn channel_data(ch: u16, data: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(4 + data.len() + 3);
    b.extend_from_slice(&ch.to_be_bytes());
    b.extend_from_slice(&(data.len() as u16).to_be_bytes());
    b.extend_from_slice(data);
    b.resize(b.len() + (4 - data.len() % 4) % 4, 0);
    b
}

/// ChannelData 프레임이면 (채널, 데이터).
pub fn parse_channel_data(frame: &[u8]) -> Option<(u16, &[u8])> {
    if frame.len() < 4 || frame[0] & 0xC0 != 0x40 {
        return None;
    }
    let ch = u16::from_be_bytes([frame[0], frame[1]]);
    let len = u16::from_be_bytes([frame[2], frame[3]]) as usize;
    Some((ch, frame.get(4..4 + len)?))
}

/// 버퍼 앞의 프레임(STUN 메시지 또는 ChannelData) 하나의 길이(모자라면 None).
pub fn frame_len(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 {
        return None;
    }
    let len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    Some(match buf[0] & 0xC0 {
        0x40 => 4 + len.div_ceil(4) * 4,
        _ => 20 + len,
    })
}

pub fn random_tx() -> [u8; 12] {
    crate::random32()[..12].try_into().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_class_round_trip() {
        for (method, class) in [(ALLOCATE, CLASS_REQUEST), (CHANNEL_BIND, CLASS_SUCCESS), (DATA, CLASS_INDICATION), (REFRESH, CLASS_ERROR)] {
            let b = Build::new(method, class, [3; 12]).finish(None);
            let m = Msg::parse(&b).unwrap();
            assert_eq!((m.method, m.class), (method, class));
        }
        // RFC 8656: Allocate 요청 0x0003, 성공 0x0103, 오류 0x0113, Data 지시 0x0017.
        assert_eq!(&Build::new(ALLOCATE, CLASS_SUCCESS, [0; 12]).finish(None)[..2], &[0x01, 0x03]);
        assert_eq!(&Build::new(ALLOCATE, CLASS_ERROR, [0; 12]).finish(None)[..2], &[0x01, 0x13]);
        assert_eq!(&Build::new(DATA, CLASS_INDICATION, [0; 12]).finish(None)[..2], &[0x00, 0x17]);
    }

    #[test]
    fn integrity_round_trip() {
        let key = long_term_key("1:ab", "pw");
        let b = Build::new(ALLOCATE, CLASS_REQUEST, [5; 12]).attr(A_USERNAME, b"1:ab").attr(A_TRANSPORT, &TRANSPORT_UDP).finish(Some(&key));
        let m = Msg::parse(&b).unwrap();
        assert!(m.integrity_ok(&key));
        assert!(!m.integrity_ok(&long_term_key("1:ab", "other")));
        assert_eq!(m.get(A_USERNAME), Some(&b"1:ab"[..]));
        let e = error(ALLOCATE, [1; 12], 401, "Unauthorized", |b| b.attr(A_NONCE, b"n"));
        assert_eq!(Msg::parse(&e).unwrap().error_code(), Some(401));
    }

    #[test]
    fn md5_and_hmac_known_values() {
        let mut h = Md5::new();
        h.update(b"abc");
        let k: [u8; 16] = h.finalize().into();
        assert_eq!(crate::relay::hex(&k), "900150983cd24fb0d6963f7d28e17f72");
        // RFC 2202 HMAC-SHA1 시험 2.
        assert_eq!(crate::relay::hex(&hmac_sha1(b"Jefe", b"what do ya want for nothing?")), "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79");
    }

    #[test]
    fn credentials_expire_only_when_checked() {
        let secret = [4u8; 32];
        let (user, pass) = credentials(&secret, 1000, "ab");
        assert_eq!(key_for(&secret, &user, 999, true), Some(long_term_key(&user, &pass)));
        assert!(key_for(&secret, &user, 1001, true).is_none(), "만료");
        assert!(key_for(&secret, &user, 1001, false).is_some(), "갱신은 만료를 보지 않는다");
        assert!(key_for(&secret, "nope", 0, false).is_none());
        assert_ne!(key_for(&[5u8; 32], &user, 0, false), key_for(&secret, &user, 0, false), "다른 비밀");
    }

    #[test]
    fn frames() {
        assert_eq!(frame_len(&[0x40, 0x00, 0x00, 0x05]), Some(12));
        assert_eq!(frame_len(&[0x00, 0x03, 0x00, 0x08]), Some(28));
        let cd = channel_data(0x4001, b"abcde");
        assert_eq!(cd.len(), 12);
        assert_eq!(parse_channel_data(&cd), Some((0x4001, &b"abcde"[..])));
        assert_eq!(parse_channel_data(&[0x00, 0x01, 0, 0]), None);
    }
}
