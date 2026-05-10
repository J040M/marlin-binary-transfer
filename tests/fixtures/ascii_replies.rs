// Canonical ASCII reply lines from the device. These are what the host parses
// out of the inbound serial stream once binary mode is engaged.
//
// Cross-checked against the Python reference's token list and reply parsing in
// trippwill/marlin-binary-protocol/binproto2/protocols.py.

#![allow(dead_code)]

// ---- Control plane (protocol = 0) -----------------------------------------

/// Generic ack: `ok<n>\n` where <n> is the sync number as decimal text.
pub const REPLY_OK_0:   &[u8] = b"ok0\n";
pub const REPLY_OK_42:  &[u8] = b"ok42\n";
pub const REPLY_OK_255: &[u8] = b"ok255\n";

/// Resend request: `rs<n>\n`.
pub const REPLY_RS_5: &[u8] = b"rs5\n";

/// Sync handshake response: `ss<sync>,<max_block_size>,<protocol_version>\n`.
pub const REPLY_SS: &[u8] = b"ss0,512,1.0\n";

/// Fatal error.
pub const REPLY_FE: &[u8] = b"fe\n";

// ---- File-transfer plane (protocol = 1) -----------------------------------

/// QUERY response: `PFT:version:<v>:<compression-spec>\n`. Two canonical
/// shapes — heatshrink-capable and none-only.
pub const REPLY_PFT_VERSION_HEATSHRINK: &[u8] = b"PFT:version:1.0:heatshrink,8,4\n";
pub const REPLY_PFT_VERSION_NONE:       &[u8] = b"PFT:version:1.0:none\n";

/// OPEN / CLOSE / ABORT replies.
pub const REPLY_PFT_SUCCESS: &[u8] = b"PFT:success\n";
pub const REPLY_PFT_BUSY:    &[u8] = b"PFT:busy\n";
pub const REPLY_PFT_FAIL:    &[u8] = b"PFT:fail\n";
pub const REPLY_PFT_IOERROR: &[u8] = b"PFT:ioerror\n";
pub const REPLY_PFT_INVALID: &[u8] = b"PFT:invalid\n";
