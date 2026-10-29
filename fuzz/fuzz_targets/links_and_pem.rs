//! Things a user pastes or a peer sends as text: join links, peer addresses, transports, PEM bundles.
#![no_main]
use libfuzzer_sys::fuzz_target;
use playbox_net::{decode_peer, transport::pem_certs, Link, Transport};

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = Link::parse(text);
        let _ = decode_peer(text);
        let _ = pem_certs(text);
        // `Transport::parse` reads a file named by an environment variable only for relay URLs; without it
        // that path is skipped, so this exercises just the parsing.
        let _ = Transport::parse(text);
        let _ = playbox_core::tpm_ek::pem_to_ders(text);
        let _ = playbox_core::taint::parse_approvers(text);
    }
});
