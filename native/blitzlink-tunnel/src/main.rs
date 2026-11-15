//! `blitzlink-tunnel` — BlitzLink 사이드카. 모드(Java)가 jar에서 풀어 띄우고 stdin/stdout JSON 줄로 제어한다(`docs/en/PROTOCOL.md`).
//!
//!   blitzlink-tunnel host --target 127.0.0.1:<mcPort> --port <mcPort> [--relay <url>] [--stun <url>]...
//!                         [--key-file <path>] [--code-file <path>] [--public] [--upnp] [--no-turn] [--json]
//!   blitzlink-tunnel join --code MCJE-XXXX [--relay <url>] [--stun <url>]... [--turn-only] [--json]
//!
//! `--relay`·`--stun`이 없으면 `data/net.json`(빌드 때 품은 것)을 쓴다. `--stun off`면 STUN을 묻지 않는다.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod control;
mod host;
mod ice;
mod join;
mod meta;
mod proxy;
mod relay_link;
mod tunnel;
mod turn_client;
mod turn_server;
mod upnp;

#[derive(Parser)]
#[command(name = "blitzlink-tunnel", version, about = "BlitzLink sidecar — Minecraft TCP over WebRTC DataChannels (join codes MCJE-XXXX)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Expose a local Minecraft server through the relay.
    Host {
        /// Local Minecraft server to dial for every tunneled connection.
        #[arg(long)]
        target: SocketAddr,
        /// Minecraft TCP port (the UDP socket tries the same number; TURN is advertised on it).
        #[arg(long)]
        port: u16,
        #[arg(long)]
        relay: Option<String>,
        #[arg(long)]
        stun: Vec<String>,
        #[arg(long, default_value = "blitzlink-host.key")]
        key_file: PathBuf,
        #[arg(long, default_value = "blitzlink-code.txt")]
        code_file: PathBuf,
        /// List the server on the relay's public list.
        #[arg(long)]
        public: bool,
        /// Ask the router to forward TCP+UDP via UPnP-IGD.
        #[arg(long)]
        upnp: bool,
        /// Do not run the embedded TURN server.
        #[arg(long)]
        no_turn: bool,
        /// Bind the embedded TURN directly on this public TCP address (e.g. 0.0.0.0:25566) instead of loopback;
        /// its port is advertised to clients. For hosts that cannot split the game port by first byte (Bukkit).
        #[arg(long)]
        turn_listen: Option<SocketAddr>,
        /// Send a HAProxy PROXY protocol v2 header with the player's real address when dialing --target.
        #[arg(long)]
        proxy_protocol: bool,
        /// Machine-readable JSON lines on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Connect to a host by join code and expose it on a local TCP port.
    Join {
        #[arg(long)]
        code: String,
        #[arg(long)]
        relay: Option<String>,
        #[arg(long)]
        stun: Vec<String>,
        /// Development: skip the direct UDP attempt and use the host's TURN only.
        #[arg(long)]
        turn_only: bool,
        #[arg(long)]
        json: bool,
    },
}

/// `--relay`·`--stun`, 없으면 빌드 때 품은 `data/net.json`. 자리표시 중계면 `notConfigured`(시작하지 않는다),
/// 자리표시 STUN은 뺀다(있지도 않은 곳에 묻지 않게).
fn net_defaults(relay: Option<String>, stun: Vec<String>) -> Result<(String, Vec<String>), String> {
    use blitzlink_core::net::is_placeholder;
    let d = blitzlink_core::net::defaults();
    // `--stun a,b`(쉼표 하나로)도, `--stun a --stun b`도.
    let stun: Vec<String> = stun.iter().flat_map(|s| s.split(',')).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    let stun: Vec<String> = if stun.is_empty() { d.stun } else { stun }.into_iter().filter(|s| !is_placeholder(s)).collect();
    let relay = relay.unwrap_or(d.relay);
    if is_placeholder(&relay) {
        control::log(format!("no relay configured: {relay} is a placeholder (this build uses the repository's data/net.json); pass --relay <url>"));
        return Err("notConfigured".into());
    }
    Ok((relay, stun))
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let r = match cli.cmd {
        Cmd::Host { target, port, relay, stun, key_file, code_file, public, upnp, no_turn, turn_listen, proxy_protocol, json } => {
            control::set_json(json);
            match net_defaults(relay, stun) {
                Ok((relay, stun)) => host::run(host::HostOptions { target, port, relay, stun, key_file, code_file, public, upnp, turn: !no_turn, turn_listen, proxy_protocol })
                .await,
                Err(e) => Err(e),
            }
        }
        Cmd::Join { code, relay, stun, turn_only, json } => {
            control::set_json(json);
            match net_defaults(relay, stun) {
                Ok((relay, stun)) => join::run(join::JoinOptions { code, relay, stun, turn_only }).await,
                Err(e) => Err(e),
            }
        }
    };
    // 끝낼 때는 프로세스째 — tokio의 stdin 읽기는 막힌 스레드라 런타임이 그것을 기다리며 멈춘다.
    let code = match r {
        Ok(()) => 0,
        Err(reason) => {
            control::log(format!("exiting: {reason}"));
            control::error(&reason);
            1
        }
    };
    std::process::exit(code)
}
