//! Layer 2: sans-I/O session state machine.
//!
//! Owns the sync counter, the outbound queue, and the inbound ASCII line
//! parser. Exposes a `feed` / `poll_outbound` / `poll_event` / `tick` API
//! so callers can drive it from any I/O model.
//!
//! Implementation lands in Stage 3 of the development plan.
