//! Layer 1: packet encoding, decoding, and Fletcher-16 checksum.
//!
//! This module deals only in bytes and packet structures. It has no notion
//! of sessions, sync counters, or file transfer.
//!
//! # Wire format
//!
//! ```text
//! offset  size  field                     notes
//! ------  ----  ----------------------    -----------------------------------
//! 0       2     start_token (0xB5AD)      little-endian; NOT in checksum
//! 2       1     sync                      wraps mod 256
//! 3       1     proto<<4 | packet_type    high nibble proto, low nibble type
//! 4       2     payload_len (u16 LE)
//! 6       2     header_checksum (u16)     Fletcher-16 of bytes [2..6]
//! 8       N     payload                   absent if N == 0
//! 8+N     2     payload_checksum (u16)    Fletcher-16 of bytes [2..8+N]
//!                                         ABSENT if payload_len == 0
//! ```

use thiserror::Error;

/// Marlin BFT packet start token. Encoded little-endian on the wire.
pub const PACKET_TOKEN: u16 = 0xB5AD;

/// Header overhead in bytes: token(2) + sync(1) + proto/type(1) + length(2)
/// + header_checksum(2).
pub const HEADER_LEN: usize = 8;

/// Maximum payload size in a single packet (the length field is u16).
pub const MAX_PAYLOAD: usize = u16::MAX as usize;

/// A protocol packet, borrowed from a buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet<'a> {
    /// Sync counter. Wraps mod 256.
    pub sync: u8,
    /// Protocol id. Only the low 4 bits are used; values >15 will fail to encode.
    pub protocol: u8,
    /// Packet type within the protocol. Only the low 4 bits are used.
    pub packet_type: u8,
    /// Payload bytes. May be empty.
    pub payload: &'a [u8],
}

/// Reasons a [`decode`] call may fail.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DecodeError {
    /// Buffer ended before a complete packet could be read.
    #[error("not enough bytes (need {need}, have {have})")]
    Incomplete {
        /// Number of bytes the decoder needed to be present.
        need: usize,
        /// Number of bytes actually available.
        have: usize,
    },
    /// Start token at offset 0 did not match [`PACKET_TOKEN`].
    #[error("start token mismatch: expected {expected:#06x}, got {got:#06x}")]
    BadToken {
        /// The constant token expected.
        expected: u16,
        /// The token actually decoded.
        got: u16,
    },
    /// Recomputed Fletcher-16 over the header bytes did not match the stored value.
    #[error("header checksum mismatch: stored {stored:#06x}, computed {computed:#06x}")]
    HeaderChecksum {
        /// Checksum stored in the packet.
        stored: u16,
        /// Checksum computed from the header bytes.
        computed: u16,
    },
    /// Recomputed Fletcher-16 over the body bytes did not match the stored value.
    #[error("payload checksum mismatch: stored {stored:#06x}, computed {computed:#06x}")]
    PayloadChecksum {
        /// Checksum stored after the payload.
        stored: u16,
        /// Checksum computed from the body bytes.
        computed: u16,
    },
}

/// Reasons an [`encode`] call may fail. None of these can occur for packets
/// constructed via [`Packet::new`], which validates up front.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum EncodeError {
    /// Payload exceeded [`MAX_PAYLOAD`].
    #[error("payload length {len} exceeds maximum {MAX_PAYLOAD}")]
    PayloadTooLarge {
        /// Length the caller attempted to encode.
        len: usize,
    },
    /// Protocol id was greater than 15.
    #[error("protocol id {0} out of range (0..=15)")]
    BadProtocol(u8),
    /// Packet type was greater than 15.
    #[error("packet type {0} out of range (0..=15)")]
    BadPacketType(u8),
}

impl<'a> Packet<'a> {
    /// Construct a packet, validating that the protocol id, packet type,
    /// and payload length all fit on the wire.
    pub fn new(
        sync: u8,
        protocol: u8,
        packet_type: u8,
        payload: &'a [u8],
    ) -> Result<Self, EncodeError> {
        if protocol > 0xF {
            return Err(EncodeError::BadProtocol(protocol));
        }
        if packet_type > 0xF {
            return Err(EncodeError::BadPacketType(packet_type));
        }
        if payload.len() > MAX_PAYLOAD {
            return Err(EncodeError::PayloadTooLarge { len: payload.len() });
        }
        Ok(Self {
            sync,
            protocol,
            packet_type,
            payload,
        })
    }

    /// Total bytes this packet occupies on the wire.
    pub fn wire_len(&self) -> usize {
        if self.payload.is_empty() {
            HEADER_LEN
        } else {
            HEADER_LEN + self.payload.len() + 2
        }
    }
}

/// Fletcher-16, mod-255 variant — exactly matches Marlin's host protocol.
///
/// The 16-bit result has the running byte-sum (`sum1`) in its low byte and
/// the running sum-of-sum1 (`sum2`) in its high byte, both reduced mod 255.
pub fn fletcher16(buf: &[u8]) -> u16 {
    let mut cs: u16 = 0;
    for &b in buf {
        let cs_low = ((cs & 0xFF) + b as u16) % 255;
        cs = ((((cs >> 8) + cs_low) % 255) << 8) | cs_low;
    }
    cs
}

/// Encode a packet, appending the wire bytes to `out`.
///
/// Returns the number of bytes appended.
pub fn encode(packet: &Packet<'_>, out: &mut Vec<u8>) -> Result<usize, EncodeError> {
    if packet.protocol > 0xF {
        return Err(EncodeError::BadProtocol(packet.protocol));
    }
    if packet.packet_type > 0xF {
        return Err(EncodeError::BadPacketType(packet.packet_type));
    }
    if packet.payload.len() > MAX_PAYLOAD {
        return Err(EncodeError::PayloadTooLarge {
            len: packet.payload.len(),
        });
    }

    let start = out.len();

    // 2-byte start token (NOT in checksum).
    out.extend_from_slice(&PACKET_TOKEN.to_le_bytes());

    // 6 header bytes covered by the header checksum: sync(1) + proto/type(1)
    // + payload_len(2) + header_cs(2). We push the first 4 then compute the
    // checksum over them, then push the checksum.
    let header_payload_start = out.len();
    out.push(packet.sync);
    out.push(((packet.protocol & 0xF) << 4) | (packet.packet_type & 0xF));
    out.extend_from_slice(&(packet.payload.len() as u16).to_le_bytes());
    let header_cs = fletcher16(&out[header_payload_start..]);
    out.extend_from_slice(&header_cs.to_le_bytes());

    if !packet.payload.is_empty() {
        out.extend_from_slice(packet.payload);
        let body_end = out.len();
        // Payload checksum covers everything after the start token: the
        // 6-byte header (incl. its checksum) plus the payload itself.
        let payload_cs = fletcher16(&out[header_payload_start..body_end]);
        out.extend_from_slice(&payload_cs.to_le_bytes());
    }

    Ok(out.len() - start)
}

/// Decode a packet from the start of `buf`.
///
/// On success, returns the parsed packet and the number of bytes consumed
/// (i.e. the wire length of the decoded packet). If the buffer is too short
/// to contain a full packet but is otherwise consistent so far, returns
/// [`DecodeError::Incomplete`] — callers can read more bytes and retry.
pub fn decode(buf: &[u8]) -> Result<(Packet<'_>, usize), DecodeError> {
    if buf.len() < HEADER_LEN {
        return Err(DecodeError::Incomplete {
            need: HEADER_LEN,
            have: buf.len(),
        });
    }

    let token = u16::from_le_bytes([buf[0], buf[1]]);
    if token != PACKET_TOKEN {
        return Err(DecodeError::BadToken {
            expected: PACKET_TOKEN,
            got: token,
        });
    }

    let sync = buf[2];
    let proto_type = buf[3];
    let protocol = proto_type >> 4;
    let packet_type = proto_type & 0x0F;
    let payload_len = u16::from_le_bytes([buf[4], buf[5]]) as usize;
    let stored_header_cs = u16::from_le_bytes([buf[6], buf[7]]);

    let computed_header_cs = fletcher16(&buf[2..6]);
    if stored_header_cs != computed_header_cs {
        return Err(DecodeError::HeaderChecksum {
            stored: stored_header_cs,
            computed: computed_header_cs,
        });
    }

    if payload_len == 0 {
        return Ok((
            Packet {
                sync,
                protocol,
                packet_type,
                payload: &[],
            },
            HEADER_LEN,
        ));
    }

    let total_len = HEADER_LEN + payload_len + 2;
    if buf.len() < total_len {
        return Err(DecodeError::Incomplete {
            need: total_len,
            have: buf.len(),
        });
    }

    let payload = &buf[HEADER_LEN..HEADER_LEN + payload_len];
    let cs_off = HEADER_LEN + payload_len;
    let stored_payload_cs = u16::from_le_bytes([buf[cs_off], buf[cs_off + 1]]);
    let computed_payload_cs = fletcher16(&buf[2..cs_off]);
    if stored_payload_cs != computed_payload_cs {
        return Err(DecodeError::PayloadChecksum {
            stored: stored_payload_cs,
            computed: computed_payload_cs,
        });
    }

    Ok((
        Packet {
            sync,
            protocol,
            packet_type,
            payload,
        },
        total_len,
    ))
}
