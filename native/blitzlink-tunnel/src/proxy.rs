//! HAProxy PROXY protocol v2 머리(`--proxy-protocol`) — 호스트가 `--target`에 붙을 때 맨 앞에 보내, 서버(Paper의
//! `proxies.proxy-protocol: true` 등)가 루프백 대신 플레이어의 실제 주소로 로그·밴을 하게 한다.
//! 규격: https://www.haproxy.org/download/2.9/doc/proxy-protocol.txt (2.2절).

use std::net::{IpAddr, SocketAddr};

const SIGNATURE: [u8; 12] = [0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A];

/// `src`(플레이어) → `dst`(우리가 붙는 서버) 머리. `src`를 모르면 LOCAL(서버는 소켓 주소를 그대로 쓴다).
pub fn v2_header(src: Option<SocketAddr>, dst: SocketAddr) -> Vec<u8> {
    let mut b = SIGNATURE.to_vec();
    let Some(src) = src else {
        b.extend_from_slice(&[0x20, 0x00, 0x00, 0x00]);
        return b;
    };
    match (src.ip(), dst.ip()) {
        (IpAddr::V4(s), IpAddr::V4(d)) => {
            b.extend_from_slice(&[0x21, 0x11, 0x00, 12]);
            b.extend_from_slice(&s.octets());
            b.extend_from_slice(&d.octets());
        }
        (s, d) => {
            let v6 = |ip: IpAddr| match ip {
                IpAddr::V4(v) => v.to_ipv6_mapped(),
                IpAddr::V6(v) => v,
            };
            b.extend_from_slice(&[0x21, 0x21, 0x00, 36]);
            b.extend_from_slice(&v6(s).octets());
            b.extend_from_slice(&v6(d).octets());
        }
    }
    b.extend_from_slice(&src.port().to_be_bytes());
    b.extend_from_slice(&dst.port().to_be_bytes());
    b
}

/// (시험용) 머리를 풀어 (머리 길이, 원본 주소).
#[cfg(test)]
pub fn parse_v2(b: &[u8]) -> Option<(usize, Option<SocketAddr>)> {
    if b.len() < 16 || b[..12] != SIGNATURE || b[12] >> 4 != 2 {
        return None;
    }
    let len = u16::from_be_bytes([b[14], b[15]]) as usize;
    let body = b.get(16..16 + len)?;
    let src = match (b[12] & 0x0F, b[13]) {
        (0x1, 0x11) if len >= 12 => Some(SocketAddr::new(IpAddr::from(<[u8; 4]>::try_from(&body[0..4]).ok()?), u16::from_be_bytes([body[8], body[9]]))),
        (0x1, 0x21) if len >= 36 => {
            let ip = std::net::Ipv6Addr::from(<[u8; 16]>::try_from(&body[0..16]).ok()?);
            let ip = ip.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(ip));
            Some(SocketAddr::new(ip, u16::from_be_bytes([body[32], body[33]])))
        }
        _ => None,
    };
    Some((16 + len, src))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4_header_bytes() {
        let h = v2_header(Some("203.0.113.5:40000".parse().unwrap()), "127.0.0.1:25565".parse().unwrap());
        assert_eq!(h.len(), 16 + 12);
        assert_eq!(&h[12..16], &[0x21, 0x11, 0x00, 0x0C]);
        assert_eq!(&h[16..20], &[203, 0, 113, 5]);
        assert_eq!(&h[20..24], &[127, 0, 0, 1]);
        assert_eq!(&h[24..26], &40000u16.to_be_bytes());
        assert_eq!(&h[26..28], &25565u16.to_be_bytes());
        assert_eq!(parse_v2(&h), Some((28, Some("203.0.113.5:40000".parse().unwrap()))));
    }

    #[test]
    fn v6_and_local() {
        let src: SocketAddr = "[2001:db8::7]:5000".parse().unwrap();
        let h = v2_header(Some(src), "127.0.0.1:25565".parse().unwrap());
        assert_eq!(&h[12..16], &[0x21, 0x21, 0x00, 36]);
        assert_eq!(parse_v2(&h), Some((52, Some(src))));
        let l = v2_header(None, "127.0.0.1:25565".parse().unwrap());
        assert_eq!(&l[12..], &[0x20, 0x00, 0x00, 0x00]);
        assert_eq!(parse_v2(&l), Some((16, None)));
    }
}
