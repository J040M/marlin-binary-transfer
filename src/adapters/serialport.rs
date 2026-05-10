//! Convenience integration with the [`serialport`] crate.
//!
//! [`serialport::SerialPort`] already implements [`std::io::Read`] +
//! [`std::io::Write`], so the [blocking adapter](crate::adapters::blocking)
//! works on it directly. This module provides a tiny `open` helper that
//! configures sensible defaults for talking to Marlin in binary mode.

use std::time::Duration;

use ::serialport::{SerialPort, SerialPortBuilder};

/// Open a serial port with defaults suitable for Marlin BFT:
///
/// - 8N1
/// - read timeout 100 ms (so the host can pump retransmits without
///   hanging in `read`)
/// - DTR/RTS not asserted explicitly — the OS-default behaviour applies
pub fn open(path: &str, baud_rate: u32) -> std::io::Result<Box<dyn SerialPort>> {
    builder(path, baud_rate).open().map_err(map_err)
}

/// Return a [`SerialPortBuilder`] preconfigured with the same defaults
/// as [`open`], but unopened, so the caller can apply further tweaks
/// before calling `.open()`.
pub fn builder(path: &str, baud_rate: u32) -> SerialPortBuilder {
    ::serialport::new(path, baud_rate).timeout(Duration::from_millis(100))
}

fn map_err(e: ::serialport::Error) -> std::io::Error {
    std::io::Error::other(e.to_string())
}
