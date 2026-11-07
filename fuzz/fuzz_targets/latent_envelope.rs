//! Hub latent messages arrive from any member: the signed `PBL2` envelope, the reveal that opens its commitment, and the
//! host's receipt that a client checks. Nothing here may panic, and what the parsers accept must satisfy the bounds the
//! rest of the host relies on.
#![no_main]
use libfuzzer_sys::fuzz_target;
use playbox_net::latent::{parse, verify_receipt, Reveal};

fuzz_target!(|data: &[u8]| {
    if let Ok(env) = parse(data) {
        assert!(env.k > 0 && env.k <= 4096 && env.body.len() == env.k, "envelope bounds");
        assert!(env.scale().is_finite(), "scale");
    }
    if let Ok(rev) = Reveal::parse(data) {
        assert!(!rev.sentences.is_empty() && rev.sentences.len() <= 64, "reveal bounds");
        assert_eq!(rev.commit().len(), 64, "commitment is a 32-byte hash in hex");
    }
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = verify_receipt(text, &[1u8; 32], &[2u8; 32], "0123456789abcdef", b"envelope");
    }
});
