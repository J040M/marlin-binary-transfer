//! Layer 1: packet encoding, decoding, and Fletcher-16 checksum.
//!
//! This module is the lowest layer of the protocol stack. It deals only in
//! bytes and packet structures; it has no notion of sessions, sync counters,
//! or file transfer.
//!
//! Implementation lands in Stage 2 of the development plan.

/// Marlin BFT packet start token (little-endian on the wire).
pub const PACKET_TOKEN: u16 = 0xB5AD;
