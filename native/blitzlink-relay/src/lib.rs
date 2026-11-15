//! 공식 중계(`blitzlink-relay`) — hyproj.com(`/relay/mcje/`)에 하나 서서, **SDP만** 건넨다. 마크 데이터는 거치지 않는다
//! (WebRTC DataChannel로 호스트↔클라 직접, UDP가 막히면 호스트 내장 TURN).
//!
//!  - `GET /ws`       WebSocket — 호스트(`tunnel host`)가 바깥으로 붙어 참가 코드를 받고(`Host`), 클라(`tunnel join`)가 코드로 들어온다(`Join`).
//!    메시지 모양의 정본은 `blitzlink_core::relay`.
//!  - `GET /servers`  공개 목록(JSON `ServerList`) — `--public`으로 연 서버만.
//!  - `GET /health`   `ok`
//!  - UDP 3478        STUN Binding(선택 — hyproj.com에서는 이미 도는 3478을 같이 쓰므로 `--no-stun`).
//!
//! 막는 것: 메시지 크기(16 KB), IP마다 접속 빈도(토큰 통), IP마다 올릴 수 있는 서버 수, 전체 서버 수, 호스트의 서명(Ed25519),
//! 공개 목록 글의 검열(STRICT), 브라우저의 출처(`Origin` — 네이티브 사이드카는 보내지 않아 늘 통과). 배포는 `docs/en/RELAY.md`.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use blitzlink_core::identity;
use blitzlink_core::relay::{
    code_from_bits, normalize_code, FromRelay, Listing, PublicServer, RelayError, ServerList, ToRelay, TurnInfo, MAX_MESSAGE, MAX_SDP,
    RELAY_PROTOCOL,
};
use blitzlink_core::{random32, random_u64};
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::{mpsc, oneshot};

use crate::chat_filter::{ChatFilter, ChatFilterFile, FilterLevel};

pub mod chat_filter;
pub mod log;

const FILTER_KO: &str = include_str!("../data/chat-filter/ko.json");
const FILTER_EN: &str = include_str!("../data/chat-filter/en.json");

#[derive(Debug, Clone)]
pub struct Config {
    /// HTTP·WebSocket(nginx 뒤 — 기본 127.0.0.1:1947).
    pub listen: SocketAddr,
    /// STUN(UDP). 없으면 열지 않는다.
    pub stun: Option<SocketAddr>,
    /// 브라우저가 `/ws`를 열 수 있는 출처(`Origin` 머리). 비면 막지 않는다. `http://localhost:*`처럼 포트 자리에 `*`.
    /// 브라우저가 아닌 것(사이드카)은 `Origin`을 보내지 않아 늘 통과한다 — 이 검사는 남의 웹페이지가 우리 중계를 쓰지 못하게 할 뿐이다.
    pub allow_origins: Vec<String>,
    /// 앞의 프록시(nginx)가 준 `X-Real-IP`를 믿는다.
    pub trust_proxy: bool,
    /// 한꺼번에 올라와 있을 수 있는 서버 수.
    pub max_hosts: usize,
    /// IP 하나가 올릴 수 있는 서버 수.
    pub max_hosts_per_ip: u32,
    /// 호스트가 Offer에 답할 시간.
    pub answer_timeout: Duration,
    /// 호스트가 끊긴 뒤 그 참가 코드를 그 호스트 몫으로 남겨 두는 시간.
    pub code_reserve: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:1947".parse().unwrap(),
            stun: Some("0.0.0.0:3478".parse().unwrap()),
            allow_origins: default_origins(),
            trust_proxy: false,
            max_hosts: 5000,
            max_hosts_per_ip: 8,
            answer_timeout: Duration::from_secs(8),
            code_reserve: Duration::from_secs(24 * 3600),
        }
    }
}

/// hyproj.com과 개발 서버.
pub fn default_origins() -> Vec<String> {
    ["https://hyproj.com", "https://www.hyproj.com", "http://localhost:*", "http://127.0.0.1:*"].map(String::from).to_vec()
}

/// 그 출처가 허용 목록에 드는가(`*`는 포트 자리 — `:` 뒤 숫자만, 없어도 된다).
pub fn origin_allowed(origin: &str, allow: &[String]) -> bool {
    let port_ok = |rest: &str| rest.is_empty() || rest.strip_prefix(':').is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    allow.is_empty()
        || allow.iter().any(|a| match a.strip_suffix(":*") {
            Some(base) => origin.strip_prefix(base).is_some_and(port_ok),
            None => origin.eq_ignore_ascii_case(a),
        })
}

/// 첫 메시지를 기다리는 시간 · 호스트가 아무 말 없이 있을 수 있는 시간(호스트는 20초마다 ping).
const FIRST_MESSAGE: Duration = Duration::from_secs(10);
const HOST_IDLE: Duration = Duration::from_secs(75);
/// IP마다 WebSocket 접속 토큰 통 — 30개, 2초에 하나 참.
const BUCKET_CAP: f32 = 30.0;
const BUCKET_REFILL_PER_SEC: f32 = 0.5;

struct Host {
    key: [u8; 32],
    fingerprint: String,
    listing: Listing,
    public: bool,
    ip: IpAddr,
    /// 같은 키가 다시 붙으면 옛 접속을 알아보는 번호.
    conn: u64,
    tx: mpsc::Sender<FromRelay>,
}

/// 호스트의 답(SDP와 내장 TURN) 또는 거절.
type Settled = Result<(String, Option<TurnInfo>), RelayError>;

struct Bucket {
    tokens: f32,
    at: Instant,
}

#[derive(Default)]
struct Inner {
    hosts: HashMap<String, Host>,
    /// 키 → (코드, 언제까지) — 호스트가 잠깐 끊겼다 다시 붙어도 같은 코드.
    reserved: HashMap<[u8; 32], (String, Instant)>,
    /// 세션 → (그 호스트의 코드, 답을 기다리는 클라).
    pending: HashMap<u64, (String, oneshot::Sender<Settled>)>,
    next: u64,
    buckets: HashMap<IpAddr, Bucket>,
    joins: u64,
}

pub struct Relay {
    cfg: Config,
    filter: ChatFilter,
    inner: Mutex<Inner>,
}

pub struct RelayHandle {
    pub http: SocketAddr,
    pub stun: Option<SocketAddr>,
    pub relay: Arc<Relay>,
}

impl Relay {
    pub fn new(cfg: Config) -> Result<Self, String> {
        let files: Vec<ChatFilterFile> =
            [FILTER_KO, FILTER_EN].iter().map(|s| serde_json::from_str(s).expect("data/chat-filter")).collect();
        Ok(Self { cfg, filter: ChatFilter::new(&files, &[]), inner: Mutex::new(Inner::default()) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 토큰 하나를 쓴다 — 모자라면 false.
    fn take_token(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut g = self.lock();
        if g.buckets.len() > 50_000 {
            g.buckets.retain(|_, b| now.duration_since(b.at) < Duration::from_secs(120));
        }
        let b = g.buckets.entry(ip).or_insert(Bucket { tokens: BUCKET_CAP, at: now });
        b.tokens = (b.tokens + now.duration_since(b.at).as_secs_f32() * BUCKET_REFILL_PER_SEC).min(BUCKET_CAP);
        b.at = now;
        if b.tokens < 1.0 {
            return false;
        }
        b.tokens -= 1.0;
        true
    }

    /// 공개 목록·클라에게 보여 줄 모양 — 길이를 다듬고 이름·MOTD를 STRICT로 가린다.
    fn present(&self, l: &Listing) -> Listing {
        let mut l = l.sanitized();
        l.name = self.filter.mask(&l.name, FilterLevel::Strict);
        l.motd = self.filter.mask(&l.motd, FilterLevel::Strict);
        l
    }

    pub fn list(&self) -> ServerList {
        let g = self.lock();
        let mut servers: Vec<PublicServer> = g
            .hosts
            .iter()
            .filter(|(_, h)| h.public)
            .map(|(code, h)| PublicServer { code: code.clone(), fingerprint: h.fingerprint.clone(), listing: self.present(&h.listing) })
            .collect();
        servers.sort_by(|a, b| b.listing.players.cmp(&a.listing.players).then_with(|| a.listing.name.cmp(&b.listing.name)));
        servers.truncate(500);
        ServerList { relay: RELAY_PROTOCOL, servers }
    }

    pub fn stats(&self) -> (usize, usize, u64) {
        let g = self.lock();
        (g.hosts.len(), g.hosts.values().filter(|h| h.public).count(), g.joins)
    }

    /// 호스트를 올린다 — 코드를 정한다. 같은 키의 옛 접속이 남아 있으면 그것을 밀어낸다.
    fn register(&self, key: [u8; 32], want: Option<&str>, host: Host) -> Result<String, RelayError> {
        let now = Instant::now();
        let mut g = self.lock();
        g.reserved.retain(|_, (_, until)| *until > now);
        // 같은 키가 이미 올라와 있으면(다시 붙는 중) 그 자리를 넘겨받는다.
        let old = g.hosts.iter().find(|(_, h)| h.key == key).map(|(c, _)| c.clone());
        if let Some(c) = &old {
            g.hosts.remove(c);
        }
        if g.hosts.len() >= self.cfg.max_hosts {
            return Err(RelayError::Full);
        }
        if g.hosts.values().filter(|h| h.ip == host.ip).count() as u32 >= self.cfg.max_hosts_per_ip {
            return Err(RelayError::Full);
        }
        let free = |g: &Inner, c: &str| !g.hosts.contains_key(c) && g.reserved.iter().all(|(k, (rc, _))| rc != c || *k == key);
        let mine = g.reserved.get(&key).map(|(c, _)| c.clone());
        let code = match (want.and_then(normalize_code), old, mine) {
            (Some(w), _, _) if free(&g, &w) => w,
            (_, Some(o), _) => o,
            (_, _, Some(m)) if free(&g, &m) => m,
            _ => {
                let mut picked = None;
                for _ in 0..64 {
                    let c = code_from_bits(random_u64());
                    if free(&g, &c) {
                        picked = Some(c);
                        break;
                    }
                }
                picked.ok_or(RelayError::Full)?
            }
        };
        g.reserved.insert(key, (code.clone(), now + self.cfg.code_reserve));
        g.hosts.insert(code.clone(), host);
        Ok(code)
    }

    fn unregister(&self, code: &str, conn: u64) {
        let now = Instant::now();
        let mut g = self.lock();
        // 같은 키의 새 접속이 자리를 넘겨받았으면 그 접속의 것은 건드리지 않는다.
        if g.hosts.get(code).is_some_and(|h| h.conn == conn) {
            let h = g.hosts.remove(code).unwrap();
            g.reserved.insert(h.key, (code.to_string(), now + self.cfg.code_reserve));
            g.pending.retain(|_, (c, _)| c != code);
        }
    }

    /// 호스트가 세션에 답했다 — 그 호스트 몫의 세션일 때만.
    fn settle(&self, code: &str, session: u64, result: Settled) {
        let mut g = self.lock();
        if g.pending.get(&session).is_some_and(|(c, _)| c == code) {
            let (_, reply) = g.pending.remove(&session).unwrap();
            let _ = reply.send(result);
        }
    }
}

/// 열고 돌리기 시작한다(포트 0이면 아무 빈 포트).
pub async fn start(cfg: Config) -> Result<RelayHandle, String> {
    let listener = TcpListener::bind(cfg.listen).await.map_err(|e| format!("{}: cannot bind ({e})", cfg.listen))?;
    let http = listener.local_addr().map_err(|e| e.to_string())?;
    let stun = match cfg.stun {
        Some(addr) => {
            let sock = UdpSocket::bind(addr).await.map_err(|e| format!("STUN {addr}: cannot bind ({e})"))?;
            let local = sock.local_addr().map_err(|e| e.to_string())?;
            tokio::spawn(stun_loop(sock));
            Some(local)
        }
        None => None,
    };
    let relay = Arc::new(Relay::new(cfg)?);
    let app = Router::new()
        .route("/ws", get(ws_route))
        .route("/servers", get(servers_route))
        .route("/health", get(|| async { "ok" }))
        .with_state(relay.clone());
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await {
            log::warn(format!("HTTP: {e}"));
        }
    });
    Ok(RelayHandle { http, stun, relay })
}

async fn stun_loop(sock: UdpSocket) {
    let mut buf = [0u8; 1500];
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf).await else { continue };
        if let Some(tx) = blitzlink_core::stun::parse_binding_request(&buf[..n]) {
            let _ = sock.send_to(&blitzlink_core::stun::binding_response(tx, from), from).await;
        }
    }
}

/// 상대 IP — nginx 뒤면 `X-Real-IP`(그 프록시를 믿을 때만).
fn client_ip(relay: &Relay, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
    if relay.cfg.trust_proxy {
        let from = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).and_then(|s| s.split(',').next()?.trim().parse().ok());
        if let Some(ip) = from("x-real-ip").or_else(|| from("x-forwarded-for")) {
            return ip;
        }
    }
    peer.ip()
}

fn json_response(body: String, cache: &'static str) -> Response {
    let mut r = (StatusCode::OK, body).into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json; charset=utf-8"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    r
}

async fn servers_route(State(relay): State<Arc<Relay>>) -> Response {
    json_response(serde_json::to_string(&relay.list()).unwrap_or_default(), "public, max-age=5")
}

async fn ws_route(
    ws: WebSocketUpgrade,
    State(relay): State<Arc<Relay>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(origin) = headers.get(header::ORIGIN) {
        if !origin_allowed(origin.to_str().unwrap_or("?"), &relay.cfg.allow_origins) {
            return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
        }
    }
    let ip = client_ip(&relay, peer, &headers);
    if !relay.take_token(ip) {
        return (StatusCode::TOO_MANY_REQUESTS, "rate limited").into_response();
    }
    ws.max_message_size(MAX_MESSAGE).max_frame_size(MAX_MESSAGE).on_upgrade(move |socket| session(socket, relay, ip))
}

async fn send(ws: &mut WebSocket, m: &FromRelay) -> bool {
    ws.send(Message::Text(m.to_json().into())).await.is_ok()
}

async fn recv(ws: &mut WebSocket, within: Duration) -> Option<Result<ToRelay, RelayError>> {
    let until = tokio::time::Instant::now() + within;
    loop {
        let msg = tokio::time::timeout_at(until, ws.next()).await.ok()??.ok()?;
        match msg {
            Message::Text(t) => return Some(ToRelay::from_json(t.as_str()).map_err(|_| RelayError::BadMessage)),
            Message::Ping(_) | Message::Pong(_) => continue,
            _ => return None,
        }
    }
}

async fn session(mut ws: WebSocket, relay: Arc<Relay>, ip: IpAddr) {
    let nonce = random32();
    if !send(&mut ws, &FromRelay::Hello { relay: RELAY_PROTOCOL, nonce }).await {
        return;
    }
    let first = match recv(&mut ws, FIRST_MESSAGE).await {
        Some(Ok(m)) => m,
        Some(Err(e)) => {
            send(&mut ws, &FromRelay::Error { reason: e }).await;
            return;
        }
        None => return,
    };
    match first {
        ToRelay::Host { relay: v, key, sig, listing, public, code } => {
            if v != RELAY_PROTOCOL {
                send(&mut ws, &FromRelay::Error { reason: RelayError::Version }).await;
                return;
            }
            if !identity::verify_relay(&key, &nonce, &sig) {
                send(&mut ws, &FromRelay::Error { reason: RelayError::BadSignature }).await;
                return;
            }
            host_session(ws, relay, ip, key, listing, public, code).await;
        }
        ToRelay::Join { relay: v, code, sdp } => {
            let reply = if v != RELAY_PROTOCOL { FromRelay::Error { reason: RelayError::Version } } else { join(&relay, &code, sdp).await };
            send(&mut ws, &reply).await;
            let _ = ws.close().await;
        }
        _ => {
            send(&mut ws, &FromRelay::Error { reason: RelayError::BadMessage }).await;
        }
    }
}

async fn host_session(
    mut ws: WebSocket,
    relay: Arc<Relay>,
    ip: IpAddr,
    key: [u8; 32],
    listing: Listing,
    public: bool,
    want: Option<String>,
) {
    let fingerprint = identity::fingerprint(&key);
    let conn = random_u64();
    let (tx, mut rx) = mpsc::channel::<FromRelay>(64);
    let host = Host { key, fingerprint: fingerprint.clone(), listing: listing.sanitized(), public, ip, conn, tx };
    let code = match relay.register(key, want.as_deref(), host) {
        Ok(c) => c,
        Err(e) => {
            send(&mut ws, &FromRelay::Error { reason: e }).await;
            return;
        }
    };
    log::info(format!("host up {code} {fingerprint} ({ip}){}", if public { " public" } else { "" }));
    if !send(&mut ws, &FromRelay::Hosted { code: code.clone(), fingerprint: fingerprint.clone() }).await {
        relay.unregister(&code, conn);
        return;
    }
    loop {
        tokio::select! {
            out = rx.recv() => {
                // 보낼 길(tx)이 사라졌다 = 같은 키의 새 접속이 자리를 넘겨받았다.
                let Some(out) = out else { break };
                if !send(&mut ws, &out).await { break; }
            }
            got = recv(&mut ws, HOST_IDLE) => {
                match got {
                    Some(Ok(ToRelay::Update { listing, public })) => {
                        if let Some(h) = relay.lock().hosts.get_mut(&code).filter(|h| h.conn == conn) {
                            h.listing = listing.sanitized();
                            if let Some(p) = public {
                                h.public = p;
                            }
                        }
                    }
                    Some(Ok(ToRelay::Answer { session, sdp, turn })) => relay.settle(
                        &code,
                        session,
                        if sdp.len() <= MAX_SDP { Ok((sdp, turn.and_then(|t| t.sanitized()))) } else { Err(RelayError::BadMessage) },
                    ),
                    Some(Ok(ToRelay::Refuse { session })) => relay.settle(&code, session, Err(RelayError::Refused)),
                    Some(Ok(_)) | Some(Err(_)) | None => break,
                }
            }
        }
    }
    relay.unregister(&code, conn);
    log::info(format!("host down {code}"));
    let _ = ws.close().await;
}

async fn join(relay: &Relay, code: &str, sdp: String) -> FromRelay {
    if sdp.len() > MAX_SDP {
        return FromRelay::Error { reason: RelayError::BadMessage };
    }
    let Some(code) = normalize_code(code) else { return FromRelay::Error { reason: RelayError::NoSuchCode } };
    let (otx, orx) = oneshot::channel();
    let (session, fingerprint, listing) = {
        let mut g = relay.lock();
        let Some(h) = g.hosts.get(&code) else { return FromRelay::Error { reason: RelayError::NoSuchCode } };
        let (tx, fingerprint, listing) = (h.tx.clone(), h.fingerprint.clone(), h.listing.clone());
        g.next += 1;
        g.joins += 1;
        let session = g.next;
        if tx.try_send(FromRelay::Offer { session, sdp }).is_err() {
            return FromRelay::Error { reason: RelayError::Refused };
        }
        g.pending.insert(session, (code.clone(), otx));
        (session, fingerprint, listing)
    };
    match tokio::time::timeout(relay.cfg.answer_timeout, orx).await {
        Ok(Ok(Ok((sdp, turn)))) => FromRelay::Answer { sdp, fingerprint, listing: relay.present(&listing), turn },
        Ok(Ok(Err(e))) => FromRelay::Error { reason: e },
        Ok(Err(_)) => FromRelay::Error { reason: RelayError::NoSuchCode },
        Err(_) => {
            relay.lock().pending.remove(&session);
            FromRelay::Error { reason: RelayError::HostTimeout }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins() {
        let a = default_origins();
        assert!(origin_allowed("https://hyproj.com", &a));
        assert!(origin_allowed("http://localhost:5180", &a));
        assert!(origin_allowed("http://localhost", &a));
        assert!(!origin_allowed("http://localhost.evil.com", &a));
        assert!(!origin_allowed("http://localhost:5180.evil.com", &a));
        assert!(!origin_allowed("https://evil.example", &a));
        assert!(!origin_allowed("https://hyproj.com.evil.example", &a));
        assert!(origin_allowed("https://anything", &[]));
    }

    #[test]
    fn public_list_is_masked() {
        let r = Relay::new(Config::default()).unwrap();
        let l = r.present(&Listing { name: "fuck server".into(), motd: "hi".into(), ..Default::default() });
        assert_eq!(l.name, "**** server");
        assert_eq!(l.motd, "hi");
    }
}
