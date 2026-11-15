//! 출력 — 한 줄씩 stdout(도커 로그). IP·코드·지문은 남기지만 SDP 같은 내용은 남기지 않는다.

use std::io::Write;

fn stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let s = secs % 86_400;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

fn line(level: &str, msg: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "[{} UTC] {level:5} {msg}", stamp());
    let _ = out.flush();
}

pub fn info(msg: impl AsRef<str>) {
    line("info", msg.as_ref());
}

pub fn warn(msg: impl AsRef<str>) {
    line("warn", msg.as_ref());
}
