//! Host-side implementation of Marlin's Binary File Transfer Mark II protocol.
//!
//! This crate is a sans-I/O implementation: the core (`session`, `file_transfer`,
//! `codec`) does no I/O of its own — callers feed bytes in and pull events out.
//! Optional adapter modules wrap the core for the common cases:
//!
//! - `blocking` — synchronous loop over a `Transport`
//! - `tokio` — async loop over an `AsyncTransport`
//! - `serial` — default `Transport` backed by the `serialport` crate
//!
//! See the protocol reference: <https://github.com/MarlinFirmware/Marlin/pull/14817>.
//!
//! ## Status
//!
//! Pre-1.0. The Marlin protocol itself is documented as experimental upstream
//! and may change. API will evolve in 0.x.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod codec;
pub mod compression;
pub mod file_transfer;
pub mod session;
pub mod transport;

#[cfg(any(feature = "blocking", feature = "tokio", feature = "serial"))]
pub mod adapters;
