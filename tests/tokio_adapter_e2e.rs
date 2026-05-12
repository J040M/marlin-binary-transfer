//! End-to-end test for the tokio adapter. Mirror of blocking_adapter_e2e
//! but driven over AsyncRead+AsyncWrite. Confirms the control CLOSE
//! (proto=0, type=2) is sent and that read_with_timeout lets tick()
//! fire when the transport is silent.

#![cfg(feature = "tokio")]

use std::cell::RefCell;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use marlin_binary_transfer::adapters::tokio::{upload, UploadOptions};
use marlin_binary_transfer::file_transfer::Compression;

#[path = "fixtures/canonical.rs"]
#[allow(dead_code)]
mod canonical;

#[path = "fixtures/fake_device.rs"]
#[allow(dead_code)]
mod fake_device;

use fake_device::FakeDevice;

/// AsyncRead + AsyncWrite wrapper around a FakeDevice. Reads return
/// Pending when the device has nothing queued; a Waker is registered
/// and re-armed by the next write (mirroring how a real async serial
/// transport behaves).
struct AsyncDeviceTransport {
    device: Rc<RefCell<FakeDevice>>,
    pending: Vec<u8>,
}

impl AsyncDeviceTransport {
    fn new(device: Rc<RefCell<FakeDevice>>) -> Self {
        Self {
            device,
            pending: Vec::new(),
        }
    }
}

impl AsyncWrite for AsyncDeviceTransport {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        this.device.borrow_mut().feed(buf);
        Poll::Ready(Ok(buf.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AsyncRead for AsyncDeviceTransport {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.pending.is_empty() {
            this.pending = this.device.borrow_mut().drain_reply();
        }
        if this.pending.is_empty() {
            // No data right now. Returning Pending without arming a
            // waker would deadlock — but the adapter wraps every read
            // in tokio::time::timeout, so the runtime's timer wakes us.
            // We still re-schedule eagerly so the test runtime makes
            // progress.
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        let n = this.pending.len().min(buf.remaining());
        buf.put_slice(&this.pending[..n]);
        this.pending.drain(..n);
        Poll::Ready(Ok(()))
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn tokio_upload_sends_control_close_after_file_close() {
    let device = Rc::new(RefCell::new(FakeDevice::new(512, "1.0", 0)));
    let mut transport = AsyncDeviceTransport::new(device.clone());
    let mut src: &[u8] = b"G28\nG1 X10\nM104 S200\n";
    let opts = UploadOptions {
        dest_filename: "out.gco".into(),
        compression: Compression::None,
        dummy: false,
        chunk_size: 0,
        progress: None,
    };
    let stats = upload(&mut transport, &mut src, opts)
        .await
        .expect("upload");

    assert_eq!(stats.source_bytes, b"G28\nG1 X10\nM104 S200\n".len() as u64);
    let dev = device.borrow();
    assert!(dev.closed, "file CLOSE should have been processed");
    assert!(
        dev.control_closed,
        "control CLOSE (proto=0,type=2) must be sent so device exits binary mode"
    );
}

/// Transport that returns Pending forever on read (simulating a dead
/// link). The tokio adapter's read_with_timeout must let the loop fall
/// through to tick(), eventually surfacing HandshakeFailed after the
/// 200-iteration budget instead of hanging.
struct DeadTransport;

impl AsyncWrite for DeadTransport {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AsyncRead for DeadTransport {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        // Never wake — simulates a dead transport. The adapter must
        // make progress anyway via its read timeout.
        Poll::Pending
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn tokio_progress_callback_fires_once_per_chunk() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    let device = Rc::new(RefCell::new(FakeDevice::new(512, "1.0", 0)));
    let mut transport = AsyncDeviceTransport::new(device.clone());
    let payload = vec![b'G'; 256];
    let mut src: &[u8] = &payload[..];
    let chunks = Arc::new(AtomicU64::new(0));
    let bytes = Arc::new(AtomicU64::new(0));
    let chunks_cb = Arc::clone(&chunks);
    let bytes_cb = Arc::clone(&bytes);
    let opts = UploadOptions {
        dest_filename: "out.gco".into(),
        compression: Compression::None,
        dummy: false,
        chunk_size: 64,
        progress: Some(Box::new(move |p| {
            chunks_cb.store(p.chunks_sent, Ordering::SeqCst);
            bytes_cb.store(p.bytes_sent, Ordering::SeqCst);
        })),
    };
    let stats = upload(&mut transport, &mut src, opts)
        .await
        .expect("upload");
    assert_eq!(stats.chunks_sent, 4);
    assert_eq!(chunks.load(Ordering::SeqCst), 4);
    assert_eq!(bytes.load(Ordering::SeqCst), 256);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn tokio_dead_transport_does_not_deadlock() {
    // With paused time, tokio::time::timeout auto-advances the clock
    // each await. So 200 iterations * response_timeout completes
    // virtually instantly, ending in HandshakeFailed rather than
    // hanging the test runtime.
    let mut transport = DeadTransport;
    let mut src: &[u8] = b"";
    let opts = UploadOptions {
        dest_filename: "out.gco".into(),
        compression: Compression::None,
        dummy: false,
        chunk_size: 0,
        progress: None,
    };
    let err = upload(&mut transport, &mut src, opts)
        .await
        .expect_err("expected failure");
    use marlin_binary_transfer::adapters::tokio::UploadError;
    match err {
        UploadError::HandshakeFailed => {}
        other => panic!("expected HandshakeFailed, got {other:?}"),
    }
}
