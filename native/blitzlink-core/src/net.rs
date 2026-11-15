//! 주소 정본 — `data/net.json`(`relay`·`stun`)을 빌드 때 품는다. 사이드카는 `--relay`·`--stun`이 없으면 이것을 쓴다
//! (모드 Java 쪽도 같은 파일을 읽는다).
//!
//! 저장소의 `data/net.json`은 **자리표시**(`blitzlink.example.com`)다. 공식 빌드는 CI(GitHub Actions)가 저장소 변수
//! `BLITZLINK_NET_JSON`으로 이 파일을 갈아 끼운 뒤 빌드한다. 자리표시로 빌드한 사이드카는 `--relay` 없이는 시작하지 않고
//! (`notConfigured`), 자리표시 STUN에는 묻지 않는다.

use serde::Deserialize;

/// `data/net.json` 원문.
pub const NET_JSON: &str = include_str!("../../../data/net.json");

#[derive(Debug, Clone, Deserialize)]
pub struct NetDefaults {
    pub relay: String,
    #[serde(default)]
    pub stun: Vec<String>,
}

pub fn defaults() -> NetDefaults {
    serde_json::from_str(NET_JSON).expect("data/net.json")
}

/// 자리표시 도메인(RFC 2606의 `example.com` 아래).
pub const PLACEHOLDER_DOMAIN: &str = "example.com";

/// 그 주소가 자리표시인가(`…example.com`·`…example.org`·`…example.net`·`….example`).
pub fn is_placeholder(url: &str) -> bool {
    let host = url.split("://").nth(1).unwrap_or(url);
    let host = host.strip_prefix("stun:").or_else(|| host.strip_prefix("turn:")).unwrap_or(host);
    let host = host.split(['/', '?']).next().unwrap_or("");
    let host = match host.rfind(':') {
        Some(i) if !host.ends_with(']') => &host[..i],
        _ => host,
    }
    .to_ascii_lowercase();
    ["example.com", "example.org", "example.net"].iter().any(|d| host == *d || host.ends_with(&format!(".{d}"))) || host.ends_with(".example")
}

/// 중계 주소(`https://relay.example.org/relay/mcje` · `ws://127.0.0.1:1947` …)를 WebSocket 주소로.
pub fn ws_url(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    let b = if let Some(r) = b.strip_prefix("https://") {
        format!("wss://{r}")
    } else if let Some(r) = b.strip_prefix("http://") {
        format!("ws://{r}")
    } else if b.starts_with("ws://") || b.starts_with("wss://") {
        b.to_string()
    } else {
        format!("wss://{b}")
    };
    if b.ends_with("/ws") { b } else { format!("{b}/ws") }
}

/// 중계 주소의 HTTP 꼴(`/servers`를 붙일 수 있게).
pub fn http_url(base: &str) -> String {
    let w = ws_url(base);
    let w = w.strip_suffix("/ws").unwrap_or(&w);
    if let Some(r) = w.strip_prefix("wss://") {
        format!("https://{r}")
    } else if let Some(r) = w.strip_prefix("ws://") {
        format!("http://{r}")
    } else {
        w.to_string()
    }
}

/// `stun:host:port` · `host:port` · `host` → `host:port`(기본 3478). `off`·빈 것은 None.
pub fn stun_host(url: &str) -> Option<String> {
    let s = url.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("off") || s.eq_ignore_ascii_case("none") {
        return None;
    }
    let s = s.strip_prefix("stun:").unwrap_or(s);
    let s = s.split('?').next()?;
    Some(with_port(s, 3478))
}

/// `turn:host:port?transport=tcp` → `host:port`(기본 3478). TCP가 아닌 것은 None(이 TURN은 TCP만).
pub fn turn_host(url: &str) -> Option<String> {
    let s = url.trim().strip_prefix("turn:")?;
    let (addr, query) = match s.split_once('?') {
        Some((a, q)) => (a, q),
        None => (s, ""),
    };
    if !query.is_empty() && !query.split('&').any(|kv| kv.eq_ignore_ascii_case("transport=tcp")) {
        return None;
    }
    (!addr.is_empty()).then(|| with_port(addr, 3478))
}

fn with_port(s: &str, port: u16) -> String {
    if s.starts_with('[') {
        if s.contains("]:") { s.to_string() } else { format!("{s}:{port}") }
    } else if s.matches(':').count() == 1 {
        s.to_string()
    } else if s.contains(':') {
        format!("[{s}]:{port}")
    } else {
        format!("{s}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn net_json_is_valid() {
        // 자리표시든 CI가 넣은 공식 값이든 꼴은 같아야 한다.
        let d = defaults();
        assert!(d.relay.starts_with("https://") || d.relay.starts_with("http://"), "{}", d.relay);
        assert!(ws_url(&d.relay).ends_with("/ws"));
        assert!(!d.stun.is_empty() && d.stun.iter().all(|s| stun_host(s).is_some()), "{:?}", d.stun);
    }

    #[test]
    fn placeholders() {
        assert!(is_placeholder("https://blitzlink.example.com/relay/mcje"));
        assert!(is_placeholder("stun:blitzlink.example.com:3478"));
        assert!(is_placeholder("http://example.org"));
        assert!(is_placeholder("wss://relay.test.example/ws"));
        assert!(!is_placeholder("https://relay.mydomain.com/relay/mcje"));
        assert!(!is_placeholder("http://127.0.0.1:1947"));
        assert!(!is_placeholder("stun:notexample.com:3478"));
        assert!(!is_placeholder("stun:[2001:db8::1]:3478"));
    }

    #[test]
    fn urls() {
        assert_eq!(ws_url("https://relay.example.org/relay/mcje"), "wss://relay.example.org/relay/mcje/ws");
        assert_eq!(ws_url("http://127.0.0.1:1946/"), "ws://127.0.0.1:1946/ws");
        assert_eq!(ws_url("ws://127.0.0.1:1946/ws"), "ws://127.0.0.1:1946/ws");
        assert_eq!(http_url("https://relay.example.org/relay/mcje"), "https://relay.example.org/relay/mcje");
        assert_eq!(http_url("ws://127.0.0.1:1946/ws"), "http://127.0.0.1:1946");
        assert_eq!(stun_host("stun:relay.example.org:3478").as_deref(), Some("relay.example.org:3478"));
        assert_eq!(stun_host("127.0.0.1:3478").as_deref(), Some("127.0.0.1:3478"));
        assert_eq!(stun_host("relay.example.org").as_deref(), Some("relay.example.org:3478"));
        assert_eq!(stun_host("off"), None);
        assert_eq!(turn_host("turn:1.2.3.4:25565?transport=tcp").as_deref(), Some("1.2.3.4:25565"));
        assert_eq!(turn_host("turn:[2001:db8::1]:25565?transport=tcp").as_deref(), Some("[2001:db8::1]:25565"));
        assert_eq!(turn_host("turn:1.2.3.4:25565?transport=udp"), None);
        assert_eq!(turn_host("stun:1.2.3.4"), None);
    }
}
