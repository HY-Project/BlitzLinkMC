//! 끝에서 끝까지(로컬) — 중계를 띄우고, 사이드카 바이너리 둘(`host`·`join`)을 진짜로 돌린다.
//!
//! 「MC 서버」 자리에는 메아리 TCP 서버를 둔다. 그 포트는 모드(Java)의 첫 바이트 분기를 흉내 낸다 — 첫 바이트가 STUN 머리
//! (0x00~0x03)면 호스트가 알린 `turnPort`로 잇고, 아니면 메아리(첫 글이 `CLOSE`면 곧바로 닫는다). 그래서 `--turn-only`도
//! 실제 배치와 같은 길(MC 포트 → TURN → 호스트 UDP)로 간다.
//!
//! 본다: 코드 받기(`hosted`), 공개 목록·`public`, `status`, `meta`(모드 목록 그대로·지문), `listen`, 1 MB 왕복, 닫힘 전파
//! (클라 → MC 서버, MC 서버 → 클라), `peer`·`peerClosed`, `via`(udp / turn), 호스트가 끝나면 join의 `closed`.
//! Bukkit 자리는 `--turn-listen`(TURN을 바로 묶음) + `--proxy-protocol`(PROXY v2 머리의 원본 주소 = `peer.addr`).

use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, watch};

const BIN: &str = env!("CARGO_BIN_EXE_blitzlink-tunnel");

struct Proc {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::UnboundedReceiver<Value>,
    seen: Vec<Value>,
}

impl Proc {
    fn spawn(name: &'static str, args: &[String]) -> Proc {
        let mut child = Command::new(BIN)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("sidecar");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut r = BufReader::new(stdout).lines();
            while let Ok(Some(l)) = r.next_line().await {
                eprintln!("[{name} out] {l}");
                let v: Value = serde_json::from_str(&l).unwrap_or_else(|e| panic!("{name}: stdout is not JSON ({e}): {l}"));
                let _ = tx.send(v);
            }
        });
        tokio::spawn(async move {
            let mut r = BufReader::new(stderr).lines();
            while let Ok(Some(l)) = r.next_line().await {
                eprintln!("[{name} err] {l}");
            }
        });
        Proc { child, stdin, lines, seen: Vec::new() }
    }

    async fn send(&mut self, v: Value) {
        self.stdin.write_all(format!("{v}\n").as_bytes()).await.unwrap();
        self.stdin.flush().await.unwrap();
    }

    /// `t`가 그것인 줄이 올 때까지(이미 본 것도 찾는다).
    async fn wait(&mut self, t: &str, within: Duration) -> Value {
        if let Some(v) = self.seen.iter().find(|v| v["t"] == t) {
            return v.clone();
        }
        let until = tokio::time::Instant::now() + within;
        loop {
            let v = tokio::time::timeout_at(until, self.lines.recv())
                .await
                .unwrap_or_else(|_| panic!("no `{t}` line within {within:?}; seen {:?}", self.seen))
                .unwrap_or_else(|| panic!("sidecar exited before `{t}`; seen {:?}", self.seen));
            assert_ne!(v["t"], "error", "sidecar error: {v}");
            self.seen.push(v.clone());
            if v["t"] == t {
                return v;
            }
        }
    }

    async fn quit(mut self) {
        let _ = self.send(json!({ "t": "quit" })).await;
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
    }
}

/// 「MC 서버」 — 첫 바이트 분기 + 메아리. 돌려주는 것: 포트, TURN 포트를 알려 줄 곳, 메아리가 본 EOF 수.
async fn mc_server() -> (u16, watch::Sender<u16>, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let (turn_tx, turn_rx) = watch::channel(0u16);
    let eofs = Arc::new(AtomicUsize::new(0));
    let e2 = eofs.clone();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let turn_port = *turn_rx.borrow();
            let eofs = e2.clone();
            tokio::spawn(async move {
                let mut first = [0u8; 1];
                if s.peek(&mut first).await.unwrap_or(0) == 0 {
                    return;
                }
                if first[0] <= 0x03 {
                    // 모드의 FirstByteHandler 자리 — TURN 포트로 바이트를 잇는다.
                    let Ok(mut t) = TcpStream::connect(("127.0.0.1", turn_port)).await else { return };
                    let _ = tokio::io::copy_bidirectional(&mut s, &mut t).await;
                    return;
                }
                let mut buf = vec![0u8; 64 * 1024];
                let mut start = true;
                loop {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => {
                            eofs.fetch_add(1, Ordering::SeqCst);
                            return;
                        }
                        Ok(n) => {
                            if start && buf[..n].starts_with(b"CLOSE") {
                                return; // 서버가 먼저 닫는다.
                            }
                            start = false;
                            if s.write_all(&buf[..n]).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }
    });
    (port, turn_tx, eofs)
}

async fn round_trip(port: u16, size: usize) {
    let s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let (mut rd, mut wr) = s.into_split();
    // 첫 바이트가 0x00~0x03이면 분기가 TURN으로 보낸다(MC 핸드셰이크는 그렇게 시작하지 않는다).
    let data: Vec<u8> = (0..size).map(|i| ((i * 31 + 100) % 251) as u8).collect();
    let d2 = data.clone();
    let w = tokio::spawn(async move {
        for c in d2.chunks(7919) {
            wr.write_all(c).await.unwrap();
        }
        wr
    });
    let mut got = vec![0u8; size];
    tokio::time::timeout(Duration::from_secs(30), rd.read_exact(&mut got)).await.expect("1 MB round trip timed out").unwrap();
    assert!(got == data, "echoed bytes differ");
    let mut wr = w.await.unwrap();
    // 클라가 닫으면 → 채널이 닫히고 → 호스트가 MC 서버 쪽을 닫고 → 이쪽도 닫힌다.
    wr.shutdown().await.unwrap();
    let mut rest = Vec::new();
    let n = tokio::time::timeout(Duration::from_secs(5), rd.read_to_end(&mut rest)).await.expect("close did not propagate back").unwrap();
    assert_eq!(n, 0);
}

async fn server_closes_first(port: u16) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(b"CLOSE").await.unwrap();
    let mut rest = Vec::new();
    let n = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut rest)).await.expect("server close did not reach the client").unwrap();
    assert_eq!(n, 0);
}

/// (Bukkit 자리) 첫 바이트 분기 없이 — 접속마다 PROXY v2 머리를 읽어 원본 주소를 알리고, 나머지는 메아리(`CLOSE`면 닫음).
async fn proxy_echo_server() -> (u16, mpsc::UnboundedReceiver<Option<std::net::SocketAddr>>, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = mpsc::unbounded_channel();
    let eofs = Arc::new(AtomicUsize::new(0));
    let e2 = eofs.clone();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let (tx, eofs) = (tx.clone(), e2.clone());
            tokio::spawn(async move {
                let mut head = [0u8; 16];
                if s.read_exact(&mut head).await.is_err() {
                    return;
                }
                assert_eq!(&head[..12], b"\r\n\r\n\0\r\nQUIT\n", "PROXY v2 signature first");
                let mut body = vec![0u8; u16::from_be_bytes([head[14], head[15]]) as usize];
                s.read_exact(&mut body).await.unwrap();
                let src = match head[13] {
                    0x11 => Some(std::net::SocketAddr::new(<[u8; 4]>::try_from(&body[0..4]).unwrap().into(), u16::from_be_bytes([body[8], body[9]]))),
                    0x21 => Some(std::net::SocketAddr::new(<[u8; 16]>::try_from(&body[0..16]).unwrap().into(), u16::from_be_bytes([body[32], body[33]]))),
                    _ => None,
                };
                let _ = tx.send(src);
                let mut buf = vec![0u8; 64 * 1024];
                let mut start = true;
                loop {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => {
                            eofs.fetch_add(1, Ordering::SeqCst);
                            return;
                        }
                        Ok(n) => {
                            if start && buf[..n].starts_with(b"CLOSE") {
                                return;
                            }
                            start = false;
                            if s.write_all(&buf[..n]).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }
    });
    (port, rx, eofs)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    /// 모드 서버(NeoForge·Fabric) — 모드가 MC 포트에서 첫 바이트로 TURN을 가른다.
    Direct,
    TurnOnly,
    /// Bukkit — TURN을 공개 포트에 바로(`--turn-listen`), MC 서버에는 PROXY v2(`--proxy-protocol`). TURN만으로 붙는다.
    Bukkit,
}

async fn scenario(mode: Mode) {
    let r = blitzlink_relay::start(blitzlink_relay::Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        stun: Some("127.0.0.1:0".parse().unwrap()),
        ..Default::default()
    })
    .await
    .unwrap();
    let relay = format!("http://{}", r.http);
    // `--stun a,b` — 쉼표 하나로 여럿(앞의 것은 답이 없는 곳).
    let stun = format!("stun:127.0.0.1:9,stun:{}", r.stun.unwrap());
    let (mc_port, turn_port_tx, mut proxied, eofs) = if mode == Mode::Bukkit {
        let (p, rx, e) = proxy_echo_server().await;
        (p, None, Some(rx), e)
    } else {
        let (p, tx, e) = mc_server().await;
        (p, Some(tx), None, e)
    };
    let dir = std::env::temp_dir().join(format!("blitzlink-e2e-{}-{mode:?}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut args: Vec<String> = vec![
        "host".into(),
        "--target".into(),
        format!("127.0.0.1:{mc_port}"),
        "--port".into(),
        mc_port.to_string(),
        "--relay".into(),
        relay.clone(),
        "--stun".into(),
        stun.clone(),
        "--key-file".into(),
        dir.join("host.key").display().to_string(),
        "--code-file".into(),
        dir.join("code.txt").display().to_string(),
        "--public".into(),
        "--json".into(),
    ];
    if mode == Mode::Bukkit {
        args.extend(["--turn-listen".into(), "127.0.0.1:0".into(), "--proxy-protocol".into()]);
    }
    let mut host = Proc::spawn("host", &args);
    let tp = host.wait("turnPort", Duration::from_secs(10)).await;
    if let Some(tx) = &turn_port_tx {
        tx.send(tp["port"].as_u64().unwrap() as u16).unwrap();
    }
    let loader = if mode == Mode::Bukkit { "bukkit" } else { "neoforge" };
    host.send(json!({ "t": "listing", "listing": { "name": "E2E", "mcVersion": "26.3", "loader": loader, "loaderVersion": "Paper 26.3-251", "maxPlayers": 10, "modCount": 2 } })).await;
    let mods = json!({ "mcVersion": "26.3", "loader": loader, "mods": [{ "id": "blitzlink", "version": "26.3.0", "side": "plugin", "required": true }] });
    host.send(json!({ "t": "meta", "info": mods })).await;
    let hosted = host.wait("hosted", Duration::from_secs(15)).await;
    let code = hosted["code"].as_str().unwrap().to_string();
    assert!(code.starts_with("MCJE-"), "{code}");
    assert_eq!(std::fs::read_to_string(dir.join("code.txt")).unwrap().trim(), code);
    // 공개 목록(중계가 받은 listing — bukkit도 로더로 받는다).
    let until = tokio::time::Instant::now() + Duration::from_secs(10);
    while !r.relay.list().servers.iter().any(|s| s.code == code && s.listing.name == "E2E" && s.listing.loader == loader) {
        assert!(tokio::time::Instant::now() < until, "listing never reached the public list");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // 공개를 끄면 목록에서 빠진다(`public{value}`).
    host.send(json!({ "t": "public", "value": false })).await;
    let until = tokio::time::Instant::now() + Duration::from_secs(10);
    while r.relay.list().servers.iter().any(|s| s.code == code) {
        assert!(tokio::time::Instant::now() < until, "public:false was not applied");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let online = loop {
        let s = host.wait("status", Duration::from_secs(5)).await;
        if s["state"] == "online" {
            break s;
        }
        host.seen.retain(|v| v != &s);
    };
    assert_eq!(online["code"], code);

    let mut args = vec!["join".to_string(), "--code".into(), code.to_lowercase(), "--relay".into(), relay, "--stun".into(), stun, "--json".into()];
    if mode != Mode::Direct {
        args.push("--turn-only".into());
    }
    let mut join = Proc::spawn("join", &args);
    assert_eq!(join.wait("status", Duration::from_secs(5)).await["state"], "relay");
    let meta = join.wait("meta", Duration::from_secs(20)).await;
    assert_eq!(meta["info"], mods, "mod list relayed verbatim");
    assert_eq!(meta["fingerprint"], hosted["fingerprint"]);
    assert_eq!(meta["listing"]["name"], "E2E");
    assert_eq!(meta["via"], if mode == Mode::Direct { "udp" } else { "turn" });
    let states: Vec<Value> = join.seen.iter().filter(|v| v["t"] == "status").map(|v| v["state"].clone()).collect();
    assert!(states.contains(&json!(if mode == Mode::Direct { "connecting" } else { "turn" })), "{states:?}");
    assert!(states.contains(&json!("open")), "{states:?}");
    let listen = join.wait("listen", Duration::from_secs(5)).await;
    let port = listen["port"].as_u64().unwrap() as u16;

    let t0 = std::time::Instant::now();
    round_trip(port, 1024 * 1024).await;
    eprintln!("[{mode:?}] 1 MB round trip + close: {:?}", t0.elapsed());
    let peer = host.wait("peer", Duration::from_secs(5)).await;
    let local_port = peer["localPort"].as_u64().unwrap();
    assert!(local_port > 0);
    let addr: std::net::SocketAddr = peer["addr"].as_str().unwrap().parse().unwrap_or_else(|_| panic!("{peer}"));
    if let Some(rx) = proxied.as_mut() {
        // PROXY 머리의 원본 주소 = `peer`가 알린 실제 주소(여기서는 TURN TCP로 붙은 join 사이드카).
        let src = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        assert_eq!(src, Some(addr), "PROXY v2 source matches peer.addr");
        assert_ne!(addr.port() as u64, local_port);
    }
    let until = tokio::time::Instant::now() + Duration::from_secs(5);
    while eofs.load(Ordering::SeqCst) < 1 {
        assert!(tokio::time::Instant::now() < until, "client close did not reach the MC server");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let closed = host.wait("peerClosed", Duration::from_secs(5)).await;
    assert_eq!(closed["localPort"].as_u64(), Some(local_port));
    server_closes_first(port).await;
    // 같은 연결 위에 새 터널 하나 더(두 번째 접속도 된다).
    round_trip(port, 64 * 1024).await;

    // 호스트가 끝나면 join은 `closed`를 알리고 0으로 끝난다.
    let t1 = std::time::Instant::now();
    host.quit().await;
    join.wait("closed", Duration::from_secs(10)).await;
    let st = tokio::time::timeout(Duration::from_secs(5), join.child.wait()).await.unwrap().unwrap();
    assert_eq!(st.code(), Some(0));
    eprintln!("[{mode:?}] host quit -> join closed: {:?}", t1.elapsed());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_udp_tunnel() {
    scenario(Mode::Direct).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn turn_only_tunnel() {
    scenario(Mode::TurnOnly).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bukkit_turn_listen_and_proxy_protocol() {
    scenario(Mode::Bukkit).await;
}

/// 없는 코드 — 모드에 `error{noSuchCode}`를 알리고 1로 끝난다.
#[tokio::test]
async fn unknown_code_reports_relay_error() {
    let r = blitzlink_relay::start(blitzlink_relay::Config { listen: "127.0.0.1:0".parse().unwrap(), stun: None, ..Default::default() })
        .await
        .unwrap();
    let mut join = Proc::spawn("join", &["join".into(), "--code".into(), "MCJE-2222".into(), "--relay".into(), format!("http://{}", r.http), "--stun".into(), "off".into(), "--json".into()]);
    let until = tokio::time::Instant::now() + Duration::from_secs(10);
    let err = loop {
        let v = tokio::time::timeout_at(until, join.lines.recv()).await.expect("no error line").expect("exited without error line");
        if v["t"] == "error" {
            break v;
        }
    };
    assert_eq!(err["reason"], "noSuchCode");
    let status = tokio::time::timeout(Duration::from_secs(5), join.child.wait()).await.unwrap().unwrap();
    assert_eq!(status.code(), Some(1));
}
