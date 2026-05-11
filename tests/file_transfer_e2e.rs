//! File-transfer integration tests: full upload happy path through the
//! sans-I/O state machines wired byte-for-byte against the FakeDevice
//! fixture.

use std::time::Instant;

use marlin_binary_transfer::file_transfer::{Compression, FileError, FileEvent, FileTransfer};
use marlin_binary_transfer::session::{Event, Session};

#[path = "fixtures/canonical.rs"]
#[allow(dead_code)]
mod canonical;

#[path = "fixtures/fake_device.rs"]
#[allow(dead_code)]
mod fake_device;

use fake_device::{CompressionSpec, DeviceBehaviour, FakeDevice};

/// Pump bytes between session and fake device exactly once.
fn pump(session: &mut Session, device: &mut FakeDevice) {
    while let Some(out) = session.poll_outbound() {
        device.feed(&out);
    }
    let reply = device.drain_reply();
    if !reply.is_empty() {
        session.feed(&reply, Instant::now());
    }
}

/// Drive the SYNC handshake to completion. Returns once `Synced` is seen.
fn complete_handshake(session: &mut Session, device: &mut FakeDevice) {
    session.connect(Instant::now());
    for _ in 0..10 {
        pump(session, device);
        let mut synced = false;
        while let Some(evt) = session.poll_event() {
            if matches!(evt, Event::Synced { .. }) {
                synced = true;
            }
        }
        if synced {
            return;
        }
    }
    panic!("handshake did not complete within 10 pumps");
}

/// Drain `FileTransfer::poll` until at least one event is produced or `n`
/// pumps have passed without progress.
fn pump_until_event(
    ft: &mut FileTransfer<'_>,
    device: &mut FakeDevice,
    pumps: usize,
) -> Option<FileEvent> {
    for _ in 0..pumps {
        pump_ft(ft, device);
        if let Some(evt) = ft.poll() {
            return Some(evt);
        }
    }
    None
}

fn pump_ft(ft: &mut FileTransfer<'_>, device: &mut FakeDevice) {
    while let Some(out) = ft.poll_outbound() {
        device.feed(&out);
    }
    let reply = device.drain_reply();
    if !reply.is_empty() {
        ft.feed(&reply, Instant::now());
    }
}

#[test]
fn happy_path_query_open_write_close_against_fake_device() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0);
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();

    ft.query(Compression::None, now);
    match pump_until_event(&mut ft, &mut device, 10).expect("Negotiated event") {
        FileEvent::Negotiated {
            version,
            compression,
        } => {
            assert_eq!(version, "1.0");
            assert_eq!(compression, Compression::None);
        }
        other => panic!("expected Negotiated, got {other:?}"),
    }

    ft.open("test.gco", false, now);
    assert_eq!(
        pump_until_event(&mut ft, &mut device, 10),
        Some(FileEvent::Opened)
    );
    assert_eq!(device.last_open_filename.as_deref(), Some("test.gco"));
    assert_eq!(device.last_open_dummy, Some(false));
    assert_eq!(device.last_open_compression_byte, Some(0));

    let chunks: &[&[u8]] = &[b"G28\nG1 X10\n", b"M104 S200\n", b"; end\n"];
    for chunk in chunks {
        ft.write(chunk, now);
        assert_eq!(
            pump_until_event(&mut ft, &mut device, 10),
            Some(FileEvent::WriteAcked)
        );
    }

    ft.close(now);
    assert_eq!(
        pump_until_event(&mut ft, &mut device, 10),
        Some(FileEvent::Closed)
    );

    let total: Vec<u8> = chunks.iter().flat_map(|c| c.iter().copied()).collect();
    assert_eq!(device.written_bytes, total);
    assert!(device.closed);
}

#[test]
fn auto_compression_chooses_heatshrink_when_available() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0).with_compression(CompressionSpec::Heatshrink {
        window: 8,
        lookahead: 4,
    });
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    ft.query(Compression::Auto, Instant::now());
    let evt = pump_until_event(&mut ft, &mut device, 10).expect("Negotiated event");
    match evt {
        FileEvent::Negotiated { compression, .. } => {
            assert_eq!(
                compression,
                Compression::Heatshrink {
                    window: 8,
                    lookahead: 4
                }
            );
        }
        other => panic!("expected Negotiated, got {other:?}"),
    }
    assert_eq!(
        ft.negotiated_compression(),
        Some(&Compression::Heatshrink {
            window: 8,
            lookahead: 4
        })
    );
}

#[test]
fn auto_compression_falls_back_to_none_when_device_only_supports_none() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0).with_compression(CompressionSpec::None);
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    ft.query(Compression::Auto, Instant::now());
    let evt = pump_until_event(&mut ft, &mut device, 10).expect("Negotiated event");
    match evt {
        FileEvent::Negotiated { compression, .. } => {
            assert_eq!(compression, Compression::None);
        }
        other => panic!("expected Negotiated, got {other:?}"),
    }
}

#[test]
fn open_busy_surfaces_as_open_busy_failure() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0).with_behaviour(DeviceBehaviour {
        busy_on_next_open: true,
        ..DeviceBehaviour::default()
    });
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();
    ft.query(Compression::None, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);

    ft.open("a.gco", false, now);
    let evt = pump_until_event(&mut ft, &mut device, 10).expect("OpenBusy failure");
    assert_eq!(evt, FileEvent::Failed(FileError::OpenBusy));
}

#[test]
fn open_fail_surfaces_as_open_fail() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0).with_behaviour(DeviceBehaviour {
        fail_on_next_open: true,
        ..DeviceBehaviour::default()
    });
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();
    ft.query(Compression::None, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);

    ft.open("a.gco", false, now);
    let evt = pump_until_event(&mut ft, &mut device, 10).expect("OpenFail failure");
    assert_eq!(evt, FileEvent::Failed(FileError::OpenFail));
}

#[test]
fn abort_after_open_emits_abort_acked() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0);
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();
    ft.query(Compression::None, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);
    ft.open("doomed.gco", false, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);

    ft.abort(now);
    assert_eq!(
        pump_until_event(&mut ft, &mut device, 10),
        Some(FileEvent::AbortAcked)
    );
    assert!(device.aborted);
}

#[test]
fn explicit_heatshrink_against_none_device_fails_with_compression_unsupported() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0).with_compression(CompressionSpec::None);
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    ft.query(
        Compression::Heatshrink {
            window: 8,
            lookahead: 4,
        },
        Instant::now(),
    );
    let evt = pump_until_event(&mut ft, &mut device, 10).expect("Failed event");
    assert_eq!(evt, FileEvent::Failed(FileError::CompressionUnsupported));
    assert!(
        ft.negotiated_compression().is_none(),
        "no compression should be negotiated when QUERY fails"
    );
}

#[test]
fn explicit_heatshrink_against_heatshrink_device_uses_caller_params() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0).with_compression(CompressionSpec::Heatshrink {
        window: 10,
        lookahead: 5,
    });
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    ft.query(
        Compression::Heatshrink {
            window: 8,
            lookahead: 4,
        },
        Instant::now(),
    );
    let evt = pump_until_event(&mut ft, &mut device, 10).expect("Negotiated event");
    match evt {
        FileEvent::Negotiated { compression, .. } => {
            // Caller's params override the device-advertised ones — matching
            // window/lookahead is documented as the caller's responsibility.
            assert_eq!(
                compression,
                Compression::Heatshrink {
                    window: 8,
                    lookahead: 4
                }
            );
        }
        other => panic!("expected Negotiated, got {other:?}"),
    }
}

#[test]
fn query_without_pft_version_emits_protocol_violation() {
    // Drive handshake, send QUERY, then feed a bare ok<n> without the
    // expected PFT:version: preamble.
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0);
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();
    ft.query(Compression::None, now);
    let query_bytes = ft.poll_outbound().expect("QUERY bytes pending");
    let sync = query_bytes[2];
    ft.feed(format!("ok{sync}\n").as_bytes(), now);

    let evt = ft.poll().expect("Failed event");
    match evt {
        FileEvent::Failed(FileError::ProtocolViolation { state, .. }) => {
            assert_eq!(state, "AwaitingQueryReply");
        }
        other => panic!("expected ProtocolViolation, got {other:?}"),
    }
}

#[test]
fn open_without_pft_reply_emits_protocol_violation() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0);
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();
    ft.query(Compression::None, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);

    ft.open("a.gco", false, now);
    let open_bytes = ft.poll_outbound().expect("OPEN bytes pending");
    let sync = open_bytes[2];
    ft.feed(format!("ok{sync}\n").as_bytes(), now);

    let evt = ft.poll().expect("Failed event");
    match evt {
        FileEvent::Failed(FileError::ProtocolViolation { state, .. }) => {
            assert_eq!(state, "AwaitingOpenReply");
        }
        other => panic!("expected ProtocolViolation, got {other:?}"),
    }
}

#[test]
fn close_without_pft_preamble_emits_protocol_violation() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0).with_behaviour(DeviceBehaviour {
        skip_pft_on_terminal: true,
        ..DeviceBehaviour::default()
    });
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();
    ft.query(Compression::None, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);
    ft.open("a.gco", false, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);
    ft.close(now);
    let evt = pump_until_event(&mut ft, &mut device, 10).expect("Failed event");
    match evt {
        FileEvent::Failed(FileError::ProtocolViolation { state, .. }) => {
            assert_eq!(state, "AwaitingCloseReply");
        }
        other => panic!("expected ProtocolViolation, got {other:?}"),
    }
}

#[test]
fn abort_without_pft_preamble_emits_protocol_violation() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0).with_behaviour(DeviceBehaviour {
        skip_pft_on_terminal: true,
        ..DeviceBehaviour::default()
    });
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();
    ft.query(Compression::None, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);
    ft.open("doomed.gco", false, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);
    ft.abort(now);
    let evt = pump_until_event(&mut ft, &mut device, 10).expect("Failed event");
    match evt {
        FileEvent::Failed(FileError::ProtocolViolation { state, .. }) => {
            assert_eq!(state, "AwaitingAbortReply");
        }
        other => panic!("expected ProtocolViolation, got {other:?}"),
    }
}

#[test]
fn close_ioerror_surfaces_as_io_error() {
    // Drive a normal handshake/query/open via FakeDevice, then synthesize
    // the ioerror reply by manually feeding it (FakeDevice always replies
    // success). This covers the previously-untested CloseIoError path.
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0);
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();
    ft.query(Compression::None, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);
    ft.open("a.gco", false, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);

    // Send CLOSE, capture its bytes (don't pump them through the device),
    // then feed back our own ioerror reply.
    ft.close(now);
    let close_bytes = ft.poll_outbound().expect("CLOSE bytes pending");
    let sync = close_bytes[2];
    ft.feed(b"PFT:ioerror\n", now);
    ft.feed(format!("ok{sync}\n").as_bytes(), now);

    let evt = ft.poll().expect("Failed event");
    assert_eq!(evt, FileEvent::Failed(FileError::IoError));
}

#[test]
fn close_invalid_surfaces_as_no_open_file() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0);
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();
    ft.query(Compression::None, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);
    ft.open("a.gco", false, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);

    ft.close(now);
    let close_bytes = ft.poll_outbound().expect("CLOSE bytes pending");
    let sync = close_bytes[2];
    ft.feed(b"PFT:invalid\n", now);
    ft.feed(format!("ok{sync}\n").as_bytes(), now);

    let evt = ft.poll().expect("Failed event");
    assert_eq!(evt, FileEvent::Failed(FileError::NoOpenFile));
}

#[test]
#[should_panic(expected = "query() requires Idle state")]
fn query_after_query_panics() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0);
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();
    ft.query(Compression::None, now);
    ft.query(Compression::None, now); // panic: state is AwaitingQueryReply
}

#[test]
#[should_panic(expected = "open() requires Negotiated state")]
fn open_before_query_panics() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0);
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    ft.open("a.gco", false, Instant::now()); // panic: state is Idle
}

#[test]
#[should_panic(expected = "write() requires Opened state")]
fn write_before_open_panics() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0);
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();
    ft.query(Compression::None, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);
    ft.write(b"data", now); // panic: state is Negotiated
}

#[test]
#[should_panic(expected = "close() requires Opened state")]
fn close_before_open_panics() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0);
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();
    ft.query(Compression::None, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);
    ft.close(now); // panic: state is Negotiated
}

#[test]
#[should_panic(expected = "abort() requires Opened state")]
fn abort_before_open_panics() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0);
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();
    ft.query(Compression::None, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);
    ft.abort(now); // panic: state is Negotiated
}

#[test]
#[should_panic(expected = "write() requires Opened state")]
fn back_to_back_writes_without_pump_panics() {
    // The state machine intentionally keeps one packet in flight at a
    // time (mirrors the Python reference). Issuing a second write
    // without first pumping until WriteAcked is a programmer error.
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0);
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();
    ft.query(Compression::None, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);
    ft.open("a.gco", false, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);

    ft.write(b"first", now); // OK
    ft.write(b"second", now); // panic: state is AwaitingWriteAck
}

#[test]
fn file_transfer_exposes_session_response_timeout() {
    let mut session = Session::new().with_response_timeout(std::time::Duration::from_millis(250));
    let ft = FileTransfer::new(&mut session);
    assert_eq!(ft.response_timeout(), std::time::Duration::from_millis(250));
}

#[test]
fn stray_pft_version_outside_query_state_is_ignored() {
    // Regression for the state-gate on PFT:version. Previously a stray
    // PFT:version line could overwrite pending_ascii outside QUERY.
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0);
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();
    ft.query(Compression::None, now);
    let _ = pump_until_event(&mut ft, &mut device, 10); // Negotiated, state=Negotiated

    // State is now Negotiated. A stray PFT:version line here must NOT
    // touch pending_ascii. Feed one, then open(), and verify the open
    // happy path still works.
    ft.feed(b"PFT:version:9.9:none\n", now);
    ft.open("x.gco", false, now);
    assert_eq!(
        pump_until_event(&mut ft, &mut device, 10),
        Some(FileEvent::Opened)
    );
}

#[test]
fn stray_pft_during_write_does_not_swallow_ack() {
    // Regression: a stray PFT line (busy/fail/ioerror/invalid/version)
    // arriving while we're in AwaitingWriteAck must not corrupt the
    // pending_ascii slot and silently consume the legitimate ok<n>.
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0);
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();
    ft.query(Compression::None, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);
    ft.open("x.gco", false, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);

    ft.write(b"chunk", now);
    let write_bytes = ft.poll_outbound().expect("WRITE bytes");
    let sync = write_bytes[2];

    // Stray PFT:ioerror before the ack — must be ignored (state-gated).
    ft.feed(b"PFT:ioerror\n", now);
    ft.feed(format!("ok{sync}\n").as_bytes(), now);

    assert_eq!(ft.poll(), Some(FileEvent::WriteAcked));
}

#[test]
fn stray_pft_busy_during_write_does_not_corrupt_state() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0);
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();
    ft.query(Compression::None, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);
    ft.open("x.gco", false, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);

    ft.write(b"chunk", now);
    let write_bytes = ft.poll_outbound().expect("WRITE bytes");
    let sync = write_bytes[2];
    ft.feed(b"PFT:busy\n", now);
    ft.feed(format!("ok{sync}\n").as_bytes(), now);
    assert_eq!(ft.poll(), Some(FileEvent::WriteAcked));
}

#[test]
fn dummy_open_sets_dummy_byte() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0);
    complete_handshake(&mut session, &mut device);

    let mut ft = FileTransfer::new(&mut session);
    let now = Instant::now();
    ft.query(Compression::None, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);
    ft.open("smoketest.gco", true, now);
    let _ = pump_until_event(&mut ft, &mut device, 10);
    assert_eq!(device.last_open_dummy, Some(true));
}
