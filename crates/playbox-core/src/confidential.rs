//! T3 evidence from Google Confidential Space (Intel TDX): a Google-signed attestation token.
//!
//! A Confidential Space VM runs one measured container. Its launcher mints an OIDC token, signed by
//! Google Cloud Attestation, whose claims say what hardware it ran on (`hwmodel`), whether the OS is
//! the hardened image and debugging is off (`swname`, `dbgstat`, `secboot`), and which container image
//! digest was launched (`submods.container.image_digest`). If the token echoes a nonce we chose
//! (`eat_nonce`), it is fresh and bound to the statement we are verifying.
//!
//! [`ConfidentialSpaceVerifier`] checks that token offline against signing keys the operator pinned
//! (a saved copy of Google's JWKS). It is the `tdx` [`PlatformVerifier`].
//!
//! # Tiers
//! - Signature, issuer, audience, freshness, nonce, hardware and hardening claims all pass **and** the
//!   image digest is on the operator's allow-list: [`Tier::T3`]. The host runs exactly an image you
//!   named, on measured confidential hardware, and its operator cannot read or alter it.
//! - Everything passes but no image is pinned: [`Tier::T1`]. It is a genuine confidential VM, but you
//!   have not said which workload you expect, so the token proves little about behaviour.
//! - Anything else is rejected.
//!
//! # Limits
//! This was written from Google's documented claim names and is tested with tokens signed by a local
//! key, **not** against a token from a live Confidential Space VM. Check it against a real token before
//! relying on it. Trusting the pinned Google keys, Intel's TDX root, and the pinned image's own contents
//! is still required. The image digest identifies the container, not that its code is safe or that it is
//! the *Playbox binary* the statement names; pin the digest of an image built from source you audited.
//! Use a Playbox-specific `audience` so a leaked token cannot be federated to cloud resources.

use crate::attest::{Measurements, PlatformProvider, PlatformVerdict, PlatformVerifier, Produced, Statement, Tier};
use crate::record::{hex, unhex_vec};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use rsa::{
    pkcs1v15::{Signature, VerifyingKey},
    signature::Verifier,
    BigUint, RsaPublicKey,
};
use serde_json::Value;
use sha2::Sha256;
use std::{
    collections::BTreeSet,
    time::{SystemTime, UNIX_EPOCH},
};

/// The `platform` value in statements carrying this evidence.
pub const TDX_KIND: &str = "tdx";
pub const GOOGLE_ISSUER: &str = "https://confidentialcomputing.googleapis.com";

pub struct ConfidentialSpaceVerifier {
    keys: Vec<(String, RsaPublicKey)>,
    issuer: String,
    audience: String,
    images: BTreeSet<String>,
    projects: BTreeSet<String>,
    models: BTreeSet<String>,
    require_stable: bool,
    clock: Box<dyn Fn() -> u64 + Send + Sync>,
}

fn system_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl ConfidentialSpaceVerifier {
    /// `audience` must equal the audience the host asked its token for.
    pub fn new(audience: &str) -> Self {
        ConfidentialSpaceVerifier {
            keys: Vec::new(),
            issuer: GOOGLE_ISSUER.into(),
            audience: audience.into(),
            images: BTreeSet::new(),
            projects: BTreeSet::new(),
            models: ["INTEL_TDX".to_string()].into(),
            require_stable: true,
            clock: Box::new(system_secs),
        }
    }

    /// Pin signing keys from a JWKS document (`{"keys":[{"kid","kty":"RSA","n","e"}]}`).
    pub fn with_jwks(mut self, jwks: &str) -> Result<Self, String> {
        let v: Value = serde_json::from_str(jwks).map_err(|e| format!("JWKS: {e}"))?;
        for k in v["keys"].as_array().ok_or("JWKS has no keys array")? {
            if k["kty"] != "RSA" {
                continue;
            }
            let kid = k["kid"].as_str().ok_or("JWKS key without kid")?.to_string();
            let n = B64.decode(k["n"].as_str().ok_or("JWKS key without n")?).map_err(|e| e.to_string())?;
            let e = B64.decode(k["e"].as_str().ok_or("JWKS key without e")?).map_err(|e| e.to_string())?;
            let key = RsaPublicKey::new(BigUint::from_bytes_be(&n), BigUint::from_bytes_be(&e)).map_err(|e| e.to_string())?;
            self.keys.push((kid, key));
        }
        if self.keys.is_empty() {
            return Err("JWKS has no RSA keys".into());
        }
        Ok(self)
    }

    pub fn with_key(mut self, kid: &str, key: RsaPublicKey) -> Self {
        self.keys.push((kid.to_string(), key));
        self
    }

    /// Allow a container image digest (`sha256:…`). With none pinned, evidence tops out at T1.
    pub fn with_image_digest(mut self, digest: &str) -> Self {
        self.images.insert(digest.to_string());
        self
    }

    /// Only accept VMs from these GCP projects (if any are given).
    pub fn with_project(mut self, project: &str) -> Self {
        self.projects.insert(project.to_string());
        self
    }

    pub fn with_issuer(mut self, issuer: &str) -> Self {
        self.issuer = issuer.into();
        self
    }

    /// Seconds since the epoch; tests set it so tokens do not expire under them.
    pub fn with_clock(mut self, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    fn check(&self, token: &str, binding_hex: &str) -> Result<PlatformVerdict, String> {
        let parts: Vec<&str> = token.split('.').collect();
        let [h, p, s] = parts[..] else { return Err("token is not a three-part JWT".into()) };
        let header: Value = serde_json::from_slice(&B64.decode(h).map_err(|e| format!("header: {e}"))?).map_err(|e| format!("header: {e}"))?;
        // Only RS256, never `none` or a symmetric algorithm an attacker could pick.
        if header["alg"] != "RS256" {
            return Err(format!("unsupported alg {}", header["alg"]));
        }
        let kid = header["kid"].as_str().ok_or("token has no kid")?;
        let key = &self.keys.iter().find(|(k, _)| k == kid).ok_or_else(|| format!("no pinned key {kid:?}"))?.1;
        let sig = Signature::try_from(B64.decode(s).map_err(|e| format!("signature: {e}"))?.as_slice()).map_err(|e| e.to_string())?;
        VerifyingKey::<Sha256>::new(key.clone()).verify(format!("{h}.{p}").as_bytes(), &sig).map_err(|_| "token signature does not verify")?;

        let c: Value = serde_json::from_slice(&B64.decode(p).map_err(|e| format!("claims: {e}"))?).map_err(|e| format!("claims: {e}"))?;
        let now = (self.clock)();
        if c["iss"] != self.issuer.as_str() {
            return Err(format!("issuer is {}, expected {}", c["iss"], self.issuer));
        }
        let aud_ok = match &c["aud"] {
            Value::String(a) => *a == self.audience,
            Value::Array(a) => a.iter().any(|x| x == self.audience.as_str()),
            _ => false,
        };
        if !aud_ok {
            return Err("audience is not this verifier's".into());
        }
        if c["exp"].as_u64().ok_or("token has no exp")? <= now {
            return Err("token expired".into());
        }
        if c["nbf"].as_u64().is_some_and(|n| n > now + 60) {
            return Err("token not yet valid".into());
        }
        let nonces: Vec<&str> = match &c["eat_nonce"] {
            Value::String(n) => vec![n.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        if !nonces.contains(&binding_hex) {
            return Err("token does not carry this statement's binding as its nonce".into());
        }
        let model = c["hwmodel"].as_str().unwrap_or("");
        if !self.models.contains(model) {
            return Err(format!("hardware model {model:?} is not accepted"));
        }
        if c["swname"] != "CONFIDENTIAL_SPACE" {
            return Err("not a Confidential Space image".into());
        }
        if c["dbgstat"] != "disabled-since-boot" {
            return Err(format!("debug is not disabled since boot: {}", c["dbgstat"]));
        }
        if c["secboot"] != true {
            return Err("secure boot is off".into());
        }
        if self.require_stable && !c["submods"]["confidential_space"]["support_attributes"].as_array().is_some_and(|a| a.iter().any(|x| x == "STABLE")) {
            return Err("image is not a STABLE Confidential Space release".into());
        }
        let project = c["submods"]["gce"]["project_id"].as_str().unwrap_or("");
        if !self.projects.is_empty() && !self.projects.contains(project) {
            return Err(format!("project {project:?} is not accepted"));
        }
        let image = c["submods"]["container"]["image_digest"].as_str().unwrap_or("");
        let mut notes = vec![format!("{model} VM in project {project:?}, debug disabled, secure boot on")];
        let tier = if self.images.is_empty() {
            notes.push(format!("container image {image:?} is not pinned, so this is a genuine confidential VM running an unknown workload"));
            Tier::T1
        } else if self.images.contains(image) {
            notes.push(format!("container image {image} is on the allow-list"));
            Tier::T3
        } else {
            return Err(format!("container image {image:?} is not on the allow-list"));
        };
        let instance = c["submods"]["gce"]["instance_id"].as_str().unwrap_or("");
        Ok(PlatformVerdict { tier, notes, challenge: None, device: (!instance.is_empty()).then(|| crate::platform::fingerprint(instance.as_bytes())) })
    }
}

impl PlatformVerifier for ConfidentialSpaceVerifier {
    fn kind(&self) -> &str {
        TDX_KIND
    }

    fn verify(&self, statement: &Statement) -> Result<PlatformVerdict, String> {
        let raw = String::from_utf8(unhex_vec(&statement.platform_evidence)?).map_err(|_| "evidence is not utf-8")?;
        let token = raw.strip_prefix("token ").ok_or("evidence has no token line")?.trim();
        self.check(token, &hex(&statement.binding()))
    }
}

/// Asks the Confidential Space launcher for a token over the statement binding. Only works inside a
/// Confidential Space VM, and has not been run on one.
#[cfg(unix)]
pub struct ConfidentialSpaceProvider {
    pub socket: std::path::PathBuf,
    pub audience: String,
}

#[cfg(unix)]
impl ConfidentialSpaceProvider {
    pub const DEFAULT_SOCKET: &'static str = "/run/container_launcher/teeserver.sock";

    pub fn new(audience: &str) -> Self {
        ConfidentialSpaceProvider { socket: Self::DEFAULT_SOCKET.into(), audience: audience.into() }
    }

    fn request(&self, nonce_hex: &str) -> Result<String, String> {
        use std::io::{Read, Write};
        let body = format!(r#"{{"audience":"{}","token_type":"OIDC","nonces":["{nonce_hex}"]}}"#, self.audience.replace(['"', '\\'], ""));
        let mut s = std::os::unix::net::UnixStream::connect(&self.socket).map_err(|e| format!("teeserver socket {}: {e}", self.socket.display()))?;
        s.set_read_timeout(Some(std::time::Duration::from_secs(10))).ok();
        write!(s, "POST /v1/token HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
            .map_err(|e| e.to_string())?;
        let mut resp = String::new();
        s.read_to_string(&mut resp).map_err(|e| e.to_string())?;
        let (head, token) = resp.split_once("\r\n\r\n").ok_or("malformed teeserver response")?;
        if !head.starts_with("HTTP/1.1 200") {
            return Err(format!("teeserver refused: {}", head.lines().next().unwrap_or("")));
        }
        Ok(token.trim().to_string())
    }
}

#[cfg(unix)]
impl PlatformProvider for ConfidentialSpaceProvider {
    fn kind(&self) -> &str {
        TDX_KIND
    }

    fn produce(&self, binding: &[u8; 32], _: &Measurements) -> Result<Produced, String> {
        let token = self.request(&hex(binding))?;
        Ok(Produced { evidence: format!("token {token}\n"), claimed: Tier::T3 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attest::{new_statement, verify};
    use ed25519_dalek::SigningKey;
    use rsa::{pkcs1v15::SigningKey as RsaSigner, rand_core::OsRng, signature::{SignatureEncoding, Signer}, traits::PublicKeyParts, RsaPrivateKey};
    use serde_json::json;
    use std::sync::OnceLock;

    const NOW: u64 = 1_800_000_000;
    const IMAGE: &str = "sha256:aaaa";

    fn google() -> &'static RsaPrivateKey {
        static K: OnceLock<RsaPrivateKey> = OnceLock::new();
        K.get_or_init(|| RsaPrivateKey::new(&mut OsRng, 2048).unwrap())
    }

    fn meas() -> Measurements {
        Measurements { binary: "bb".into(), runner: "libkrun".into(), policy: "pp".into(), os: "linux".into(), arch: "x86_64".into() }
    }

    fn claims(binding: &str) -> Value {
        json!({
            "iss": GOOGLE_ISSUER, "aud": "playbox", "exp": NOW + 600, "nbf": NOW - 5, "eat_nonce": [binding],
            "hwmodel": "INTEL_TDX", "swname": "CONFIDENTIAL_SPACE", "dbgstat": "disabled-since-boot", "secboot": true,
            "submods": {
                "confidential_space": {"support_attributes": ["LATEST", "STABLE", "USABLE"]},
                "container": {"image_digest": IMAGE},
                "gce": {"project_id": "proj", "instance_id": "123"}
            }
        })
    }

    fn jwt(kid: &str, alg: &str, claims: &Value, key: &RsaPrivateKey) -> String {
        let h = B64.encode(json!({"alg": alg, "kid": kid}).to_string());
        let p = B64.encode(claims.to_string());
        let sig = RsaSigner::<Sha256>::new(key.clone()).sign(format!("{h}.{p}").as_bytes());
        format!("{h}.{p}.{}", B64.encode(sig.to_bytes()))
    }

    fn verifier() -> ConfidentialSpaceVerifier {
        ConfidentialSpaceVerifier::new("playbox").with_key("k1", google().to_public_key()).with_clock(|| NOW)
    }

    /// A statement carrying `token_for(binding)` as evidence, signed by the session key.
    fn statement(token_for: impl Fn(&str) -> String) -> String {
        let key = SigningKey::from_bytes(&[5; 32]);
        let mut st = new_statement(&key, "ab", "", 1, meas());
        st.platform = TDX_KIND.into();
        st.claimed = Tier::T3;
        st.platform_evidence = hex(format!("token {}\n", token_for(&hex(&st.binding()))).as_bytes());
        st.sign(&key).unwrap()
    }

    fn achieved(v: &ConfidentialSpaceVerifier, text: &str) -> (Tier, String) {
        let r = verify(text, &[v]).unwrap();
        (r.achieved, r.notes.join(" | "))
    }

    #[test]
    fn pinned_image_on_genuine_hardware_reaches_t3() {
        let v = verifier().with_image_digest(IMAGE).with_project("proj");
        let text = statement(|b| jwt("k1", "RS256", &claims(b), google()));
        let (tier, notes) = achieved(&v, &text);
        assert_eq!(tier, Tier::T3, "{notes}");
    }

    #[test]
    fn unpinned_image_only_reaches_t1() {
        let text = statement(|b| jwt("k1", "RS256", &claims(b), google()));
        assert_eq!(achieved(&verifier(), &text).0, Tier::T1);
    }

    #[test]
    fn each_failed_check_leaves_the_statement_at_t0() {
        let v = verifier().with_image_digest(IMAGE);
        type Mutation = Box<dyn Fn(&mut Value)>;
        let cases: Vec<(&str, Mutation)> = vec![
            ("wrong image", Box::new(|c| c["submods"]["container"]["image_digest"] = json!("sha256:bbbb"))),
            ("debug on", Box::new(|c| c["dbgstat"] = json!("enabled"))),
            ("wrong hardware", Box::new(|c| c["hwmodel"] = json!("GCP_AMD_SEV"))),
            ("secure boot off", Box::new(|c| c["secboot"] = json!(false))),
            ("not confidential space", Box::new(|c| c["swname"] = json!("GKE"))),
            ("wrong audience", Box::new(|c| c["aud"] = json!("someone-else"))),
            ("expired", Box::new(|c| c["exp"] = json!(NOW - 1))),
            ("wrong issuer", Box::new(|c| c["iss"] = json!("https://evil.example"))),
            ("not stable", Box::new(|c| c["submods"]["confidential_space"]["support_attributes"] = json!(["LATEST"]))),
            ("stale nonce", Box::new(|c| c["eat_nonce"] = json!(["00"]))),
        ];
        for (name, mutate) in cases {
            let text = statement(|b| {
                let mut c = claims(b);
                mutate(&mut c);
                jwt("k1", "RS256", &c, google())
            });
            let (tier, notes) = achieved(&v, &text);
            assert_eq!(tier, Tier::T0, "{name} should be rejected: {notes}");
            assert!(notes.contains("rejected"), "{name}: {notes}");
        }
    }

    #[test]
    fn token_for_another_statement_is_rejected() {
        let v = verifier().with_image_digest(IMAGE);
        let other = jwt("k1", "RS256", &claims("deadbeef"), google());
        assert_eq!(achieved(&v, &statement(|_| other.clone())).0, Tier::T0);
    }

    #[test]
    fn forged_signature_unknown_key_and_weak_algorithms_are_rejected() {
        let v = verifier().with_image_digest(IMAGE);
        let attacker = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
        assert_eq!(achieved(&v, &statement(|b| jwt("k1", "RS256", &claims(b), &attacker))).0, Tier::T0, "signed by another key");
        assert_eq!(achieved(&v, &statement(|b| jwt("k2", "RS256", &claims(b), google()))).0, Tier::T0, "unknown kid");
        assert_eq!(achieved(&v, &statement(|b| jwt("k1", "none", &claims(b), google()))).0, Tier::T0, "alg none");
        assert_eq!(achieved(&v, &statement(|b| jwt("k1", "HS256", &claims(b), google()))).0, Tier::T0, "alg HS256");
        let text = statement(|b| {
            let t = jwt("k1", "RS256", &claims(b), google());
            let (h, rest) = t.split_once('.').unwrap();
            let (_, s) = rest.split_once('.').unwrap();
            let mut c = claims(b);
            c["submods"]["container"]["image_digest"] = json!("sha256:evil");
            format!("{h}.{}.{s}", B64.encode(c.to_string()))
        });
        assert_eq!(achieved(&v, &text).0, Tier::T0, "claims edited after signing");
    }

    #[test]
    fn jwks_loading_pins_keys() {
        let pk = google().to_public_key();
        let jwks = json!({"keys": [{"kid": "k1", "kty": "RSA", "alg": "RS256", "n": B64.encode(pk.n().to_bytes_be()), "e": B64.encode(pk.e().to_bytes_be())}]}).to_string();
        let v = ConfidentialSpaceVerifier::new("playbox").with_jwks(&jwks).unwrap().with_clock(|| NOW).with_image_digest(IMAGE);
        assert_eq!(achieved(&v, &statement(|b| jwt("k1", "RS256", &claims(b), google()))).0, Tier::T3);
        assert!(ConfidentialSpaceVerifier::new("playbox").with_jwks("{\"keys\":[]}").is_err());
    }
}
