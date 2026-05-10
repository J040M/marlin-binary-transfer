//! End-to-end test for the blocking adapter wired through the FakeDevice
//! over an in-memory Read+Write transport. Confirms the host-level
//! contract (binary trigger → handshake → file ops → control CLOSE).

#![cfg(feature = "blocking")]

use std::cell::RefCell;
use std::io::{Read, Write};
use std::rc::Rc;

use marlin_binary_transfer::adapters::blocking::{upload, UploadOptions};
use marlin_binary_transfer::file_transfer::Compression;

#[path = "fixtures/canonical.rs"]
#[allow(dead_code)]
mod canonical;

#[path = "fixtures/fake_device.rs"]
#[allow(dead_code)]
mod fake_device;

use fake_device::FakeDevice;

/// Read+Write transport that drives a FakeDevice. Writes from the host
/// are fed into the device; reads pull from the device's reply buffer
/// (or return `WouldBlock`-equivalent zero bytes if empty, so the
/// adapter's loop falls through to `tick`).
struct DeviceTransport {
    device: Rc<RefCell<FakeDevice>>,
    pending: Vec<u8>,
}

impl DeviceTransport {
    fn new(device: Rc<RefCell<FakeDevice>>) -> Self {
        Self {
            device,
            pending: Vec::new(),
        }
    }
}

impl Write for DeviceTransport {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.device.borrow_mut().feed(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Read for DeviceTransport {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pending.is_empty() {
            self.pending = self.device.borrow_mut().drain_reply();
        }
        if self.pending.is_empty() {
            // Mimic a real serial port with a short read timeout: no
            // data right now. The adapter's loop treats this as
            // "fall through to tick".
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "idle"));
        }
        let n = buf.len().min(self.pending.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(n)
    }
}

#[test]
fn upload_sends_control_close_after_file_close() {
    let device = Rc::new(RefCell::new(FakeDevice::new(512, "1.0", 0)));
    let mut transport = DeviceTransport::new(device.clone());
    let payload = b"G28\nG1 X10 Y10\nM104 S200\n; end\n";
    let opts = UploadOptions {
        dest_filename: "out.gco".into(),
        compression: Compression::None,
        dummy: false,
        chunk_size: 0,
    };
    let stats = upload(&mut transport, &payload[..], opts).expect("upload");

    assert_eq!(stats.source_bytes, payload.len() as u64);
    let dev = device.borrow();
    assert!(dev.closed, "file CLOSE should have been processed");
    assert!(
        dev.control_closed,
        "control CLOSE (proto=0,type=2) must be sent so device exits binary mode"
    );
}
