//! Synchronous adapter — drives the sans-I/O core over a [`Read`] +
//! [`Write`] transport.
//!
//! ```no_run
//! use std::io::{Read, Write};
//! use marlin_binary_transfer::adapters::blocking::{upload, UploadOptions};
//! use marlin_binary_transfer::file_transfer::Compression;
//!
//! # fn run<T: Read + Write>(mut port: T) -> Result<(), Box<dyn std::error::Error>> {
//! let opts = UploadOptions {
//!     dest_filename: "model.gco".into(),
//!     compression: Compression::Auto,
//!     ..UploadOptions::default()
//! };
//! let stats = upload(&mut port, std::fs::File::open("model.gco")?, opts)?;
//! println!("Uploaded {} bytes in {} chunks", stats.bytes_sent, stats.chunks_sent);
//! # Ok(()) }
//! ```
//!
//! The example uses any `Read + Write`. With the `serial` feature enabled,
//! [`serialport::SerialPort`](https://docs.rs/serialport/latest/serialport/trait.SerialPort.html)
//! satisfies both traits and works directly; see
//! [`adapters::serialport`](crate::adapters::serialport) for an `open` helper.

use std::io::{Read, Write};
use std::time::Instant;

use crate::adapters::common::resolve_chunk_size;
use crate::file_transfer::{Compression, FileEvent, FileTransfer};
use crate::session::Session;

pub use crate::adapters::common::{UploadError, UploadOptions, UploadStats};

/// Perform a complete upload: SYNC, QUERY, OPEN, WRITE×N, CLOSE.
///
/// On success, `transport` is left synced and idle.
pub fn upload<T: Read + Write + ?Sized, S: Read>(
    transport: &mut T,
    mut src: S,
    options: UploadOptions,
) -> Result<UploadStats, UploadError> {
    // Send the binary-mode trigger as plain ASCII first.
    transport.write_all(b"M28B1\n")?;

    let mut session = Session::new();
    let now = Instant::now();
    session.connect(now);

    drive_session_until_synced(transport, &mut session)?;

    // Capture the device-advertised block size before the FileTransfer
    // borrow takes the session mutably.
    let device_max = session.max_block_size().unwrap_or(0);

    let mut ft = FileTransfer::new(&mut session);
    ft.query(options.compression.clone(), Instant::now());
    let negotiated = drive_until_negotiated(transport, &mut ft)?;

    ft.open(&options.dest_filename, options.dummy, Instant::now());
    drive_until_event(transport, &mut ft, |e| matches!(e, FileEvent::Opened))?;

    let mut stats = UploadStats {
        compression: negotiated.clone(),
        ..UploadStats::default()
    };

    let chunk_size = resolve_chunk_size(options.chunk_size, device_max);
    // Read whole source into memory once, then either compress or chunk
    // through it. Mirrors the Python ref's behaviour and keeps the chunk
    // boundary deterministic.
    let mut source_bytes = Vec::new();
    src.read_to_end(&mut source_bytes)?;
    stats.source_bytes = source_bytes.len() as u64;

    let payload: Vec<u8> = match &negotiated {
        Compression::None => source_bytes,
        Compression::Heatshrink { window, lookahead } => {
            #[cfg(feature = "heatshrink")]
            {
                crate::compression::compress(&source_bytes, *window, *lookahead)?
            }
            #[cfg(not(feature = "heatshrink"))]
            {
                let _ = (window, lookahead);
                return Err(UploadError::CompressionFeatureDisabled);
            }
        }
        Compression::Auto => unreachable!("FileTransfer resolves Auto during query"),
    };

    for chunk in payload.chunks(chunk_size) {
        ft.write(chunk, Instant::now());
        drive_until_event(transport, &mut ft, |e| matches!(e, FileEvent::WriteAcked))?;
        stats.bytes_sent += chunk.len() as u64;
        stats.chunks_sent += 1;
    }

    ft.close(Instant::now());
    drive_until_event(transport, &mut ft, |e| matches!(e, FileEvent::Closed))?;

    Ok(stats)
}

fn drive_session_until_synced<T: Read + Write + ?Sized>(
    transport: &mut T,
    session: &mut Session,
) -> Result<(), UploadError> {
    use crate::file_transfer::FileError;
    use crate::session::Event;
    let mut buf = [0u8; 1024];
    for _ in 0..200 {
        while let Some(out) = session.poll_outbound() {
            transport.write_all(&out)?;
        }
        let n = match transport.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => 0,
            Err(e) => return Err(UploadError::Io(e)),
        };
        if n > 0 {
            session.feed(&buf[..n], Instant::now());
        }
        while let Some(evt) = session.poll_event() {
            match evt {
                Event::Synced { .. } => return Ok(()),
                Event::FatalError => {
                    return Err(UploadError::Transfer(FileError::SessionFatalError));
                }
                Event::Timeout { .. } => {
                    return Err(UploadError::Transfer(FileError::SessionTimeout));
                }
                Event::OutOfSync { expected, got } => {
                    return Err(UploadError::Transfer(FileError::SessionOutOfSync {
                        expected,
                        got,
                    }));
                }
                _ => {}
            }
        }
        session.tick(Instant::now());
    }
    Err(UploadError::HandshakeFailed)
}

fn drive_until_negotiated<T: Read + Write + ?Sized>(
    transport: &mut T,
    ft: &mut FileTransfer<'_>,
) -> Result<Compression, UploadError> {
    let mut buf = [0u8; 1024];
    for _ in 0..200 {
        while let Some(out) = ft.poll_outbound() {
            transport.write_all(&out)?;
        }
        let n = match transport.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => 0,
            Err(e) => return Err(UploadError::Io(e)),
        };
        if n > 0 {
            ft.feed(&buf[..n], Instant::now());
        }
        while let Some(evt) = ft.poll() {
            match evt {
                FileEvent::Negotiated { compression, .. } => return Ok(compression),
                FileEvent::Failed(err) => return Err(UploadError::Transfer(err)),
                _ => {}
            }
        }
        ft.tick(Instant::now());
    }
    Err(UploadError::Stalled("negotiation did not complete"))
}

fn drive_until_event<T: Read + Write + ?Sized, F: Fn(&FileEvent) -> bool>(
    transport: &mut T,
    ft: &mut FileTransfer<'_>,
    pred: F,
) -> Result<(), UploadError> {
    let mut buf = [0u8; 1024];
    for _ in 0..200 {
        while let Some(out) = ft.poll_outbound() {
            transport.write_all(&out)?;
        }
        let n = match transport.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => 0,
            Err(e) => return Err(UploadError::Io(e)),
        };
        if n > 0 {
            ft.feed(&buf[..n], Instant::now());
        }
        while let Some(evt) = ft.poll() {
            if let FileEvent::Failed(err) = &evt {
                return Err(UploadError::Transfer(err.clone()));
            }
            if pred(&evt) {
                return Ok(());
            }
        }
        ft.tick(Instant::now());
    }
    Err(UploadError::Stalled("event did not arrive in time"))
}
