//! UPnP-IGD 보조(`--upnp`) — igd-next.
//! 공유기에 MC 포트의 TCP(MC 접속·내장 TURN)와 WebRTC UDP 포트를 연다. 실패해도 홀펀칭·TURN이 남으니 알리기만 한다.
//! 끝낼 때 연 매핑을 지운다.

use std::net::SocketAddr;
use std::time::Duration;

use igd_next::aio::tokio::{search_gateway, Tokio};
use igd_next::aio::Gateway;
use igd_next::{PortMappingProtocol, SearchOptions};

use crate::control::log;

pub struct Mapped {
    gw: Gateway<Tokio>,
    tcp: Vec<u16>,
    udp: Option<u16>,
    pub external: Option<String>,
}

/// TCP 포트들(`tcp` — MC 포트, 바로 묶은 TURN 포트)과 UDP `udp`를 연다. 하나라도 열리면 Ok.
pub async fn open(tcp: &[u16], udp: u16) -> Result<Mapped, String> {
    let mut opts = SearchOptions::default();
    opts.timeout = Some(Duration::from_secs(3));
    let gw = search_gateway(opts).await.map_err(|e| format!("no gateway found ({e})"))?;
    let local_ip = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| s.connect(gw.addr).and_then(|_| s.local_addr()))
        .map_err(|e| format!("unknown LAN address ({e})"))?
        .ip();
    let mut m = Mapped { gw, tcp: Vec::new(), udp: None, external: None };
    for &p in tcp {
        match m.gw.add_port(PortMappingProtocol::TCP, p, SocketAddr::new(local_ip, p), 3600, "BlitzLink (TCP)").await {
            Ok(()) => m.tcp.push(p),
            Err(e) => log(format!("UPnP: cannot map TCP {p} ({e}); forward TCP {p} manually")),
        }
    }
    match m.gw.add_port(PortMappingProtocol::UDP, udp, SocketAddr::new(local_ip, udp), 3600, "BlitzLink (WebRTC UDP)").await {
        Ok(()) => m.udp = Some(udp),
        Err(e) => log(format!("UPnP: cannot map UDP {udp} ({e}); forwarding UDP {udp} lets peers connect without TURN")),
    }
    if m.tcp.is_empty() && m.udp.is_none() {
        return Err("mapping failed".into());
    }
    m.external = m.gw.get_external_ip().await.ok().map(|ip| ip.to_string());
    log(format!("UPnP: mapped TCP {:?}, UDP {:?} (external IP {})", m.tcp, m.udp, m.external.as_deref().unwrap_or("?")));
    Ok(m)
}

impl Mapped {
    /// 한 시간 임대 — 50분마다 다시 연다.
    pub async fn renew(&self) {
        let Ok(local) = std::net::UdpSocket::bind("0.0.0.0:0").and_then(|s| s.connect(self.gw.addr).and_then(|_| s.local_addr())) else { return };
        for &p in &self.tcp {
            let _ = self.gw.add_port(PortMappingProtocol::TCP, p, SocketAddr::new(local.ip(), p), 3600, "BlitzLink (TCP)").await;
        }
        if let Some(p) = self.udp {
            let _ = self.gw.add_port(PortMappingProtocol::UDP, p, SocketAddr::new(local.ip(), p), 3600, "BlitzLink (WebRTC UDP)").await;
        }
    }

    pub async fn close(&self) {
        for &p in &self.tcp {
            let _ = self.gw.remove_port(PortMappingProtocol::TCP, p).await;
        }
        if let Some(p) = self.udp {
            let _ = self.gw.remove_port(PortMappingProtocol::UDP, p).await;
        }
    }
}
