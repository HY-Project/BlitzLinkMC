//! TURN 클라(join) — str0m에는 TURN 클라가 없어 직접 만든다. codec은 호스트 TURN 서버와 같은 `blitzlink_core::turn_codec`.
//!
//! 순서: TCP 접속(`turn:IP:포트?transport=tcp`, 호스트의 MC 포트) → Allocate(자격 없이 → 401 + nonce → 자격 붙여 다시)
//! → 할당받은 주소(XOR-RELAYED-ADDRESS)를 str0m에 `Candidate::relayed`로 싣는다. 그 후보에서 나가는 패킷은 여기로 와서
//! 피어마다 ChannelBind(권한도 같이 생긴다) 뒤 ChannelData로 감싸 보낸다(묶이기 전에는 잠깐 줄 세운다). 돌아온 ChannelData·
//! Data 지시는 (피어, 바이트)로 돌려준다. 할당은 4분마다 Refresh, 채널은 다시 Bind해 수명을 늘린다.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use blitzlink_core::relay::TurnInfo;
use blitzlink_core::turn_codec::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::control::log;

const REFRESH_EVERY: Duration = Duration::from_secs(240);
const LIFETIME: u32 = 600;
/// 피어 하나가 묶이기 전에 줄 세울 수 있는 패킷 수.
const WAIT_QUEUE: usize = 32;

/// 할당 하나.
pub struct TurnLink {
    /// 할당받은 중계 주소 — relayed 후보.
    pub relayed: SocketAddr,
    /// TURN TCP 접속의 내 쪽 주소.
    pub local: SocketAddr,
    /// TURN 서버의 주소(TCP).
    pub server: SocketAddr,
    /// (피어, 바이트) — 보낼 것.
    pub out: mpsc::Sender<(SocketAddr, Vec<u8>)>,
    /// (피어, 바이트) — 받은 것. 닫히면 TURN 접속이 끊겼다.
    pub inbound: mpsc::Receiver<(SocketAddr, Vec<u8>)>,
}

async fn read_frame(s: &mut TcpStream, buf: &mut Vec<u8>) -> Result<Vec<u8>, String> {
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(len) = frame_len(buf) {
            if len > 8192 {
                return Err("frame too large".into());
            }
            if buf.len() >= len {
                return Ok(buf.drain(..len).collect());
            }
        }
        let n = s.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("closed".into());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

async fn request(s: &mut TcpStream, buf: &mut Vec<u8>, bytes: Vec<u8>, tx: [u8; 12]) -> Result<Vec<u8>, String> {
    s.write_all(&bytes).await.map_err(|e| e.to_string())?;
    loop {
        let f = tokio::time::timeout(Duration::from_secs(4), read_frame(s, buf)).await.map_err(|_| "no answer".to_string())??;
        if Msg::parse(&f).is_some_and(|m| m.tx == tx) {
            return Ok(f);
        }
    }
}

/// 호스트의 내장 TURN에 붙어 할당받는다(주소를 차례로 시도).
pub async fn connect(info: &TurnInfo) -> Result<TurnLink, String> {
    let mut last = "no TURN address".to_string();
    for url in &info.urls {
        let Some(host) = blitzlink_core::net::turn_host(url) else { continue };
        match tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(&host)).await {
            Ok(Ok(s)) => match allocate(s, &info.username, &info.credential).await {
                Ok(link) => return Ok(link),
                Err(e) => last = format!("{host}: {e}"),
            },
            Ok(Err(e)) => last = format!("{host}: {e}"),
            Err(_) => last = format!("{host}: timeout"),
        }
    }
    Err(last)
}

async fn allocate(mut s: TcpStream, user: &str, pass: &str) -> Result<TurnLink, String> {
    let _ = s.set_nodelay(true);
    let local = s.local_addr().map_err(|e| e.to_string())?;
    let server = s.peer_addr().map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    let tx = random_tx();
    let r = request(&mut s, &mut buf, Build::new(ALLOCATE, CLASS_REQUEST, tx).attr(A_TRANSPORT, &TRANSPORT_UDP).finish(None), tx).await?;
    let m = Msg::parse(&r).ok_or("not STUN")?;
    if m.error_code() != Some(401) {
        return Err(format!("expected 401, got {:?}", m.error_code()));
    }
    let realm = m.text(A_REALM).unwrap_or(REALM).to_string();
    let nonce = m.get(A_NONCE).ok_or("no nonce")?.to_vec();
    let key = long_term_key(user, pass);
    let tx = random_tx();
    let req = Build::new(ALLOCATE, CLASS_REQUEST, tx)
        .attr(A_USERNAME, user.as_bytes())
        .attr(A_REALM, realm.as_bytes())
        .attr(A_NONCE, &nonce)
        .attr(A_TRANSPORT, &TRANSPORT_UDP)
        .attr(A_LIFETIME, &LIFETIME.to_be_bytes())
        .finish(Some(&key));
    let r = request(&mut s, &mut buf, req, tx).await?;
    let m = Msg::parse(&r).ok_or("not STUN")?;
    if m.class != CLASS_SUCCESS {
        return Err(format!("allocation refused ({:?})", m.error_code()));
    }
    if !m.integrity_ok(&key) {
        return Err("bad response integrity".into());
    }
    let relayed = m.xaddr(m.get(A_RELAYED).ok_or("no relayed address")?).ok_or("bad relayed address")?;
    let (out_tx, out_rx) = mpsc::channel(512);
    let (in_tx, in_rx) = mpsc::channel(512);
    let auth = Auth { user: user.to_string(), realm, nonce, key };
    tokio::spawn(run(s, buf, auth, out_rx, in_tx));
    log(format!("TURN: allocated on {server}, relayed address {relayed}"));
    Ok(TurnLink { relayed, local, server, out: out_tx, inbound: in_rx })
}

struct Auth {
    user: String,
    realm: String,
    nonce: Vec<u8>,
    key: [u8; 16],
}

impl Auth {
    fn sign(&self, b: Build) -> Vec<u8> {
        b.attr(A_USERNAME, self.user.as_bytes()).attr(A_REALM, self.realm.as_bytes()).attr(A_NONCE, &self.nonce).finish(Some(&self.key))
    }
}

fn bind_req(auth: &Auth, ch: u16, peer: SocketAddr) -> ([u8; 12], Vec<u8>) {
    let tx = random_tx();
    let mut v = ch.to_be_bytes().to_vec();
    v.extend_from_slice(&[0, 0]);
    (tx, auth.sign(Build::new(CHANNEL_BIND, CLASS_REQUEST, tx).attr(A_CHANNEL, &v).addr(A_PEER, peer)))
}

async fn run(
    mut s: TcpStream,
    mut buf: Vec<u8>,
    mut auth: Auth,
    mut out_rx: mpsc::Receiver<(SocketAddr, Vec<u8>)>,
    in_tx: mpsc::Sender<(SocketAddr, Vec<u8>)>,
) {
    let mut bound: HashMap<SocketAddr, u16> = HashMap::new();
    let mut by_ch: HashMap<u16, SocketAddr> = HashMap::new();
    let mut binding: HashMap<[u8; 12], (SocketAddr, u16)> = HashMap::new();
    let mut waiting: HashMap<SocketAddr, (u16, Vec<Vec<u8>>)> = HashMap::new();
    let mut next_ch: u16 = 0x4000;
    let mut chunk = vec![0u8; 8192];
    let mut refresh = tokio::time::interval_at(tokio::time::Instant::now() + REFRESH_EVERY, REFRESH_EVERY);
    loop {
        tokio::select! {
            got = out_rx.recv() => {
                let Some((peer, data)) = got else { return };
                let frame = if let Some(&ch) = bound.get(&peer) {
                    channel_data(ch, &data)
                } else if let Some((_, q)) = waiting.get_mut(&peer) {
                    if q.len() < WAIT_QUEUE {
                        q.push(data);
                    }
                    continue;
                } else {
                    if next_ch > 0x4FFF {
                        continue;
                    }
                    let ch = next_ch;
                    next_ch += 1;
                    by_ch.insert(ch, peer);
                    waiting.insert(peer, (ch, vec![data]));
                    let (tx, req) = bind_req(&auth, ch, peer);
                    binding.insert(tx, (peer, ch));
                    req
                };
                if s.write_all(&frame).await.is_err() {
                    return;
                }
            }
            r = s.read(&mut chunk) => {
                let n = match r { Ok(0) | Err(_) => return, Ok(n) => n };
                buf.extend_from_slice(&chunk[..n]);
                while let Some(len) = frame_len(&buf) {
                    if len > 8192 {
                        return;
                    }
                    if buf.len() < len {
                        break;
                    }
                    let f: Vec<u8> = buf.drain(..len).collect();
                    if let Some((ch, data)) = parse_channel_data(&f) {
                        if let Some(&peer) = by_ch.get(&ch) {
                            if in_tx.send((peer, data.to_vec())).await.is_err() {
                                return;
                            }
                        }
                        continue;
                    }
                    let Some(m) = Msg::parse(&f) else { continue };
                    match (m.method, m.class) {
                        (DATA, CLASS_INDICATION) => {
                            if let (Some(peer), Some(data)) = (m.get(A_PEER).and_then(|v| m.xaddr(v)), m.get(A_DATA)) {
                                if in_tx.send((peer, data.to_vec())).await.is_err() {
                                    return;
                                }
                            }
                        }
                        (CHANNEL_BIND, CLASS_SUCCESS) => {
                            if let Some((peer, ch)) = binding.remove(&m.tx) {
                                bound.insert(peer, ch);
                                if let Some((_, q)) = waiting.remove(&peer) {
                                    for d in q {
                                        if s.write_all(&channel_data(ch, &d)).await.is_err() {
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                        (_, CLASS_ERROR) => {
                            if m.error_code() == Some(438) {
                                if let Some(n) = m.get(A_NONCE) {
                                    auth.nonce = n.to_vec();
                                }
                            }
                            if let Some((peer, ch)) = binding.remove(&m.tx) {
                                // 다시 묶어 본다(새 nonce일 수 있다).
                                let (tx, req) = bind_req(&auth, ch, peer);
                                binding.insert(tx, (peer, ch));
                                if s.write_all(&req).await.is_err() {
                                    return;
                                }
                            } else {
                                log(format!("TURN error {:?}", m.error_code()));
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ = refresh.tick() => {
                let tx = random_tx();
                let req = auth.sign(Build::new(REFRESH, CLASS_REQUEST, tx).attr(A_LIFETIME, &LIFETIME.to_be_bytes()));
                if s.write_all(&req).await.is_err() {
                    return;
                }
                for (&peer, &ch) in &bound {
                    let (tx, req) = bind_req(&auth, ch, peer);
                    let _ = tx;
                    if s.write_all(&req).await.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turn_server;
    use tokio::net::UdpSocket;

    /// 호스트 TURN 서버에 붙어 할당 → (묶기 전에 보낸 것도) 피어로 가고 답이 그 피어에서 온 것으로 돌아온다.
    #[tokio::test]
    async fn client_allocates_and_relays_through_host_turn() {
        let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = echo.local_addr().unwrap();
        tokio::spawn(async move {
            let mut b = [0u8; 2048];
            while let Ok((n, from)) = echo.recv_from(&mut b).await {
                let _ = echo.send_to(&b[..n], from).await;
            }
        });
        let turn = turn_server::Turn::new(target, 4);
        let port = turn_server::listen(turn.clone(), "127.0.0.1:0".parse().unwrap()).await.unwrap();
        let (username, credential) = turn.credentials(Duration::from_secs(60));
        let bad = TurnInfo { urls: vec![format!("turn:127.0.0.1:{port}?transport=tcp")], username: username.clone(), credential: "x".into() };
        assert!(connect(&bad).await.is_err(), "틀린 비밀번호");
        let info = TurnInfo {
            urls: vec!["turn:127.0.0.1:1?transport=tcp".into(), format!("turn:127.0.0.1:{port}?transport=tcp")],
            username,
            credential,
        };
        let mut link = connect(&info).await.unwrap();
        assert!(link.relayed.ip().is_loopback());
        let peer = SocketAddr::new("198.51.100.4".parse().unwrap(), target.port());
        for i in 0..3u8 {
            link.out.send((peer, vec![i; 100])).await.unwrap();
        }
        for i in 0..3u8 {
            let (from, data) = tokio::time::timeout(Duration::from_secs(3), link.inbound.recv()).await.unwrap().unwrap();
            assert_eq!(from, peer);
            assert_eq!(data, vec![i; 100]);
        }
    }
}
