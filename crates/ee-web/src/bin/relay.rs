//! Native WebSocket signaling relay (CP-WEB.6) — the server behind M6's
//! co-edit, so two browsers on **different machines** converge (not just
//! same-origin tabs over `BroadcastChannel`).
//!
//! It is a thin wrapper: every routing decision lives in the pure, host-tested
//! [`ee_web::Relay`]; the socket plumbing (hand-rolled WebSocket upgrade + frame
//! parse, no third-party deps) lives in [`ee_web::server`]. Both are gated
//! behind the native `relay` cargo feature, so the default workspace build and
//! the `wasm32` browser bundle never see any of it.
//!
//! Run it (listens on `127.0.0.1:9001` unless a `HOST:PORT` arg is given):
//!
//! ```text
//! cargo run -p ee-web --features relay --bin relay -- 127.0.0.1:9001
//! ```
//!
//! A browser co-edit client connects over `ws://HOST:PORT`, sends `JOIN <room>`,
//! then `OP <token>` for each local [`ee_web::EditOp`]; the relay forwards every
//! op to the other members of that room, who feed it straight into their replica.

fn main() -> std::io::Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:9001".to_string());
    eprintln!("ee-web relay listening on ws://{addr}");
    ee_web::server::serve(&addr)
}
