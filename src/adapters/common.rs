//! Shared types used by both the [`blocking`](crate::adapters::blocking) and
//! [`tokio`](crate::adapters::tokio) adapter modules.
//!
//! Living here rather than in either adapter module so that enabling one
//! adapter doesn't require enabling the other to resolve `UploadOptions` /
//! `UploadStats` / `UploadError` symbols.

use thiserror::Error;

use crate::file_transfer::{Compression, FileError};

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
    /// Compression actually used (resolved from [`Compression::Auto`]).
    pub compression: Compression,
}

/// Errors the adapter upload helpers can produce.
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
