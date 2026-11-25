//! Reset Generation Revived multiplayer server: the relay, the casino (matchmaking) and the game
//! room bots that stand in for N-Gage Arena. docs/MULTIPLAYER.md describes the protocol.
//!
//! [`relay`], [`casino`] and [`gameroom`] are the I/O-free core; [`net`] is the transport
//! (WebSocket over TCP, LAN discovery over UDP).

pub mod casino;
pub mod gameroom;
pub mod net;
pub mod relay;
pub mod wire;
