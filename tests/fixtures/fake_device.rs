//! In-process fake of a Marlin device speaking the BFT Mark II protocol.
//!
//! Decodes incoming binary packets and emits the corresponding ASCII reply
//! lines a real printer would. Used by integration tests so we can exercise
//! the host-side state machines without hardware.
//!
//! Behaviour mirrors the Python reference for the happy path. Optional
//! corruption knobs let tests inject bit-flips or dropped bytes into the
//! host's outbound stream so the resend/timeout paths can be exercised
//! deterministically.

use marlin_binary_transfer::codec::{decode, DecodeError};

/// Built-in compression spec the device advertises in its QUERY response.
#[derive(Debug, Clone)]
pub enum CompressionSpec {
    None,
    Heatshrink { window: u8, lookahead: u8 },
}

impl CompressionSpec {
    fn render(&self) -> String {
        match self {
            CompressionSpec::None => "none".into(),
            CompressionSpec::Heatshrink { window, lookahead } => {
                format!("heatshrink,{window},{lookahead}")
            }
        }
    }
}

/// Optional behaviours a test can switch on.
#[derive(Debug, Default, Clone)]
pub struct DeviceBehaviour {
    /// If set, the device will request a resend of this packet sync number
    /// once before acking it. Used to exercise `rs<n>` handling.
    pub resend_once_for_sync: Option<u8>,
    /// If set, the device will reply to the next OPEN with `PFT:busy`
    /// before the test rearms it.
    pub busy_on_next_open: bool,
    /// If set, the device will reply `PFT:fail` to the next OPEN.
    pub fail_on_next_open: bool,
    /// If set, CLOSE / ABORT replies omit the `PFT:success` preamble —
    /// the device just sends the bare `ok<n>` ack. Used to exercise
    /// the host's protocol-violation detection.
    pub skip_pft_on_terminal: bool,
}

/// In-process fake of a Marlin printer speaking BFT.
pub struct FakeDevice {
    inbound: Vec<u8>,
    reply_buf: Vec<u8>,
    initial_sync: u8,
    max_block_size: u16,
    protocol_version: String,
    compression: CompressionSpec,
    pub behaviour: DeviceBehaviour,
    pub last_open_filename: Option<String>,
    pub last_open_dummy: Option<bool>,
    pub last_open_compression_byte: Option<u8>,
    pub written_bytes: Vec<u8>,
    pub aborted: bool,
    pub closed: bool,
}

impl FakeDevice {
    pub fn new(max_block_size: u16, protocol_version: &str, initial_sync: u8) -> Self {
        Self {
            inbound: Vec::new(),
            reply_buf: Vec::new(),
            initial_sync,
            max_block_size,
            protocol_version: protocol_version.into(),
            compression: CompressionSpec::None,
            behaviour: DeviceBehaviour::default(),
            last_open_filename: None,
            last_open_dummy: None,
            last_open_compression_byte: None,
            written_bytes: Vec::new(),
            aborted: false,
            closed: false,
        }
    }

    pub fn with_compression(mut self, spec: CompressionSpec) -> Self {
        self.compression = spec;
        self
    }

    pub fn with_behaviour(mut self, behaviour: DeviceBehaviour) -> Self {
        self.behaviour = behaviour;
        self
    }

    /// Push host-side bytes into the device. Decoded packets are processed
    /// and any reply bytes are queued; pull them with `drain_reply`.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.inbound.extend_from_slice(bytes);
        loop {
            match decode(&self.inbound) {
                Ok((pkt, consumed)) => {
                    let pkt_owned = OwnedPacket {
                        sync: pkt.sync,
                        protocol: pkt.protocol,
                        packet_type: pkt.packet_type,
                        payload: pkt.payload.to_vec(),
                    };
                    self.inbound.drain(..consumed);
                    self.handle(pkt_owned);
                }
                Err(DecodeError::Incomplete { .. }) => break,
                Err(_) => {
                    // Corrupt frame on the wire — request a resend of
                    // whatever sync we last expected. With our minimal
                    // fake we just drop one byte and retry; production
                    // tests can reach into the reply buffer if they need
                    // to assert a specific `rs<n>`.
                    if !self.inbound.is_empty() {
                        self.inbound.remove(0);
                    } else {
                        break;
                    }
                }
            }
        }
    }

    /// Drain any reply bytes the host should `feed()` back into the session.
    pub fn drain_reply(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.reply_buf)
    }

    fn write_line(&mut self, line: &str) {
        self.reply_buf.extend_from_slice(line.as_bytes());
        self.reply_buf.push(b'\n');
    }

    fn handle(&mut self, pkt: OwnedPacket) {
        // Optional one-shot "request resend" behaviour.
        if Some(pkt.sync) == self.behaviour.resend_once_for_sync {
            self.behaviour.resend_once_for_sync = None;
            self.write_line(&format!("rs{}", pkt.sync));
            return;
        }

        match (pkt.protocol, pkt.packet_type) {
            // Control plane.
            (0, 1) => {
                // SYNC: respond with `ss<sync>,<max>,<version>` and nothing
                // else (no `ok<n>`, the ss line itself is the ack).
                self.write_line(&format!(
                    "ss{},{},{}",
                    self.initial_sync, self.max_block_size, self.protocol_version
                ));
            }
            (0, 2) => {
                // Control CLOSE.
                self.write_line(&format!("ok{}", pkt.sync));
            }
            // File-transfer plane.
            (1, 0) => {
                // QUERY: emit the version/compression line, then ack.
                self.write_line(&format!(
                    "PFT:version:{}:{}",
                    self.protocol_version,
                    self.compression.render()
                ));
                self.write_line(&format!("ok{}", pkt.sync));
            }
            (1, 1) => {
                // OPEN: parse payload [dummy:u8, compression:u8, name..., 0].
                if pkt.payload.len() >= 3 {
                    self.last_open_dummy = Some(pkt.payload[0] != 0);
                    self.last_open_compression_byte = Some(pkt.payload[1]);
                    let name_end = pkt.payload[2..]
                        .iter()
                        .position(|&b| b == 0)
                        .map(|p| p + 2)
                        .unwrap_or(pkt.payload.len());
                    self.last_open_filename =
                        Some(String::from_utf8_lossy(&pkt.payload[2..name_end]).into_owned());
                }
                if self.behaviour.busy_on_next_open {
                    self.behaviour.busy_on_next_open = false;
                    self.write_line("PFT:busy");
                } else if self.behaviour.fail_on_next_open {
                    self.behaviour.fail_on_next_open = false;
                    self.write_line("PFT:fail");
                } else {
                    self.write_line("PFT:success");
                }
                self.write_line(&format!("ok{}", pkt.sync));
            }
            (1, 2) => {
                // File CLOSE.
                self.closed = true;
                if !self.behaviour.skip_pft_on_terminal {
                    self.write_line("PFT:success");
                }
                self.write_line(&format!("ok{}", pkt.sync));
            }
            (1, 3) => {
                // WRITE: store payload, ack.
                self.written_bytes.extend_from_slice(&pkt.payload);
                self.write_line(&format!("ok{}", pkt.sync));
            }
            (1, 4) => {
                // ABORT.
                self.aborted = true;
                if !self.behaviour.skip_pft_on_terminal {
                    self.write_line("PFT:success");
                }
                self.write_line(&format!("ok{}", pkt.sync));
            }
            _ => {
                // Unknown packet type: ack so the host doesn't stall, but
                // don't claim file-transfer success.
                self.write_line(&format!("ok{}", pkt.sync));
            }
        }
    }
}

struct OwnedPacket {
    sync: u8,
    protocol: u8,
    packet_type: u8,
    payload: Vec<u8>,
}
