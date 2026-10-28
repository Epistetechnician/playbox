//! Real Secure Enclave, on a Mac: build the helper, produce evidence, verify it.
#![cfg(target_os = "macos")]

use ed25519_dalek::SigningKey;
use playbox_core::attest::{self, Tier};
use playbox_core::platform::{fingerprint, PinStore, SecureEnclaveProvider, SecureEnclaveVerifier, SE_KIND};
use std::{path::PathBuf, process::Command};

fn build_helper(dir: &std::path::Path) -> Option<PathBuf> {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/playbox-se/playbox-se.swift");
    let out = dir.join("playbox-se");
    let ok = Command::new("swiftc").arg("-O").arg(&src).arg("-o").arg(&out).status().ok()?.success();
    ok.then_some(out)
}

#[test]
fn secure_enclave_statement_verifies_and_pins() {
    let dir = std::env::temp_dir().join(format!("playbox-se-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let Some(helper) = build_helper(&dir) else {
        eprintln!("skipping: swiftc not available");
        return;
    };
    let provider = SecureEnclaveProvider { helper: helper.clone(), key_file: dir.join("se.key") };
    let session = SigningKey::from_bytes(&[4; 32]);
    let m = attest::measure_self("none", "test");

    let text = attest::new_platform_statement(&session, "0a0b", "", 1, m.clone(), &provider).unwrap();
    // Unpinned: valid, but stays T0 and reports the fingerprint to pin.
    let unpinned = SecureEnclaveVerifier::new(PinStore::in_memory());
    let v = attest::verify(&text, &[&unpinned]).unwrap();
    assert_eq!(v.achieved, Tier::T0);
    let device = v.device.clone().expect("device fingerprint is reported");
    let fp = device.strip_prefix("secure-enclave:").unwrap().to_string();

    // Pinned: T1.
    let mut pins = PinStore::in_memory();
    pins.add(SE_KIND, &fp).unwrap();
    assert_eq!(attest::verify(&text, &[&SecureEnclaveVerifier::new(pins.clone())]).unwrap().achieved, Tier::T1);

    // The key handle persists: a later statement comes from the same device key.
    let again = attest::new_platform_statement(&session, "0c0d", "", 2, m, &provider).unwrap();
    let v2 = attest::verify(&again, &[&SecureEnclaveVerifier::new(pins)]).unwrap();
    assert_eq!(v2.achieved, Tier::T1);
    assert_eq!(v2.device, v.device);

    // A different key file is a different device key, so the pin does not carry over.
    let other = SecureEnclaveProvider { helper, key_file: dir.join("other.key") };
    let t3 = attest::new_platform_statement(&session, "0e0f", "", 3, attest::measure_self("none", "test"), &other).unwrap();
    let mut only_first = PinStore::in_memory();
    only_first.add(SE_KIND, &fp).unwrap();
    let r = attest::verify(&t3, &[&SecureEnclaveVerifier::new(only_first)]).unwrap();
    assert_eq!(r.achieved, Tier::T0);
    assert_ne!(r.device, v.device);
    let _ = fingerprint(b"");
}
