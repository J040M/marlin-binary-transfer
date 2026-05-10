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
        session.feed(&reply);
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
        ft.feed(&reply);
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
