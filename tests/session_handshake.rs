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

    s.feed(b"ss5,512,1.0\n", Instant::now());
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
    s.feed(b"ss0,512,1.0\n", Instant::now());
    let _ = s.poll_event();

    s.send(1, 0, &[], Instant::now());
    let bytes = s.poll_outbound().unwrap();
    assert_eq!(bytes[2], 0, "first packet should use sync 0");
    s.feed(b"ok0\n", Instant::now());
    assert!(matches!(s.poll_event().unwrap(), Event::Ack(0)));
    assert_eq!(s.current_sync(), 1);
}

#[test]
fn out_of_sync_ack_is_surfaced() {
    let mut s = Session::new();
    s.connect(Instant::now());
    let _ = s.poll_outbound();
    s.feed(b"ss0,512,1.0\n", Instant::now());
    let _ = s.poll_event();

    s.send(1, 0, &[], Instant::now());
    let _ = s.poll_outbound();
    s.feed(b"ok99\n", Instant::now());
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
    s.feed(b"rs7\n", Instant::now());
    assert!(matches!(s.poll_event().unwrap(), Event::ResendRequested(7)));
}

#[test]
fn fe_emits_fatal_error() {
    let mut s = Session::new();
    s.feed(b"fe\n", Instant::now());
    assert_eq!(s.poll_event(), Some(Event::FatalError));
}

#[test]
fn unknown_lines_pass_through_as_ascii() {
    let mut s = Session::new();
    s.feed(b"PFT:success\n", Instant::now());
    match s.poll_event().unwrap() {
        Event::AsciiLine(line) => assert_eq!(line, "PFT:success"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn feed_handles_partial_lines() {
    let mut s = Session::new();
    s.feed(b"f", Instant::now());
    s.feed(b"e", Instant::now());
    assert!(s.poll_event().is_none());
    s.feed(b"\n", Instant::now());
    assert_eq!(s.poll_event(), Some(Event::FatalError));
}

#[test]
fn feed_handles_crlf() {
    let mut s = Session::new();
    s.feed(b"fe\r\n", Instant::now());
    assert_eq!(s.poll_event(), Some(Event::FatalError));
}

#[test]
fn second_send_queues_until_first_acked() {
    let mut s = Session::new();
    s.connect(Instant::now());
    let _ = s.poll_outbound();
    s.feed(b"ss0,512,1.0\n", Instant::now());
    let _ = s.poll_event();

    s.send(1, 0, &[], Instant::now());
    s.send(1, 4, &[], Instant::now());
    let first = s.poll_outbound().unwrap();
    assert!(s.poll_outbound().is_none());

    s.feed(b"ok0\n", Instant::now());
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
#[should_panic(expected = "Session::send called before SYNC handshake completed")]
fn send_before_handshake_panics() {
    let mut s = Session::new();
    s.send(1, 0, &[], Instant::now()); // panic: is_synced still false
}

#[test]
#[should_panic(expected = "Session::send called before SYNC handshake completed")]
fn send_after_reset_without_reconnect_panics() {
    let mut s = Session::new();
    let now = Instant::now();
    s.connect(now);
    let _ = s.poll_outbound();
    s.feed(b"ss0,512,1.0\n", now);
    let _ = s.poll_event();
    assert!(s.is_synced());

    s.reset();
    // After reset, is_synced is false again — send without re-connecting
    // would dispatch a packet with sync=0 that the device wouldn't ack.
    s.send(1, 0, &[], now); // panic
}

#[test]
fn reset_clears_state_so_new_connect_works() {
    let mut s = Session::new();
    let now = Instant::now();

    // Drive a normal connect+sync, then a normal send.
    s.connect(now);
    let _ = s.poll_outbound();
    s.feed(b"ss5,512,1.0\n", now);
    let _ = s.poll_event(); // consume Synced
    s.send(1, 0, &[], now);
    let _ = s.poll_outbound();

    // Force an OutOfSync — device acks a number we don't expect.
    s.feed(b"ok99\n", now);
    assert!(matches!(s.poll_event().unwrap(), Event::OutOfSync { .. }));

    // After reset, every observable bit of state is back to baseline.
    s.reset();
    assert!(!s.is_synced());
    assert_eq!(s.max_block_size(), None);
    assert_eq!(s.protocol_version(), None);
    assert!(!s.has_pending());
    assert!(s.poll_outbound().is_none());
    assert!(s.poll_event().is_none());
    assert_eq!(s.current_sync(), 0);

    // A fresh connect now produces the canonical SYNC bytes again.
    s.connect(now);
    let bytes = s.poll_outbound().expect("post-reset SYNC pending");
    assert_eq!(bytes, SYNC_PACKET);
}

#[test]
fn reset_preserves_configured_timeouts() {
    let response = Duration::from_millis(123);
    let total = Duration::from_millis(4567);
    let mut s = Session::new()
        .with_response_timeout(response)
        .with_total_timeout(total);
    let t0 = Instant::now();
    s.connect(t0);
    let _ = s.poll_outbound();
    s.reset();

    // Timeout configuration must survive reset: a connect+tick after
    // exactly `response` ms must produce a retransmit, just as it would
    // on a freshly-built session with these timeouts.
    s.connect(t0);
    let first = s.poll_outbound().expect("SYNC pending");
    s.tick(t0 + response + Duration::from_millis(1));
    let retransmit = s.poll_outbound().expect("retransmit pending");
    assert_eq!(first, retransmit);
}

#[test]
fn response_timeout_accessor_returns_configured_value() {
    let custom = Duration::from_millis(345);
    let s = Session::new().with_response_timeout(custom);
    assert_eq!(s.response_timeout(), custom);
}

#[test]
fn total_timeout_accessor_returns_configured_value() {
    let custom = Duration::from_millis(67_890);
    let s = Session::new().with_total_timeout(custom);
    assert_eq!(s.total_timeout(), custom);
}

#[test]
fn response_timeout_has_a_sensible_default() {
    let s = Session::new();
    // We don't assert a specific value (it's a tuning knob), but it
    // must be non-zero so adapters that wrap reads in this duration
    // don't busy-loop.
    assert!(s.response_timeout() > Duration::from_millis(0));
    assert!(s.total_timeout() >= s.response_timeout());
}

#[test]
fn ss_with_garbage_does_not_panic() {
    let mut s = Session::new();
    s.feed(b"ss not a comma list\n", Instant::now());
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
        session.feed(&reply, Instant::now());
    }
}
