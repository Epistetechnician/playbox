//! Revocation notices arrive from any peer over gossip and pull and are parsed before being believed.
#![no_main]
use libfuzzer_sys::fuzz_target;
use playbox_net::{grants::Authority, revoke::Notice};
use std::sync::OnceLock;

fn auth() -> &'static Authority {
    static A: OnceLock<Authority> = OnceLock::new();
    A.get_or_init(|| Authority::from_seed("fuzz", &[7u8; 32]).unwrap())
}

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = Notice::parse(text, auth());
    }
});
