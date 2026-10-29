//! A Confidential Space attestation token (a JWT) comes from the host being judged.
#![no_main]
use ed25519_dalek::SigningKey;
use libfuzzer_sys::fuzz_target;
use playbox_core::{attest, attest::PlatformVerifier, confidential::{ConfidentialSpaceVerifier, TDX_KIND}};

fn measurements() -> &'static attest::Measurements {
    static M: std::sync::OnceLock<attest::Measurements> = std::sync::OnceLock::new();
    M.get_or_init(|| attest::measure_self("none", "fuzz"))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fuzz_target!(|data: &[u8]| {
    let verifier = ConfidentialSpaceVerifier::new("fuzz-audience").with_clock(|| 1_700_000_000);
    let key = SigningKey::from_bytes(&[3u8; 32]);
    let mut st = attest::new_statement(&key, "aa", "", 1, measurements().clone());
    st.platform = TDX_KIND.to_string();
    st.claimed = attest::Tier::T3;
    // Raw bytes as hex (how evidence travels): this mostly checks the evidence envelope.
    st.platform_evidence = hex(data);
    let _ = verifier.verify(&st);
    // With the `token ` line the verifier expects, so the bytes reach the JWT parser.
    if let Ok(text) = std::str::from_utf8(data) {
        st.platform_evidence = hex(format!("token {text}").as_bytes());
        let _ = verifier.verify(&st);
    }
});
