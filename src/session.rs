//! Layer 2: sans-I/O session state machine.
//!
//! Owns the sync counter, the outbound queue, and the inbound ASCII line
//! parser. Callers drive it with `feed` / `poll_outbound` / `poll_event` /
//! `tick`, plumbing the bytes through any I/O of their choice.
//!
//! # Lifecycle
//!
//! 1. Caller writes the ASCII trigger `b"M28B1\n"` to the device.
//! 2. Caller calls [`Session::connect`], drains `poll_outbound`, writes
//!    those bytes to the device.
//! 3. Caller reads bytes from the device and pushes them via
//!    [`Session::feed`].
//! 4. Caller drains [`Session::poll_event`] until [`Event::Synced`] is
//!    observed — the device has acknowledged the SYNC and reported its
//!    block size and protocol version.
//! 5. Caller calls [`Session::send`] for each subsequent binary packet,
//!    pumping `poll_outbound` / `feed` / `poll_event` as before, calling
//!    [`Session::tick`] periodically so retransmits fire on timeout.
//!
//! # Concurrency model
//!
//! Mirrors the Python reference: only one packet is in flight at a time.
//! Calls to [`Session::send`] while a packet is in flight are queued FIFO
//! and dispatched as each ack arrives.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::codec::{self, Packet};

/// Maximum sync-counter value before wrapping to 0.
const SYNC_MOD: u16 = 256;

/// Default per-attempt response timeout.
const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_millis(1000);
/// Default total budget for a single packet (= 20 attempts at the per-attempt timeout).
const DEFAULT_TOTAL_TIMEOUT: Duration = Duration::from_secs(20);

/// Things the session emits to the caller as parsed bytes arrive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Sync handshake completed. Carries the device's reported block size
    /// and protocol version.
    Synced {
        /// Device-advertised maximum payload bytes per packet.
        max_block_size: u16,
        /// Device-advertised protocol version string.
        protocol_version: String,
    },
    /// An `ok<n>` line was received, acknowledging the packet with sync `n`.
    Ack(u8),
    /// A `rs<n>` line was received: the device is requesting we retransmit
    /// the packet with sync `n`.
    ResendRequested(u8),
    /// A line was received that did not match any known control token.
    /// The file-transfer layer consumes these to parse `PFT:*` replies.
    AsciiLine(String),
    /// A `fe` line was received: device reports a fatal protocol error.
    FatalError,
    /// The session received an `ok<m>` whose number did not match the
    /// in-flight packet's sync. Recovery requires calling
    /// [`Session::reset`] then [`Session::connect`] — the protocol
    /// has no way to resynchronise mid-stream.
    OutOfSync {
        /// Sync number we expected an ack for.
        expected: u8,
        /// Sync number the device acked.
        got: u8,
    },
    /// A queued packet exceeded its total retransmit budget without an ack.
    /// The packet has been dropped from the queue.
    Timeout {
        /// Sync number of the packet that timed out.
        sync: u8,
    },
}

#[derive(Debug)]
struct InFlight {
    sync: u8,
    bytes: Vec<u8>,
    first_sent: Instant,
    last_sent: Instant,
    /// True until the SYNC handshake completes; an `ss` reply consumes
    /// this packet rather than `ok<n>`.
    is_sync_handshake: bool,
}

#[derive(Debug)]
struct Queued {
    /// Will be assigned a sync number at dispatch time so retransmits in
    /// the meantime don't bump the counter underneath us.
    bytes_without_sync: BytesBuilder,
    is_sync_handshake: bool,
}

/// Pre-built packet bytes minus the parts that depend on the sync number.
/// We rebuild the header on dispatch so the sync number reflects whatever
/// was last `Synced` from the device (and so that retransmits use the same
/// counter).
#[derive(Debug, Clone)]
struct BytesBuilder {
    protocol: u8,
    packet_type: u8,
    payload: Vec<u8>,
}

impl BytesBuilder {
    fn build(&self, sync: u8) -> Vec<u8> {
        let mut out = Vec::with_capacity(codec::HEADER_LEN + self.payload.len() + 2);
        let pkt = Packet::new(sync, self.protocol, self.packet_type, &self.payload)
            .expect("session validates protocol/type/length at queue time");
        codec::encode(&pkt, &mut out).expect("validation already passed");
        out
    }
}

/// Sans-I/O session driver.
#[derive(Debug)]
pub struct Session {
    sync: u8,
    is_synced: bool,
    max_block_size: Option<u16>,
    protocol_version: Option<String>,

    in_flight: Option<InFlight>,
    queued: VecDeque<Queued>,
    outbound: VecDeque<Vec<u8>>,
    events: VecDeque<Event>,
    inbound_buf: Vec<u8>,

    response_timeout: Duration,
    total_timeout: Duration,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// Construct a fresh, unconnected session with default timeouts.
    pub fn new() -> Self {
        Self {
            sync: 0,
            is_synced: false,
            max_block_size: None,
            protocol_version: None,
            in_flight: None,
            queued: VecDeque::new(),
            outbound: VecDeque::new(),
            events: VecDeque::new(),
            inbound_buf: Vec::with_capacity(256),
            response_timeout: DEFAULT_RESPONSE_TIMEOUT,
            total_timeout: DEFAULT_TOTAL_TIMEOUT,
        }
    }

    /// Set the per-attempt response timeout. Retransmits fire after this
    /// long without an ack.
    pub fn with_response_timeout(mut self, timeout: Duration) -> Self {
        self.response_timeout = timeout;
        self
    }

    /// Set the total budget for a single packet across all retransmits.
    /// When this elapses, the packet is dropped and [`Event::Timeout`] is
    /// emitted.
    pub fn with_total_timeout(mut self, timeout: Duration) -> Self {
        self.total_timeout = timeout;
        self
    }

    /// Current per-attempt response timeout. Adapters wrap their inbound
    /// reads in this so [`Self::tick`] can fire even when the transport
    /// stays idle.
    pub fn response_timeout(&self) -> Duration {
        self.response_timeout
    }

    /// Current total per-packet budget across all retransmits.
    pub fn total_timeout(&self) -> Duration {
        self.total_timeout
    }

    /// True once an `ss` handshake reply has been received.
    pub fn is_synced(&self) -> bool {
        self.is_synced
    }

    /// Device-advertised maximum payload bytes per packet, set during the
    /// SYNC handshake.
    pub fn max_block_size(&self) -> Option<u16> {
        self.max_block_size
    }

    /// Device-advertised protocol version, set during the SYNC handshake.
    pub fn protocol_version(&self) -> Option<&str> {
        self.protocol_version.as_deref()
    }

    /// Current sync counter value. Diagnostic accessor used in tests; not
    /// load-bearing for protocol clients.
    pub fn current_sync(&self) -> u8 {
        self.sync
    }

    /// True if a packet is currently in flight (sent, awaiting ack).
    pub fn has_pending(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Reset the session to its construction baseline.
    ///
    /// Drops any in-flight packet, queued packets, pending outbound
    /// bytes, pending events, and inbound buffer; clears the sync
    /// counter, sync state, and device-advertised values. Timeouts
    /// (`response_timeout` / `total_timeout`) are preserved.
    ///
    /// Use this after observing [`Event::OutOfSync`] before calling
    /// [`connect`](Self::connect) again: the BFT protocol has no
    /// way to resynchronise mid-stream, so the only recovery path is
    /// to clear local state and redo the handshake from scratch.
    pub fn reset(&mut self) {
        self.sync = 0;
        self.is_synced = false;
        self.max_block_size = None;
        self.protocol_version = None;
        self.in_flight = None;
        self.queued.clear();
        self.outbound.clear();
        self.events.clear();
        self.inbound_buf.clear();
    }

    /// Queue the SYNC control packet (protocol=0, packet_type=1).
    /// The caller should already have written the ASCII trigger
    /// `b"M28B1\n"` before calling this.
    pub fn connect(&mut self, now: Instant) {
        self.queue(0, 1, &[], /* is_sync_handshake = */ true);
        self.dispatch_if_idle(now);
    }

    /// Queue a binary packet for transmission.
    ///
    /// If the session is idle (no packet in flight), the bytes are pushed
    /// to the outbound queue immediately. Otherwise the packet waits until
    /// the in-flight packet is acked.
    ///
    /// # Panics
    ///
    /// Panics if:
    /// - the session has not yet observed an `ss` handshake reply
    ///   (`is_synced() == false`) — without a device-confirmed sync
    ///   counter, the packet would go out with `sync=0` and almost
    ///   certainly desynchronise the protocol;
    /// - `protocol > 15`, `packet_type > 15`, or the payload is longer
    ///   than [`codec::MAX_PAYLOAD`].
    ///
    /// All of the above are programmer errors; production callers should
    /// drive [`connect`](Self::connect) to completion, observe
    /// [`Event::Synced`], and clamp payload size to
    /// [`max_block_size`](Self::max_block_size).
    pub fn send(&mut self, protocol: u8, packet_type: u8, payload: &[u8], now: Instant) {
        assert!(
            self.is_synced,
            "Session::send called before SYNC handshake completed; call connect() and drive feed() until Event::Synced first"
        );
        assert!(protocol <= 0xF, "protocol id out of range");
        assert!(packet_type <= 0xF, "packet type out of range");
        assert!(
            payload.len() <= codec::MAX_PAYLOAD,
            "payload exceeds MAX_PAYLOAD"
        );
        self.queue(protocol, packet_type, payload, false);
        self.dispatch_if_idle(now);
    }

    fn queue(&mut self, protocol: u8, packet_type: u8, payload: &[u8], is_sync_handshake: bool) {
        self.queued.push_back(Queued {
            bytes_without_sync: BytesBuilder {
                protocol,
                packet_type,
                payload: payload.to_vec(),
            },
            is_sync_handshake,
        });
    }

    fn dispatch_if_idle(&mut self, now: Instant) {
        if self.in_flight.is_some() {
            return;
        }
        let Some(next) = self.queued.pop_front() else {
            return;
        };
        let bytes = next.bytes_without_sync.build(self.sync);
        self.outbound.push_back(bytes.clone());
        self.in_flight = Some(InFlight {
            sync: self.sync,
            bytes,
            first_sent: now,
            last_sent: now,
            is_sync_handshake: next.is_sync_handshake,
        });
    }

    /// Drain a single chunk of bytes the caller should write to the wire.
    /// Returns `None` when no more bytes are pending.
    pub fn poll_outbound(&mut self) -> Option<Vec<u8>> {
        self.outbound.pop_front()
    }

    /// Push received bytes from the wire. Bytes are accumulated until a
    /// newline-terminated ASCII line is recognised, at which point an
    /// [`Event`] is queued for [`poll_event`](Self::poll_event).
    ///
    /// `now` is used to timestamp any queued packet that gets dispatched
    /// as a side effect of an inbound ack — fully sans-I/O, no internal
    /// wall-clock reads.
    pub fn feed(&mut self, bytes: &[u8], now: Instant) {
        self.inbound_buf.extend_from_slice(bytes);
        while let Some(pos) = self.inbound_buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.inbound_buf.drain(..=pos).collect();
            // Strip trailing \r and \n.
            let trimmed = strip_line_endings(&line);
            if trimmed.is_empty() {
                continue;
            }
            self.process_line(trimmed, now);
        }
    }

    fn process_line(&mut self, line: &[u8], now: Instant) {
        if let Some(rest) = strip_prefix(line, b"ok") {
            if let Some(n) = parse_decimal_u8(rest) {
                self.handle_ok(n, now);
                return;
            }
        }
        if let Some(rest) = strip_prefix(line, b"rs") {
            if let Some(n) = parse_decimal_u8(rest) {
                self.events.push_back(Event::ResendRequested(n));
                return;
            }
        }
        if let Some(rest) = strip_prefix(line, b"ss") {
            self.handle_ss(rest, now);
            return;
        }
        if line == b"fe" {
            self.events.push_back(Event::FatalError);
            return;
        }
        // Anything else: pass through as a UTF-8 string so file_transfer
        // (or arbitrary callers) can match on PFT:* tokens.
        match std::str::from_utf8(line) {
            Ok(s) => self.events.push_back(Event::AsciiLine(s.to_string())),
            Err(_) => {
                // Non-UTF-8 garbage: drop, the caller can't usefully parse it.
            }
        }
    }

    fn handle_ok(&mut self, n: u8, now: Instant) {
        let Some(flight) = self.in_flight.as_ref() else {
            // No outstanding packet — stray ack. Surface as a passthrough so
            // callers can debug, but don't crash.
            self.events.push_back(Event::AsciiLine(format!("ok{n}")));
            return;
        };
        if flight.is_sync_handshake {
            // SYNC handshake is acked with `ss`, not `ok`. Treat this as
            // out-of-sync.
            self.events.push_back(Event::OutOfSync {
                expected: flight.sync,
                got: n,
            });
            return;
        }
        if n != flight.sync {
            self.events.push_back(Event::OutOfSync {
                expected: flight.sync,
                got: n,
            });
            return;
        }
        self.in_flight = None;
        self.sync = ((self.sync as u16 + 1) % SYNC_MOD) as u8;
        self.events.push_back(Event::Ack(n));
        self.dispatch_if_idle(now);
    }

    fn handle_ss(&mut self, rest: &[u8], now: Instant) {
        let s = match std::str::from_utf8(rest) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut parts = s.splitn(3, ',');
        let (Some(sync_str), Some(bsize_str), Some(version_str)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return;
        };
        let Ok(new_sync) = sync_str.trim().parse::<u16>() else {
            return;
        };
        let Ok(max_block_size) = bsize_str.trim().parse::<u16>() else {
            return;
        };
        let new_sync = (new_sync % SYNC_MOD) as u8;
        self.sync = new_sync;
        self.max_block_size = Some(max_block_size);
        let protocol_version = version_str.trim().to_string();
        self.protocol_version = Some(protocol_version.clone());
        self.is_synced = true;
        // SS consumes the in-flight SYNC packet (if any).
        if let Some(flight) = self.in_flight.as_ref() {
            if flight.is_sync_handshake {
                self.in_flight = None;
            }
        }
        self.events.push_back(Event::Synced {
            max_block_size,
            protocol_version,
        });
        self.dispatch_if_idle(now);
    }

    /// Drain the next queued event. Returns `None` when the queue is empty.
    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// Drive retransmit and total-timeout logic. Callers should call this
    /// at least as often as the per-attempt response timeout.
    pub fn tick(&mut self, now: Instant) {
        let Some(flight) = self.in_flight.as_mut() else {
            return;
        };
        if now.saturating_duration_since(flight.first_sent) >= self.total_timeout {
            let sync = flight.sync;
            self.in_flight = None;
            self.events.push_back(Event::Timeout { sync });
            self.dispatch_if_idle(now);
            return;
        }
        if now.saturating_duration_since(flight.last_sent) >= self.response_timeout {
            // Retransmit.
            self.outbound.push_back(flight.bytes.clone());
            flight.last_sent = now;
        }
    }
}

fn strip_line_endings(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && (line[end - 1] == b'\n' || line[end - 1] == b'\r') {
        end -= 1;
    }
    &line[..end]
}

fn strip_prefix<'a>(line: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    if line.starts_with(prefix) {
        Some(&line[prefix.len()..])
    } else {
        None
    }
}

fn parse_decimal_u8(b: &[u8]) -> Option<u8> {
    if b.is_empty() {
        return None;
    }
    let s = std::str::from_utf8(b).ok()?;
    let n: u32 = s.trim().parse().ok()?;
    if n > 255 {
        return None;
    }
    Some(n as u8)
}
