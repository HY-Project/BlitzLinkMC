//! `tunnel join --code MCJE-XXXX` — 참가 코드로 호스트에 붙는다.
//!
//! 1. UDP 소켓 하나에 host 후보(LAN·루프백)와 srflx 후보(STUN)를 모아 Offer(채널 `meta`)를 만들고 중계로 `join` → 답(SDP·지문·
//!    listing·TURN 자격).
//! 2. 그 연결이 `TURN_AFTER`(2.5초) 안에 열리지 않거나 실패하면, 답에 실려 온 **호스트 내장 TURN**만 쓰는 두 번째 연결을 함께
//!    시작한다(TURN TCP 접속 → Allocate → relayed 후보 하나 → 새 Offer로 다시 `join`). 먼저 열리는 쪽을 쓰고 나머지는 닫는다.
//!    전체 14초 안에 못 열면 `rtcFailed`. `--turn-only`(개발)는 첫 연결을 곧바로 버리고 TURN만 쓴다.
//! 3. 열리면 `meta`로 `probe` → 호스트의 서명을 확인하고 그 지문이 중계가 건넨 것과 같은지 본다(다르면 `badIdentity`).
//!    모드에 `meta{info…}`를 알리고 `127.0.0.1:0`에서 듣는다(`listen{port}`). 로컬 TCP 접속마다 `mc-N` 채널을 연다(`tunnel.rs`).

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use blitzlink_core::relay::{normalize_code, FromRelay, Listing, ToRelay, TurnInfo, RELAY_PROTOCOL};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use str0m::change::{SdpAnswer, SdpPendingOffer};
use str0m::channel::{ChannelConfig, ChannelId};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::control::{self, emit, log, Control};
use crate::meta::{verify_info, MetaMsg};
use crate::tunnel::{self, Pause, TcpMsg, Tun, Tunnels};
use crate::{ice, turn_client};

pub struct JoinOptions {
    pub code: String,
    pub relay: String,
    pub stun: Vec<String>,
    pub turn_only: bool,
}

/// 첫 연결이 이만큼 열리지 않으면 내장 TURN 연결을 같이 시작한다(첫 연결은 그대로 기다린다).
const TURN_AFTER: Duration = Duration::from_millis(2500);
const TOTAL: Duration = Duration::from_secs(14);
const PROBE_TIMEOUT: Duration = Duration::from_secs(6);
const DISCONNECT_GRACE: Duration = Duration::from_secs(10);
const PING_EVERY: Duration = Duration::from_secs(15);

/// 패킷이 오가는 길 — UDP 소켓 하나, 또는 호스트 내장 TURN.
enum Net {
    Udp { sock: UdpSocket, port: u16, lan: Option<Ipv4Addr> },
    Turn(turn_client::TurnLink),
}

struct Datagram {
    source: SocketAddr,
    destination: SocketAddr,
    data: Vec<u8>,
}

impl Net {
    /// 다음 패킷. None이면 길이 끊겼다(TURN).
    async fn recv(&mut self, buf: &mut [u8]) -> Option<Datagram> {
        match self {
            Net::Udp { sock, port, lan } => loop {
                if let Ok((n, source)) = sock.recv_from(buf).await {
                    return Some(Datagram { source, destination: ice::destination_for(*lan, *port, source), data: buf[..n].to_vec() });
                }
            },
            Net::Turn(link) => {
                let (source, data) = link.inbound.recv().await?;
                Some(Datagram { source, destination: link.relayed, data })
            }
        }
    }

    fn send(&self, destination: SocketAddr, contents: &[u8]) {
        match self {
            Net::Udp { sock, .. } => {
                let _ = sock.try_send_to(contents, destination);
            }
            Net::Turn(link) => {
                let _ = link.out.try_send((destination, contents.to_vec()));
            }
        }
    }

    fn via(&self) -> &'static str {
        match self {
            Net::Udp { .. } => "udp",
            Net::Turn(_) => "turn",
        }
    }
}

enum Ev {
    Open(ChannelId, String),
    Data(ChannelId, Vec<u8>),
    Close(ChannelId),
}

/// 연결 하나(시도 하나) — Rtc + 길.
struct Conn {
    rtc: Rtc,
    net: Net,
    meta: Option<ChannelId>,
    disconnected: Option<Instant>,
    dead: bool,
}

impl Conn {
    /// 출력을 비운다 — 다음에 깨어날 시각과 사건들.
    fn drive(&mut self, evs: &mut Vec<Ev>) -> Instant {
        let now = Instant::now();
        let mut next = now + Duration::from_millis(100);
        if self.disconnected.is_some_and(|t| now.duration_since(t) > DISCONNECT_GRACE) {
            self.rtc.disconnect();
        }
        loop {
            if !self.rtc.is_alive() {
                self.dead = true;
                return next;
            }
            match self.rtc.poll_output() {
                Ok(Output::Timeout(t)) => return next.min(t),
                Ok(Output::Transmit(t)) => self.net.send(t.destination, &t.contents),
                Ok(Output::Event(e)) => match e {
                    Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                        self.disconnected.get_or_insert(now);
                    }
                    Event::IceConnectionStateChange(s) if s.is_connected() => self.disconnected = None,
                    Event::ChannelOpen(id, label) => {
                        if label == "meta" {
                            self.meta = Some(id);
                        }
                        evs.push(Ev::Open(id, label));
                    }
                    Event::ChannelData(d) => evs.push(Ev::Data(d.id, d.data)),
                    Event::ChannelClose(id) => {
                        if Some(id) == self.meta {
                            self.rtc.disconnect();
                        }
                        evs.push(Ev::Close(id));
                    }
                    _ => {}
                },
                Err(_) => self.rtc.disconnect(),
            }
            next = next.min(now + Duration::from_millis(100));
        }
    }

    fn input(&mut self, d: Datagram) {
        let Ok(contents) = d.data.as_slice().try_into() else { return };
        let input = Input::Receive(Instant::now(), Receive { proto: Protocol::Udp, source: d.source, destination: d.destination, contents });
        if self.rtc.handle_input(input).is_err() {
            self.rtc.disconnect();
        }
    }

    fn tick(&mut self) {
        if self.rtc.is_alive() {
            let _ = self.rtc.handle_input(Input::Timeout(Instant::now()));
        }
    }

    /// `meta`가 열릴 때까지(또는 죽을 때까지) 돌린다.
    async fn open(mut self) -> Result<Conn, String> {
        let mut buf = vec![0u8; 2048];
        let mut evs = Vec::new();
        loop {
            let next = self.drive(&mut evs);
            evs.clear();
            if self.meta.is_some() {
                return Ok(self);
            }
            if self.dead || self.disconnected.is_some() {
                return Err("rtcFailed".into());
            }
            tokio::select! {
                d = self.net.recv(&mut buf) => match d {
                    Some(d) => self.input(d),
                    None => return Err("turn closed".into()),
                },
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(next)) => {}
            }
            self.tick();
        }
    }

    fn write_meta(&mut self, m: &MetaMsg) -> bool {
        let Some(id) = self.meta else { return false };
        let Some(mut ch) = self.rtc.channel(id) else { return false };
        ch.write(false, m.to_json().as_bytes()).unwrap_or(false)
    }
}

/// 중계의 답.
struct Answer {
    sdp: String,
    fingerprint: String,
    listing: Listing,
    turn: Option<TurnInfo>,
}

/// 중계에 Offer를 건네고 호스트의 Answer를 받는다(중계 소켓은 그 뒤 닫는다). 실패 까닭은 모드 문구 열쇠.
async fn exchange(relay: &str, code: &str, sdp: String) -> Result<Answer, String> {
    let url = blitzlink_core::net::ws_url(relay);
    let (mut ws, _) = tokio::time::timeout(Duration::from_secs(6), tokio_tungstenite::connect_async(&url))
        .await
        .map_err(|_| "relayUnreachable".to_string())?
        .map_err(|e| {
            log(format!("relay {url}: {e}"));
            "relayUnreachable".to_string()
        })?;
    let r = async {
        loop {
            let m = tokio::time::timeout(Duration::from_secs(12), ws.next()).await.map_err(|_| "hostTimeout".to_string())?;
            let text = match m {
                Some(Ok(Message::Text(t))) => t,
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                _ => return Err("relayUnreachable".to_string()),
            };
            match FromRelay::from_json(text.as_str()).map_err(|_| "badPacket".to_string())? {
                FromRelay::Hello { relay, .. } => {
                    if relay != RELAY_PROTOCOL {
                        return Err("version".into());
                    }
                    let j = ToRelay::Join { relay: RELAY_PROTOCOL, code: code.to_string(), sdp: sdp.clone() };
                    ws.send(Message::Text(j.to_json().into())).await.map_err(|_| "relayUnreachable".to_string())?;
                }
                FromRelay::Answer { sdp, fingerprint, listing, turn } => return Ok(Answer { sdp, fingerprint, listing, turn }),
                FromRelay::Error { reason } => return Err(reason.key().to_string()),
                _ => {}
            }
        }
    }
    .await;
    let _ = ws.close(None).await;
    r
}

fn offer_with_meta(rtc: &mut Rtc) -> Result<(String, SdpPendingOffer), String> {
    let mut api = rtc.sdp_api();
    api.add_channel_with_config(ChannelConfig { label: "meta".into(), ..Default::default() });
    let (offer, pending) = api.apply().ok_or("offer")?;
    Ok((offer.to_sdp_string(), pending))
}

fn accept_answer(rtc: &mut Rtc, pending: SdpPendingOffer, sdp: &str) -> Result<(), String> {
    let a = SdpAnswer::from_sdp_string(sdp).map_err(|_| "badPacket".to_string())?;
    rtc.sdp_api().accept_answer(pending, a).map_err(|_| "badPacket".to_string())
}

/// 첫 시도 — UDP 소켓에 host·srflx 후보.
async fn udp_conn(stun: &[String]) -> Result<(Conn, String, SdpPendingOffer), String> {
    let sock = UdpSocket::bind("0.0.0.0:0").await.map_err(|e| format!("udp: {e}"))?;
    let port = sock.local_addr().map_err(|e| e.to_string())?.port();
    let lan = ice::primary_ipv4();
    let servers = ice::resolve_stun(stun).await;
    let public = ice::stun_query(&sock, &servers, Duration::from_millis(600)).await;
    if let Some(p) = public {
        log(format!("STUN: public address udp {p}"));
    }
    let mut rtc = Rtc::builder().build(Instant::now());
    ice::add_candidates(&mut rtc, lan, port, public);
    let (offer, pending) = offer_with_meta(&mut rtc)?;
    Ok((Conn { rtc, net: Net::Udp { sock, port, lan }, meta: None, disconnected: None, dead: false }, offer, pending))
}

/// 두 번째 시도 — 호스트 내장 TURN의 relayed 후보 하나만(iceTransportPolicy: relay).
async fn turn_attempt(relay: String, code: String, turn: TurnInfo) -> Result<Conn, String> {
    let link = turn_client::connect(&turn).await.map_err(|e| {
        log(format!("TURN: {e}"));
        "rtcFailed".to_string()
    })?;
    log(format!("TURN: connected to host TURN at {}", link.server));
    let mut rtc = Rtc::builder().build(Instant::now());
    let c = Candidate::relayed(link.relayed, link.local, "udp").map_err(|e| format!("relayed: {e}"))?;
    rtc.add_local_candidate(c);
    let (offer, pending) = offer_with_meta(&mut rtc)?;
    let ans = exchange(&relay, &code, offer).await?;
    accept_answer(&mut rtc, pending, &ans.sdp)?;
    Conn { rtc, net: Net::Turn(link), meta: None, disconnected: None, dead: false }.open().await
}

pub async fn run(opt: JoinOptions) -> Result<(), String> {
    str0m::crypto::from_feature_flags().install_process_default();
    let Some(code) = normalize_code(&opt.code) else { return Err("noSuchCode".into()) };
    emit(json!({ "t": "status", "state": "relay", "code": code }));
    let (mut first, offer, pending) = udp_conn(&opt.stun).await?;
    let ans = exchange(&opt.relay, &code, offer).await?;
    accept_answer(&mut first.rtc, pending, &ans.sdp)?;
    let Answer { fingerprint, listing, turn, .. } = ans;
    emit(json!({ "t": "status", "state": "connecting" }));

    // 경쟁 — 먼저 열리는 쪽.
    let (tx, mut rx) = mpsc::channel::<Result<Conn, String>>(2);
    let mut race = Race { tasks: Vec::new(), pending: 0, turn_started: false, turn, tx, relay: opt.relay.clone(), code: code.clone() };
    if opt.turn_only {
        drop(first);
        if !race.start_turn() {
            return Err("rtcFailed".into());
        }
    } else {
        race.pending += 1;
        let tx = race.tx.clone();
        race.tasks.push(tokio::spawn(async move {
            let _ = tx.send(first.open().await).await;
        }));
    }
    let turn_at = tokio::time::Instant::now() + TURN_AFTER;
    let deadline = tokio::time::Instant::now() + TOTAL;
    let winner = loop {
        tokio::select! {
            Some(r) = rx.recv() => match r {
                Ok(c) => break c,
                Err(e) => {
                    log(format!("connection attempt failed ({e})"));
                    race.pending -= 1;
                    if !race.start_turn() && race.pending <= 0 {
                        return Err("rtcFailed".into());
                    }
                }
            },
            _ = tokio::time::sleep_until(turn_at), if !race.turn_started && race.turn.is_some() => {
                race.start_turn();
            }
            _ = tokio::time::sleep_until(deadline) => return Err("rtcFailed".into()),
        }
    };
    let tasks = race.tasks;
    for t in &tasks {
        t.abort();
    }
    drop(rx);
    Session::new(winner, fingerprint, listing).run().await
}

/// UDP 시도와 TURN 시도의 경쟁.
struct Race {
    tasks: Vec<tokio::task::JoinHandle<()>>,
    pending: i32,
    turn_started: bool,
    turn: Option<TurnInfo>,
    tx: mpsc::Sender<Result<Conn, String>>,
    relay: String,
    code: String,
}

impl Race {
    /// 호스트가 답에 실어 준 TURN만 쓰는 두 번째 연결 — 한 번만.
    fn start_turn(&mut self) -> bool {
        let Some(t) = self.turn.clone() else { return false };
        if self.turn_started {
            return false;
        }
        self.turn_started = true;
        self.pending += 1;
        emit(json!({ "t": "status", "state": "turn" }));
        let (tx, relay, code) = (self.tx.clone(), self.relay.clone(), self.code.clone());
        self.tasks.push(tokio::spawn(async move {
            let _ = tx.send(turn_attempt(relay, code, t).await).await;
        }));
        true
    }
}

struct Session {
    conn: Conn,
    fingerprint: String,
    listing: Listing,
    tuns: Tunnels,
    next_tun: u64,
    nonce: [u8; 32],
    probe_at: Instant,
    verified: bool,
    ping_n: u64,
    ping_at: Option<Instant>,
}

impl Session {
    fn new(conn: Conn, fingerprint: String, listing: Listing) -> Session {
        Session {
            conn,
            fingerprint,
            listing,
            tuns: Tunnels::default(),
            next_tun: 0,
            nonce: blitzlink_core::random32(),
            probe_at: Instant::now(),
            verified: false,
            ping_n: 0,
            ping_at: None,
        }
    }

    async fn run(mut self) -> Result<(), String> {
        let via = self.conn.net.via();
        log(format!("connected via {via}"));
        emit(json!({ "t": "status", "state": "open", "via": via }));
        self.probe_at = Instant::now();
        let probe = MetaMsg::Probe { nonce: self.nonce };
        if !self.conn.write_meta(&probe) {
            return Err("rtcFailed".into());
        }
        let mut listener: Option<TcpListener> = None;
        let (tcp_tx, mut tcp_rx) = mpsc::channel::<TcpMsg>(1024);
        let mut ctl = control::spawn_stdin();
        let mut buf = vec![0u8; 2048];
        let mut evs = Vec::new();
        let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + PING_EVERY, PING_EVERY);
        loop {
            let next = self.conn.drive(&mut evs);
            for ev in evs.drain(..) {
                if self.on_event(ev)?.is_some() {
                    // 확인이 끝났다 — 로컬 포트를 연다.
                    if listener.is_none() {
                        let lst = TcpListener::bind("127.0.0.1:0").await.map_err(|e| format!("listen: {e}"))?;
                        let port = lst.local_addr().map_err(|e| e.to_string())?.port();
                        emit(json!({ "t": "listen", "port": port }));
                        listener = Some(lst);
                    }
                }
            }
            self.tuns.flush(&mut self.conn.rtc);
            self.tuns.closed.clear();
            if self.conn.dead {
                self.tuns.close_all();
                if !self.verified {
                    return Err("rtcFailed".into());
                }
                // 열린 뒤 끊겼다 — 오류가 아니라 세션의 끝.
                log("connection closed");
                emit(json!({ "t": "closed" }));
                return Ok(());
            }
            if !self.verified && self.probe_at.elapsed() > PROBE_TIMEOUT {
                return Err("timeout".into());
            }
            let accept = async {
                match &listener {
                    Some(l) => l.accept().await.ok(),
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                d = self.conn.net.recv(&mut buf) => match d {
                    Some(d) => self.conn.input(d),
                    None => self.conn.rtc.disconnect(),
                },
                Some((stream, _)) = accept => {
                    self.next_tun += 1;
                    let label = format!("mc-{}", self.next_tun);
                    let id = self.conn.rtc.direct_api().create_data_channel(ChannelConfig { label, ..Default::default() });
                    let pause = std::sync::Arc::new(Pause::default());
                    let mut t = Tun::new(pause.clone());
                    let local_port = stream.peer_addr().map(|a| a.port()).unwrap_or(0);
                    let (w, r) = tunnel::spawn_pipes(stream, 0, id, tcp_tx.clone(), pause);
                    t.attach(w, r, local_port);
                    self.tuns.map.insert(id, t);
                }
                Some((_, id, ev)) = tcp_rx.recv() => { self.tuns.on_tcp(&mut self.conn.rtc, id, ev); }
                c = ctl.recv() => match c {
                    Some(Control::Quit) | None => {
                        self.tuns.close_all();
                        // 닫는다고 알리는 패킷(SCTP·DTLS)을 내보낸 뒤 끝낸다 — 호스트가 곧바로 세션을 정리한다.
                        let _ = self.conn.rtc.close();
                        let mut evs = Vec::new();
                        self.conn.drive(&mut evs);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        return Ok(());
                    }
                    Some(_) => {}
                },
                _ = ping.tick() => {
                    if self.verified {
                        self.ping_n += 1;
                        self.ping_at = Some(Instant::now());
                        self.conn.write_meta(&MetaMsg::Ping { n: self.ping_n });
                    }
                }
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(next)) => {}
            }
            self.conn.tick();
        }
    }

    /// 사건 하나. `Some(())` = 신원 확인이 방금 끝났다.
    fn on_event(&mut self, ev: Ev) -> Result<Option<()>, String> {
        match ev {
            Ev::Open(id, label) => {
                if let Some(t) = self.tuns.map.get_mut(&id) {
                    t.open = true;
                    if let Some(mut ch) = self.conn.rtc.channel(id) {
                        ch.set_buffered_amount_low_threshold(tunnel::LOW_WATER);
                    }
                } else if label != "meta" {
                    self.conn.rtc.direct_api().close_data_channel(id);
                }
            }
            Ev::Data(id, data) => {
                if Some(id) == self.conn.meta {
                    return self.on_meta(&data);
                }
                if let Some(t) = self.tuns.map.get_mut(&id) {
                    t.push_inbound(data);
                }
            }
            Ev::Close(id) => self.tuns.remote_closed(id),
        }
        Ok(None)
    }

    fn on_meta(&mut self, data: &[u8]) -> Result<Option<()>, String> {
        match MetaMsg::parse(data) {
            Some(MetaMsg::Info { key, sig, info }) => {
                if !self.verified {
                    let fp = verify_info(key.as_deref(), sig.as_deref(), &self.nonce).ok_or("badIdentity")?;
                    // 중계가 서명으로 확인해 건넨 지문과 호스트가 지금 증명한 지문이 다르면 중간에 누가 있다.
                    if fp != self.fingerprint {
                        return Err("badIdentity".into());
                    }
                    self.verified = true;
                    let rtt = self.probe_at.elapsed().as_millis() as u64;
                    self.emit_meta(info, Some(rtt));
                    return Ok(Some(()));
                }
                self.emit_meta(info, None);
            }
            Some(MetaMsg::Pong { n }) if n == self.ping_n => {
                if let Some(at) = self.ping_at.take() {
                    emit(json!({ "t": "status", "state": "open", "via": self.conn.net.via(), "rttMs": at.elapsed().as_millis() as u64 }));
                }
            }
            _ => {}
        }
        Ok(None)
    }

    fn emit_meta(&self, info: Value, rtt: Option<u64>) {
        emit(json!({
            "t": "meta",
            "info": info,
            "fingerprint": self.fingerprint,
            "listing": self.listing,
            "via": self.conn.net.via(),
            "rttMs": rtt,
        }));
    }
}
