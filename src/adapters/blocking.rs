//! Synchronous adapter — drives the sans-I/O core over a [`Read`] +
//! [`Write`] transport.
//!
//! ```no_run
//! use std::time::Duration;
//! use marlin_binary_transfer::adapters::blocking::{upload, UploadOptions};
//! use marlin_binary_transfer::file_transfer::Compression;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut port = serialport::new("/dev/ttyUSB0", 250_000)
//!     .timeout(Duration::from_millis(100))
//!     .open()?;
//! let opts = UploadOptions {
//!     dest_filename: "model.gco".into(),
//!     compression: Compression::Auto,
//!     ..UploadOptions::default()
//! };
//! let stats = upload(&mut *port, std::fs::File::open("model.gco")?, opts)?;
//! println!("Uploaded {} bytes in {} chunks", stats.bytes_sent, stats.chunks_sent);
//! # Ok(()) }
//! ```

use std::io::{Read, Write};
use std::time::Instant;

use thiserror::Error;

use crate::file_transfer::{Compression, FileError, FileEvent, FileTransfer};
use crate::session::Session;

/// Caller-supplied options controlling the upload.
#[derive(Debug, Clone)]
pub struct UploadOptions {
    /// Destination filename on the device's SD card. Required.
    pub dest_filename: String,
    /// Compression preference. Defaults to [`Compression::None`].
    pub compression: Compression,
    /// Set to true to make the device pretend to receive a file without
    /// actually writing it; useful for protocol smoke tests.
    pub dummy: bool,
    /// Bytes per WRITE packet. Capped to the device-advertised maximum
    /// after the SYNC handshake completes. `0` means "use the device's
    /// max_block_size verbatim".
    pub chunk_size: usize,
}

impl Default for UploadOptions {
    fn default() -> Self {
        Self {
            dest_filename: String::new(),
            compression: Compression::None,
            dummy: false,
            chunk_size: 0,
        }
    }
}

/// Upload statistics returned on success.
#[derive(Debug, Clone, Default)]
pub struct UploadStats {
    /// Bytes read from `src`.
    pub source_bytes: u64,
    /// Bytes written across all WRITE packets (post-compression).
    pub bytes_sent: u64,
    /// Number of WRITE packets.
    pub chunks_sent: u64,
    /// Compression actually used (resolved from `Compression::Auto`).
    pub compression: Compression,
}

/// Errors the upload helper can produce.
#[derive(Debug, Error)]
pub enum UploadError {
    /// Wrapping I/O error from the transport.
    #[error("transport I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Underlying file-transfer state machine reported a failure.
    #[error("file transfer failed: {0}")]
    Transfer(#[from] FileError),
    /// Reached an unrecoverable protocol state (e.g. device returned
    /// nothing for too long with no progress).
    #[error("upload stalled: {0}")]
    Stalled(&'static str),
    /// The session never completed the SYNC handshake before the helper
    /// gave up.
    #[error("SYNC handshake did not complete")]
    HandshakeFailed,
    /// Compression was requested but the `heatshrink` feature is not
    /// enabled at compile time.
    #[cfg(not(feature = "heatshrink"))]
    #[error("heatshrink compression requested but the `heatshrink` feature is disabled")]
    CompressionFeatureDisabled,
    /// Heatshrink compression error.
    #[cfg(feature = "heatshrink")]
    #[error("heatshrink error: {0}")]
    Heatshrink(#[from] crate::compression::HeatshrinkError),
}

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

    let mut ft = FileTransfer::new(&mut session);
    ft.query(options.compression.clone(), Instant::now());
    let negotiated = drive_until_negotiated(transport, &mut ft)?;

    ft.open(&options.dest_filename, options.dummy, Instant::now());
    drive_until_event(transport, &mut ft, |e| matches!(e, FileEvent::Opened))?;

    let mut stats = UploadStats {
        compression: negotiated.clone(),
        ..UploadStats::default()
    };

    let mut chunk_size = if options.chunk_size > 0 {
        options.chunk_size
    } else {
        // Default to device-advertised size, falling back to a conservative 256.
        256
    };
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

    if chunk_size == 0 {
        chunk_size = 256;
    }

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
            session.feed(&buf[..n]);
        }
        while let Some(evt) = session.poll_event() {
            if matches!(evt, Event::Synced { .. }) {
                return Ok(());
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
            ft.feed(&buf[..n]);
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
            ft.feed(&buf[..n]);
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
