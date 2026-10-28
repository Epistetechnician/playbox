//! Real TPM quotes from a software TPM (swtpm 0.10.1) running on Linux, verified on whatever OS runs
//! this test. Regenerate with the `platform_attest` example against swtpm (see docs/attestation.md).
//!
//! These prove the verifier agrees with a real TPM implementation about quote layout, signatures, PCR
//! digests and PCR extension. They prove nothing about trust: swtpm is software.

use ed25519_dalek::SigningKey;
use playbox_core::attest::{self, Statement, Tier};
use playbox_core::platform::{
    extended_from_zero, fingerprint, measurement_digest, PinStore, TpmVerifier, MEASUREMENT_PCR, TPM_KIND,
};
use std::collections::BTreeMap;

const ONE: &str = include_str!("fixtures/swtpm-statement-1.txt");
const TWO: &str = include_str!("fixtures/swtpm-statement-2.txt");

fn evidence_lines(st: &Statement) -> BTreeMap<String, String> {
    let bytes: Vec<u8> = (0..st.platform_evidence.len() / 2)
        .map(|i| u8::from_str_radix(&st.platform_evidence[i * 2..i * 2 + 2], 16).unwrap())
        .collect();
    String::from_utf8(bytes).unwrap().lines().map(|l| l.split_once(' ').map(|(k, v)| (k.to_string(), v.to_string())).unwrap()).collect()
}

fn ak_fingerprint(st: &Statement) -> String {
    let ak = evidence_lines(st)["ak"].clone();
    let bytes: Vec<u8> = (0..ak.len() / 2).map(|i| u8::from_str_radix(&ak[i * 2..i * 2 + 2], 16).unwrap()).collect();
    fingerprint(&bytes)
}

fn pcrs(st: &Statement) -> BTreeMap<u32, String> {
    evidence_lines(st)["pcrs"]
        .split(',')
        .map(|e| {
            let (i, v) = e.split_once(':').unwrap();
            (i.parse().unwrap(), v.to_string())
        })
        .collect()
}

fn pinned_for(text: &str) -> PinStore {
    let mut p = PinStore::in_memory();
    p.add(TPM_KIND, &ak_fingerprint(&Statement::parse(text).unwrap())).unwrap();
    p
}

#[test]
fn a_real_quote_verifies_but_is_not_trusted_until_pinned() {
    let v = attest::verify(ONE, &[&TpmVerifier::new(PinStore::in_memory())]).unwrap();
    assert_eq!(v.achieved, Tier::T0, "{:?}", v.notes);
    assert!(v.notes.iter().any(|n| n.contains("TPM quote verified")), "{:?}", v.notes);
    assert!(v.notes.iter().any(|n| n.contains("not pinned")));
    let st = Statement::parse(ONE).unwrap();
    assert_eq!(st.platform, "tpm");
    assert_eq!(st.measurements.os, "linux");
    assert_eq!(v.device.as_deref(), Some(format!("tpm:{}", ak_fingerprint(&st)).as_str()));

    let pinned = TpmVerifier::new(pinned_for(ONE));
    let v = attest::verify(ONE, &[&pinned]).unwrap();
    assert_eq!(v.achieved, Tier::T1, "{:?}", v.notes);
}

#[test]
fn the_tpm_really_holds_the_declared_measurements_in_pcr_23() {
    // Cross-check against a real TPM: our computation of "reset, then extend once" must equal what
    // the TPM reported.
    let st = Statement::parse(ONE).unwrap();
    let want = extended_from_zero(&measurement_digest(&st.measurements));
    let want_hex: String = want.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(pcrs(&st)[&MEASUREMENT_PCR], want_hex);
}

#[test]
fn t2_needs_operator_reference_values_for_the_boot_pcrs() {
    let st = Statement::parse(ONE).unwrap();
    let boot: BTreeMap<u32, String> = pcrs(&st).into_iter().filter(|(i, _)| *i < 8).collect();
    let strict = TpmVerifier::new(pinned_for(ONE)).with_boot_reference(boot.clone()).unwrap();
    let v = attest::verify(ONE, &[&strict]).unwrap();
    assert_eq!(v.achieved, Tier::T2, "{:?}", v.notes);

    let mut wrong = boot;
    wrong.insert(7, "11".repeat(32));
    let strict = TpmVerifier::new(pinned_for(ONE)).with_boot_reference(wrong).unwrap();
    assert_eq!(attest::verify(ONE, &[&strict]).unwrap().achieved, Tier::T1);
}

#[test]
fn two_statements_from_the_same_tpm_share_one_attestation_key() {
    let (a, b) = (Statement::parse(ONE).unwrap(), Statement::parse(TWO).unwrap());
    assert_eq!(ak_fingerprint(&a), ak_fingerprint(&b), "the persistent AK is reused across calls");
    assert_ne!(a.nonce, b.nonce);
    // One pin covers both.
    let v = TpmVerifier::new(pinned_for(ONE));
    assert_eq!(attest::verify(TWO, &[&v]).unwrap().achieved, Tier::T1);
}

#[test]
fn evidence_cannot_be_replayed_into_another_statement() {
    let (one, two) = (Statement::parse(ONE).unwrap(), Statement::parse(TWO).unwrap());
    // Put statement 1's genuine quote into statement 2 (whose session key is derived from its nonce)
    // and sign it properly. The quote was made for statement 1's binding, so it must not verify.
    let mut forged = two.clone();
    forged.platform_evidence = one.platform_evidence.clone();
    let key = SigningKey::from_bytes(blake3::hash(two.nonce.as_bytes()).as_bytes());
    let text = forged.sign(&key).unwrap();
    let v = attest::verify(&text, &[&TpmVerifier::new(pinned_for(ONE))]).unwrap();
    assert_eq!(v.achieved, Tier::T0);
    assert!(v.notes.iter().any(|n| n.contains("not bound to this statement")), "{:?}", v.notes);
}

#[test]
fn corrupting_a_real_quote_is_detected() {
    let v = TpmVerifier::new(pinned_for(ONE));
    let st = Statement::parse(ONE).unwrap();
    let key = SigningKey::from_bytes(blake3::hash(st.nonce.as_bytes()).as_bytes());
    let resign = |evidence: String| {
        let mut s = st.clone();
        s.platform_evidence = evidence.bytes().map(|b| format!("{b:02x}")).collect();
        s.sign(&key).unwrap()
    };
    let ev: String = String::from_utf8(
        (0..st.platform_evidence.len() / 2).map(|i| u8::from_str_radix(&st.platform_evidence[i * 2..i * 2 + 2], 16).unwrap()).collect(),
    )
    .unwrap();
    // Change one PCR value.
    let pcr_line = ev.lines().find(|l| l.starts_with("pcrs ")).unwrap();
    let tampered = ev.replace(pcr_line, &pcr_line.replacen("0000", "0001", 1));
    assert_eq!(attest::verify(&resign(tampered), &[&v]).unwrap().achieved, Tier::T0);
    // Change one byte of the signed quote.
    let quote_line = ev.lines().find(|l| l.starts_with("quote ")).unwrap();
    let mut q = quote_line.to_string();
    let pos = q.len() - 10;
    q.replace_range(pos..pos + 1, if &q[pos..pos + 1] == "a" { "b" } else { "a" });
    assert_eq!(attest::verify(&resign(ev.replace(quote_line, &q)), &[&v]).unwrap().achieved, Tier::T0);
}
