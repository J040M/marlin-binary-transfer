//! Session-layer integration tests: SYNC handshake against an in-process
//! fake device, ack/nack/sync/fatal handling, retransmit and total-timeout
//! behaviour.

use std::time::{Duration, Instant};

use marlin_binary_transfer::session::{Event, Session};

#[path = "fixtures/canonical.rs"]
#[allow(dead_code)]
mod canonical;

#[path = "fixtures/fake_device.rs"]
#[allow(dead_code)]
mod fake_device;

use canonical::SYNC_PACKET;
use fake_device::FakeDevice;

#[test]
fn connect_emits_canonical_sync_packet() {
    let mut s = Session::new();
    s.connect(Instant::now());
    let bytes = s.poll_outbound().expect("SYNC bytes pending");
    assert_eq!(bytes, SYNC_PACKET);
    assert!(s.poll_outbound().is_none());
}

#[test]
fn ss_reply_completes_handshake() {
    let mut s = Session::new();
    s.connect(Instant::now());
    let _ = s.poll_outbound();

    s.feed(b"ss5,512,1.0\n");
    match s.poll_event().unwrap() {
        Event::Synced {
            max_block_size,
            protocol_version,
        } => {
            assert_eq!(max_block_size, 512);
            assert_eq!(protocol_version, "1.0");
        }
        other => panic!("expected Synced, got {other:?}"),
    }
    assert!(s.is_synced());
    assert_eq!(s.max_block_size(), Some(512));
    assert_eq!(s.protocol_version(), Some("1.0"));
    assert_eq!(s.current_sync(), 5);
    assert!(!s.has_pending());
}

#[test]
fn ok_acks_advance_sync_counter() {
    let mut s = Session::new();
    s.connect(Instant::now());
    let _ = s.poll_outbound();
    s.feed(b"ss0,512,1.0\n");
    let _ = s.poll_event();

    s.send(1, 0, &[], Instant::now());
    let bytes = s.poll_outbound().unwrap();
    assert_eq!(bytes[2], 0, "first packet should use sync 0");
    s.feed(b"ok0\n");
    assert!(matches!(s.poll_event().unwrap(), Event::Ack(0)));
    assert_eq!(s.current_sync(), 1);
}

#[test]
fn out_of_sync_ack_is_surfaced() {
    let mut s = Session::new();
    s.connect(Instant::now());
    let _ = s.poll_outbound();
    s.feed(b"ss0,512,1.0\n");
    let _ = s.poll_event();

    s.send(1, 0, &[], Instant::now());
    let _ = s.poll_outbound();
    s.feed(b"ok99\n");
    match s.poll_event().unwrap() {
        Event::OutOfSync { expected, got } => {
            assert_eq!(expected, 0);
            assert_eq!(got, 99);
        }
        other => panic!("expected OutOfSync, got {other:?}"),
    }
}

#[test]
fn rs_request_is_surfaced() {
    let mut s = Session::new();
    s.feed(b"rs7\n");
    assert!(matches!(s.poll_event().unwrap(), Event::ResendRequested(7)));
}

#[test]
fn fe_emits_fatal_error() {
    let mut s = Session::new();
    s.feed(b"fe\n");
    assert_eq!(s.poll_event(), Some(Event::FatalError));
}

#[test]
fn unknown_lines_pass_through_as_ascii() {
    let mut s = Session::new();
    s.feed(b"PFT:success\n");
    match s.poll_event().unwrap() {
        Event::AsciiLine(line) => assert_eq!(line, "PFT:success"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn feed_handles_partial_lines() {
    let mut s = Session::new();
    s.feed(b"f");
    s.feed(b"e");
    assert!(s.poll_event().is_none());
    s.feed(b"\n");
    assert_eq!(s.poll_event(), Some(Event::FatalError));
}

#[test]
fn feed_handles_crlf() {
    let mut s = Session::new();
    s.feed(b"fe\r\n");
    assert_eq!(s.poll_event(), Some(Event::FatalError));
}

#[test]
fn second_send_queues_until_first_acked() {
    let mut s = Session::new();
    s.connect(Instant::now());
    let _ = s.poll_outbound();
    s.feed(b"ss0,512,1.0\n");
    let _ = s.poll_event();

    s.send(1, 0, &[], Instant::now());
    s.send(1, 4, &[], Instant::now());
    let first = s.poll_outbound().unwrap();
    assert!(s.poll_outbound().is_none());

    s.feed(b"ok0\n");
    let _ = s.poll_event();
    let second = s.poll_outbound().unwrap();
    assert_ne!(first, second);
    assert_eq!(second[2], 1, "second packet should use sync 1");
}

#[test]
fn tick_retransmits_after_response_timeout() {
    let mut s = Session::new().with_response_timeout(Duration::from_millis(100));
    let t0 = Instant::now();
    s.connect(t0);
    let first = s.poll_outbound().expect("initial transmit");
    assert!(s.poll_outbound().is_none());

    s.tick(t0 + Duration::from_millis(50));
    assert!(s.poll_outbound().is_none());

    s.tick(t0 + Duration::from_millis(150));
    let retransmit = s.poll_outbound().expect("retransmit pending");
    assert_eq!(first, retransmit);
}

#[test]
fn tick_emits_timeout_after_total_budget() {
    let mut s = Session::new()
        .with_response_timeout(Duration::from_millis(50))
        .with_total_timeout(Duration::from_millis(200));
    let t0 = Instant::now();
    s.connect(t0);
    let _ = s.poll_outbound();

    s.tick(t0 + Duration::from_millis(300));
    assert!(matches!(s.poll_event().unwrap(), Event::Timeout { .. }));
}

#[test]
fn ss_with_garbage_does_not_panic() {
    let mut s = Session::new();
    s.feed(b"ss not a comma list\n");
    assert!(s.poll_event().is_none());
}

// ---- Full handshake against the FakeDevice fixture --------------------------

#[test]
fn handshake_round_trip_through_fake_device() {
    let mut s = Session::new();
    let mut device = FakeDevice::new(/* max_block_size = */ 512, "1.0", /* sync = */ 0);

    let now = Instant::now();
    s.connect(now);
    pump_once(&mut s, &mut device);

    let mut saw_synced = false;
    while let Some(evt) = s.poll_event() {
        if let Event::Synced { max_block_size, .. } = evt {
            assert_eq!(max_block_size, 512);
            saw_synced = true;
        }
    }
    assert!(saw_synced, "Synced event must fire after handshake");
    assert!(s.is_synced());
}

fn pump_once(session: &mut Session, device: &mut FakeDevice) {
    while let Some(out) = session.poll_outbound() {
        device.feed(&out);
    }
    let reply = device.drain_reply();
    if !reply.is_empty() {
        session.feed(&reply);
    }
}
