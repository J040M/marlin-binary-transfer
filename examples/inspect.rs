//! Decode a captured byte stream and print packet structure. Useful for
//! debugging real-printer captures.
//!
//! ```sh
//! cargo run --example inspect -- some-capture.bin
//! ```

use marlin_binary_transfer::codec::{decode, DecodeError};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: inspect <FILE>")?;
    let bytes = std::fs::read(&path)?;
    let mut offset = 0;
    while offset < bytes.len() {
        match decode(&bytes[offset..]) {
            Ok((pkt, consumed)) => {
                println!(
                    "@{offset:04x} sync={:3} proto={} type={} payload={}B",
                    pkt.sync,
                    pkt.protocol,
                    pkt.packet_type,
                    pkt.payload.len()
                );
                offset += consumed;
            }
            Err(DecodeError::Incomplete { need, have }) => {
                println!("@{offset:04x} INCOMPLETE need={need} have={have}");
                break;
            }
            Err(e) => {
                println!("@{offset:04x} ERROR {e}; skipping 1 byte");
                offset += 1;
            }
        }
    }
    Ok(())
}
