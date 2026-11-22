#![no_main]
use ed25519_dalek::SigningKey;
use libfuzzer_sys::fuzz_target;
use playbox_core::{evidence, receipt, roomlog, witness};
use std::collections::BTreeSet;

// Fuzz roomlog text/JSON/checkpoint/equivocation parsers, witness cosigning,
// receipt verification, promotion records, and score evidence verification.
fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    let room_key = SigningKey::from_bytes(&[7u8; 32])
        .verifying_key()
        .to_bytes();

    // 1. Roomlog text deserialization and offline bundle verification.
    let _ = roomlog::text::from_text(room_key, text);
    let _ = playbox_net::roomsync::read_status_text(room_key, text);

    // 2. Roomlog JSON helpers (entry, equivocation) and Config overflow checks.
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(text) {
        let _ = roomlog::text::entry_from_json(&val);
        if let Some(eq) = roomlog::text::equivocation_from_json(&val) {
            let _ = eq.verify(&room_key);
        }
        if let Some(k) = val.get("k").and_then(serde_json::Value::as_u64) {
            if let Ok(k) = usize::try_from(k) {
                let mut hosts = BTreeSet::new();
                hosts.insert(room_key);
                let cfg = roomlog::Config {
                    epoch: 0,
                    leader: room_key,
                    hosts,
                    k,
                };
                let _ = cfg.check();
            }
        }
    }

    // 3. Witness checkpoint parsing, equivocation detection, and cosigning (must never panic).
    let _ = witness::Checkpoint::open(text);
    let mut w = witness::Witness::new(SigningKey::from_bytes(&[11u8; 32]));
    let _ = w.cosign(text, &[]);

    // 4. Receipt and score evidence parsers.
    let _ = receipt::Receipt::open(text);
    let _ = evidence::ScoreEvidence::open(text);
});
