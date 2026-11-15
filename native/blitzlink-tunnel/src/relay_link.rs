//! 공식 중계에 바깥으로 붙어 있기 — 참가 코드를 받고, 공개 목록의 인원을 고치고, 들어오려는 클라의 Offer를 Hub에 넘겨 Answer를
//! 돌려준다. 끊기면 2초부터 60초까지 늘려 가며 다시 붙는다. 20초마다 ping, 5초마다 바뀐 것만 `update`. 받은 코드는 파일에 적어
//! 두었다가 다음에 같은 것을 청한다(중계도 키마다 하루 동안 그 코드를 남겨 둔다).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use blitzlink_core::identity::Identity;
use blitzlink_core::relay::{FromRelay, Listing, RelayError, ToRelay, TurnInfo, RELAY_PROTOCOL};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;

use crate::control::{emit, log};

/// 중계가 건넨 Offer 하나 — 답(SDP와 내장 TURN) 또는 거절 까닭.
pub struct OfferReq {
    pub sdp: String,
    pub reply: oneshot::Sender<Result<(String, Option<TurnInfo>), String>>,
}

pub struct RelayOptions {
    /// `wss://hyproj.com/relay/mcje/ws` 꼴.
    pub url: String,
    pub code_file: PathBuf,
}

/// 중계 쪽 상태(모드에 `status.relay`로 알린다).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RelayState {
    /// `connecting` · `online` · `retrying` · `failed`.
    pub state: &'static str,
    pub code: Option<String>,
    pub reason: Option<String>,
}

pub async fn run(
    opt: RelayOptions,
    identity: Identity,
    mut listing: watch::Receiver<Option<Listing>>,
    public: watch::Receiver<bool>,
    offers: mpsc::Sender<OfferReq>,
    state: Arc<Mutex<RelayState>>,
) {
    // 모드가 첫 `listing`을 줄 때까지(5초 넘으면 기본값으로 올린다 — 나중 `listing`이 `update`로 고친다).
    let _ = tokio::time::timeout(Duration::from_secs(5), listing.wait_for(|l| l.is_some())).await;
    let set = |s: &'static str, reason: Option<String>| {
        let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
        g.state = s;
        g.reason = reason;
    };
    let mut backoff = Duration::from_secs(2);
    loop {
        set("connecting", None);
        let mut was_online = false;
        match session(&opt, &identity, &mut listing, &public, &offers, &state, &mut was_online).await {
            Ok(()) => return,
            Err(Fatal(e)) => {
                log(format!("relay: {e}, giving up"));
                crate::control::error(&e);
                set("failed", Some(e));
                return;
            }
            Err(Retry(e)) => {
                log(format!("relay: {e}, retrying in {}s", backoff.as_secs()));
                set("retrying", Some(e));
                if was_online {
                    backoff = Duration::from_secs(2);
                }
            }
        }
        if offers.is_closed() {
            return;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
        // 다시 붙기 전에도 Hub가 사라졌으면 끝.
    }
}

enum Failure {
    Retry(String),
    Fatal(String),
}
use Failure::{Fatal, Retry};

fn current(listing: &watch::Receiver<Option<Listing>>) -> Listing {
    listing.borrow().clone().unwrap_or_else(|| Listing { name: "Minecraft Server".into(), ..Default::default() })
}

async fn session(
    opt: &RelayOptions,
    identity: &Identity,
    listing: &mut watch::Receiver<Option<Listing>>,
    public: &watch::Receiver<bool>,
    offers: &mpsc::Sender<OfferReq>,
    state: &Arc<Mutex<RelayState>>,
    was_online: &mut bool,
) -> Result<(), Failure> {
    let (ws, _) = tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(&opt.url))
        .await
        .map_err(|_| Retry("cannot connect (timeout)".into()))?
        .map_err(|e| Retry(format!("cannot connect ({e})")))?;
    let (mut sink, mut source) = ws.split();
    let mut next_text = async || -> Result<FromRelay, Failure> {
        loop {
            match tokio::time::timeout(Duration::from_secs(10), source.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => return FromRelay::from_json(t.as_str()).map_err(|e| Retry(format!("unreadable message ({e})"))),
                Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
                Ok(_) => return Err(Retry("closed".into())),
                Err(_) => return Err(Retry("no answer".into())),
            }
        }
    };
    let FromRelay::Hello { relay, nonce } = next_text().await? else { return Err(Retry("no hello".into())) };
    if relay != RELAY_PROTOCOL {
        return Err(Fatal("version".into()));
    }
    let mut sent = current(listing);
    let mut sent_public = *public.borrow();
    let want = std::fs::read_to_string(&opt.code_file).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let host = ToRelay::Host {
        relay: RELAY_PROTOCOL,
        key: identity.public(),
        sig: identity.relay_sig(&nonce),
        listing: sent.clone(),
        public: sent_public,
        code: want,
    };
    sink.send(Message::Text(host.to_json().into())).await.map_err(|e| Retry(e.to_string()))?;
    let (code, fingerprint) = match next_text().await? {
        FromRelay::Hosted { code, fingerprint } => (code, fingerprint),
        FromRelay::Error { reason: r @ (RelayError::BadSignature | RelayError::Version) } => return Err(Fatal(r.key().into())),
        FromRelay::Error { reason } => return Err(Retry(reason.key().into())),
        _ => return Err(Retry("unexpected reply".into())),
    };
    if let Some(dir) = opt.code_file.parent().filter(|d| !d.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&opt.code_file, format!("{code}\n"));
    log(format!("relay: join code {code}{}", if sent_public { " (public)" } else { "" }));
    emit(serde_json::json!({ "t": "hosted", "code": code, "fingerprint": fingerprint }));
    {
        let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
        g.state = "online";
        g.code = Some(code.clone());
        g.reason = None;
    }
    *was_online = true;

    let (answers_tx, mut answers) = mpsc::channel::<ToRelay>(32);
    let mut refresh = tokio::time::interval(Duration::from_secs(5));
    let mut ping = tokio::time::interval(Duration::from_secs(20));
    loop {
        tokio::select! {
            msg = source.next() => {
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    _ => return Err(Retry("connection to relay lost".into())),
                };
                match FromRelay::from_json(text.as_str()) {
                    Ok(FromRelay::Offer { session, sdp }) => {
                        let (reply, rx) = oneshot::channel();
                        let tx = answers_tx.clone();
                        if offers.send(OfferReq { sdp, reply }).await.is_err() {
                            return Ok(());
                        }
                        tokio::spawn(async move {
                            let msg = match rx.await {
                                Ok(Ok((sdp, turn))) => ToRelay::Answer { session, sdp, turn },
                                Ok(Err(e)) => {
                                    log(format!("refused a WebRTC offer ({e})"));
                                    ToRelay::Refuse { session }
                                }
                                Err(_) => ToRelay::Refuse { session },
                            };
                            let _ = tx.send(msg).await;
                        });
                    }
                    Ok(_) => {}
                    Err(e) => log(format!("relay: unreadable message ({e})")),
                }
            }
            Some(out) = answers.recv() => {
                sink.send(Message::Text(out.to_json().into())).await.map_err(|e| Retry(e.to_string()))?;
            }
            _ = refresh.tick() => {
                if offers.is_closed() {
                    let _ = sink.close().await;
                    return Ok(());
                }
                let l = current(listing);
                let p = *public.borrow();
                if l != sent || p != sent_public {
                    let public_change = (p != sent_public).then_some(p);
                    sent = l.clone();
                    sent_public = p;
                    sink.send(Message::Text(ToRelay::Update { listing: l, public: public_change }.to_json().into())).await.map_err(|e| Retry(e.to_string()))?;
                }
            }
            _ = ping.tick() => {
                sink.send(Message::Ping(Vec::new().into())).await.map_err(|e| Retry(e.to_string()))?;
            }
        }
    }
}
