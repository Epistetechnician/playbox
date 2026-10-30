//! Real TPM quotes from a software TPM (swtpm 0.10.1) running on Linux, verified on whatever OS runs
//! this test. Regenerate with the `platform_attest` example against swtpm (see docs/attestation.md).
//!
//! These prove the verifier agrees with a real TPM implementation about quote layout, signatures, PCR
//! digests and PCR extension. They prove nothing about trust: swtpm is software.

use ed25519_dalek::SigningKey;
use playbox_core::attest::{self, Statement, Tier};
use playbox_core::platform::{
    extended_from_zero, fingerprint, measurement_digest, PinStore, TpmVerifier, MEASUREMENT_PCR,
    TPM_KIND,
};
use playbox_core::tpm_ek;
use std::collections::BTreeMap;

const ONE: &str = include_str!("fixtures/swtpm-statement-1.txt");
const TWO: &str = include_str!("fixtures/swtpm-statement-2.txt");
const ROOT_PEM: &str = include_str!("fixtures/swtpm-ca-root.pem");
const ISSUER_PEM: &str = include_str!("fixtures/swtpm-ca-issuer.pem");

fn evidence_lines(st: &Statement) -> BTreeMap<String, String> {
    let bytes: Vec<u8> = (0..st.platform_evidence.len() / 2)
        .map(|i| u8::from_str_radix(&st.platform_evidence[i * 2..i * 2 + 2], 16).unwrap())
        .collect();
    String::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|l| {
            l.split_once(' ')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .unwrap()
        })
        .collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
        .collect()
}

fn ak(st: &Statement) -> tpm_ek::AkPublic {
    tpm_ek::parse_ak(&unhex(&evidence_lines(st)["akpub"])).unwrap()
}

fn ak_fingerprint(st: &Statement) -> String {
    let a = ak(st);
    fingerprint(&[a.x, a.y].concat())
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
    p.add(TPM_KIND, &ak_fingerprint(&Statement::parse(text).unwrap()))
        .unwrap();
    p
}

#[test]
fn a_real_quote_verifies_but_is_not_trusted_until_pinned() {
    let v = attest::verify(ONE, &[&TpmVerifier::new(PinStore::in_memory())]).unwrap();
    assert_eq!(v.achieved, Tier::T0, "{:?}", v.notes);
    assert!(
        v.notes.iter().any(|n| n.contains("TPM quote verified")),
        "{:?}",
        v.notes
    );
    assert!(v.notes.iter().any(|n| n.contains("not pinned")));
    let st = Statement::parse(ONE).unwrap();
    assert_eq!(st.platform, "tpm");
    assert_eq!(st.measurements.os, "linux");
    assert_eq!(
        v.device.as_deref(),
        Some(format!("tpm:{}", ak_fingerprint(&st)).as_str())
    );

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
    let strict = TpmVerifier::new(pinned_for(ONE))
        .with_boot_reference(boot.clone())
        .unwrap();
    let v = attest::verify(ONE, &[&strict]).unwrap();
    assert_eq!(v.achieved, Tier::T2, "{:?}", v.notes);

    let mut wrong = boot;
    wrong.insert(7, "11".repeat(32));
    let strict = TpmVerifier::new(pinned_for(ONE))
        .with_boot_reference(wrong)
        .unwrap();
    assert_eq!(attest::verify(ONE, &[&strict]).unwrap().achieved, Tier::T1);
}

#[test]
fn two_statements_from_the_same_tpm_share_one_attestation_key() {
    let (a, b) = (
        Statement::parse(ONE).unwrap(),
        Statement::parse(TWO).unwrap(),
    );
    assert_eq!(
        ak_fingerprint(&a),
        ak_fingerprint(&b),
        "the persistent AK is reused across calls"
    );
    assert_ne!(a.nonce, b.nonce);
    // One pin covers both.
    let v = TpmVerifier::new(pinned_for(ONE));
    assert_eq!(attest::verify(TWO, &[&v]).unwrap().achieved, Tier::T1);
}

#[test]
fn evidence_cannot_be_replayed_into_another_statement() {
    let (one, two) = (
        Statement::parse(ONE).unwrap(),
        Statement::parse(TWO).unwrap(),
    );
    // Put statement 1's genuine quote into statement 2 (whose session key is derived from its nonce)
    // and sign it properly. The quote was made for statement 1's binding, so it must not verify.
    let mut forged = two.clone();
    forged.platform_evidence = one.platform_evidence.clone();
    let key = SigningKey::from_bytes(blake3::hash(two.nonce.as_bytes()).as_bytes());
    let text = forged.sign(&key).unwrap();
    let v = attest::verify(&text, &[&TpmVerifier::new(pinned_for(ONE))]).unwrap();
    assert_eq!(v.achieved, Tier::T0);
    assert!(
        v.notes
            .iter()
            .any(|n| n.contains("not bound to this statement")),
        "{:?}",
        v.notes
    );
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
        (0..st.platform_evidence.len() / 2)
            .map(|i| u8::from_str_radix(&st.platform_evidence[i * 2..i * 2 + 2], 16).unwrap())
            .collect(),
    )
    .unwrap();
    // Change one PCR value.
    let pcr_line = ev.lines().find(|l| l.starts_with("pcrs ")).unwrap();
    let tampered = ev.replace(pcr_line, &pcr_line.replacen("0000", "0001", 1));
    assert_eq!(
        attest::verify(&resign(tampered), &[&v]).unwrap().achieved,
        Tier::T0
    );
    // Change one byte of the signed quote.
    let quote_line = ev.lines().find(|l| l.starts_with("quote ")).unwrap();
    let mut q = quote_line.to_string();
    let pos = q.len() - 10;
    q.replace_range(pos..=pos, if &q[pos..=pos] == "a" { "b" } else { "a" });
    assert_eq!(
        attest::verify(&resign(ev.replace(quote_line, &q)), &[&v])
            .unwrap()
            .achieved,
        Tier::T0
    );
}

// ---- the endorsement-key side ---------------------------------------------------------------------

fn anchors() -> Vec<Vec<u8>> {
    tpm_ek::pem_to_ders(ROOT_PEM).unwrap()
}

fn ek_parts(st: &Statement) -> (Vec<u8>, Vec<Vec<u8>>) {
    let ev = evidence_lines(st);
    (
        unhex(&ev["ek_cert"]),
        ev["ek_chain"].split(',').map(unhex).collect(),
    )
}

#[test]
fn the_real_ak_is_restricted_and_the_ek_certificate_chains_to_the_ca() {
    let st = Statement::parse(ONE).unwrap();
    assert_eq!(ak(&st).name.len(), 34);
    let (leaf, chain) = ek_parts(&st);
    // The EK certificate is padded with zeros in the TPM's NV index; the provider trims it.
    assert_eq!(tpm_ek::trim_der(&leaf).unwrap().len(), leaf.len());
    let cert = tpm_ek::check_ek_chain(&leaf, &chain, &anchors(), tpm_ek::now_unix()).unwrap();
    assert_eq!(cert.key.kind(), "rsa");
    assert!(cert.issuer.contains("swtpm-localca"), "{}", cert.issuer);
    assert!(
        cert.tpm_info.as_deref().unwrap_or("").contains("swtpm"),
        "manufacturer info comes from the SAN: {:?}",
        cert.tpm_info
    );
}

#[test]
fn ek_chain_checks_reject_what_they_should() {
    let st = Statement::parse(ONE).unwrap();
    let (leaf, chain) = ek_parts(&st);
    let now = tpm_ek::now_unix();
    // No trust anchor, or the wrong one: the leaf is signed by the issuer, which only the root vouches for.
    assert!(tpm_ek::check_ek_chain(&leaf, &chain, &[], now).is_err());
    let other = tpm_ek::pem_to_ders(ISSUER_PEM).unwrap(); // an anchor that did not sign the chain's top
    assert!(
        tpm_ek::check_ek_chain(&leaf, &[], &anchors(), now).is_err(),
        "the issuer is missing, so the root cannot be reached"
    );
    // Trusting the issuer directly is fine: anchors may be intermediates.
    assert!(tpm_ek::check_ek_chain(&leaf, &[], &other, now).is_ok());
    // Not valid yet (the certificate was issued in late 2026), and a tampered certificate.
    assert!(
        tpm_ek::check_ek_chain(&leaf, &chain, &anchors(), 1_000_000_000)
            .unwrap_err()
            .to_string()
            .contains("not valid")
    );
    let mut forged = leaf.clone();
    let i = forged.len() - 40;
    forged[i] ^= 1;
    assert!(tpm_ek::check_ek_chain(&forged, &chain, &anchors(), now).is_err());
    // An intermediate that is not a CA cannot extend the chain: use the leaf itself as the "intermediate".
    assert!(tpm_ek::check_ek_chain(&leaf, &[leaf.clone()], &anchors(), now).is_err());
}

#[test]
fn a_pinned_host_with_anchors_gets_a_challenge_not_a_tier() {
    let verifier = TpmVerifier::new(pinned_for(ONE)).with_ek_anchors(anchors());
    let v = attest::verify(ONE, &[&verifier]).unwrap();
    // Offline, the EK certificate is public information anyone could replay, so nothing is granted.
    assert_eq!(v.achieved, Tier::T0, "{:?}", v.notes);
    let ch = v
        .challenge
        .as_ref()
        .expect("a credential-activation challenge");
    assert_eq!(ch.tier_on_success, Tier::T1);
    let payload = String::from_utf8(ch.payload.clone()).unwrap();
    assert!(
        payload.starts_with("ek rsa\ncred ") && payload.contains("\nsecret "),
        "{payload}"
    );
    // Each verification makes a fresh credential, so a recorded answer cannot be replayed.
    let again = attest::verify(ONE, &[&verifier]).unwrap();
    assert_ne!(again.challenge.unwrap().expect, ch.expect);
    // A guess does not pass.
    let mut v = v;
    assert!(!v.complete_challenge(&[0u8; 32]));
    assert_eq!(v.achieved, Tier::T0);
}

#[test]
fn wrong_anchors_leave_a_pinned_host_at_t0_without_a_challenge() {
    let wrong = tpm_ek::pem_to_ders(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/swtpm-ca-root.pem"
        ))
        .unwrap()
        .replace('A', "B"),
    )
    .ok();
    // A garbage "anchor" simply cannot vouch for anything.
    let verifier = TpmVerifier::new(pinned_for(ONE))
        .with_ek_anchors(wrong.unwrap_or_else(|| vec![vec![0x30, 0x03, 1, 2, 3]]));
    let v = attest::verify(ONE, &[&verifier]).unwrap();
    assert_eq!(v.achieved, Tier::T0);
    assert!(v.challenge.is_none());
    assert!(
        v.notes
            .iter()
            .any(|n| n.contains("no genuine TPM is established")),
        "{:?}",
        v.notes
    );
}
