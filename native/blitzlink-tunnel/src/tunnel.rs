//! 스트림 터널 — 로컬 TCP 접속 하나 ↔ DataChannel `mc-N` 하나(신뢰·순서). host와 join이 같이 쓴다.
//!
//! - TCP에서 읽은 바이트는 16 KB씩 잘라 채널에 쓴다. 채널에 밀린 양(`buffered_amount` + 아직 못 쓴 것)이 1 MB를 넘으면
//!   TCP 읽기를 멈추고, 256 KB 밑으로 내려가면 다시 읽는다.
//! - 채널에서 온 것은 그대로 TCP에 쓴다(쓰는 일은 따로 task — 이 루프를 막지 않는다).
//! - 닫힘 전파: TCP가 EOF면 밀린 것을 다 보낸 뒤 채널을 닫는다. 채널이 닫히면 받은 것을 다 쓴 뒤 TCP를 닫는다.
//!   TCP 쓰기가 실패하면 곧바로 채널을 닫는다.
//!
//! 이 모듈은 Rtc를 직접 갖지 않는다 — 가진 쪽(host의 `Peer`, join의 연결)이 이벤트를 넘기고 `flush`를 부른다.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use str0m::channel::ChannelId;
use str0m::Rtc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Notify};
use tokio::task::AbortHandle;

/// TCP에서 한 번에 읽어 채널 메시지 하나로 싣는 크기.
pub const CHUNK: usize = 16 * 1024;
/// 이보다 많이 밀리면 TCP 읽기를 멈춘다.
pub const HIGH_WATER: usize = 1024 * 1024;
/// 이 밑으로 내려가면 다시 읽는다(`ChannelBufferedAmountLow` 문턱).
pub const LOW_WATER: usize = 256 * 1024;

/// TCP 읽기 멈춤 스위치.
#[derive(Default)]
pub struct Pause {
    paused: AtomicBool,
    notify: Notify,
}

impl Pause {
    pub async fn wait(&self) {
        loop {
            let n = self.notify.notified();
            if !self.paused.load(Ordering::Acquire) {
                return;
            }
            n.await;
        }
    }

    pub fn set(&self, paused: bool) {
        if self.paused.swap(paused, Ordering::AcqRel) && !paused {
            self.notify.notify_waiters();
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }
}

/// TCP task가 주인에게 알리는 것.
pub enum TcpEv {
    Data(Vec<u8>),
    /// 읽기가 끝났다(상대가 닫음).
    Eof,
    /// 읽기·쓰기 실패.
    Failed,
    /// (host) `--target`에 붙었다.
    Connected { writer: mpsc::UnboundedSender<Vec<u8>>, reader: AbortHandle, local_port: u16 },
    /// (host) `--target`에 붙지 못했다.
    ConnectFailed,
}

/// (어느 피어, 어느 채널, 무엇).
pub type TcpMsg = (u64, ChannelId, TcpEv);

/// 터널 하나의 상태.
pub struct Tun {
    /// TCP에서 읽었지만 아직 채널에 못 쓴 것.
    queue: VecDeque<Vec<u8>>,
    queued: usize,
    /// TCP 쓰기 task로 — 떨어뜨리면 남은 것을 쓰고 닫는다.
    writer: Option<mpsc::UnboundedSender<Vec<u8>>>,
    /// (host) 아직 TCP가 붙기 전에 채널에서 온 것.
    early: Vec<Vec<u8>>,
    pause: Arc<Pause>,
    reader: Option<AbortHandle>,
    /// TCP가 EOF — 밀린 것을 다 보내면 채널을 닫는다.
    eof: bool,
    /// 채널이 열렸다(쓸 수 있다).
    pub open: bool,
    /// TCP 접속의 로컬 포트(host: MC 서버가 보는 상대 포트).
    pub local_port: u16,
    pub sent: u64,
    pub received: u64,
}

impl Tun {
    pub fn new(pause: Arc<Pause>) -> Tun {
        Tun {
            queue: VecDeque::new(),
            queued: 0,
            writer: None,
            early: Vec::new(),
            pause,
            reader: None,
            eof: false,
            open: false,
            local_port: 0,
            sent: 0,
            received: 0,
        }
    }

    /// TCP 쪽이 붙었다 — 그 전에 채널에서 온 것을 먼저 넘긴다.
    pub fn attach(&mut self, writer: mpsc::UnboundedSender<Vec<u8>>, reader: AbortHandle, local_port: u16) {
        for b in self.early.drain(..) {
            let _ = writer.send(b);
        }
        self.writer = Some(writer);
        self.reader = Some(reader);
        self.local_port = local_port;
    }

    /// 채널에서 온 것 → TCP.
    pub fn push_inbound(&mut self, data: Vec<u8>) {
        self.received += data.len() as u64;
        match &self.writer {
            Some(w) => {
                let _ = w.send(data);
            }
            None => self.early.push(data),
        }
    }

    /// TCP에서 온 것 → (나중에) 채널.
    pub fn push_outbound(&mut self, data: Vec<u8>) {
        self.queued += data.len();
        self.queue.push_back(data);
    }

    pub fn set_eof(&mut self) {
        self.eof = true;
    }

    /// TCP 쪽을 닫는다(쓰기 task는 남은 것을 다 쓰고 닫는다, 읽기는 멈춘다).
    pub fn shutdown_tcp(&mut self) {
        self.writer = None;
        if let Some(r) = self.reader.take() {
            r.abort();
        }
        self.pause.set(false);
    }
}

impl Drop for Tun {
    fn drop(&mut self) {
        self.shutdown_tcp();
    }
}

/// 한 Rtc의 터널들.
#[derive(Default)]
pub struct Tunnels {
    pub map: HashMap<ChannelId, Tun>,
    /// 닫힌 터널의 TCP 로컬 포트(host: `peerClosed`로 알린다). 주인이 비운다.
    pub closed: Vec<u16>,
}

impl Tunnels {
    /// 밀린 것을 채널에 쓰고, 멈춤 스위치를 맞추고, EOF 뒤 다 비운 터널의 채널을 닫는다.
    pub fn flush(&mut self, rtc: &mut Rtc) {
        let mut done = Vec::new();
        let mut broken = Vec::new();
        for (&id, t) in self.map.iter_mut() {
            if !t.open {
                if t.queued > HIGH_WATER {
                    t.pause.set(true);
                }
                continue;
            }
            let Some(mut ch) = rtc.channel(id) else { continue };
            while let Some(front) = t.queue.front() {
                match ch.write(true, front) {
                    Ok(true) => {
                        let b = t.queue.pop_front().unwrap();
                        t.queued -= b.len();
                        t.sent += b.len() as u64;
                    }
                    Ok(false) => break,
                    Err(_) => {
                        broken.push(id);
                        break;
                    }
                }
            }
            let backlog = ch.buffered_amount() + t.queued;
            if backlog > HIGH_WATER {
                t.pause.set(true);
            } else if backlog < LOW_WATER && t.pause.is_paused() {
                t.pause.set(false);
            }
            if t.eof && t.queue.is_empty() && backlog == 0 {
                done.push(id);
            }
        }
        for id in done.into_iter().chain(broken) {
            self.close(rtc, id);
        }
    }

    /// 채널을 닫고 TCP도 닫는다(이쪽에서 끝낼 때).
    pub fn close(&mut self, rtc: &mut Rtc, id: ChannelId) {
        if let Some(mut t) = self.map.remove(&id) {
            t.shutdown_tcp();
            if t.local_port != 0 {
                self.closed.push(t.local_port);
            }
            rtc.direct_api().close_data_channel(id);
        }
    }

    /// 상대가 채널을 닫았다 — 받은 것을 다 쓴 뒤 TCP를 닫는다.
    pub fn remote_closed(&mut self, id: ChannelId) {
        if let Some(mut t) = self.map.remove(&id) {
            t.shutdown_tcp();
            if t.local_port != 0 {
                self.closed.push(t.local_port);
            }
        }
    }

    /// TCP task가 알린 것을 처리한다. 돌려주는 값: (host) 새로 붙은 터널의 로컬 포트.
    pub fn on_tcp(&mut self, rtc: &mut Rtc, id: ChannelId, ev: TcpEv) -> Option<u16> {
        match ev {
            TcpEv::Data(b) => {
                if let Some(t) = self.map.get_mut(&id) {
                    t.push_outbound(b);
                }
            }
            TcpEv::Eof => {
                if let Some(t) = self.map.get_mut(&id) {
                    t.set_eof();
                }
            }
            TcpEv::Failed | TcpEv::ConnectFailed => self.close(rtc, id),
            TcpEv::Connected { writer, reader, local_port } => match self.map.get_mut(&id) {
                Some(t) => {
                    t.attach(writer, reader, local_port);
                    return Some(local_port);
                }
                None => reader.abort(),
            },
        }
        None
    }

    pub fn close_all(&mut self) {
        for (_, mut t) in self.map.drain() {
            t.shutdown_tcp();
            if t.local_port != 0 {
                self.closed.push(t.local_port);
            }
        }
    }
}

/// TCP 접속 하나의 읽기·쓰기 task를 띄운다. 읽기는 `pause`를 지킨다.
pub fn spawn_pipes(
    stream: TcpStream,
    peer: u64,
    id: ChannelId,
    events: mpsc::Sender<TcpMsg>,
    pause: Arc<Pause>,
) -> (mpsc::UnboundedSender<Vec<u8>>, AbortHandle) {
    let _ = stream.set_nodelay(true);
    let (mut rd, mut wr) = stream.into_split();
    let (wtx, mut wrx) = mpsc::unbounded_channel::<Vec<u8>>();
    let ev = events.clone();
    tokio::spawn(async move {
        while let Some(b) = wrx.recv().await {
            if wr.write_all(&b).await.is_err() {
                let _ = ev.send((peer, id, TcpEv::Failed)).await;
                return;
            }
        }
        let _ = wr.shutdown().await;
    });
    let reader = tokio::spawn(async move {
        let mut buf = vec![0u8; CHUNK];
        loop {
            pause.wait().await;
            match rd.read(&mut buf).await {
                Ok(0) => {
                    let _ = events.send((peer, id, TcpEv::Eof)).await;
                    return;
                }
                Ok(n) => {
                    if events.send((peer, id, TcpEv::Data(buf[..n].to_vec()))).await.is_err() {
                        return;
                    }
                }
                Err(_) => {
                    let _ = events.send((peer, id, TcpEv::Failed)).await;
                    return;
                }
            }
        }
    });
    (wtx, reader.abort_handle())
}

/// (host) `--target`에 붙고, 붙으면 `Connected`를 알린 뒤 읽기·쓰기 task를 띄운다.
pub fn spawn_connect(
    target: std::net::SocketAddr,
    peer: u64,
    id: ChannelId,
    events: mpsc::Sender<TcpMsg>,
    pause: Arc<Pause>,
    header: Option<Vec<u8>>,
) {
    tokio::spawn(async move {
        let mut stream = match tokio::time::timeout(std::time::Duration::from_secs(5), TcpStream::connect(target)).await {
            Ok(Ok(s)) => s,
            _ => {
                let _ = events.send((peer, id, TcpEv::ConnectFailed)).await;
                return;
            }
        };
        // (Bukkit) PROXY protocol 머리는 어떤 바이트보다도 먼저.
        if let Some(h) = header {
            if stream.write_all(&h).await.is_err() {
                let _ = events.send((peer, id, TcpEv::ConnectFailed)).await;
                return;
            }
        }
        let local_port = stream.local_addr().map(|a| a.port()).unwrap_or(0);
        // 읽기 task가 보낼 Data보다 Connected가 먼저 가야 한다 — 같은 길로, 먼저 보낸다.
        let (writer, reader) = {
            let (wtx, reader) = spawn_pipes_paused(stream, peer, id, events.clone(), pause.clone());
            (wtx, reader)
        };
        if events.send((peer, id, TcpEv::Connected { writer, reader: reader.clone(), local_port })).await.is_err() {
            reader.abort();
            return;
        }
        pause.set(false);
    });
}

/// 멈춘 채로 띄운다(Connected를 먼저 알리려고) — 부른 쪽이 `pause.set(false)`로 푼다.
fn spawn_pipes_paused(
    stream: TcpStream,
    peer: u64,
    id: ChannelId,
    events: mpsc::Sender<TcpMsg>,
    pause: Arc<Pause>,
) -> (mpsc::UnboundedSender<Vec<u8>>, AbortHandle) {
    pause.set(true);
    spawn_pipes(stream, peer, id, events, pause)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pause_blocks_and_releases() {
        let p = Arc::new(Pause::default());
        p.set(true);
        let q = p.clone();
        let h = tokio::spawn(async move { q.wait().await });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(!h.is_finished());
        p.set(false);
        tokio::time::timeout(std::time::Duration::from_secs(1), h).await.unwrap().unwrap();
    }
}
