//! Optional I/O adapters that wrap the sans-I/O core into convenience APIs.
//!
//! The crate's core ([`session`](crate::session), [`file_transfer`](crate::file_transfer),
//! [`codec`](crate::codec)) does no I/O of its own — callers feed bytes in
//! and pull events out. These adapter modules lift that core over a real
//! transport. Each is gated behind its own feature flag so users only pay
//! for what they use:
//!
//! - [`blocking`] (feature `blocking`) — synchronous loop over any
//!   `std::io::Read + std::io::Write` transport.
//! - [`tokio`] (feature `tokio`) — async loop over any
//!   `tokio::io::AsyncRead + AsyncWrite + Unpin` transport.
//! - [`serialport`] (feature `serial`) — convenience helpers for opening a
//!   [`serialport::SerialPort`], which already implements `Read + Write`
//!   and so plugs into [`blocking`] directly.
//!
//! Custom transports — TCP-to-serial bridges, USB CDC via `rusb`, in-memory
//! pipes for tests — just need to implement those standard traits.
//!
//! Shared option / result / error types live in [`common`].
//!
//! [`serialport::SerialPort`]: https://docs.rs/serialport/latest/serialport/trait.SerialPort.html

#[cfg(any(feature = "blocking", feature = "tokio"))]
pub mod common;

#[cfg(feature = "blocking")]
#[cfg_attr(docsrs, doc(cfg(feature = "blocking")))]
pub mod blocking;

#[cfg(feature = "tokio")]
#[cfg_attr(docsrs, doc(cfg(feature = "tokio")))]
pub mod tokio;

#[cfg(feature = "serial")]
#[cfg_attr(docsrs, doc(cfg(feature = "serial")))]
pub mod serialport;
