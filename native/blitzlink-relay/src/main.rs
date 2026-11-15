//! `blitzlink-relay` — 공식 시그널링 중계(hyproj.com 도커). 무엇을 하는지는 `lib.rs` 머리, 배포는 `docs/en/RELAY.md`.
//!
//!   blitzlink-relay [--listen 127.0.0.1:1947] [--stun 0.0.0.0:3478 | --no-stun] [--trust-proxy]
//!                   [--allow-origin https://hyproj.com ...] [--max-hosts 5000] [--max-hosts-per-ip 8]

use std::net::SocketAddr;
use std::time::Duration;

use blitzlink_relay::{log, Config};
use clap::Parser;

#[derive(Parser)]
#[command(name = "blitzlink-relay", version, about = "BlitzLink (Minecraft join codes MCJE-XXXX) signaling relay")]
struct Cli {
    /// HTTP·WebSocket 주소(nginx 뒤).
    #[arg(long, default_value = "127.0.0.1:1947")]
    listen: SocketAddr,
    /// STUN(UDP) 주소.
    #[arg(long, default_value = "0.0.0.0:3478")]
    stun: SocketAddr,
    /// STUN을 열지 않는다(hyproj.com에서는 이미 도는 3478을 같이 쓴다).
    #[arg(long)]
    no_stun: bool,
    /// 브라우저가 /ws를 열 수 있는 출처(여러 번 — 주면 기본 목록을 바꾼다. `*`는 포트 자리). `any`면 막지 않는다.
    #[arg(long = "allow-origin")]
    allow_origin: Vec<String>,
    /// 앞의 프록시가 준 X-Real-IP를 믿는다(nginx 뒤에서만 켠다).
    #[arg(long)]
    trust_proxy: bool,
    #[arg(long, default_value_t = 5000)]
    max_hosts: usize,
    #[arg(long, default_value_t = 8)]
    max_hosts_per_ip: u32,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let cfg = Config {
        listen: cli.listen,
        stun: (!cli.no_stun).then_some(cli.stun),
        allow_origins: match cli.allow_origin.as_slice() {
            [] => blitzlink_relay::default_origins(),
            [x] if x == "any" => Vec::new(),
            v => v.to_vec(),
        },
        trust_proxy: cli.trust_proxy,
        max_hosts: cli.max_hosts,
        max_hosts_per_ip: cli.max_hosts_per_ip,
        ..Config::default()
    };
    let h = match blitzlink_relay::start(cfg).await {
        Ok(h) => h,
        Err(e) => {
            log::warn(e);
            return std::process::ExitCode::FAILURE;
        }
    };
    log::info(format!(
        "BlitzLink relay {} - relay protocol {} - http://{} - STUN {}",
        blitzlink_core::VERSION,
        blitzlink_core::relay::RELAY_PROTOCOL,
        h.http,
        h.stun.map_or("off".into(), |a| format!("udp://{a}")),
    ));
    let relay = h.relay.clone();
    tokio::spawn(async move {
        let mut last = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let (hosts, public, joins) = relay.stats();
            log::info(format!("hosts {hosts} (public {public}), join attempts in the last minute: {}", joins - last));
            last = joins;
        }
    });
    let _ = tokio::signal::ctrl_c().await;
    log::info("relay stopped");
    std::process::ExitCode::SUCCESS
}
