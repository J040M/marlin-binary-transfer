#![no_main]
//! cargo-fuzz target for the codec decoder. Must never panic on any input.

use libfuzzer_sys::fuzz_target;
use marlin_binary_transfer::codec;

fuzz_target!(|data: &[u8]| {
    // Decode arbitrary input. We only assert no panic — Err results are
    // expected for non-packet input.
    let _ = codec::decode(data);
});
