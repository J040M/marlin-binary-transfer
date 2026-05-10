//! Optional heatshrink payload compression.
//!
//! Behind the `heatshrink` feature flag. Wraps the `embedded-heatshrink`
//! crate with the dynamically-negotiated window/lookahead parameters that
//! the device advertises in its QUERY response.
//!
//! Implementation lands in Stage 5 of the development plan.
