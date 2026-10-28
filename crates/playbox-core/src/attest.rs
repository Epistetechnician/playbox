//! Attestation: evidence about the machine and software behind a key.
//!
//! A [`Statement`] says "this key runs this software with this policy, and (optionally) here is
//! platform evidence". It is signed by the key it describes, so on its own it proves only that the
//! key holder *claims* it. What a verifier may believe is the [`Tier`] it can actually *achieve*:
//!
//! | Tier | Means | Needs |
//! |---|---|---|
//! | T0 | The holder of this key signed these measurements (self-attested) | nothing |
//! | T1 | A device-bound key / platform quote shows the same physical device | a platform verifier (TPM, Secure Enclave) |
//! | T2 | T1, and the quote covers the sandbox image, kernel and policy | a platform verifier |
//! | T3 | A confidential-VM report covers the whole host | a platform verifier (SEV-SNP, TDX) |
//!
//! No platform verifier ships yet, so every statement achieves T0 here. Evidence for a higher tier is
//! kept and reported, but it is never believed without a [`PlatformVerifier`] that checks it.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::{collections::BTreeMap, fmt, fs, io::Read, path::Path};

const DOMAIN: &str = "playbox.attest.v1\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    T0,
    T1,
    T2,
    T3,
}

impl Tier {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "t0" | "0" => Ok(Tier::T0),
            "t1" | "1" => Ok(Tier::T1),
            "t2" | "2" => Ok(Tier::T2),
            "t3" | "3" => Ok(Tier::T3),
            o => Err(format!("unknown tier {o:?}; use t0, t1, t2 or t3")),
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "T{}", *self as u8)
    }
}

/// What the host says it is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Measurements {
    /// blake3 of the running executable.
    pub binary: String,
    /// Which runner executes member tasks (`docker`, `libkrun`, `none`, ...).
    pub runner: String,
    /// blake3 of the room's policy (limits and options), so a change in policy changes the hash.
    pub policy: String,
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// The key that signs this statement and is being described.
    pub subject: [u8; 32],
    /// Verifier-chosen freshness value (hex), or empty for none.
    pub nonce: String,
    /// History head (hex) when issued, or empty. Ties the statement to a point in a log.
    pub log_head: String,
    pub issued_ms: u64,
    pub claimed: Tier,
    pub measurements: Measurements,
    /// `none`, or the kind of platform evidence: `tpm`, `secure-enclave`, `sev-snp`, `tdx`.
    pub platform: String,
    /// The raw platform evidence, hex, or empty.
    pub platform_evidence: String,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex<const N: usize>(s: &str) -> Result<[u8; N], String> {
    if s.len() != N * 2 || !s.is_ascii() {
        return Err(format!("expected {} hex chars", N * 2));
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Values must be single-line and free of leading/trailing spaces so the text form is unambiguous.
fn clean(v: &str) -> Result<&str, String> {
    if v.contains(['\n', '\r']) || v != v.trim() {
        return Err(format!("value {v:?} must be a single trimmed line"));
    }
    Ok(v)
}

impl Statement {
    fn body(&self) -> Result<String, String> {
        let m = &self.measurements;
        let mut s = String::new();
        let mut put = |k: &str, v: &str| -> Result<(), String> {
            s.push_str(&format!("{k} {}\n", if v.is_empty() { "-" } else { clean(v)? }));
            Ok(())
        };
        put("subject", &hex(&self.subject))?;
        put("nonce", &self.nonce)?;
        put("log_head", &self.log_head)?;
        put("issued_ms", &self.issued_ms.to_string())?;
        put("claimed", &self.claimed.to_string())?;
        put("binary", &m.binary)?;
        put("runner", &m.runner)?;
        put("policy", &m.policy)?;
        put("os", &m.os)?;
        put("arch", &m.arch)?;
        put("platform", &self.platform)?;
        put("platform_evidence", &self.platform_evidence)?;
        Ok(s)
    }

    /// What platform evidence must be bound to: a digest of the statement *except* the evidence itself
    /// (which would be circular) and the claimed tier (which is only a cap that verifiers re-derive
    /// from the evidence). Hardware signs or quotes this, so the evidence cannot be moved to another
    /// subject, nonce, history position, set of measurements or platform kind.
    pub fn binding(&self) -> [u8; 32] {
        let mut unbound = self.clone();
        unbound.platform_evidence = String::new();
        unbound.claimed = Tier::T0;
        let body = unbound.body().unwrap_or_default();
        let mut h = blake3::Hasher::new();
        h.update(b"playbox.binding.v1\n");
        h.update(body.as_bytes());
        *h.finalize().as_bytes()
    }

    /// The signed text form: body lines followed by `sig <hex>`.
    pub fn sign(&self, key: &SigningKey) -> Result<String, String> {
        if key.verifying_key().to_bytes() != self.subject {
            return Err("statement subject is not the signing key".into());
        }
        let body = self.body()?;
        let sig = key.sign(format!("{DOMAIN}{body}").as_bytes());
        Ok(format!("{body}sig {}\n", hex(&sig.to_bytes())))
    }

    /// Parse and check the signature. Does not judge freshness, the subject, or evidence.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut kv: BTreeMap<&str, &str> = BTreeMap::new();
        let mut body = String::new();
        let mut sig = None;
        for line in text.lines() {
            let (k, v) = line.split_once(' ').ok_or_else(|| format!("bad line {line:?}"))?;
            if k == "sig" {
                if sig.replace(v).is_some() {
                    return Err("duplicate sig".into());
                }
                continue;
            }
            if sig.is_some() {
                return Err("content after sig".into());
            }
            if kv.insert(k, v).is_some() {
                return Err(format!("duplicate key {k}"));
            }
            body.push_str(line);
            body.push('\n');
        }
        let sig: [u8; 64] = unhex(sig.ok_or("missing sig")?)?;
        let get = |k: &str| -> Result<String, String> {
            let v = *kv.get(k).ok_or_else(|| format!("missing {k}"))?;
            Ok(if v == "-" { String::new() } else { v.to_string() })
        };
        let known = ["subject", "nonce", "log_head", "issued_ms", "claimed", "binary", "runner", "policy", "os", "arch", "platform", "platform_evidence"];
        if let Some(extra) = kv.keys().find(|k| !known.contains(k)) {
            return Err(format!("unknown key {extra}"));
        }
        let st = Statement {
            subject: unhex(&get("subject")?)?,
            nonce: get("nonce")?,
            log_head: get("log_head")?,
            issued_ms: get("issued_ms")?.parse().map_err(|_| "bad issued_ms")?,
            claimed: Tier::parse(&get("claimed")?)?,
            measurements: Measurements {
                binary: get("binary")?,
                runner: get("runner")?,
                policy: get("policy")?,
                os: get("os")?,
                arch: get("arch")?,
            },
            platform: get("platform")?,
            platform_evidence: get("platform_evidence")?,
        };
        // Re-serialising must give exactly the signed bytes, so no field has two spellings.
        if st.body()? != body {
            return Err("statement is not in canonical form".into());
        }
        let key = VerifyingKey::from_bytes(&st.subject).map_err(|_| "bad subject key")?;
        key.verify(format!("{DOMAIN}{body}").as_bytes(), &Signature::from_bytes(&sig))
            .map_err(|_| "signature does not verify")?;
        Ok(st)
    }
}

/// What a platform verifier concluded about some evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformVerdict {
    /// The highest tier the evidence supports.
    pub tier: Tier,
    pub notes: Vec<String>,
    /// Fingerprint of the device key the evidence is about (see [`crate::platform::fingerprint`]).
    /// Reported even when the key is not trusted, so an operator can decide to pin it.
    pub device: Option<String>,
}

/// Checks platform evidence (a TPM quote, a confidential-VM report, ...) and says what tier it supports.
pub trait PlatformVerifier: Send + Sync {
    /// The `platform` value this verifier handles.
    fn kind(&self) -> &str;
    /// What the evidence supports, or why it does not verify. The evidence must be bound to
    /// `statement.binding()`; a verifier that does not check that proves nothing.
    fn verify(&self, statement: &Statement) -> Result<PlatformVerdict, String>;
}

#[derive(Debug, Clone)]
pub struct Verified {
    pub statement: Statement,
    /// The signed text exactly as received, for keeping.
    pub text: String,
    /// What the verifier may believe: at most `claimed`, and only as high as evidence was verified.
    pub achieved: Tier,
    /// Why the achieved tier is what it is.
    pub notes: Vec<String>,
    /// `<kind>:<fingerprint>` of the device key behind verified platform evidence, if any.
    pub device: Option<String>,
}

/// Verify a signed statement and work out the tier it achieves.
pub fn verify(text: &str, verifiers: &[&dyn PlatformVerifier]) -> Result<Verified, String> {
    let statement = Statement::parse(text)?;
    let mut notes = vec!["signature valid: the holder of the subject key made these claims".to_string()];
    let mut achieved = Tier::T0;
    let mut device = None;
    if statement.platform != "none" {
        match verifiers.iter().find(|v| v.kind() == statement.platform) {
            None => notes.push(format!("platform evidence of kind `{}` was not verified: no verifier available", statement.platform)),
            Some(v) => match v.verify(&statement) {
                Ok(verdict) => {
                    achieved = verdict.tier.min(statement.claimed);
                    notes.push(format!("`{}` evidence verified, supports {}", statement.platform, verdict.tier));
                    notes.extend(verdict.notes);
                    device = verdict.device.map(|d| format!("{}:{d}", statement.platform));
                }
                Err(e) => notes.push(format!("`{}` evidence rejected: {e}", statement.platform)),
            },
        }
    }
    if statement.claimed > achieved {
        notes.push(format!("claims {} but only {} is supported", statement.claimed, achieved));
    }
    Ok(Verified { statement, text: text.to_string(), achieved, notes, device })
}

/// What a platform provider produced for one statement.
#[derive(Debug, Clone)]
pub struct Produced {
    /// Provider-specific evidence text (see the provider's module).
    pub evidence: String,
    /// The tier the provider believes the evidence can support.
    pub claimed: Tier,
}

/// Produces platform evidence on the machine that holds the hardware.
pub trait PlatformProvider: Send + Sync {
    /// The `platform` value written into statements, e.g. `tpm` or `secure-enclave`.
    fn kind(&self) -> &str;
    /// Evidence bound to `binding` (see [`Statement::binding`]) about `m`.
    fn produce(&self, binding: &[u8; 32], m: &Measurements) -> Result<Produced, String>;
}

/// A signed statement carrying evidence from `provider`.
pub fn new_platform_statement(
    key: &SigningKey,
    nonce: &str,
    log_head: &str,
    issued_ms: u64,
    m: Measurements,
    provider: &dyn PlatformProvider,
) -> Result<String, String> {
    let mut st = new_statement(key, nonce, log_head, issued_ms, m);
    st.platform = provider.kind().to_string();
    let produced = provider.produce(&st.binding(), &st.measurements)?;
    st.claimed = produced.claimed;
    st.platform_evidence = hex(produced.evidence.as_bytes());
    st.sign(key)
}

/// Per-operation minimum tiers a room asks of the machines it trusts.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    min: BTreeMap<String, Tier>,
}

impl Policy {
    pub fn require(mut self, op: &str, tier: Tier) -> Self {
        self.min.insert(op.to_string(), tier);
        self
    }

    pub fn check(&self, op: &str, achieved: Tier) -> Result<(), String> {
        match self.min.get(op) {
            Some(need) if achieved < *need => Err(format!("`{op}` needs {need} but only {achieved} is verified")),
            _ => Ok(()),
        }
    }
}

/// blake3 of a file, streamed.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(h.finalize().as_bytes()))
}

pub fn hash_bytes(b: &[u8]) -> String {
    hex(blake3::hash(b).as_bytes())
}

/// Measurements of the running process. `policy` is hashed from the caller's description of its options.
pub fn measure_self(runner: &str, policy_description: &str) -> Measurements {
    let binary = std::env::current_exe()
        .and_then(|p| hash_file(&p))
        .unwrap_or_else(|_| "unknown".to_string());
    Measurements {
        binary,
        runner: runner.to_string(),
        policy: hash_bytes(policy_description.as_bytes()),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
    }
}

pub fn new_statement(key: &SigningKey, nonce: &str, log_head: &str, issued_ms: u64, m: Measurements) -> Statement {
    Statement {
        subject: key.verifying_key().to_bytes(),
        nonce: nonce.to_string(),
        log_head: log_head.to_string(),
        issued_ms,
        claimed: Tier::T0,
        measurements: m,
        platform: "none".into(),
        platform_evidence: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }

    fn stmt(k: &SigningKey) -> Statement {
        new_statement(k, "ab12", "", 5, Measurements { binary: "b".into(), runner: "docker".into(), policy: "p".into(), os: "macos".into(), arch: "aarch64".into() })
    }

    struct Mock(Tier, bool);
    impl PlatformVerifier for Mock {
        fn kind(&self) -> &str {
            "tpm"
        }
        fn verify(&self, _: &Statement) -> Result<PlatformVerdict, String> {
            if self.1 { Ok(PlatformVerdict { tier: self.0, notes: vec![], device: Some("dev".into()) }) } else { Err("bad quote".into()) }
        }
    }

    #[test]
    fn round_trip_and_tier_t0() {
        let k = key(1);
        let text = stmt(&k).sign(&k).unwrap();
        let v = verify(&text, &[]).unwrap();
        assert_eq!(v.achieved, Tier::T0);
        assert_eq!(v.statement, stmt(&k));
    }

    #[test]
    fn tampering_and_wrong_keys_are_rejected() {
        let k = key(1);
        let text = stmt(&k).sign(&k).unwrap();
        assert!(Statement::parse(&text.replace("docker", "libkrun")).is_err());
        assert!(Statement::parse(&text.replace("nonce ab12", "nonce ab13")).is_err());
        // Signed by someone other than the subject.
        let mut s = stmt(&k);
        assert!(s.sign(&key(2)).is_err());
        s.subject = key(2).verifying_key().to_bytes();
        let forged = s.sign(&key(2)).unwrap().replace(&hex(&key(2).verifying_key().to_bytes()), &hex(&k.verifying_key().to_bytes()));
        assert!(Statement::parse(&forged).is_err());
    }

    #[test]
    fn malformed_text_is_rejected() {
        let k = key(1);
        let text = stmt(&k).sign(&k).unwrap();
        assert!(Statement::parse(&format!("{text}extra x\n")).is_err());
        assert!(Statement::parse(&text.replacen("os macos\n", "os macos\nos linux\n", 1)).is_err());
        assert!(Statement::parse("garbage").is_err());
        let mut s = stmt(&k);
        s.measurements.runner = "a\nb".into();
        assert!(s.sign(&k).is_err());
    }

    #[test]
    fn unverified_platform_evidence_is_not_believed() {
        let k = key(1);
        let mut s = stmt(&k);
        s.claimed = Tier::T2;
        s.platform = "tpm".into();
        s.platform_evidence = "deadbeef".into();
        let text = s.sign(&k).unwrap();
        let v = verify(&text, &[]).unwrap();
        assert_eq!(v.achieved, Tier::T0);
        assert!(v.notes.iter().any(|n| n.contains("no verifier")));
        // A verifier that rejects the quote keeps it at T0.
        assert_eq!(verify(&text, &[&Mock(Tier::T2, false)]).unwrap().achieved, Tier::T0);
        // A verifier that accepts it lifts the tier, but never above what was claimed.
        assert_eq!(verify(&text, &[&Mock(Tier::T2, true)]).unwrap().achieved, Tier::T2);
        assert_eq!(verify(&text, &[&Mock(Tier::T3, true)]).unwrap().achieved, Tier::T2);
        assert_eq!(verify(&text, &[&Mock(Tier::T1, true)]).unwrap().achieved, Tier::T1);
    }

    #[test]
    fn binding_covers_everything_but_the_evidence_and_the_claim() {
        let k = key(1);
        let base = stmt(&k);
        let mut other = base.clone();
        other.claimed = Tier::T3;
        other.platform_evidence = "ff".into();
        assert_eq!(base.binding(), other.binding());
        for change in [
            |s: &mut Statement| s.nonce = "cd".into(),
            |s: &mut Statement| s.log_head = "aa".into(),
            |s: &mut Statement| s.issued_ms += 1,
            |s: &mut Statement| s.platform = "tpm".into(),
            |s: &mut Statement| s.measurements.binary = "other".into(),
            |s: &mut Statement| s.subject = [9; 32],
        ] {
            let mut t = base.clone();
            change(&mut t);
            assert_ne!(base.binding(), t.binding());
        }
    }

    #[test]
    fn policy_gates_operations_by_tier() {
        let p = Policy::default().require("merge", Tier::T2);
        assert!(p.check("merge", Tier::T1).is_err());
        assert!(p.check("merge", Tier::T2).is_ok());
        assert!(p.check("echo", Tier::T0).is_ok());
        assert!(Tier::T3 > Tier::T0 && Tier::parse("T2").unwrap() == Tier::T2 && Tier::parse("x").is_err());
    }
}
