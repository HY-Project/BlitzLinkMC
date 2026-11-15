//! **내장 TURN**(RFC 8656의 작은 한 벌, TCP만) — UDP가 막힌 학교·회사망의 클라가 WebRTC로 들어오는 길.
//!
//! - 바깥에서는 **MC 서버의 TCP 포트**로 들어온다. 모드(Java)가 첫 바이트로 가른다 — STUN 머리(0x00~0x03)면 이 사이드카의
//!   루프백 TURN 포트(`turnPort`)로 바이트를 잇고, 아니면 MC 접속이다(MC 핸드셰이크 첫 바이트는 varint 길이 ≥ 0x07 또는 0xFE).
//! - **이 호스트에만 잇는다.** 클라가 어느 피어 주소를 대든(호스트의 공인·LAN·루프백 후보) 데이터는 이 호스트의 WebRTC UDP 소켓
//!   (`target`)으로만 간다 — 남의 호스트로 나가는 열린 중계가 아니다. 피어 주소마다 루프백 UDP 소켓을 하나씩 열어, 돌아오는 패킷을
//!   어느 피어에서 온 것으로 알릴지 가른다(ICE는 요청을 보낸 그 주소에서 답이 와야 짝으로 본다).
//! - 자격: 장기 자격에 시간제한 사용자명(`<만료 unix초>:<무작위>`), 비밀번호 = base64(HMAC-SHA1(호스트 비밀, 사용자명)).
//!   호스트가 Offer에 답할 때 만들어 중계로 클라에 건넨다(`relay::TurnInfo`, TTL 300초). 만료는 Allocate 때만 본다.
//! - 막는 것: 동시 할당 수, 할당마다 피어 소켓 8개, 프레임 크기, 할당 수명(기본 600초, 상한 3600초), 들어와서 10초 안에 Allocate.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use blitzlink_core::stun;
use blitzlink_core::turn_codec::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc;

use crate::control::log;

const DEFAULT_LIFETIME: u32 = 600;
const MAX_LIFETIME: u32 = 3600;
const PERMISSION_LIFETIME: Duration = Duration::from_secs(300);
const CHANNEL_LIFETIME: Duration = Duration::from_secs(600);
const PEER_SOCKETS: usize = 8;
const FIRST_ALLOCATE: Duration = Duration::from_secs(10);
/// TCP 한 프레임(STUN 메시지 또는 ChannelData)의 상한.
const MAX_FRAME: usize = 4096;

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// 내장 TURN의 설정 — 호스트가 켤 때 한 번.
pub struct Turn {
    secret: [u8; 32],
    /// 데이터를 넘길 곳 — 이 호스트의 WebRTC UDP 소켓(루프백).
    target: SocketAddr,
    max: usize,
    active: AtomicUsize,
    /// 피어 소켓의 로컬 주소 → 그 할당의 TCP 상대(모드가 넘겨준 루프백 접속, 또는 직접 붙은 클라).
    sources: Mutex<HashMap<SocketAddr, SocketAddr>>,
}

impl Turn {
    pub fn new(target: SocketAddr, max: usize) -> Arc<Turn> {
        Arc::new(Turn { secret: blitzlink_core::random32(), target, max, active: AtomicUsize::new(0), sources: Mutex::new(HashMap::new()) })
    }

    /// 클라에 건넬 자격 — 만료 `ttl` 뒤(그때까지 Allocate해야 한다).
    pub fn credentials(&self, ttl: Duration) -> (String, String) {
        let tag = blitzlink_core::relay::hex(&blitzlink_core::random32()[..4]);
        credentials(&self.secret, unix_now() + ttl.as_secs(), &tag)
    }

    fn key_for(&self, user: &str, check_expiry: bool) -> Option<[u8; 16]> {
        key_for(&self.secret, user, unix_now(), check_expiry)
    }

    /// 호스트 UDP 소켓이 받은 패킷의 출처가 TURN 피어 소켓이면 그 할당의 TCP 상대.
    pub fn tcp_peer_of(&self, udp_source: SocketAddr) -> Option<SocketAddr> {
        self.sources.lock().unwrap_or_else(|e| e.into_inner()).get(&udp_source).copied()
    }

    pub fn active(&self) -> usize {
        self.active.load(Ordering::Relaxed)
    }
}

/// 루프백 포트를 열고 받기 시작한다 — 그 포트를 돌려준다.
pub async fn listen(turn: Arc<Turn>, bind: SocketAddr) -> Result<u16, String> {
    let l = TcpListener::bind(bind).await.map_err(|e| format!("TURN {bind}: cannot bind ({e})"))?;
    let port = l.local_addr().map_err(|e| e.to_string())?.port();
    tokio::spawn(async move {
        loop {
            match l.accept().await {
                Ok((s, peer)) => {
                    tokio::spawn(serve(s, peer, turn.clone()));
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    });
    Ok(port)
}

struct Alloc {
    user: String,
    key: [u8; 16],
    expires: Instant,
    perms: HashMap<IpAddr, Instant>,
    channels: HashMap<u16, (SocketAddr, Instant)>,
    /// 피어 주소 → 그 피어 몫의 UDP 소켓(이 호스트의 `target`으로만 보낸다).
    sockets: HashMap<SocketAddr, Arc<UdpSocket>>,
}

struct Guard {
    turn: Arc<Turn>,
    locals: Vec<SocketAddr>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.turn.active.fetch_sub(1, Ordering::Relaxed);
        let mut s = self.turn.sources.lock().unwrap_or_else(|e| e.into_inner());
        for a in &self.locals {
            s.remove(a);
        }
    }
}

/// 첫 바이트가 STUN 머리인 TCP 접속 하나를 맡는다.
pub async fn serve(mut stream: TcpStream, peer: SocketAddr, turn: Arc<Turn>) {
    if turn.active.fetch_add(1, Ordering::Relaxed) >= turn.max {
        turn.active.fetch_sub(1, Ordering::Relaxed);
        return;
    }
    let mut guard = Guard { turn: turn.clone(), locals: Vec::new() };
    let local_ip = stream.local_addr().map(|a| a.ip()).unwrap_or(turn.target.ip());
    let nonce = blitzlink_core::relay::hex(&blitzlink_core::random32()[..12]);
    let opened = Instant::now();
    let mut alloc: Option<Alloc> = None;
    let mut buf: Vec<u8> = Vec::with_capacity(MAX_FRAME * 2);
    let mut chunk = vec![0u8; MAX_FRAME];
    let (udp_tx, mut udp_rx) = mpsc::channel::<(SocketAddr, Vec<u8>)>(256);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            r = stream.read(&mut chunk) => {
                let n = match r { Ok(0) | Err(_) => break, Ok(n) => n };
                buf.extend_from_slice(&chunk[..n]);
                while let Some(len) = frame_len(&buf) {
                    if len > MAX_FRAME {
                        return;
                    }
                    if buf.len() < len {
                        break;
                    }
                    let frame: Vec<u8> = buf.drain(..len).collect();
                    let reply = on_frame(&frame, peer, local_ip, &turn, &nonce, &mut alloc, &udp_tx, &mut guard).await;
                    match reply {
                        Some(Reply::Send(bytes)) => if stream.write_all(&bytes).await.is_err() { return },
                        Some(Reply::Close(bytes)) => { let _ = stream.write_all(&bytes).await; return }
                        None => {}
                    }
                }
            }
            Some((from, data)) = udp_rx.recv() => {
                let Some(a) = alloc.as_mut() else { continue };
                let now = Instant::now();
                if !a.perms.get(&from.ip()).is_some_and(|t| *t > now) {
                    continue;
                }
                let out = match a.channels.iter().find(|(_, (p, t))| *p == from && *t > now) {
                    Some((&ch, _)) => channel_data(ch, &data),
                    None => Build::new(DATA, CLASS_INDICATION, random_tx()).addr(A_PEER, from).attr(A_DATA, &data).finish(None),
                };
                if stream.write_all(&out).await.is_err() {
                    return;
                }
            }
            _ = tick.tick() => {
                let now = Instant::now();
                match &alloc {
                    None if now.duration_since(opened) > FIRST_ALLOCATE => return,
                    Some(a) if a.expires <= now => return,
                    _ => {}
                }
            }
        }
    }
}

enum Reply {
    Send(Vec<u8>),
    Close(Vec<u8>),
}

#[allow(clippy::too_many_arguments)]
async fn on_frame(
    frame: &[u8],
    peer: SocketAddr,
    local_ip: IpAddr,
    turn: &Turn,
    nonce: &str,
    alloc: &mut Option<Alloc>,
    udp_tx: &mpsc::Sender<(SocketAddr, Vec<u8>)>,
    guard: &mut Guard,
) -> Option<Reply> {
    if let Some((ch, data)) = parse_channel_data(frame) {
        let a = alloc.as_mut()?;
        let now = Instant::now();
        let (to, _) = *a.channels.get(&ch).filter(|(_, t)| *t > now)?;
        relay_out(a, to, data, turn, udp_tx, peer, guard).await;
        return None;
    }
    let m = Msg::parse(frame)?;
    if m.class == CLASS_INDICATION {
        if m.method == SEND {
            let a = alloc.as_mut()?;
            let to = m.xaddr(m.get(A_PEER)?)?;
            if a.perms.get(&to.ip()).is_some_and(|t| *t > Instant::now()) {
                relay_out(a, to, m.get(A_DATA)?, turn, udp_tx, peer, guard).await;
            }
        }
        return None;
    }
    if m.class != CLASS_REQUEST {
        return None;
    }
    // 인증 — 처음(할당 전)은 호스트가 만든 사용자명이고 만료 전이어야, 그 뒤는 할당 때의 사용자명·열쇠 그대로.
    let unauthorized = || {
        Reply::Send(error(m.method, m.tx, 401, "Unauthorized", |b| b.attr(A_REALM, REALM.as_bytes()).attr(A_NONCE, nonce.as_bytes())))
    };
    let Some(user) = m.text(A_USERNAME) else { return Some(unauthorized()) };
    if m.text(A_NONCE) != Some(nonce) {
        return Some(Reply::Send(error(m.method, m.tx, 438, "Stale Nonce", |b| b.attr(A_REALM, REALM.as_bytes()).attr(A_NONCE, nonce.as_bytes()))));
    }
    let key = match alloc.as_ref() {
        Some(a) if a.user == user => a.key,
        Some(_) => return Some(unauthorized()),
        None => match turn.key_for(user, m.method == ALLOCATE) {
            Some(k) => k,
            None => return Some(unauthorized()),
        },
    };
    if !m.integrity_ok(&key) {
        return Some(unauthorized());
    }
    let ok = |b: Build| Reply::Send(b.finish(Some(&key)));
    let lifetime = |m: &Msg| m.get(A_LIFETIME).filter(|v| v.len() == 4).map(|v| u32::from_be_bytes([v[0], v[1], v[2], v[3]]));
    match m.method {
        ALLOCATE => {
            if alloc.is_some() {
                return Some(Reply::Send(error(ALLOCATE, m.tx, 437, "Allocation Mismatch", |b| b)));
            }
            if m.get(A_TRANSPORT).is_none_or(|v| v.first() != Some(&17)) {
                return Some(Reply::Send(error(ALLOCATE, m.tx, 442, "Unsupported Transport Protocol", |b| b)));
            }
            let secs = lifetime(&m).unwrap_or(DEFAULT_LIFETIME).clamp(DEFAULT_LIFETIME, MAX_LIFETIME);
            // 알리는 중계 주소 — 클라가 들어온 이 호스트의 IP와 첫 피어 소켓의 포트(데이터는 어차피 `target`으로만 간다).
            let first = bind_peer_socket(turn).await?;
            let first_local = first.local_addr().ok()?;
            let relayed = SocketAddr::new(local_ip, first_local.port());
            let mut a = Alloc {
                user: user.to_string(),
                key,
                expires: Instant::now() + Duration::from_secs(secs as u64),
                perms: HashMap::new(),
                channels: HashMap::new(),
                sockets: HashMap::new(),
            };
            a.sockets.insert(SocketAddr::new(IpAddr::from([0, 0, 0, 0]), 0), first);
            register_source(turn, guard, first_local, peer);
            *alloc = Some(a);
            log(format!("TURN: allocation for {peer} (client behind blocked UDP)"));
            Some(ok(Build::new(ALLOCATE, CLASS_SUCCESS, m.tx)
                .addr(A_RELAYED, relayed)
                .addr(stun::XOR_MAPPED_ADDRESS, peer)
                .attr(A_LIFETIME, &secs.to_be_bytes())
                .attr(A_SOFTWARE, b"BlitzLink")))
        }
        REFRESH => {
            let a = alloc.as_mut()?;
            let secs = lifetime(&m).unwrap_or(DEFAULT_LIFETIME).min(MAX_LIFETIME);
            let reply = Build::new(REFRESH, CLASS_SUCCESS, m.tx).attr(A_LIFETIME, &secs.to_be_bytes()).finish(Some(&key));
            if secs == 0 {
                return Some(Reply::Close(reply));
            }
            a.expires = Instant::now() + Duration::from_secs(secs as u64);
            Some(Reply::Send(reply))
        }
        CREATE_PERMISSION => {
            let a = alloc.as_mut()?;
            let peers: Vec<SocketAddr> = m.all(A_PEER).filter_map(|v| m.xaddr(v)).collect();
            if peers.is_empty() {
                return Some(Reply::Send(error(CREATE_PERMISSION, m.tx, 400, "Bad Request", |b| b)));
            }
            let until = Instant::now() + PERMISSION_LIFETIME;
            for p in peers {
                a.perms.insert(p.ip(), until);
            }
            Some(ok(Build::new(CREATE_PERMISSION, CLASS_SUCCESS, m.tx)))
        }
        CHANNEL_BIND => {
            let a = alloc.as_mut()?;
            let ch = m.get(A_CHANNEL).filter(|v| v.len() == 4).map(|v| u16::from_be_bytes([v[0], v[1]]));
            let to = m.get(A_PEER).and_then(|v| m.xaddr(v));
            let (Some(ch), Some(to)) = (ch, to) else { return Some(Reply::Send(error(CHANNEL_BIND, m.tx, 400, "Bad Request", |b| b))) };
            let now = Instant::now();
            let clash = a.channels.iter().any(|(&c, (p, t))| *t > now && ((c == ch) != (*p == to)));
            if !(0x4000..=0x4FFF).contains(&ch) || clash {
                return Some(Reply::Send(error(CHANNEL_BIND, m.tx, 400, "Bad Request", |b| b)));
            }
            a.channels.insert(ch, (to, now + CHANNEL_LIFETIME));
            a.perms.insert(to.ip(), now + PERMISSION_LIFETIME);
            Some(ok(Build::new(CHANNEL_BIND, CLASS_SUCCESS, m.tx)))
        }
        _ => Some(Reply::Send(error(m.method, m.tx, 400, "Bad Request", |b| b))),
    }
}

fn register_source(turn: &Turn, guard: &mut Guard, local: SocketAddr, tcp_peer: SocketAddr) {
    turn.sources.lock().unwrap_or_else(|e| e.into_inner()).insert(local, tcp_peer);
    guard.locals.push(local);
}

async fn bind_peer_socket(turn: &Turn) -> Option<Arc<UdpSocket>> {
    let s = UdpSocket::bind(SocketAddr::new(turn.target.ip(), 0)).await.ok()?;
    s.connect(turn.target).await.ok()?;
    Some(Arc::new(s))
}

/// 피어(`to`)에게 보낼 것을 이 호스트의 WebRTC 소켓으로. 피어마다 소켓 하나(돌아오는 답을 그 피어의 것으로 알린다).
async fn relay_out(
    a: &mut Alloc,
    to: SocketAddr,
    data: &[u8],
    turn: &Turn,
    udp_tx: &mpsc::Sender<(SocketAddr, Vec<u8>)>,
    tcp_peer: SocketAddr,
    guard: &mut Guard,
) {
    if to.port() != turn.target.port() {
        return; // 이 호스트의 WebRTC 포트가 아닌 곳 — 이 TURN은 이 호스트에만 잇는다.
    }
    let sock = match a.sockets.get(&to) {
        Some(s) => s.clone(),
        None => {
            if a.sockets.len() > PEER_SOCKETS {
                return;
            }
            // 할당 때 미리 연 첫 소켓을 첫 피어에게 준다(알린 중계 포트와 같은 것).
            let unspecified = SocketAddr::new(IpAddr::from([0, 0, 0, 0]), 0);
            let s = match a.sockets.remove(&unspecified) {
                Some(s) => s,
                None => match bind_peer_socket(turn).await {
                    Some(s) => {
                        if let Ok(l) = s.local_addr() {
                            register_source(turn, guard, l, tcp_peer);
                        }
                        s
                    }
                    None => return,
                },
            };
            a.sockets.insert(to, s.clone());
            let tx = udp_tx.clone();
            let reader = s.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 2048];
                while let Ok(n) = reader.recv(&mut buf).await {
                    if tx.send((to, buf[..n].to_vec())).await.is_err() {
                        return;
                    }
                }
            });
            s
        }
    };
    let _ = sock.send(data).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_expire_only_for_allocate() {
        let t = Turn::new("127.0.0.1:25565".parse().unwrap(), 4);
        let (user, pass) = t.credentials(Duration::from_secs(60));
        assert_eq!(t.key_for(&user, true), Some(long_term_key(&user, &pass)));
        assert!(t.key_for("1:00", true).is_none(), "만료");
        assert!(t.key_for("1:00", false).is_some(), "갱신은 만료를 보지 않는다");
        assert!(t.key_for("nope", false).is_none());
    }

    /// 진짜 TCP로: 자격 없는 Allocate → 401, 틀린 비밀번호 → 401, 맞으면 할당 → ChannelBind → ChannelData가 `target`(여기서는
    /// 메아리 UDP 소켓)에 닿고 답이 ChannelData로 돌아온다. Send 지시는 Data 지시로 돌아온다. 남의 포트로는 나가지 않는다.
    /// 피어 소켓의 출처로 TCP 상대를 알아낸다(`tcp_peer_of`).
    #[tokio::test]
    async fn allocate_bind_and_relay() {
        let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = echo.local_addr().unwrap();
        let (seen_tx, mut seen_rx) = mpsc::unbounded_channel::<SocketAddr>();
        tokio::spawn(async move {
            let mut b = [0u8; 2048];
            while let Ok((n, from)) = echo.recv_from(&mut b).await {
                let _ = seen_tx.send(from);
                let mut out = b"echo:".to_vec();
                out.extend_from_slice(&b[..n]);
                let _ = echo.send_to(&out, from).await;
            }
        });
        let turn = Turn::new(target, 4);
        let port = listen(turn.clone(), "127.0.0.1:0".parse().unwrap()).await.unwrap();
        let mut c = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let me = c.local_addr().unwrap();
        async fn read_frame(c: &mut TcpStream) -> Vec<u8> {
            let mut head = [0u8; 4];
            tokio::time::timeout(Duration::from_secs(3), c.read_exact(&mut head)).await.expect("timeout").unwrap();
            let len = frame_len(&head).unwrap();
            let mut rest = vec![0u8; len - 4];
            c.read_exact(&mut rest).await.unwrap();
            [head.to_vec(), rest].concat()
        }
        let code = |f: &[u8]| Msg::parse(f).unwrap().error_code();
        c.write_all(&Build::new(ALLOCATE, CLASS_REQUEST, [1; 12]).attr(A_TRANSPORT, &TRANSPORT_UDP).finish(None)).await.unwrap();
        let r = read_frame(&mut c).await;
        assert_eq!(code(&r), Some(401));
        let nonce = Msg::parse(&r).unwrap().get(A_NONCE).unwrap().to_vec();
        let (user, pass) = turn.credentials(Duration::from_secs(60));
        let req = |pw: &str, tx: u8| {
            Build::new(ALLOCATE, CLASS_REQUEST, [tx; 12])
                .attr(A_USERNAME, user.as_bytes())
                .attr(A_REALM, REALM.as_bytes())
                .attr(A_NONCE, &nonce)
                .attr(A_TRANSPORT, &TRANSPORT_UDP)
                .finish(Some(&long_term_key(&user, pw)))
        };
        c.write_all(&req("wrong", 2)).await.unwrap();
        assert_eq!(code(&read_frame(&mut c).await), Some(401));
        c.write_all(&req(&pass, 3)).await.unwrap();
        let r = read_frame(&mut c).await;
        let m = Msg::parse(&r).unwrap();
        assert_eq!((m.method, m.class), (ALLOCATE, CLASS_SUCCESS));
        let key = long_term_key(&user, &pass);
        assert!(m.integrity_ok(&key), "응답에도 MESSAGE-INTEGRITY");
        let relayed = m.xaddr(m.get(A_RELAYED).unwrap()).unwrap();
        assert!(relayed.ip().is_loopback());
        // 호스트의 「공인」 후보인 셈 치는 주소 — 포트만 맞으면 데이터는 target으로 간다.
        let host_peer: SocketAddr = SocketAddr::new("203.0.113.9".parse().unwrap(), target.port());
        let with_auth = |b: Build| b.attr(A_USERNAME, user.as_bytes()).attr(A_REALM, REALM.as_bytes()).attr(A_NONCE, &nonce).finish(Some(&key));
        let mut ch = 0x4001u16.to_be_bytes().to_vec();
        ch.extend_from_slice(&[0, 0]);
        c.write_all(&with_auth(Build::new(CHANNEL_BIND, CLASS_REQUEST, [4; 12]).attr(A_CHANNEL, &ch).addr(A_PEER, host_peer))).await.unwrap();
        let r = read_frame(&mut c).await;
        assert_eq!(Msg::parse(&r).unwrap().class, CLASS_SUCCESS, "{:?}", code(&r));
        c.write_all(&channel_data(0x4001, b"hi")).await.unwrap();
        let r = read_frame(&mut c).await;
        assert_eq!(parse_channel_data(&r), Some((0x4001, &b"echo:hi"[..])));
        // 첫 피어 소켓은 알린 중계 주소 그대로이고, 그 출처로 TCP 상대를 알아낸다.
        let src = seen_rx.recv().await.unwrap();
        assert_eq!(src, relayed);
        assert_eq!(turn.tcp_peer_of(src), Some(me));
        // 다른 피어(같은 포트) — 권한을 주고 Send 지시로 보내면 Data 지시로 돌아온다(피어마다 소켓이 따로).
        let other: SocketAddr = SocketAddr::new("192.168.0.9".parse().unwrap(), target.port());
        c.write_all(&with_auth(Build::new(CREATE_PERMISSION, CLASS_REQUEST, [5; 12]).addr(A_PEER, other))).await.unwrap();
        assert_eq!(Msg::parse(&read_frame(&mut c).await).unwrap().class, CLASS_SUCCESS);
        c.write_all(&Build::new(SEND, CLASS_INDICATION, [6; 12]).addr(A_PEER, other).attr(A_DATA, b"yo").finish(None)).await.unwrap();
        let r = read_frame(&mut c).await;
        let m = Msg::parse(&r).unwrap();
        assert_eq!((m.method, m.class), (DATA, CLASS_INDICATION));
        assert_eq!(m.get(A_DATA), Some(&b"echo:yo"[..]));
        assert_eq!(m.xaddr(m.get(A_PEER).unwrap()), Some(other), "답은 보낸 그 피어에서 온 것으로");
        let src2 = seen_rx.recv().await.unwrap();
        assert_ne!(src2, src);
        assert_eq!(turn.tcp_peer_of(src2), Some(me));
        // 남의 포트로는 보내지 않는다 — 권한이 있어도 답이 없다.
        let elsewhere: SocketAddr = SocketAddr::new(other.ip(), 9);
        c.write_all(&Build::new(SEND, CLASS_INDICATION, [7; 12]).addr(A_PEER, elsewhere).attr(A_DATA, b"x").finish(None)).await.unwrap();
        let mut b = [0u8; 4];
        assert!(tokio::time::timeout(Duration::from_millis(300), c.read_exact(&mut b)).await.is_err());
        // 접속이 끝나면 출처 표에서도 빠진다.
        drop(c);
        let until = Instant::now() + Duration::from_secs(2);
        while turn.tcp_peer_of(src).is_some() || turn.active() > 0 {
            assert!(Instant::now() < until, "정리되지 않음");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
