# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.2] - 2026-05-13

### Added

- `adapters::{blocking,tokio}::UploadOptions.progress` — optional
  `Box<dyn FnMut(Progress) + Send>` callback fired once after each
  acknowledged WRITE. The `Progress` payload exposes cumulative
  `bytes_sent`, `chunks_sent` and `source_bytes` so callers can drive
  their own UI / rate calculations.

### Changed

- `UploadOptions` no longer derives `Clone` (the new `progress` field
  holds a `FnMut` closure). `Debug` is preserved via a manual impl, so
  the existing `..UploadOptions::default()` patterns keep working.

### Yanked

- 0.1.1 was published with experimental `Progress::percent()` /
  `fraction()` helpers and immediately yanked. 0.1.2 ships the same
  progress callback without those helpers.

## [0.1.0] - 2026-05-11

Initial public release.

### Fixed

- `file_transfer`: state-gate `pending_ascii` setters for every `PFT:*`
  variant. Previously a stray `PFT:busy`/`fail`/`ioerror`/`invalid`/`version`
  line arriving while `AwaitingWriteAck` would corrupt that slot and cause
  the legitimate `ok<n>` to be silently swallowed; the next `write()` would
  then panic with state still `AwaitingWriteAck`. Now out-of-context PFT
  lines are cleanly ignored.
- `adapters::tokio`: every `transport.read(..).await` is now wrapped in
  `tokio::time::timeout(session.response_timeout(), ..)`. Without this,
  a quiet transport meant the loop never reached `session.tick()`, so the
  retransmit and total-budget timeouts were effectively dead code.
- `adapters` (both blocking and tokio): `drive_session_until_synced` now
  surfaces `FileError::SessionFatalError` / `SessionTimeout` /
  `SessionOutOfSync` instead of always returning the generic
  `UploadError::HandshakeFailed` after exhausting the retry budget.
- `adapters` (both blocking and tokio): send the control-plane CLOSE
  (proto=0, type=2) after a successful upload so the device exits binary
  mode and resumes accepting ASCII g-code on the same serial session.
  Previously the device remained in binary mode after `upload()` returned
  `Ok`.

### Added

- `Session::response_timeout()` / `Session::total_timeout()` and
  `FileTransfer::response_timeout()` accessors so adapters and external
  callers can bound their own I/O against the session's timing
  configuration.
- Tokio's `time` feature is now enabled by the `tokio` feature flag.
- New integration test `tests/blocking_adapter_e2e.rs` driving the
  blocking adapter against an in-memory `Read+Write` transport,
  asserting the control CLOSE lands on the device.
- Initial repository scaffold: dual MIT/Apache-2.0 license, README, CI
  scaffolding, module placeholders.
- Codec layer (`codec`) — packet encode/decode, Fletcher-16 mod-255,
  full error variants, validated against the Python reference fixtures.
- Sans-I/O session state machine (`session`) — sync handshake, ack
  matching, retransmit on response timeout, total-budget timeout,
  resend-request and fatal-error pass-through.
- File-transfer state machine (`file_transfer`) — QUERY / OPEN / WRITE
  / CLOSE / ABORT, with compression negotiation (none, heatshrink, auto).
- Optional heatshrink wrapper (`compression`, behind `heatshrink`
  feature) using `embedded-heatshrink`.
- Adapters (`adapters::blocking`, `adapters::tokio`,
  `adapters::serialport`) wrapping the sans-I/O core for the common
  transport choices.
- Examples: `examples/upload.rs` (CLI mirror of Python's `transfer.py`)
  and `examples/inspect.rs` (decoder for captured byte streams).
- Criterion benches for codec encode/decode and Fletcher-16.
- `cargo-fuzz` target for the decoder.

[Unreleased]: https://github.com/J040M/marlin-binary-transfer/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/J040M/marlin-binary-transfer/releases/tag/v0.1.2
[0.1.0]: https://github.com/J040M/marlin-binary-transfer/releases/tag/v0.1.0

## Acknowledgements

The wire format and reference behaviour are taken from Marlin's
[Binary File Transfer Mark II PR (#14817)](https://github.com/MarlinFirmware/Marlin/pull/14817)
and the Python reference implementation
[`marlin-binary-protocol`](https://github.com/trippwill/marlin-binary-protocol)
(MIT) by Chris Pepper (`@p3p`). This is an independent Rust port of the
documented protocol.
