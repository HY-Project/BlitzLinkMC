//! STUN Binding(RFC 5389)의 아주 작은 한 벌 — 중계(`blitzlink-relay`)가 UDP 3478에서 답하고(선택), 사이드카(`blitzlink-tunnel`)가
//! 자기 공인 주소(NAT 바깥 — srflx 후보)를 알아낼 때 묻는다. 인증·재전송·다른 속성은 없다. IO는 부르는 쪽이.
//! TURN codec(`turn_codec`)도 머리·주소 속성·FINGERPRINT를 여기서 가져다 쓴다.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

pub const MAGIC: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
pub const XOR_MAPPED_ADDRESS: u16 = 0x0020;
const MAPPED_ADDRESS: u16 = 0x0001;
pub const FINGERPRINT: u16 = 0x8028;
pub const FINGERPRINT_XOR: u32 = 0x5354_554E;

/// 메시지 머리 — (종류, 속성 길이, 트랜잭션). STUN이 아니거나 길이가 버퍼와 맞지 않으면 None.
pub fn header(buf: &[u8]) -> Option<(u16, usize, [u8; 12])> {
    if buf.len() < 20 || buf[0] & 0xC0 != 0 {
        return None;
    }
    let ty = u16::from_be_bytes([buf[0], buf[1]]);
    let len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    if u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) != MAGIC || !len.is_multiple_of(4) || 20 + len != buf.len() {
        return None;
    }
    let mut tx = [0u8; 12];
    tx.copy_from_slice(&buf[8..20]);
    Some((ty, len, tx))
}

/// Binding 요청 한 통(속성 없음).
pub fn binding_request(tx: [u8; 12]) -> [u8; 20] {
    let mut b = [0u8; 20];
    b[0..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());
    b[4..8].copy_from_slice(&MAGIC.to_be_bytes());
    b[8..20].copy_from_slice(&tx);
    b
}

/// Binding 요청이면 트랜잭션 번호(속성은 보지 않는다 — 브라우저가 붙이는 SOFTWARE·FINGERPRINT 따위).
pub fn parse_binding_request(buf: &[u8]) -> Option<[u8; 12]> {
    match header(buf)? {
        (BINDING_REQUEST, _, tx) => Some(tx),
        _ => None,
    }
}

/// Binding 성공 응답 — 보낸 쪽 주소를 XOR-MAPPED-ADDRESS로, 끝에 FINGERPRINT.
pub fn binding_response(tx: [u8; 12], addr: SocketAddr) -> Vec<u8> {
    let v = xor_address(addr, &tx);
    let mut attr = Vec::with_capacity(24);
    attr.extend_from_slice(&XOR_MAPPED_ADDRESS.to_be_bytes());
    attr.extend_from_slice(&(v.len() as u16).to_be_bytes());
    attr.extend_from_slice(&v);
    let mut b = Vec::with_capacity(20 + attr.len() + 8);
    b.extend_from_slice(&BINDING_SUCCESS.to_be_bytes());
    b.extend_from_slice(&((attr.len() + 8) as u16).to_be_bytes());
    b.extend_from_slice(&MAGIC.to_be_bytes());
    b.extend_from_slice(&tx);
    b.extend_from_slice(&attr);
    let crc = crc32(&b) ^ FINGERPRINT_XOR;
    b.extend_from_slice(&FINGERPRINT.to_be_bytes());
    b.extend_from_slice(&4u16.to_be_bytes());
    b.extend_from_slice(&crc.to_be_bytes());
    b
}

/// 이 트랜잭션의 Binding 성공 응답이면 거기 적힌 내 주소.
pub fn parse_binding_response(buf: &[u8], want: &[u8; 12]) -> Option<SocketAddr> {
    let (ty, len, tx) = header(buf)?;
    if ty != BINDING_SUCCESS || &tx != want {
        return None;
    }
    let mut at = 20;
    let mut plain = None;
    while at + 4 <= 20 + len {
        let aty = u16::from_be_bytes([buf[at], buf[at + 1]]);
        let alen = u16::from_be_bytes([buf[at + 2], buf[at + 3]]) as usize;
        let v = buf.get(at + 4..at + 4 + alen)?;
        if aty == XOR_MAPPED_ADDRESS || aty == MAPPED_ADDRESS {
            let xor = aty == XOR_MAPPED_ADDRESS;
            let addr = read_address(v, &tx, xor)?;
            if xor {
                return Some(addr);
            }
            plain = Some(addr);
        }
        at += 4 + alen.div_ceil(4) * 4;
    }
    plain
}

/// XOR-…-ADDRESS 속성의 값(머리 없이) — 주소를 magic·트랜잭션과 XOR한다(RFC 5389 15.2).
pub fn xor_address(addr: SocketAddr, tx: &[u8; 12]) -> Vec<u8> {
    let mut v = Vec::with_capacity(20);
    v.push(0);
    let xport = addr.port() ^ (MAGIC >> 16) as u16;
    match addr.ip() {
        IpAddr::V4(ip) => {
            v.push(0x01);
            v.extend_from_slice(&xport.to_be_bytes());
            v.extend_from_slice(&(u32::from(ip) ^ MAGIC).to_be_bytes());
        }
        IpAddr::V6(ip) => {
            v.push(0x02);
            v.extend_from_slice(&xport.to_be_bytes());
            v.extend(ip.octets().iter().zip(v6_key(tx)).map(|(a, k)| a ^ k));
        }
    }
    v
}

fn v6_key(tx: &[u8; 12]) -> [u8; 16] {
    let mut key = [0u8; 16];
    key[..4].copy_from_slice(&MAGIC.to_be_bytes());
    key[4..].copy_from_slice(tx);
    key
}

/// 주소 속성의 값을 푼다(`xor`면 XOR-…-ADDRESS, 아니면 MAPPED-ADDRESS).
pub fn read_address(v: &[u8], tx: &[u8; 12], xor: bool) -> Option<SocketAddr> {
    if v.len() < 8 {
        return None;
    }
    let port = u16::from_be_bytes([v[2], v[3]]) ^ if xor { (MAGIC >> 16) as u16 } else { 0 };
    let ip = match (v[1], v.len()) {
        (0x01, 8) => {
            let raw = u32::from_be_bytes([v[4], v[5], v[6], v[7]]);
            IpAddr::V4(Ipv4Addr::from(if xor { raw ^ MAGIC } else { raw }))
        }
        (0x02, 20) => {
            let mut o = [0u8; 16];
            o.copy_from_slice(&v[4..20]);
            if xor {
                for (a, k) in o.iter_mut().zip(v6_key(tx)) {
                    *a ^= k;
                }
            }
            IpAddr::V6(Ipv6Addr::from(o))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_known_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn binding_round_trip_v4_v6() {
        let tx = [9u8; 12];
        let req = binding_request(tx);
        assert_eq!(parse_binding_request(&req), Some(tx));
        for addr in ["203.0.113.7:51820", "[2001:db8::1]:3478"] {
            let addr: SocketAddr = addr.parse().unwrap();
            let res = binding_response(tx, addr);
            assert_eq!(parse_binding_response(&res, &tx), Some(addr));
            assert_eq!(parse_binding_response(&res, &[0; 12]), None, "다른 트랜잭션");
            assert_eq!(parse_binding_request(&res), None);
        }
    }

    #[test]
    fn junk_is_not_stun() {
        assert_eq!(parse_binding_request(&[0u8; 19]), None);
        assert_eq!(parse_binding_request(&[0x16; 40]), None, "DTLS");
        let mut req = binding_request([1; 12]);
        req[4] = 0;
        assert_eq!(parse_binding_request(&req), None, "magic");
    }
}
