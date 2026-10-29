//! A grant is attacker-supplied text that the host parses (protobuf, Datalog blocks) before it can decide
//! anything. Whatever the token, `authorize` must refuse cleanly or accept a real one; never panic or hang.
#![no_main]
use base64::{engine::general_purpose::URL_SAFE, Engine};
use libfuzzer_sys::fuzz_target;
use playbox_net::grants::{Authority, Needs};
use std::collections::HashSet;
use std::sync::OnceLock;

fn auth() -> &'static Authority {
    static A: OnceLock<Authority> = OnceLock::new();
    A.get_or_init(|| Authority::from_seed("fuzz", &[7u8; 32]).unwrap())
}

fuzz_target!(|data: &[u8]| {
    let revoked = HashSet::new();
    let needs = Needs::default();
    // The bytes as the token's raw wire form (re-encoded, so mutations reach the protobuf parser)...
    let token = URL_SAFE.encode(data);
    let _ = auth().authorize(&token, "subject", "echo", &needs, &revoked);
    let _ = auth().verify_invite(&token);
    // ...and as text, for the base64 and framing layer.
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = auth().authorize(text, "subject", "run", &needs, &revoked);
    }
});
