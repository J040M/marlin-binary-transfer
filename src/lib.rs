//! Host-side implementation of Marlin's Binary File Transfer Mark II protocol.
//!
//! Uploads G-code files to a 3D printer's SD card over serial, with framing
//! checksums, sync acknowledgement, retransmit on timeout, and optional
//! heatshrink payload compression.
//!
//! # Crate layout
//!
//! The core ([`codec`], [`session`], [`file_transfer`]) is **sans-I/O**:
//! callers feed bytes in and pull events out, plumbing them through any
//! transport. Optional adapter modules wrap the core for the common cases:
//!
//! - [`adapters::blocking`] — synchronous loop over a [`std::io::Read`] +
//!   [`std::io::Write`] transport. Behind the `blocking` feature.
//! - [`adapters::tokio`] — async loop over a [`tokio::io::AsyncRead`] +
//!   [`tokio::io::AsyncWrite`] + [`Unpin`] transport. Behind the `tokio`
//!   feature.
//! - [`adapters::serialport`] — convenience helpers for opening a
//!   [`serialport::SerialPort`], which already implements `Read + Write`
//!   and so plugs into the blocking adapter directly. Behind the `serial`
//!   feature (implies `blocking`).
//!
//! Heatshrink payload compression is gated behind the `heatshrink` feature
//! and exposed via [`compression`].
//!
//! [`tokio::io::AsyncRead`]: https://docs.rs/tokio/latest/tokio/io/trait.AsyncRead.html
//! [`tokio::io::AsyncWrite`]: https://docs.rs/tokio/latest/tokio/io/trait.AsyncWrite.html
//! [`serialport::SerialPort`]: https://docs.rs/serialport/latest/serialport/trait.SerialPort.html
//!
//! # Quickstart
//!
//! ```no_run
//! # #[cfg(all(feature = "blocking", feature = "serial"))]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use marlin_binary_transfer::adapters::blocking::{upload, UploadOptions};
//! use marlin_binary_transfer::adapters::serialport;
//! use marlin_binary_transfer::file_transfer::Compression;
//!
//! let mut port = serialport::open("/dev/ttyUSB0", 250_000)?;
//! let file = std::fs::File::open("model.gco")?;
//!
//! let stats = upload(&mut *port, file, UploadOptions {
//!     dest_filename: "model.gco".into(),
//!     compression: Compression::Auto,
//!     ..UploadOptions::default()
//! })?;
//! println!("uploaded {} bytes in {} chunks", stats.bytes_sent, stats.chunks_sent);
//! # Ok(()) }
//! # #[cfg(not(all(feature = "blocking", feature = "serial")))]
//! # fn main() {}
//! ```
//!
//! See the [README] for the tokio quickstart, sans-I/O usage, and a complete
//! description of what the upload helpers handle on your behalf (binary-mode
//! enter/exit, retransmit, error classification).
//!
//! [README]: https://github.com/J040M/marlin-binary-transfer#readme
//!
//! # Protocol reference
//!
//! <https://github.com/MarlinFirmware/Marlin/pull/14817>
//!
//! # Status
//!
//! Pre-1.0. The Marlin protocol is documented as experimental upstream and
//! may evolve. Public API follows semver within 0.x — pin a minor version.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod codec;
pub mod compression;
pub mod file_transfer;
pub mod session;

#[cfg(any(feature = "blocking", feature = "tokio", feature = "serial"))]
pub mod adapters;
