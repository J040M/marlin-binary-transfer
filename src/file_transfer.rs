//! Layer 3: file-transfer state machine.
//!
//! Drives the protocol-1 sub-protocol (QUERY / OPEN / WRITE / CLOSE / ABORT)
//! on top of a [`Session`]. Translates the session's raw [`Event`]s into
//! file-level [`FileEvent`]s so the caller doesn't have to parse the
//! `PFT:*` tokens themselves.
//!
//! # Lifecycle
//!
//! ```text
//! Idle ── query()  ──► AwaitingQueryReply ──► Negotiated
//!                                              │
//! Negotiated ── open() ──► AwaitingOpenReply ──┴──► Opened ── close() ──► Closed
//!                                              │           ── abort() ──► Aborted
//!                                              └── (busy/fail) ──► Failed
//! Opened     ── write() ──► AwaitingWriteAck ──► Opened
//! ```
//!
//! Compression is negotiated inside [`FileTransfer::query`]: if the caller
//! passed [`Compression::Auto`] and the device advertises `heatshrink`, the
//! negotiated parameters are exposed via
//! [`FileTransfer::negotiated_compression`]. The adapter modules are
//! responsible for actually compressing chunk bytes before calling
//! [`FileTransfer::write`].

use std::time::Instant;

use thiserror::Error;

use crate::session::{Event, Session};

/// Caller-visible compression preference.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Compression {
    /// Send raw, uncompressed payloads.
    #[default]
    None,
    /// Use heatshrink with the given window/lookahead. Both parameters
    /// must match what the device advertises in its QUERY reply, or the
    /// transfer will be corrupt on the device side.
    Heatshrink {
        /// log2 of the heatshrink sliding-window size.
        window: u8,
        /// log2 of the heatshrink lookahead size.
        lookahead: u8,
    },
    /// Use whatever the device advertises during QUERY. Falls back to
    /// [`Compression::None`] if the device only supports `none`.
    Auto,
}

/// Things [`FileTransfer::poll`] emits as the protocol progresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileEvent {
    /// QUERY completed. Carries the device's protocol version and the
    /// effective compression spec the session will use.
    Negotiated {
        /// Device-reported file-transfer protocol version.
        version: String,
        /// Effective compression spec to use for this transfer.
        compression: Compression,
    },
    /// OPEN succeeded; the device is ready to receive WRITE packets.
    Opened,
    /// A WRITE packet has been acknowledged. One per [`FileTransfer::write`]
    /// call.
    WriteAcked,
    /// CLOSE succeeded; transfer is complete.
    Closed,
    /// ABORT acknowledged; transfer cancelled cleanly.
    AbortAcked,
    /// Something failed; the transfer cannot continue.
    Failed(FileError),
}

/// Reasons a transfer failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FileError {
    /// Device replied `PFT:busy` to OPEN — another transfer is in
    /// progress on the device side.
    #[error("device busy (PFT:busy)")]
    OpenBusy,
    /// Device replied `PFT:fail` to OPEN — likely SD card not present
    /// or write-protected.
    #[error("device refused open (PFT:fail)")]
    OpenFail,
    /// Device replied `PFT:ioerror` to CLOSE.
    #[error("device storage I/O error (PFT:ioerror)")]
    IoError,
    /// Device replied `PFT:invalid` to CLOSE — no file was open.
    #[error("no open file on device (PFT:invalid)")]
    NoOpenFile,
    /// The session emitted a [`Event::Timeout`] while we were waiting
    /// for a reply.
    #[error("session timed out waiting for reply")]
    SessionTimeout,
    /// The session emitted a [`Event::FatalError`].
    #[error("session reported fatal error (fe)")]
    SessionFatalError,
    /// The session emitted [`Event::OutOfSync`].
    #[error("session out of sync: expected {expected}, got {got}")]
    SessionOutOfSync {
        /// Sync number we expected.
        expected: u8,
        /// Sync number the device acked.
        got: u8,
    },
    /// Caller requested `Compression::Heatshrink` but the device only
    /// advertises `none`.
    #[error("device does not support heatshrink compression")]
    CompressionUnsupported,
    /// Device sent the closing `ok<n>` for a CLOSE or ABORT without
    /// first emitting the expected `PFT:*` preamble. Without that
    /// preamble, the transfer can't be classified as success or
    /// failure, so we surface the violation rather than silently
    /// claiming completion.
    #[error("device sent bare ok in {state} without expected {expected} preamble")]
    ProtocolViolation {
        /// Symbolic name of the FileTransfer state at the time of the ack.
        state: &'static str,
        /// The PFT preamble line(s) the protocol requires before the ack.
        expected: &'static str,
    },
}

/// Internal state-machine state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    AwaitingQueryReply,
    Negotiated,
    AwaitingOpenReply,
    Opened,
    AwaitingWriteAck,
    AwaitingCloseReply,
    Closed,
    AwaitingAbortReply,
    Aborted,
    Failed,
}

/// File-transfer state machine driving a [`Session`].
#[derive(Debug)]
pub struct FileTransfer<'a> {
    session: &'a mut Session,
    state: State,
    requested_compression: Compression,
    negotiated: Option<Compression>,
    advertised_version: Option<String>,
    /// Set once the AsciiLine half of a two-part reply has arrived; reset
    /// after the matching Ack drives the corresponding FileEvent.
    pending_ascii: Option<PendingAscii>,
    /// Queue of FileEvents ready to be drained by [`Self::poll`].
    out_events: std::collections::VecDeque<FileEvent>,
    /// File-transfer protocol id (constant 1; configurable for tests).
    protocol_id: u8,
}

#[derive(Debug)]
enum PendingAscii {
    QueryVersion {
        version: String,
        compression: Compression,
    },
    /// QUERY reply parsed but compression negotiation failed (e.g. caller
    /// asked for heatshrink and device advertises only `none`). Surfaces
    /// as `FileEvent::Failed(_)` once the matching `ok<n>` arrives.
    QueryFailed(FileError),
    OpenSuccess,
    OpenBusy,
    OpenFail,
    CloseSuccess,
    CloseIoError,
    CloseInvalid,
    AbortSuccess,
}

const PROTOCOL_FILE_TRANSFER: u8 = 1;
const PT_QUERY: u8 = 0;
const PT_OPEN: u8 = 1;
const PT_CLOSE: u8 = 2;
const PT_WRITE: u8 = 3;
const PT_ABORT: u8 = 4;

impl<'a> FileTransfer<'a> {
    /// Build a new file-transfer driver borrowing the given session.
    /// The session must already have completed the SYNC handshake
    /// (`session.is_synced() == true`).
    pub fn new(session: &'a mut Session) -> Self {
        Self {
            session,
            state: State::Idle,
            requested_compression: Compression::None,
            negotiated: None,
            advertised_version: None,
            pending_ascii: None,
            out_events: std::collections::VecDeque::new(),
            protocol_id: PROTOCOL_FILE_TRANSFER,
        }
    }

    /// Issue the QUERY control packet. Caller must specify what
    /// compression mode they want — the actual negotiated mode is
    /// reported back via [`FileEvent::Negotiated`].
    ///
    /// # Panics
    ///
    /// Panics unless the state is `Idle` (i.e. this is a fresh
    /// transfer). Programmer error; callers must construct a new
    /// `FileTransfer` for retries after a `Failed` / `Closed` /
    /// `Aborted` outcome.
    pub fn query(&mut self, compression: Compression, now: Instant) {
        assert!(
            matches!(self.state, State::Idle),
            "query() requires Idle state, found {:?}",
            self.state
        );
        self.requested_compression = compression;
        self.state = State::AwaitingQueryReply;
        self.session.send(self.protocol_id, PT_QUERY, &[], now);
    }

    /// Send the OPEN packet. `name` is the destination filename on the
    /// SD card. `dummy` requests the device pretend to receive a file
    /// without actually writing it (used for protocol smoke tests).
    ///
    /// # Panics
    ///
    /// Panics unless QUERY has completed (`Negotiated` state). Without
    /// QUERY, the compression byte in the OPEN payload would be a
    /// guess.
    pub fn open(&mut self, name: &str, dummy: bool, now: Instant) {
        assert!(
            matches!(self.state, State::Negotiated),
            "open() requires Negotiated state (call query() first), found {:?}",
            self.state
        );
        let comp_byte = match self.negotiated {
            Some(Compression::Heatshrink { .. }) => 1u8,
            _ => 0u8,
        };
        let mut payload = Vec::with_capacity(2 + name.len() + 1);
        payload.push(if dummy { 1 } else { 0 });
        payload.push(comp_byte);
        payload.extend_from_slice(name.as_bytes());
        payload.push(0);
        self.state = State::AwaitingOpenReply;
        self.session.send(self.protocol_id, PT_OPEN, &payload, now);
    }

    /// Send a WRITE packet with the given chunk. The caller is
    /// responsible for splitting the source data so each chunk fits
    /// inside the device's `max_block_size` (and, if compression is in
    /// use, for compressing each chunk before passing it here).
    ///
    /// # Panics
    ///
    /// Panics unless the file is open and no WRITE is in flight (state
    /// `Opened`). Callers must pump until [`FileEvent::WriteAcked`]
    /// before issuing the next [`write`](Self::write). This mirrors
    /// the Python reference's one-packet-in-flight policy and keeps
    /// the state machine unambiguous — pipelining is out of scope for
    /// 0.1.
    pub fn write(&mut self, chunk: &[u8], now: Instant) {
        assert!(
            matches!(self.state, State::Opened),
            "write() requires Opened state (pump until WriteAcked between writes), found {:?}",
            self.state
        );
        self.state = State::AwaitingWriteAck;
        self.session.send(self.protocol_id, PT_WRITE, chunk, now);
    }

    /// Send the CLOSE packet, finalising the transfer.
    ///
    /// # Panics
    ///
    /// Panics unless the file is open and no WRITE is in flight
    /// (state `Opened`).
    pub fn close(&mut self, now: Instant) {
        assert!(
            matches!(self.state, State::Opened),
            "close() requires Opened state, found {:?}",
            self.state
        );
        self.state = State::AwaitingCloseReply;
        self.session.send(self.protocol_id, PT_CLOSE, &[], now);
    }

    /// Send the ABORT packet, cancelling the transfer.
    ///
    /// # Panics
    ///
    /// Panics unless the file is open and no WRITE is in flight
    /// (state `Opened`). Aborting before OPEN completes makes no
    /// protocol sense — there's nothing for the device to abort.
    pub fn abort(&mut self, now: Instant) {
        assert!(
            matches!(self.state, State::Opened),
            "abort() requires Opened state, found {:?}",
            self.state
        );
        self.state = State::AwaitingAbortReply;
        self.session.send(self.protocol_id, PT_ABORT, &[], now);
    }

    /// Compression mode negotiated during QUERY, if any.
    pub fn negotiated_compression(&self) -> Option<&Compression> {
        self.negotiated.as_ref()
    }

    /// Drain bytes the caller should write to the wire (delegates to the
    /// underlying session).
    pub fn poll_outbound(&mut self) -> Option<Vec<u8>> {
        self.session.poll_outbound()
    }

    /// Push received bytes into the underlying session.
    ///
    /// `now` is forwarded into [`Session::feed`](crate::session::Session::feed)
    /// so any packet dispatched as a side effect of an inbound ack gets
    /// a real timestamp.
    pub fn feed(&mut self, bytes: &[u8], now: Instant) {
        self.session.feed(bytes, now);
    }

    /// Drive retransmit/timeout logic in the underlying session.
    pub fn tick(&mut self, now: Instant) {
        self.session.tick(now);
    }

    /// Per-attempt response timeout of the underlying session — used by
    /// adapters to bound inbound reads.
    pub fn response_timeout(&self) -> std::time::Duration {
        self.session.response_timeout()
    }

    /// Pull the next file-level event, processing whatever session events
    /// have accumulated. Returns `None` when there is nothing pending.
    pub fn poll(&mut self) -> Option<FileEvent> {
        // Drain session events until we have at least one FileEvent ready.
        while let Some(event) = self.session.poll_event() {
            self.handle_session_event(event);
        }
        self.out_events.pop_front()
    }

    fn handle_session_event(&mut self, event: Event) {
        match event {
            Event::AsciiLine(line) => self.handle_ascii_line(line),
            Event::Ack(_) => self.handle_ack(),
            Event::Synced { .. } => {
                // Synced may arrive on the session prior to FileTransfer
                // being constructed. Treat post-construction as benign
                // passthrough.
            }
            Event::ResendRequested(_) => {
                // The session re-sends on its own; nothing for us to do.
            }
            Event::FatalError => self.fail(FileError::SessionFatalError),
            Event::OutOfSync { expected, got } => {
                self.fail(FileError::SessionOutOfSync { expected, got })
            }
            Event::Timeout { .. } => self.fail(FileError::SessionTimeout),
        }
    }

    fn handle_ascii_line(&mut self, line: String) {
        if let Some(rest) = line.strip_prefix("PFT:version:") {
            // Format: <version>:<compression-spec>
            //   compression-spec is "none" or "heatshrink,<window>,<lookahead>"
            if !matches!(self.state, State::AwaitingQueryReply) {
                return;
            }
            let (version, comp) = match rest.split_once(':') {
                Some(parts) => parts,
                None => return,
            };
            self.advertised_version = Some(version.to_string());
            let device_compression = parse_compression_spec(comp);
            self.pending_ascii = Some(match self.choose_compression(&device_compression) {
                Ok(chosen) => PendingAscii::QueryVersion {
                    version: version.to_string(),
                    compression: chosen,
                },
                Err(err) => PendingAscii::QueryFailed(err),
            });
            return;
        }
        match line.as_str() {
            "PFT:success" => {
                self.pending_ascii = Some(match self.state {
                    State::AwaitingOpenReply => PendingAscii::OpenSuccess,
                    State::AwaitingCloseReply => PendingAscii::CloseSuccess,
                    State::AwaitingAbortReply => PendingAscii::AbortSuccess,
                    _ => return, // unsolicited; ignore
                });
            }
            "PFT:busy" => {
                if matches!(self.state, State::AwaitingOpenReply) {
                    self.pending_ascii = Some(PendingAscii::OpenBusy);
                }
            }
            "PFT:fail" => {
                if matches!(self.state, State::AwaitingOpenReply) {
                    self.pending_ascii = Some(PendingAscii::OpenFail);
                }
            }
            "PFT:ioerror" => {
                if matches!(self.state, State::AwaitingCloseReply) {
                    self.pending_ascii = Some(PendingAscii::CloseIoError);
                }
            }
            "PFT:invalid" => {
                if matches!(self.state, State::AwaitingCloseReply) {
                    self.pending_ascii = Some(PendingAscii::CloseInvalid);
                }
            }
            _ => {
                // Unknown line — ignore. Marlin emits chatter we don't
                // care about during binary mode.
            }
        }
    }

    fn handle_ack(&mut self) {
        let pending = self.pending_ascii.take();

        match (self.state, pending) {
            (
                State::AwaitingQueryReply,
                Some(PendingAscii::QueryVersion {
                    version,
                    compression,
                }),
            ) => {
                self.negotiated = Some(compression.clone());
                self.state = State::Negotiated;
                self.out_events.push_back(FileEvent::Negotiated {
                    version,
                    compression,
                });
            }
            (State::AwaitingQueryReply, Some(PendingAscii::QueryFailed(err))) => {
                self.fail(err);
            }
            (State::AwaitingQueryReply, None) => {
                self.fail(FileError::ProtocolViolation {
                    state: "AwaitingQueryReply",
                    expected: "PFT:version:<v>:<compression-spec>",
                });
            }
            (State::AwaitingOpenReply, Some(PendingAscii::OpenSuccess)) => {
                self.state = State::Opened;
                self.out_events.push_back(FileEvent::Opened);
            }
            (State::AwaitingOpenReply, Some(PendingAscii::OpenBusy)) => {
                self.state = State::Failed;
                self.out_events
                    .push_back(FileEvent::Failed(FileError::OpenBusy));
            }
            (State::AwaitingOpenReply, Some(PendingAscii::OpenFail)) => {
                self.state = State::Failed;
                self.out_events
                    .push_back(FileEvent::Failed(FileError::OpenFail));
            }
            (State::AwaitingOpenReply, None) => {
                self.fail(FileError::ProtocolViolation {
                    state: "AwaitingOpenReply",
                    expected: "PFT:success | PFT:busy | PFT:fail",
                });
            }
            (State::AwaitingWriteAck, None) => {
                self.state = State::Opened;
                self.out_events.push_back(FileEvent::WriteAcked);
            }
            (State::AwaitingWriteAck, Some(_)) => {
                // The PFT setters are state-gated, so reaching this arm
                // means the device sent something we couldn't interpret
                // (or we have a logic bug). Surface it loudly rather than
                // silently leaving the FSM in AwaitingWriteAck.
                self.fail(FileError::ProtocolViolation {
                    state: "AwaitingWriteAck",
                    expected: "bare ok<n> (no PFT preamble)",
                });
            }
            (State::AwaitingCloseReply, Some(PendingAscii::CloseSuccess)) => {
                self.state = State::Closed;
                self.out_events.push_back(FileEvent::Closed);
            }
            (State::AwaitingCloseReply, Some(PendingAscii::CloseIoError)) => {
                self.state = State::Failed;
                self.out_events
                    .push_back(FileEvent::Failed(FileError::IoError));
            }
            (State::AwaitingCloseReply, Some(PendingAscii::CloseInvalid)) => {
                self.state = State::Failed;
                self.out_events
                    .push_back(FileEvent::Failed(FileError::NoOpenFile));
            }
            (State::AwaitingCloseReply, None) => {
                self.fail(FileError::ProtocolViolation {
                    state: "AwaitingCloseReply",
                    expected: "PFT:success | PFT:ioerror | PFT:invalid",
                });
            }
            (State::AwaitingAbortReply, Some(PendingAscii::AbortSuccess)) => {
                self.state = State::Aborted;
                self.out_events.push_back(FileEvent::AbortAcked);
            }
            (State::AwaitingAbortReply, None) => {
                self.fail(FileError::ProtocolViolation {
                    state: "AwaitingAbortReply",
                    expected: "PFT:success",
                });
            }
            _ => {
                // Ack arrived in a state where we don't have a pending
                // ASCII line to interpret. Most often this is a file
                // open or close that we've already finalized; ignore.
            }
        }
    }

    /// Resolve the negotiated compression mode given what the caller
    /// requested and what the device just advertised.
    ///
    /// Returns `Err(CompressionUnsupported)` when the caller explicitly
    /// asked for heatshrink but the device only advertises `none` —
    /// proceeding would corrupt the upload on the device side.
    ///
    /// Caller-supplied window/lookahead override the device-advertised
    /// values when both sides speak heatshrink. Per the
    /// [`Compression::Heatshrink`] doc-comment, matching parameters is
    /// the caller's responsibility.
    fn choose_compression(&self, device: &Compression) -> Result<Compression, FileError> {
        match (&self.requested_compression, device) {
            (Compression::None, _) => Ok(Compression::None),
            (Compression::Heatshrink { window, lookahead }, Compression::Heatshrink { .. }) => {
                Ok(Compression::Heatshrink {
                    window: *window,
                    lookahead: *lookahead,
                })
            }
            (Compression::Heatshrink { .. }, _) => Err(FileError::CompressionUnsupported),
            (Compression::Auto, Compression::Heatshrink { window, lookahead }) => {
                Ok(Compression::Heatshrink {
                    window: *window,
                    lookahead: *lookahead,
                })
            }
            (Compression::Auto, _) => Ok(Compression::None),
        }
    }

    fn fail(&mut self, err: FileError) {
        self.state = State::Failed;
        self.out_events.push_back(FileEvent::Failed(err));
    }
}

fn parse_compression_spec(spec: &str) -> Compression {
    let spec = spec.trim();
    if spec == "none" {
        return Compression::None;
    }
    if let Some(rest) = spec.strip_prefix("heatshrink,") {
        let mut parts = rest.split(',');
        let window: Option<u8> = parts.next().and_then(|s| s.trim().parse().ok());
        let lookahead: Option<u8> = parts.next().and_then(|s| s.trim().parse().ok());
        if let (Some(window), Some(lookahead)) = (window, lookahead) {
            return Compression::Heatshrink { window, lookahead };
        }
    }
    // Unknown spec — be conservative and report None so we don't claim
    // compression that the device may not actually understand.
    Compression::None
}
