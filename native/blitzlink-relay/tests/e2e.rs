//! 끝에서 끝까지(호스트 자리에 str0m 피어): 중계를 띄우고, 호스트 피어가 서명해 올라와 참가 코드를 받고, 클라 피어가 그 코드로 SDP를 주고받은 뒤
//! (답에 TURN이 다듬어져 실려 온다) **DataChannel이 직접** 열려 글이 오가는지 본다. 공개 목록·코드 예약·서명·STUN·Origin도.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use blitzlink_core::identity::Identity;
use blitzlink_core::relay::{normalize_code, FromRelay, Listing, RelayError, ToRelay, TurnInfo, RELAY_PROTOCOL};
use blitzlink_relay::Config as RelayConfig;
use futures_util::{SinkExt, StreamExt};
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::channel::{ChannelConfig, ChannelId};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc};
use tokio::net::{TcpStream, UdpSocket};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn relay() -> blitzlink_relay::RelayHandle {
    blitzlink_relay::start(RelayConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        stun: Some("127.0.0.1:0".parse().unwrap()),
        ..RelayConfig::default()
    })
    .await
    .unwrap()
}

async fn next_msg(ws: &mut Ws) -> FromRelay {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(10), ws.next()).await.expect("중계 답 시간 초과").unwrap().unwrap();
        if let Message::Text(t) = m {
            return FromRelay::from_json(t.as_str()).unwrap();
        }
    }
}

async fn send(ws: &mut Ws, m: &ToRelay) {
    ws.send(Message::Text(m.to_json().into())).await.unwrap();
}

/// 호스트로 올라간다 — (소켓, 코드, 지문) 또는 중계의 거절.
async fn host(r: &blitzlink_relay::RelayHandle, id: &Identity, listing: Listing, public: bool, want: Option<String>) -> Result<(Ws, String, String), RelayError> {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws", r.http)).await.unwrap();
    let FromRelay::Hello { relay, nonce } = next_msg(&mut ws).await else { panic!("hello") };
    assert_eq!(relay, RELAY_PROTOCOL);
    send(&mut ws, &ToRelay::Host { relay: RELAY_PROTOCOL, key: id.public(), sig: id.relay_sig(&nonce), listing, public, code: want }).await;
    match next_msg(&mut ws).await {
        FromRelay::Hosted { code, fingerprint } => Ok((ws, code, fingerprint)),
        FromRelay::Error { reason } => Err(reason),
        other => panic!("{other:?}"),
    }
}

/// 중계에 Join을 보내고 답을 받는다.
async fn join(http: SocketAddr, code: String, sdp: String) -> FromRelay {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{http}/ws")).await.unwrap();
    assert!(matches!(next_msg(&mut ws).await, FromRelay::Hello { .. }));
    send(&mut ws, &ToRelay::Join { relay: RELAY_PROTOCOL, code, sdp }).await;
    next_msg(&mut ws).await
}

/// str0m 피어 하나(Full ICE, 루프백 host 후보 하나).
struct Peer {
    rtc: Rtc,
    sock: UdpSocket,
    local: SocketAddr,
    meta: Option<ChannelId>,
    inbox: Vec<Vec<u8>>,
}

impl Peer {
    async fn new() -> Peer {
        str0m::crypto::from_feature_flags().install_process_default();
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let local = sock.local_addr().unwrap();
        let mut rtc = Rtc::builder().build(Instant::now());
        rtc.add_local_candidate(Candidate::host(local, "udp").unwrap());
        Peer { rtc, sock, local, meta: None, inbox: Vec::new() }
    }

    /// 출력을 비우고 소켓을 잠깐 읽는다.
    async fn pump(&mut self) {
        let mut wait = Instant::now() + Duration::from_millis(5);
        loop {
            match self.rtc.poll_output().unwrap() {
                Output::Timeout(t) => {
                    wait = wait.min(t);
                    break;
                }
                Output::Transmit(t) => {
                    self.sock.send_to(&t.contents, t.destination).await.unwrap();
                }
                Output::Event(Event::ChannelOpen(id, label)) if label == "meta" => self.meta = Some(id),
                Output::Event(Event::ChannelData(d)) => self.inbox.push(d.data),
                Output::Event(_) => {}
            }
        }
        let mut buf = [0u8; 2048];
        let dur = wait.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
        if let Ok(Ok((n, source))) = tokio::time::timeout(dur, self.sock.recv_from(&mut buf)).await {
            let input =
                Input::Receive(Instant::now(), Receive { proto: Protocol::Udp, source, destination: self.local, contents: buf[..n].try_into().unwrap() });
            self.rtc.handle_input(input).unwrap();
        }
        self.rtc.handle_input(Input::Timeout(Instant::now())).unwrap();
    }
}

#[tokio::test]
async fn joiner_reaches_host_by_code_and_opens_a_channel() {
    let r = relay().await;
    let id = Identity::ephemeral();
    let listing = Listing { name: "중계 시험 서버".into(), mc_version: "26.3".into(), loader: "neoforge".into(), max_players: 20, ..Default::default() };
    let (mut hws, code, fp) = host(&r, &id, listing, true, None).await.unwrap();
    assert!(code.starts_with("MCJE-"));
    assert_eq!(fp, id.fingerprint());

    // 공개 목록에 있다.
    let list = r.relay.list();
    let row = list.servers.iter().find(|x| x.code == code).expect("공개 목록");
    assert_eq!((row.fingerprint.as_str(), row.listing.name.as_str(), row.listing.loader.as_str()), (fp.as_str(), "중계 시험 서버", "neoforge"));

    // 클라: Offer를 만들어 (소문자·머리 없이 적은) 코드로 보낸다.
    let mut c = Peer::new().await;
    let mut api = c.rtc.sdp_api();
    api.add_channel_with_config(ChannelConfig { label: "meta".into(), ..Default::default() });
    let (offer, pending) = api.apply().unwrap();
    let typed = code.to_lowercase().replace("mcje-", "");
    let joining = tokio::spawn(join(r.http, typed, offer.to_sdp_string()));

    // 호스트: Offer를 받아 답한다(내장 TURN 자격 + 중계가 걸러 낼 엉뚱한 주소).
    let mut h = Peer::new().await;
    let FromRelay::Offer { session, sdp } = next_msg(&mut hws).await else { panic!("offer") };
    let answer = h.rtc.sdp_api().accept_offer(SdpOffer::from_sdp_string(&sdp).unwrap()).unwrap();
    let turn = TurnInfo { urls: vec!["turn:127.0.0.1:25565?transport=tcp".into(), "http://evil".into()], username: "9999999999:ab".into(), credential: "pw".into() };
    send(&mut hws, &ToRelay::Answer { session, sdp: answer.to_sdp_string(), turn: Some(turn) }).await;

    let (sdp, fingerprint, turn, got) = match joining.await.unwrap() {
        FromRelay::Answer { sdp, fingerprint, turn, listing } => (sdp, fingerprint, turn, listing),
        other => panic!("{other:?}"),
    };
    assert_eq!(fingerprint, fp, "중계가 서명으로 확인한 지문");
    assert_eq!(got.mc_version, "26.3");
    assert_eq!(turn.unwrap().urls, vec!["turn:127.0.0.1:25565?transport=tcp".to_string()], "turn: 주소만 남는다");
    c.rtc.sdp_api().accept_answer(pending, SdpAnswer::from_sdp_string(&sdp).unwrap()).unwrap();

    // 그 뒤는 중계 없이 — 양쪽 Full ICE, DataChannel `meta`가 열리고 글이 오간다.
    let until = Instant::now() + Duration::from_secs(10);
    while c.meta.is_none() || h.meta.is_none() {
        assert!(Instant::now() < until, "DataChannel이 열리지 않음");
        c.pump().await;
        h.pump().await;
    }
    assert!(c.rtc.channel(c.meta.unwrap()).unwrap().write(false, b"{\"t\":\"probe\"}").unwrap());
    while h.inbox.is_empty() {
        assert!(Instant::now() < until, "글이 오지 않음");
        c.pump().await;
        h.pump().await;
    }
    assert_eq!(h.inbox[0], b"{\"t\":\"probe\"}");

    // 공개를 끄면 목록에서 빠진다(Update.public).
    send(&mut hws, &ToRelay::Update { listing: Listing { name: "x".into(), ..Default::default() }, public: Some(false) }).await;
    let until = Instant::now() + Duration::from_secs(3);
    while r.relay.list().servers.iter().any(|x| x.code == code) {
        assert!(Instant::now() < until, "공개가 꺼지지 않음");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // 없는 코드는 바로 거절, 코드가 아닌 것도.
    assert_eq!(join(r.http, "MCJE-2222".into(), "v=0".into()).await, FromRelay::Error { reason: RelayError::NoSuchCode });
    assert_eq!(join(r.http, "ABCD-2222".into(), "v=0".into()).await, FromRelay::Error { reason: RelayError::NoSuchCode });
}

#[tokio::test]
async fn host_refusal_and_timeout_reach_the_joiner() {
    let r = blitzlink_relay::start(RelayConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        stun: None,
        answer_timeout: Duration::from_millis(500),
        ..RelayConfig::default()
    })
    .await
    .unwrap();
    let (mut hws, code, _) = host(&r, &Identity::ephemeral(), Listing::default(), false, None).await.unwrap();
    let joining = tokio::spawn(join(r.http, code.clone(), "v=0".into()));
    let FromRelay::Offer { session, .. } = next_msg(&mut hws).await else { panic!("offer") };
    send(&mut hws, &ToRelay::Refuse { session }).await;
    assert_eq!(joining.await.unwrap(), FromRelay::Error { reason: RelayError::Refused });
    // 답하지 않으면 hostTimeout.
    let joining = tokio::spawn(join(r.http, code, "v=0".into()));
    let _ = next_msg(&mut hws).await;
    assert_eq!(joining.await.unwrap(), FromRelay::Error { reason: RelayError::HostTimeout });
}

/// 목록이 그대로인 호스트는 ping만 보낸다 — 그것도 살아 있다는 표시다(26.3.0까지는 75초마다 끊었다). 아무것도 보내지 않으면 끊는다.
#[tokio::test]
async fn pings_keep_a_quiet_host_registered() {
    let r = blitzlink_relay::start(RelayConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        stun: None,
        answer_timeout: Duration::from_millis(500),
        host_idle: Duration::from_millis(800),
        ..RelayConfig::default()
    })
    .await
    .unwrap();
    let (mut hws, code, _) = host(&r, &Identity::ephemeral(), Listing::default(), false, None).await.unwrap();
    // 3초(기한의 네 배 가까이) 동안 ping만.
    for _ in 0..10 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        hws.send(Message::Ping(Vec::new().into())).await.unwrap();
    }
    let joining = tokio::spawn(join(r.http, code.clone(), "v=0".into()));
    assert!(matches!(next_msg(&mut hws).await, FromRelay::Offer { .. }), "ping만 보낸 호스트도 살아 있다");
    assert_eq!(joining.await.unwrap(), FromRelay::Error { reason: RelayError::HostTimeout });
    // 이제 아무것도 보내지 않는다 — 기한이 지나면 내려간다.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(join(r.http, code, "v=0".into()).await, FromRelay::Error { reason: RelayError::NoSuchCode });
}

#[tokio::test]
async fn code_survives_reconnect_and_bad_signature_is_refused() {
    let r = relay().await;
    // 서명이 틀린 Host는 거절된다.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws", r.http)).await.unwrap();
    let _hello = next_msg(&mut ws).await;
    send(&mut ws, &ToRelay::Host { relay: RELAY_PROTOCOL, key: Identity::ephemeral().public(), sig: vec![0; 64], listing: Listing::default(), public: true, code: None }).await;
    assert_eq!(next_msg(&mut ws).await, FromRelay::Error { reason: RelayError::BadSignature });

    // 같은 키로 다시 올라오면(코드를 청하지 않아도) 중계가 그 키 몫으로 남겨 둔 코드.
    let id = Identity::ephemeral();
    let (ws1, first, _) = host(&r, &id, Listing::default(), false, None).await.unwrap();
    assert!(r.relay.list().servers.is_empty(), "public=false면 목록에 없다");
    drop(ws1);
    let until = Instant::now() + Duration::from_secs(3);
    while r.relay.stats().0 > 0 {
        assert!(Instant::now() < until, "내려가지 않음");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (_ws2, again, _) = host(&r, &id, Listing::default(), false, None).await.unwrap();
    assert_eq!(again, first, "중계가 그 키 몫으로 남겨 둔 코드");
    // 남의 키는 그 코드를 청해도 받지 못한다.
    let (_ws3, other, _) = host(&r, &Identity::ephemeral(), Listing::default(), false, Some(first.clone())).await.unwrap();
    assert_ne!(other, first);
    assert!(normalize_code(&other).is_some());
}

#[tokio::test]
async fn stun_tells_the_mapped_address() {
    let r = relay().await;
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let tx = [3u8; 12];
    sock.send_to(&blitzlink_core::stun::binding_request(tx), r.stun.unwrap()).await.unwrap();
    let mut buf = [0u8; 256];
    let (n, _) = tokio::time::timeout(Duration::from_secs(2), sock.recv_from(&mut buf)).await.unwrap().unwrap();
    assert_eq!(blitzlink_core::stun::parse_binding_response(&buf[..n], &tx), Some(sock.local_addr().unwrap()));
}

/// 브라우저가 허용하지 않은 출처의 페이지에서 열면 중계가 받지 않는다(사이드카는 Origin이 없어 늘 통과).
#[tokio::test]
async fn foreign_origin_is_refused() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let r = relay().await;
    let mut req = format!("ws://{}/ws", r.http).into_client_request().unwrap();
    req.headers_mut().insert("Origin", "https://evil.example".parse().unwrap());
    let err = tokio_tungstenite::connect_async(req).await.expect_err("막혀야 한다");
    assert!(err.to_string().contains("403"), "{err}");
    let mut ok = format!("ws://{}/ws", r.http).into_client_request().unwrap();
    ok.headers_mut().insert("Origin", "https://hyproj.com".parse().unwrap());
    assert!(tokio_tungstenite::connect_async(ok).await.is_ok());
}
