//! Optional heatshrink payload compression.
//!
//! Behind the `heatshrink` feature flag. Wraps the `embedded-heatshrink`
//! crate's stream-style API into byte-slice helpers that match the chunk
//! sizes our file-transfer code works in.
//!
//! The host compresses each chunk before passing it to
//! [`FileTransfer::write`](crate::file_transfer::FileTransfer::write); the
//! device decompresses on the fly using the same parameters. Window and
//! lookahead must match what the device advertised during QUERY.

#[cfg(feature = "heatshrink")]
mod imp {
    use std::io::Cursor;

    use thiserror::Error;

    /// Errors returned by the heatshrink wrappers.
    #[derive(Debug, Error)]
    pub enum HeatshrinkError {
        /// Window or lookahead parameters were rejected by the heatshrink
        /// implementation as out-of-range.
        #[error("invalid heatshrink parameters: window={window}, lookahead={lookahead}")]
        InvalidParameters {
            /// log2 sliding-window size that was rejected.
            window: u8,
            /// log2 lookahead size that was rejected.
            lookahead: u8,
        },
    }

    /// Compress `input` using heatshrink with the given window/lookahead
    /// parameters. The parameters MUST match what the device advertised
    /// during QUERY.
    pub fn compress(input: &[u8], window: u8, lookahead: u8) -> Result<Vec<u8>, HeatshrinkError> {
        validate(window, lookahead)?;
        let mut input_cursor = Cursor::new(input);
        let mut output = Vec::with_capacity(input.len());
        embedded_heatshrink::encode(window, lookahead, &mut input_cursor, &mut output);
        Ok(output)
    }

    /// Decompress `input`. The device decompresses during real transfers;
    /// this is here for the sake of round-trip tests.
    pub fn decompress(input: &[u8], window: u8, lookahead: u8) -> Result<Vec<u8>, HeatshrinkError> {
        validate(window, lookahead)?;
        let mut input_cursor = Cursor::new(input);
        let mut output = Vec::with_capacity(input.len() * 2);
        embedded_heatshrink::decode(window, lookahead, &mut input_cursor, &mut output);
        Ok(output)
    }

    fn validate(window: u8, lookahead: u8) -> Result<(), HeatshrinkError> {
        use embedded_heatshrink::{
            HEATSHRINK_MAX_WINDOW_BITS, HEATSHRINK_MIN_LOOKAHEAD_BITS, HEATSHRINK_MIN_WINDOW_BITS,
        };
        if !(HEATSHRINK_MIN_WINDOW_BITS..=HEATSHRINK_MAX_WINDOW_BITS).contains(&window)
            || lookahead < HEATSHRINK_MIN_LOOKAHEAD_BITS
            || lookahead >= window
        {
            return Err(HeatshrinkError::InvalidParameters { window, lookahead });
        }
        Ok(())
    }
}

#[cfg(feature = "heatshrink")]
pub use imp::{compress, decompress, HeatshrinkError};
