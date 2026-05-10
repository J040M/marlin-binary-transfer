# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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

[Unreleased]: https://github.com/J040M/marlin-binary-transfer/compare/HEAD...HEAD

## Acknowledgements

The wire format and reference behaviour are taken from Marlin's
[Binary File Transfer Mark II PR (#14817)](https://github.com/MarlinFirmware/Marlin/pull/14817)
and the Python reference implementation
[`marlin-binary-protocol`](https://github.com/trippwill/marlin-binary-protocol)
(MIT) by Chris Pepper (`@p3p`). This is an independent Rust port of the
documented protocol.
