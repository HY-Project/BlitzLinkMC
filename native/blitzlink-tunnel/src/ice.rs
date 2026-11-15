//! ICE 후보 모으기(Full ICE) — host 후보(LAN·루프백)와 srflx 후보(STUN으로 알아낸 공인 주소).
//! 공인 주소는 host가 아니라 srflx 후보로 싣는다(Full ICE).
//!
//! STUN 한 번이면 공인 주소를 아니 trickle은 쓰지 않는다 — 모은 뒤 SDP에 다 싣는다. 양쪽이 서로의 srflx로 동시에 연결 확인을
//! 보내 NAT에 구멍이 뚫린다(홀펀칭).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use str0m::{Candidate, Rtc};
use tokio::net::UdpSocket;

/// 이 PC의 바깥으로 나가는 LAN 주소(패킷은 보내지 않는다 — 경로만 고른다).
pub fn primary_ipv4() -> Option<Ipv4Addr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:53").ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// 0.0.0.0에 묶인 소켓이 받은 패킷의 「목적지」 — OS가 알려 주지 않으므로 보낸 쪽으로 미루어 정한다.
pub fn destination_for(lan: Option<Ipv4Addr>, port: u16, source: SocketAddr) -> SocketAddr {
    if source.ip().is_loopback() || lan.is_none() {
        return SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    }
    SocketAddr::new(IpAddr::V4(lan.unwrap()), port)
}

/// `stun:host:port` 목록을 IPv4 소켓 주소로(이름 풀이 실패는 건너뛴다).
pub async fn resolve_stun(urls: &[String]) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for u in urls {
        let Some(host) = blitzlink_core::net::stun_host(u) else { continue };
        if let Ok(Ok(addrs)) = tokio::time::timeout(Duration::from_secs(3), tokio::net::lookup_host(host)).await {
            out.extend(addrs.filter(|a| a.is_ipv4()));
        }
    }
    out
}

/// 그 소켓으로 STUN에 물어 밖에서 본 내 주소를 받는다(서버마다 두 번까지, 한 번에 `wait`). 소켓을 다른 데서 읽고 있지 않을 때만.
pub async fn stun_query(sock: &UdpSocket, servers: &[SocketAddr], wait: Duration) -> Option<SocketAddr> {
    let mut buf = [0u8; 1500];
    for server in servers {
        for _ in 0..2 {
            let tx: [u8; 12] = blitzlink_core::random32()[..12].try_into().unwrap();
            if sock.send_to(&blitzlink_core::stun::binding_request(tx), server).await.is_err() {
                break;
            }
            let until = tokio::time::Instant::now() + wait;
            while let Ok(Ok((n, _))) = tokio::time::timeout_at(until, sock.recv_from(&mut buf)).await {
                if let Some(a) = blitzlink_core::stun::parse_binding_response(&buf[..n], &tx) {
                    return Some(a);
                }
            }
        }
    }
    None
}

/// 소켓 하나(포트 `port`)의 후보를 Rtc에 싣는다 — LAN host, 루프백 host(같은 PC), srflx(공인 주소, host와 다를 때).
pub fn add_candidates(rtc: &mut Rtc, lan: Option<Ipv4Addr>, port: u16, public: Option<SocketAddr>) {
    let mut hosts: Vec<SocketAddr> = Vec::new();
    if let Some(ip) = lan {
        hosts.push(SocketAddr::new(IpAddr::V4(ip), port));
    }
    hosts.push(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port));
    for h in &hosts {
        if let Ok(c) = Candidate::host(*h, "udp") {
            rtc.add_local_candidate(c);
        }
    }
    if let Some(p) = public.filter(|p| p.is_ipv4() && !hosts.contains(p) && !p.ip().is_loopback()) {
        let base = hosts[0];
        if let Ok(c) = Candidate::server_reflexive(p, base, "udp") {
            rtc.add_local_candidate(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations() {
        let lan = Some(Ipv4Addr::new(192, 168, 0, 5));
        assert_eq!(destination_for(lan, 7, "127.0.0.1:9".parse().unwrap()), "127.0.0.1:7".parse().unwrap());
        assert_eq!(destination_for(lan, 7, "8.8.8.8:9".parse().unwrap()), "192.168.0.5:7".parse().unwrap());
        assert_eq!(destination_for(None, 7, "8.8.8.8:9".parse().unwrap()), "127.0.0.1:7".parse().unwrap());
    }

    #[tokio::test]
    async fn stun_query_against_local_relay() {
        let r = blitzlink_relay::start(blitzlink_relay::Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            stun: Some("127.0.0.1:0".parse().unwrap()),
            ..Default::default()
        })
        .await
        .unwrap();
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let got = stun_query(&sock, &[r.stun.unwrap()], Duration::from_millis(500)).await;
        assert_eq!(got, Some(sock.local_addr().unwrap()));
        let servers = resolve_stun(&[format!("stun:{}", r.stun.unwrap()), "off".into()]).await;
        assert_eq!(servers, vec![r.stun.unwrap()]);
    }
}
