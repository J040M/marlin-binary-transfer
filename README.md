# marlin-binary-transfer

[![crates.io](https://img.shields.io/crates/v/marlin-binary-transfer.svg)](https://crates.io/crates/marlin-binary-transfer)
[![docs.rs](https://docs.rs/marlin-binary-transfer/badge.svg)](https://docs.rs/marlin-binary-transfer)
[![CI](https://github.com/J040M/marlin-binary-transfer/actions/workflows/ci.yml/badge.svg)](https://github.com/J040M/marlin-binary-transfer/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Host-side Rust implementation of [Marlin's Binary File Transfer Mark II protocol][bft-pr].
Uploads G-code files to a 3D printer's SD card over serial — with framing checksums,
sync acknowledgement, retransmit on timeout, and optional heatshrink payload compression.

> **Status: pre-1.0.** The Marlin protocol itself is documented as experimental
> upstream and may evolve. This crate tracks Marlin `bugfix-2.x`. Public API
> follows semver within 0.x; pin a minor version.

## Why this exists

The text-mode `M28`/`M29` SD upload path is unreliable in practice — multiple
open Marlin issues, slow over UART, no integrity checks. The binary protocol is
the correct path: roughly 10 KiB/s on 115200-baud UART and 180 KiB/s on native
USB CDC, with per-packet checksums and explicit retransmit.

Before this crate, the only host implementation was the Python
[`marlin-binary-protocol`][py-ref] used by OctoPrint's MarlinBft plugin. This is
an independent Rust port — same wire format, sans-I/O core, three optional
adapters for the common transports.

## Design

```text
┌────────────────────────────────────────────────────────────┐
│  adapters::{blocking, tokio, serialport}   ← optional I/O  │
├────────────────────────────────────────────────────────────┤
│  file_transfer  ← protocol-1 state machine (QUERY/OPEN/…)  │
├────────────────────────────────────────────────────────────┤
│  session        ← sync counter, retransmit, ack matching   │
├────────────────────────────────────────────────────────────┤
│  codec          ← packet framing + Fletcher-16 checksum    │
└────────────────────────────────────────────────────────────┘
```

The bottom three layers are sans-I/O: you feed in bytes and pull events out,
plumbed through any transport you want. The `adapters` modules wrap the core
into one-call upload helpers for the common cases.

## Feature flags

| Feature           | Default | What you get                                                                  |
|-------------------|---------|-------------------------------------------------------------------------------|
| `std`             | yes     | Currently unconditional; reserved for the future no_std + alloc split.        |
| `blocking`        | no      | Synchronous `upload(transport, src, opts)` over any `Read + Write` transport. |
| `tokio`           | no      | Async `upload(transport, src, opts).await` over `AsyncRead + AsyncWrite`.     |
| `serial`          | no      | `serialport::open` helper preconfigured with sensible defaults. Implies `blocking`. |
| `heatshrink`      | no      | Negotiate and apply heatshrink payload compression with the device.           |

## Quickstart (blocking + serial)

```rust,no_run
use marlin_binary_transfer::adapters::blocking::{upload, UploadOptions};
use marlin_binary_transfer::adapters::serialport;
use marlin_binary_transfer::file_transfer::Compression;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut port = serialport::open("/dev/ttyUSB0", 250_000)?;
    let file = std::fs::File::open("model.gco")?;

    let stats = upload(&mut *port, file, UploadOptions {
        dest_filename: "model.gco".into(),
        compression: Compression::Auto,
        dummy: false,
        chunk_size: 0, // 0 = use the device-advertised maximum
    })?;

    println!(
        "uploaded {} bytes in {} chunks ({:?})",
        stats.bytes_sent, stats.chunks_sent, stats.compression,
    );
    Ok(())
}
```

Cargo:

```toml
[dependencies]
marlin-binary-transfer = { version = "0.1", features = ["blocking", "serial", "heatshrink"] }
```

Run the bundled CLI example:

```bash
cargo run --example upload --features blocking,serial,heatshrink -- \
    --port /dev/ttyUSB0 --baud 250000 \
    --src ./model.gco --dest model.gco --compression auto
```

## Quickstart (tokio)

```rust,no_run
use marlin_binary_transfer::adapters::tokio::{upload, UploadOptions};
use marlin_binary_transfer::file_transfer::Compression;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `transport` is any AsyncRead + AsyncWrite + Unpin — e.g. a tokio-serial
    // SerialStream, a TCP-to-serial bridge, etc.
    # let mut transport: tokio::io::DuplexStream = unreachable!();
    let mut src = tokio::fs::File::open("model.gco").await?;

    let stats = upload(&mut transport, &mut src, UploadOptions {
        dest_filename: "model.gco".into(),
        compression: Compression::Auto,
        ..UploadOptions::default()
    }).await?;
    println!("uploaded {} bytes", stats.bytes_sent);
    Ok(())
}
```

## What the upload helper does for you

The `adapters::{blocking,tokio}::upload` helpers handle the full lifecycle so
you don't have to drive the state machines manually:

1. Send the ASCII trigger `M28 B1` to put the firmware in binary mode.
2. Drive the SYNC handshake; capture the device-advertised block size and
   protocol version.
3. Issue `QUERY` to negotiate compression (`none` or `heatshrink` per the
   device's advertised capabilities).
4. `OPEN` the destination file with the chosen compression mode.
5. Stream the source through `WRITE` packets, retransmitting any packet the
   device requests via `rs<n>` or that times out.
6. `CLOSE` the file (commits to SD).
7. **Send the control-plane CLOSE** (proto=0, type=2) so the device returns to
   ASCII g-code mode. Without this, subsequent ASCII commands on the same
   serial session are ignored — this was added in 0.1.1.

On any unrecoverable failure (`PFT:busy`, `PFT:fail`, `PFT:ioerror`,
`PFT:invalid`, fatal session error, sync drift) you get a specific
`UploadError::Transfer(FileError::…)` rather than a generic timeout.

## Sans-I/O usage

If the helpers don't fit (custom transport, fan-out to many printers, your own
event loop), build directly on `Session` and `FileTransfer`:

```rust,no_run
use std::time::Instant;
use marlin_binary_transfer::session::{Session, Event};
use marlin_binary_transfer::file_transfer::{FileTransfer, FileEvent, Compression};

let mut session = Session::new();
session.connect(Instant::now());

// Drain `session.poll_outbound()` and write bytes to your transport.
// Push received bytes via `session.feed(bytes, Instant::now())`.
// Call `session.tick(Instant::now())` at least as often as
// `session.response_timeout()` so retransmits fire.
// Drain `session.poll_event()` until `Event::Synced` is observed.

# loop {
#     break;
# }
let mut ft = FileTransfer::new(&mut session);
ft.query(Compression::Auto, Instant::now());
// Same pump pattern: poll_outbound → write, read → feed, then poll() for FileEvents.
```

See the rustdoc on [`session`] and [`file_transfer`] for the full event
vocabulary.

## Timeouts & reliability

- `Session::with_response_timeout(d)` — how long to wait before retransmitting
  an in-flight packet. Default: 1 second.
- `Session::with_total_timeout(d)` — total budget for a single packet across
  all retransmits. Default: 20 seconds.

The tokio adapter wraps every inbound read in `tokio::time::timeout(...)` keyed
to `response_timeout`, so a quiet transport never deadlocks the loop. The
blocking adapter relies on the transport returning `ErrorKind::TimedOut` on
idle — `adapters::serialport::open` configures a 100 ms read timeout for this.

## Protocol prerequisites on the printer

The Marlin firmware must have:

- `BINARY_FILE_TRANSFER` enabled at compile time
- `MEATPACK` disabled (mutually exclusive with BFT)

You can detect both via the `M115` capability output before attempting an
upload.

## MSRV

Rust **1.75** for the library itself. Development tooling (criterion, recent
test deps) may require a more recent toolchain.

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be dual-licensed as above, without any additional terms or conditions.

## Credits

Protocol design and the original Python reference implementation by
Chris Pepper ([@p3p](https://github.com/p3p)). This crate is an independent
Rust port of the wire format described in
[MarlinFirmware/Marlin#14817][bft-pr], cross-checked against the
[`trippwill/marlin-binary-protocol`][py-ref] reference.

[bft-pr]: https://github.com/MarlinFirmware/Marlin/pull/14817
[py-ref]: https://github.com/trippwill/marlin-binary-protocol
[`session`]: https://docs.rs/marlin-binary-transfer/latest/marlin_binary_transfer/session/
[`file_transfer`]: https://docs.rs/marlin-binary-transfer/latest/marlin_binary_transfer/file_transfer/
