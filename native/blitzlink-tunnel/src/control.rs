//! 모드(Java)와의 제어 — stdin/stdout JSON 줄(한 줄 = 메시지 하나, 태그 `t`). 정본 설명은 `docs/en/PROTOCOL.md`.
//!
//! - stdout: 사이드카 → 모드. `--json`이면 JSON 줄만, 아니면 사람이 읽는 한 줄(개발용).
//! - stderr: 사람이 읽는 로그(모드는 그대로 자기 로그에 옮겨 적으면 된다).
//! - stdin: 모드 → 사이드카. 닫히면(모드가 죽었다) 사이드카도 정리하고 끝난다.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use blitzlink_core::relay::Listing;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

static JSON: AtomicBool = AtomicBool::new(false);

pub fn set_json(on: bool) {
    JSON.store(on, Ordering::Relaxed);
}

/// 사이드카 → 모드 한 줄. `v`는 `{"t": …}` 꼴이어야 한다.
pub fn emit(v: Value) {
    let mut out = std::io::stdout().lock();
    if JSON.load(Ordering::Relaxed) {
        let _ = writeln!(out, "{v}");
    } else {
        let t = v.get("t").and_then(Value::as_str).unwrap_or("?").to_string();
        let rest: Vec<String> = v
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(k, _)| k.as_str() != "t")
            .map(|(k, v)| match v {
                Value::String(s) => format!("{k}={s}"),
                other => format!("{k}={other}"),
            })
            .collect();
        let _ = writeln!(out, "[{t}] {}", rest.join(" "));
    }
    let _ = out.flush();
}

pub fn error(reason: &str) {
    emit(serde_json::json!({ "t": "error", "reason": reason }));
}

/// 사람이 읽는 로그(stderr).
pub fn log(msg: impl AsRef<str>) {
    let mut e = std::io::stderr().lock();
    let _ = writeln!(e, "[blitzlink] {}", msg.as_ref());
    let _ = e.flush();
}

/// 모드 → 사이드카.
#[derive(Debug, Clone, PartialEq)]
pub enum Control {
    /// 공개 목록·참가 화면에 보일 요약 — `{"t":"listing","listing":{…}}`(필드를 같은 층에 둔 꼴도 받는다).
    Listing(Listing),
    /// `meta` 채널로 클라에 건넬 서버 정보(모드 목록 포함) — 사이드카는 손대지 않고 그대로 건넨다.
    Meta(Value),
    /// 공개 목록에 올리기·내리기 — `{"t":"public","value":bool}`.
    Public(bool),
    /// (선택) 모드가 MC 포트로 받은 TURN 접속을 사이드카 TURN 포트로 넘길 때 — 그 루프백 접속의 로컬 포트와 바깥 상대 주소.
    TurnPeer { port: u16, addr: String },
    Quit,
}

impl Control {
    pub fn parse(line: &str) -> Result<Control, String> {
        let v: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
        let t = v.get("t").and_then(Value::as_str).ok_or("no `t`")?;
        let field = |k: &str| v.get(k).ok_or_else(|| format!("`{t}` without `{k}`"));
        Ok(match t {
            "listing" => {
                let body = v.get("listing").filter(|l| l.is_object()).unwrap_or(&v);
                Control::Listing(serde_json::from_value(body.clone()).map_err(|e| e.to_string())?)
            }
            "meta" => Control::Meta(field("info")?.clone()),
            "public" => Control::Public(v.get("value").or_else(|| v.get("public")).and_then(Value::as_bool).ok_or("`public` without `value`")?),
            "turnPeer" => Control::TurnPeer {
                port: field("port")?.as_u64().and_then(|p| u16::try_from(p).ok()).ok_or("bad `port`")?,
                addr: field("addr")?.as_str().ok_or("bad `addr`")?.to_string(),
            },
            "quit" => Control::Quit,
            other => return Err(format!("unknown message `{other}`")),
        })
    }
}

/// stdin을 읽어 넘긴다. stdin이 닫히면 `Quit`을 넣고 끝난다.
pub fn spawn_stdin() -> mpsc::Receiver<Control> {
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    match Control::parse(line) {
                        Ok(c) => {
                            if tx.send(c).await.is_err() {
                                return;
                            }
                        }
                        Err(e) => log(format!("unreadable control line ({e}): {}", line.chars().take(80).collect::<String>())),
                    }
                }
                _ => {
                    let _ = tx.send(Control::Quit).await;
                    return;
                }
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_lines_parse() {
        let Control::Listing(l) = Control::parse(r#"{"t":"listing","listing":{"name":"A","maxPlayers":8,"loader":"fabric"}}"#).unwrap() else { panic!() };
        assert_eq!((l.name.as_str(), l.max_players, l.loader.as_str()), ("A", 8, "fabric"));
        let Control::Listing(l) = Control::parse(r#"{"t":"listing","name":"B","loader":"bukkit"}"#).unwrap() else { panic!() };
        assert_eq!((l.name.as_str(), l.loader.as_str()), ("B", "bukkit"), "flattened form too");
        let c = Control::parse(r#"{"t":"meta","info":{"mods":[{"id":"x"}]}}"#).unwrap();
        assert!(matches!(c, Control::Meta(info) if info["mods"][0]["id"] == "x"));
        assert_eq!(Control::parse(r#"{"t":"public","value":true}"#).unwrap(), Control::Public(true));
        assert_eq!(Control::parse(r#"{"t":"public","public":false}"#).unwrap(), Control::Public(false));
        assert!(matches!(Control::parse(r#"{"t":"turnPeer","port":5,"addr":"1.2.3.4:9"}"#).unwrap(), Control::TurnPeer { port: 5, .. }));
        assert_eq!(Control::parse(r#"{"t":"quit"}"#).unwrap(), Control::Quit);
        assert!(Control::parse(r#"{"t":"nope"}"#).is_err());
        assert!(Control::parse(r#"{"t":"public"}"#).is_err());
        assert!(Control::parse("junk").is_err());
    }
}
