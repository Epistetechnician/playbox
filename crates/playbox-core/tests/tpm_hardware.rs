//! Credential activation against a real TPM implementation: a hardware TPM, or `swtpm` in a VM.
//!
//! Skipped unless `PLAYBOX_TEST_TPM=1`. Needs `tpm2-tools`; with a software TPM also
//! `TPM2TOOLS_TCTI=swtpm:host=127.0.0.1,port=2321`; and `PLAYBOX_TEST_EK_ANCHORS=<PEM file>` holding the
//! certificate that vouches for the TPM's EK (for swtpm: its local CA root), plus `PLAYBOX_TPM_EK_CHAIN=<PEM>`
//! if an intermediate sits between them. The TPM must have an EK certificate (`swtpm_setup --create-ek-cert`).
//!
//! This is the ground truth for `tpm_ek::make_credential`: its key derivations, CFB encryption, HMAC and file
//! layouts only count as right if a real TPM accepts what it makes.

use ed25519_dalek::SigningKey;
use playbox_core::attest::{self, PlatformProvider, Statement, Tier};
use playbox_core::platform::{fingerprint, PinStore, TpmProvider, TpmVerifier, TPM_KIND};
use playbox_core::tpm_ek::{self, EkKey};
use std::{fs, process::Command};

fn enabled() -> bool {
    std::env::var_os("PLAYBOX_TEST_TPM").is_some()
}

fn anchors() -> Vec<Vec<u8>> {
    tpm_ek::pem_to_ders(&fs::read_to_string(std::env::var("PLAYBOX_TEST_EK_ANCHORS").expect("PLAYBOX_TEST_EK_ANCHORS")).unwrap()).unwrap()
}

fn evidence(st: &Statement) -> std::collections::BTreeMap<String, String> {
    let b: Vec<u8> = (0..st.platform_evidence.len() / 2).map(|i| u8::from_str_radix(&st.platform_evidence[i * 2..i * 2 + 2], 16).unwrap()).collect();
    String::from_utf8(b).unwrap().lines().map(|l| l.split_once(' ').map(|(k, v)| (k.to_string(), v.to_string())).unwrap()).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap()).collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

struct Setup {
    provider: TpmProvider,
    text: String,
    ak: tpm_ek::AkPublic,
    ek: EkKey,
}

fn setup() -> Setup {
    let provider = TpmProvider::default();
    let key = SigningKey::from_bytes(&[5; 32]);
    let text = attest::new_platform_statement(&key, "abcd", "", 1, attest::measure_self("none", "hw-test"), &provider).expect("a statement from the TPM");
    let st = Statement::parse(&text).unwrap();
    let ev = evidence(&st);
    let ak = tpm_ek::parse_ak(&unhex(&ev["akpub"])).unwrap();
    let chain = ev.get("ek_chain").map(|c| c.split(',').map(unhex).collect::<Vec<_>>()).unwrap_or_default();
    let cert = tpm_ek::check_ek_chain(&unhex(ev.get("ek_cert").expect("the TPM has an EK certificate")), &chain, &anchors(), tpm_ek::now_unix()).expect("EK chain");
    Setup { provider, text, ak, ek: cert.key }
}

fn payload(kind: &str, c: &tpm_ek::Credential) -> Vec<u8> {
    format!("ek {kind}\ncred {}\nsecret {}\n", hex(&c.blob), hex(&c.secret)).into_bytes()
}

#[test]
fn verifier_challenge_is_answered_by_a_real_tpm() {
    if !enabled() {
        return;
    }
    let s = setup();
    let mut pins = PinStore::in_memory();
    pins.add(TPM_KIND, &fingerprint(&[s.ak.x, s.ak.y].concat())).unwrap();
    let verifier = TpmVerifier::new(pins).with_ek_anchors(anchors());
    let mut v = attest::verify(&s.text, &[&verifier]).unwrap();
    assert!(v.notes.iter().any(|n| n.contains("EK certificate chains to your trust anchor")), "{:?}", v.notes);
    assert_eq!(v.achieved, Tier::T0, "before the challenge is answered nothing is proven");
    let ch = v.challenge.clone().expect("a challenge");

    let answer = s.provider.respond(&ch.payload).expect("the TPM activates a credential made for its EK and AK");
    assert_eq!(answer, ch.expect);
    assert!(v.complete_challenge(&answer));
    assert_eq!(v.achieved, Tier::T1, "{:?}", v.notes);

    // A wrong answer from a host that cannot activate gains nothing.
    let mut v2 = attest::verify(&s.text, &[&verifier]).unwrap();
    assert!(!v2.complete_challenge(&[0u8; 32]));
    assert_eq!(v2.achieved, Tier::T0);
}

#[test]
fn rsa_credential_is_bound_to_the_attestation_key_and_is_integrity_protected() {
    if !enabled() {
        return;
    }
    let s = setup();
    let secret = [0x5au8; 32];
    let good = tpm_ek::make_credential(&s.ek, &s.ak.name, &secret).unwrap();
    assert_eq!(s.provider.respond(&payload("rsa", &good)).unwrap(), secret);

    // Bound to a different key's name: the TPM must refuse, which is what proves the AK is in this TPM.
    let mut other_name = s.ak.name.clone();
    *other_name.last_mut().unwrap() ^= 1;
    let wrong = tpm_ek::make_credential(&s.ek, &other_name, &secret).unwrap();
    assert!(s.provider.respond(&payload("rsa", &wrong)).is_err(), "credential for another key must not activate");

    // Flipping a bit in the blob (its integrity HMAC or ciphertext) must fail.
    let mut bad = good.clone();
    let n = bad.blob.len();
    bad.blob[n - 5] ^= 1;
    assert!(s.provider.respond(&payload("rsa", &bad)).is_err());

    // A credential made for some other EK cannot be decrypted by this TPM.
    let other = rsa::RsaPrivateKey::new(&mut rand_core::OsRng, 2048).unwrap();
    let elsewhere = tpm_ek::make_credential(&EkKey::Rsa(rsa::RsaPublicKey::from(&other)), &s.ak.name, &secret).unwrap();
    assert!(s.provider.respond(&payload("rsa", &elsewhere)).is_err());
}

#[test]
fn ecc_credential_activates_too() {
    if !enabled() {
        return;
    }
    let s = setup();
    // The ECC endorsement key of this TPM (it has no certificate here, but activation does not need one).
    let dir = std::env::temp_dir().join(format!("playbox-ecc-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let (ctx, pubf) = (dir.join("ek.ctx"), dir.join("ek.pub"));
    let _ = Command::new("tpm2_flushcontext").arg("-t").output();
    let out = Command::new("tpm2_createek").args(["-c"]).arg(&ctx).args(["-G", "ecc", "-u"]).arg(&pubf).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let _ = Command::new("tpm2_flushcontext").arg("-t").output();
    let raw = fs::read(&pubf).unwrap();

    // TPM2B_PUBLIC for an ECC key: size | type | nameAlg | attrs | authPolicy | symmetric | scheme | curve | kdf | x | y
    let u16at = |o: usize| u16::from_be_bytes([raw[o], raw[o + 1]]) as usize;
    let mut o = 2; // skip the size
    assert_eq!(u16at(o), 0x0023, "ECC EK");
    o += 2 + 2 + 4;
    o += 2 + u16at(o); // authPolicy
    let sym = u16at(o);
    o += 2;
    if sym != 0x0010 {
        o += 4; // key bits, mode
    }
    let scheme = u16at(o);
    o += 2;
    if scheme != 0x0010 {
        o += 2;
    }
    assert_eq!(u16at(o), 0x0003, "P-256");
    o += 2;
    let kdf = u16at(o);
    o += 2;
    if kdf != 0x0010 {
        o += 2;
    }
    let xl = u16at(o);
    let x = raw[o + 2..o + 2 + xl].to_vec();
    o += 2 + xl;
    let yl = u16at(o);
    let y = raw[o + 2..o + 2 + yl].to_vec();
    let point = [&[0x04u8][..], &x, &y].concat();
    let ek = EkKey::Ecc(p256::PublicKey::from_sec1_bytes(&point).unwrap());

    let secret = [0xa5u8; 32];
    let cred = tpm_ek::make_credential(&ek, &s.ak.name, &secret).unwrap();
    assert_eq!(s.provider.respond(&payload("ecc", &cred)).unwrap(), secret);
    let mut wrong = s.ak.name.clone();
    wrong[5] ^= 1;
    let bad = tpm_ek::make_credential(&ek, &wrong, &secret).unwrap();
    assert!(s.provider.respond(&payload("ecc", &bad)).is_err());
}

#[test]
fn malformed_challenges_are_refused_without_touching_the_tpm() {
    // This one needs no TPM: the provider validates before running any tool.
    let p = TpmProvider::default();
    for bad in ["", "ek rsa\n", "ek dsa\ncred 00\nsecret 00\n", "ek rsa\ncred zz\nsecret 00\n", "ek rsa\nek rsa\ncred 00\nsecret 00\n", "no-space-here"] {
        assert!(p.respond(bad.as_bytes()).is_err(), "{bad:?}");
    }
    assert!(p.respond(&vec![b'a'; 9000]).is_err());
}
