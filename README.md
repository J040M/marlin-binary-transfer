# marlin-binary-transfer

Host-side implementation of [Marlin's Binary File Transfer Mark II protocol][bft-pr]
in Rust. Lets a host computer upload G-code files to a 3D printer's SD card over
serial, with framing checksums, sync acknowledgement, and optional heatshrink
compression.

> **Status: experimental, pre-1.0.** The Marlin protocol itself is described as
> experimental in the upstream PR and may change. This crate tracks Marlin
> `bugfix-2.x`. API will evolve in 0.x; pin a minor version.

## Why this exists

The text-mode `M28`/`M29` SD upload path is unreliable in practice (multiple
open Marlin issues, slow over UART). The binary protocol is the proper path —
roughly 10 KiB/s on 115200-baud UART and 180 KiB/s on native USB CDC. Until now
there was no Rust implementation; the only host library was the Python
[`marlin-binary-protocol`][py-ref] referenced by OctoPrint's MarlinBft plugin.

## Crate layout

This is a sans-I/O crate at its core: you feed it bytes and pull events out.
Adapter modules behind feature flags wrap that core for common cases:

| Feature       | What you get                                               |
|---------------|------------------------------------------------------------|
| `std` (default) | `std::io::Error`-based errors, alloc-using API           |
| `heatshrink`  | Optional payload compression negotiated with the device   |
| `blocking`    | A synchronous `upload(transport, src, dest, ...)` adapter |
| `tokio`       | An async `upload(...).await` adapter                      |
| `serial`      | Default `Transport` impl backed by the `serialport` crate |

## Quickstart

> **Not yet — this is the 0.1.0 development branch.** A runnable example will
> be added in the README once the adapter crates are wired up (Stage 6 of the
> implementation plan).

## Protocol prerequisites

Your printer firmware must have:

- `BINARY_FILE_TRANSFER` enabled at compile time
- `MEATPACK` disabled (mutually exclusive with BFT)

You can detect both via M115 capability output before attempting an upload.

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

## Credits

Protocol design and the original Python reference implementation by
Chris Pepper ([@p3p](https://github.com/p3p)). This crate is an independent
Rust port of the wire format described in
[MarlinFirmware/Marlin#14817][bft-pr], with the
[`trippwill/marlin-binary-protocol`][py-ref] code as cross-reference.

[bft-pr]: https://github.com/MarlinFirmware/Marlin/pull/14817
[py-ref]: https://github.com/trippwill/marlin-binary-protocol
