//! Codec layer integration tests: encode/decode round-trip across every
//! packet shape, plus structural validation of the canonical fixtures.

use marlin_binary_transfer::codec::{
    self, decode, encode, fletcher16, DecodeError, EncodeError, Packet, HEADER_LEN, MAX_PAYLOAD,
    PACKET_TOKEN,
};

#[path = "fixtures/canonical.rs"]
#[allow(dead_code)]
mod canonical;

use canonical::{
    ABORT_PACKET, CLOSE_FILE_PACKET, CONTROL_CLOSE_PACKET, FLETCHER16_VECTORS, OPEN_PACKET,
    QUERY_PACKET, SYNC_PACKET, WRITE_PACKET,
};

const ALL_FIXTURES: &[(&str, &[u8])] = &[
    ("SYNC", SYNC_PACKET),
    ("CONTROL_CLOSE", CONTROL_CLOSE_PACKET),
    ("QUERY", QUERY_PACKET),
    ("OPEN", OPEN_PACKET),
    ("WRITE", WRITE_PACKET),
    ("CLOSE_FILE", CLOSE_FILE_PACKET),
    ("ABORT", ABORT_PACKET),
];

// ---- Fletcher-16 ------------------------------------------------------------

#[test]
fn fletcher16_matches_python_reference() {
    for &(input, expected) in FLETCHER16_VECTORS {
        let got = fletcher16(input);
        assert_eq!(
            got, expected,
            "Fletcher-16 mismatch on {:02X?}: expected {:#06x}, got {:#06x}",
            input, expected, got
        );
    }
}

#[test]
fn fletcher16_known_string_vectors() {
    assert_eq!(fletcher16(b""), 0x0000);
    assert_eq!(fletcher16(b"\x00"), 0x0000);
    assert_eq!(fletcher16(b"\xff"), 0x0000);
    assert_eq!(fletcher16(b"abc"), 0x4C27);
    assert_eq!(
        fletcher16(b"The quick brown fox jumps over the lazy dog"),
        0xFEE8
    );
}

// ---- Round-trip encode/decode -----------------------------------------------

#[test]
fn round_trip_empty_payload() {
    let pkt = Packet::new(0, 0, 1, b"").unwrap();
    let mut buf = Vec::new();
    let written = encode(&pkt, &mut buf).unwrap();
    assert_eq!(written, HEADER_LEN);
    let (decoded, consumed) = decode(&buf).unwrap();
    assert_eq!(consumed, HEADER_LEN);
    assert_eq!(decoded, pkt);
}

#[test]
fn round_trip_with_payload() {
    let payload = b"hello";
    let pkt = Packet::new(7, 1, 3, payload).unwrap();
    let mut buf = Vec::new();
    let written = encode(&pkt, &mut buf).unwrap();
    assert_eq!(written, HEADER_LEN + payload.len() + 2);
    let (decoded, consumed) = decode(&buf).unwrap();
    assert_eq!(consumed, written);
    assert_eq!(decoded, pkt);
}

#[test]
fn round_trip_max_proto_and_type_nibbles() {
    let payload = vec![0xAB; 1024];
    let pkt = Packet::new(0xFF, 0xF, 0xF, &payload).unwrap();
    let mut buf = Vec::new();
    encode(&pkt, &mut buf).unwrap();
    let (decoded, _) = decode(&buf).unwrap();
    assert_eq!(decoded.sync, 0xFF);
    assert_eq!(decoded.protocol, 0xF);
    assert_eq!(decoded.packet_type, 0xF);
    assert_eq!(decoded.payload, &payload[..]);
}

#[test]
fn decode_returns_consumed_byte_count_for_concatenated_packets() {
    let mut buf = Vec::new();
    encode(&Packet::new(0, 0, 1, b"").unwrap(), &mut buf).unwrap();
    let first_end = buf.len();
    encode(&Packet::new(1, 1, 3, b"abc").unwrap(), &mut buf).unwrap();

    let (p1, consumed1) = decode(&buf).unwrap();
    assert_eq!(consumed1, first_end);
    assert_eq!(p1.sync, 0);
    assert_eq!(p1.protocol, 0);
    assert_eq!(p1.packet_type, 1);

    let (p2, consumed2) = decode(&buf[consumed1..]).unwrap();
    assert_eq!(consumed1 + consumed2, buf.len());
    assert_eq!(p2.sync, 1);
    assert_eq!(p2.payload, b"abc");
}

// ---- Encode validation ------------------------------------------------------

#[test]
fn encode_rejects_out_of_range_protocol() {
    assert_eq!(
        Packet::new(0, 16, 0, b""),
        Err(EncodeError::BadProtocol(16))
    );
}

#[test]
fn encode_rejects_out_of_range_packet_type() {
    assert_eq!(
        Packet::new(0, 0, 16, b""),
        Err(EncodeError::BadPacketType(16))
    );
}

// ---- Decode error paths -----------------------------------------------------

#[test]
fn decode_rejects_short_buffer() {
    assert_eq!(
        decode(&[0xAD, 0xB5]),
        Err(DecodeError::Incomplete {
            need: HEADER_LEN,
            have: 2
        })
    );
}

#[test]
fn decode_rejects_bad_token() {
    let mut buf = Vec::new();
    encode(&Packet::new(0, 0, 1, b"").unwrap(), &mut buf).unwrap();
    buf[0] ^= 0xFF;
    assert!(matches!(decode(&buf), Err(DecodeError::BadToken { .. })));
}

#[test]
fn decode_rejects_corrupt_header_checksum_byte() {
    let mut buf = Vec::new();
    encode(&Packet::new(0, 0, 1, b"").unwrap(), &mut buf).unwrap();
    buf[6] ^= 0xFF;
    assert!(matches!(
        decode(&buf),
        Err(DecodeError::HeaderChecksum { .. })
    ));
}

#[test]
fn decode_rejects_corrupt_payload_checksum_byte() {
    let mut buf = Vec::new();
    encode(&Packet::new(0, 1, 3, b"hello").unwrap(), &mut buf).unwrap();
    let len = buf.len();
    buf[len - 1] ^= 0xFF;
    assert!(matches!(
        decode(&buf),
        Err(DecodeError::PayloadChecksum { .. })
    ));
}

#[test]
fn decode_rejects_corrupt_payload_data_byte() {
    let mut buf = Vec::new();
    encode(&Packet::new(0, 1, 3, b"hello").unwrap(), &mut buf).unwrap();
    buf[HEADER_LEN] ^= 0xFF;
    assert!(matches!(
        decode(&buf),
        Err(DecodeError::PayloadChecksum { .. })
    ));
}

#[test]
fn decode_reports_incomplete_for_truncated_payload() {
    let mut buf = Vec::new();
    encode(&Packet::new(0, 1, 3, b"hello").unwrap(), &mut buf).unwrap();
    let truncated = &buf[..buf.len() - 3];
    match decode(truncated) {
        Err(DecodeError::Incomplete { need, have }) => {
            assert_eq!(need, buf.len());
            assert_eq!(have, truncated.len());
        }
        other => panic!("expected Incomplete, got {other:?}"),
    }
}

// ---- Canonical fixture conformance ------------------------------------------

#[test]
fn encoder_matches_canonical_sync_packet() {
    let mut buf = Vec::new();
    encode(&Packet::new(0, 0, 1, b"").unwrap(), &mut buf).unwrap();
    assert_eq!(buf, SYNC_PACKET);
}

#[test]
fn encoder_matches_canonical_query_packet() {
    let mut buf = Vec::new();
    encode(&Packet::new(0, 1, 0, b"").unwrap(), &mut buf).unwrap();
    assert_eq!(buf, QUERY_PACKET);
}

#[test]
fn encoder_matches_canonical_control_close_packet() {
    let mut buf = Vec::new();
    encode(&Packet::new(1, 0, 2, b"").unwrap(), &mut buf).unwrap();
    assert_eq!(buf, CONTROL_CLOSE_PACKET);
}

#[test]
fn encoder_matches_canonical_close_file_packet() {
    let mut buf = Vec::new();
    encode(&Packet::new(3, 1, 2, b"").unwrap(), &mut buf).unwrap();
    assert_eq!(buf, CLOSE_FILE_PACKET);
}

#[test]
fn encoder_matches_canonical_abort_packet() {
    let mut buf = Vec::new();
    encode(&Packet::new(4, 1, 4, b"").unwrap(), &mut buf).unwrap();
    assert_eq!(buf, ABORT_PACKET);
}

#[test]
fn encoder_matches_canonical_open_packet() {
    let mut buf = Vec::new();
    encode(
        &Packet::new(1, 1, 1, b"\x00\x00test.gco\x00").unwrap(),
        &mut buf,
    )
    .unwrap();
    assert_eq!(buf, OPEN_PACKET);
}

#[test]
fn encoder_matches_canonical_write_packet() {
    let mut buf = Vec::new();
    encode(&Packet::new(2, 1, 3, b"hello").unwrap(), &mut buf).unwrap();
    assert_eq!(buf, WRITE_PACKET);
}

#[test]
fn decoder_round_trips_every_canonical_fixture() {
    for &(name, fixture) in ALL_FIXTURES {
        let (decoded, consumed) =
            decode(fixture).unwrap_or_else(|e| panic!("{name}: decode failed: {e}"));
        assert_eq!(
            consumed,
            fixture.len(),
            "{name}: decoder consumed {} of {} bytes",
            consumed,
            fixture.len()
        );

        let mut re_encoded = Vec::new();
        encode(&decoded, &mut re_encoded).unwrap();
        assert_eq!(
            re_encoded, fixture,
            "{name}: re-encode disagrees with canonical bytes"
        );
    }
}

// ---- Structural fixture sanity (independent of codec impl) ------------------

#[test]
fn every_canonical_starts_with_token() {
    for &(name, pkt) in ALL_FIXTURES {
        let token = u16::from_le_bytes([pkt[0], pkt[1]]);
        assert_eq!(
            token, PACKET_TOKEN,
            "{name}: token {:#06x} != {:#06x}",
            token, PACKET_TOKEN
        );
    }
}

#[test]
fn every_canonical_length_matches_formula() {
    for &(name, pkt) in ALL_FIXTURES {
        let n = u16::from_le_bytes([pkt[4], pkt[5]]) as usize;
        let expected = if n == 0 {
            HEADER_LEN
        } else {
            HEADER_LEN + n + 2
        };
        assert_eq!(
            pkt.len(),
            expected,
            "{name}: total length {} != formula {}",
            pkt.len(),
            expected
        );
    }
}

#[test]
fn max_payload_constant_matches_field_width() {
    assert_eq!(codec::MAX_PAYLOAD, MAX_PAYLOAD);
    assert_eq!(MAX_PAYLOAD, u16::MAX as usize);
}
