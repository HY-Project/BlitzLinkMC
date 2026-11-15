//! BlitzLink 공용 코드 — 중계(`blitzlink-relay`)와 사이드카(`blitzlink-tunnel`)가 같이 쓴다. IO가 없다(난수만 OS에서).

pub mod identity;
pub mod net;
pub mod relay;
pub mod stun;
pub mod turn_codec;

/// 사이드카·중계 판(Cargo).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// OS 난수 32바이트.
pub fn random32() -> [u8; 32] {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("OS random source");
    b
}

pub fn random_u64() -> u64 {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("OS random source");
    u64::from_le_bytes(b)
}
