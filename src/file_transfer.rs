//! Layer 3: file-transfer state machine.
//!
//! Drives the protocol-1 sub-protocol (QUERY / OPEN / WRITE / CLOSE / ABORT)
//! on top of a `Session` (see [`crate::session`]). Handles compression
//! negotiation and chunking.
//!
//! Implementation lands in Stage 4 of the development plan.
