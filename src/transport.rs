//! Transport requirements for the I/O adapters.
//!
//! This crate is sans-I/O at its core: [`Session`](crate::session::Session) and
//! [`FileTransfer`](crate::file_transfer::FileTransfer) deal in byte buffers and
//! events, never in sockets or serial ports. The adapter modules lift that core
//! over a real transport.
//!
//! The adapters use the standard library / tokio traits directly:
//!
//! - `adapters::blocking` (feature `blocking`) takes any
//!   `T: std::io::Read + std::io::Write`.
//! - `adapters::tokio` (feature `tokio`) takes any
//!   `T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin`.
//! - `adapters::serialport` (feature `serial`) plugs the
//!   `serialport` crate's [`SerialPort`] trait into the blocking adapter
//!   (it already implements `Read + Write`).
//!
//! Custom transports — TCP-to-serial bridges, USB CDC via `rusb`, in-memory
//! pipes for testing — just need to implement those standard traits.
//!
//! [`SerialPort`]: https://docs.rs/serialport/latest/serialport/trait.SerialPort.html
