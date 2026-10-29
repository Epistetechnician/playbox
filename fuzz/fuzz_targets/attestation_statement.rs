//! A host's attestation statement is text a guest parses before trusting anything about the host.
#![no_main]
use libfuzzer_sys::fuzz_target;
use playbox_core::attest;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = attest::Statement::parse(text);
        let _ = attest::verify(text, &[]);
        let _ = attest::Tier::parse(text);
    }
});
