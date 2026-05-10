//! Corruption / retransmit integration tests. Verifies that the host's
//! retransmit policy recovers from injected wire-level events: requested
//! resends, dropped acks, total-budget timeouts.

use std::time::{Duration, Instant};

use marlin_binary_transfer::session::{Event, Session};

#[path = "fixtures/canonical.rs"]
#[allow(dead_code)]
mod canonical;

#[path = "fixtures/fake_device.rs"]
#[allow(dead_code)]
mod fake_device;

use fake_device::{DeviceBehaviour, FakeDevice};

fn pump(session: &mut Session, device: &mut FakeDevice) {
    while let Some(out) = session.poll_outbound() {
        device.feed(&out);
    }
    let reply = device.drain_reply();
    if !reply.is_empty() {
        session.feed(&reply, Instant::now());
    }
}

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
    panic!("handshake did not complete");
}

#[test]
fn rs_request_surfaces_to_caller() {
    let mut session = Session::new();
    let mut device = FakeDevice::new(512, "1.0", 0);
    complete_handshake(&mut session, &mut device);

    // Arm the rs behaviour for the post-handshake QUERY (sync 0).
    device.behaviour = DeviceBehaviour {
        resend_once_for_sync: Some(0),
        ..DeviceBehaviour::default()
    };

    session.send(1, 0, &[], Instant::now());
    pump(&mut session, &mut device);

    let mut saw_rs = false;
    while let Some(evt) = session.poll_event() {
        if let Event::ResendRequested(0) = evt {
            saw_rs = true;
        }
    }
    assert!(
        saw_rs,
        "expected ResendRequested(0) after device's rs reply"
    );
}

#[test]
fn dropped_ack_drives_retransmit_on_tick() {
    // Simulate a flaky link: the host sends a packet, the device's reply
    // is dropped (we just don't pump the device back), and the session's
    // tick should re-emit the same bytes after the response timeout.
    let mut session = Session::new().with_response_timeout(Duration::from_millis(50));
    let t0 = Instant::now();
    session.connect(t0);
    let original = session
        .poll_outbound()
        .expect("initial transmit produces bytes");
    assert!(session.poll_outbound().is_none());

    // Without feeding any reply, advance time past the response timeout.
    session.tick(t0 + Duration::from_millis(100));

    let retransmit = session.poll_outbound().expect("retransmit pending");
    assert_eq!(
        original, retransmit,
        "retransmit should reproduce the original packet bytes"
    );
}

#[test]
fn total_budget_exhausted_emits_timeout_and_drops_pending() {
    let mut session = Session::new()
        .with_response_timeout(Duration::from_millis(20))
        .with_total_timeout(Duration::from_millis(80));
    let t0 = Instant::now();
    session.connect(t0);
    let _ = session.poll_outbound();

    // Many retransmit attempts, never seeing a reply.
    for offset in [25, 50, 75, 100, 200, 300] {
        session.tick(t0 + Duration::from_millis(offset));
    }

    let mut saw_timeout = false;
    while let Some(evt) = session.poll_event() {
        if matches!(evt, Event::Timeout { .. }) {
            saw_timeout = true;
        }
    }
    assert!(
        saw_timeout,
        "expected Event::Timeout after budget exhausted"
    );
    assert!(
        !session.has_pending(),
        "pending packet must be dropped on timeout"
    );
}

#[test]
fn corrupt_ack_token_does_not_panic_and_is_ignored() {
    let mut session = Session::new();
    session.connect(Instant::now());
    let _ = session.poll_outbound();
    session.feed(b"ss0,512,1.0\n", Instant::now());
    let _ = session.poll_event();

    // Garbage that doesn't match any known token.
    session.feed(b"not-a-real-token-line\n", Instant::now());
    while let Some(evt) = session.poll_event() {
        // Should surface as AsciiLine; nothing should panic.
        match evt {
            Event::AsciiLine(_) => {}
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
