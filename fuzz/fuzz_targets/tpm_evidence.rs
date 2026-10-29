//! Hardware evidence is chosen by the host being judged: a TPM key blob, a quote, an endorsement certificate
//! chain, a Secure Enclave signature. The guest parses all of it (including X.509) before deciding anything.
#![no_main]
use ed25519_dalek::SigningKey;
use libfuzzer_sys::fuzz_target;
use playbox_core::{attest, platform, tpm_ek};

/// Hashing the running binary is slow; do it once, not per input.
fn measurements() -> &'static attest::Measurements {
    static M: std::sync::OnceLock<attest::Measurements> = std::sync::OnceLock::new();
    M.get_or_init(|| attest::measure_self("none", "fuzz"))
}

fn verifiers() -> &'static Vec<Box<dyn attest::PlatformVerifier>> {
    static V: std::sync::OnceLock<Vec<Box<dyn attest::PlatformVerifier>>> = std::sync::OnceLock::new();
    V.get_or_init(|| platform::verifiers(platform::PinStore::in_memory(), Default::default()).unwrap())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fuzz_target!(|data: &[u8]| {
    // Direct entry points.
    let _ = tpm_ek::parse_ak(data);
    let _ = tpm_ek::trim_der(data);
    let (leaf, rest) = data.split_at(data.len() / 2);
    let _ = tpm_ek::check_ek_chain(leaf, &[rest.to_vec()], &[leaf.to_vec()], 1_700_000_000);

    // As the evidence in a statement, through the real verifiers.
    let key = SigningKey::from_bytes(&[3u8; 32]);
    for kind in ["tpm", "secure-enclave"] {
        let mut st = attest::new_statement(&key, "aa", "", 1, measurements().clone());
        st.platform = kind.to_string();
        st.platform_evidence = hex(data);
        st.claimed = attest::Tier::T2;
        for v in verifiers() {
            if v.kind() == kind {
                let _ = v.verify(&st);
            }
        }
    }
});
