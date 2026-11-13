//! Biscuit capability tokens for rooms.
//!
//! Two kinds, both signed by the host's root key:
//! - **invite**: bearer token in the join link. Valid only for the `join` operation.
//! - **grant**: issued at join, bound to the member's endpoint key. Lists the operations the member
//!   may perform and hard limits on tasks. A holder can narrow it offline with [`attenuate`].
//!
//! Every request is authorized by running the grant against facts the host supplies: the current
//! time, the authenticated peer, the operation, and the resources the request needs. Nothing the
//! token holder sends is trusted as a fact.

use anyhow::{anyhow, Context, Result};
use biscuit_auth::{
    builder::{Algorithm, AuthorizerBuilder, BlockBuilder, Term},
    Biscuit, KeyPair, PublicKey, UnverifiedBiscuit,
};
use std::{
    collections::{HashMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};

#[must_use]
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().try_into().unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn params(pairs: Vec<(&str, Term)>) -> HashMap<String, Term> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn s(v: &str) -> Term {
    Term::Str(v.to_string())
}

fn s_(v: &str) -> Term {
    s(v)
}

fn i(v: i64) -> Term {
    Term::Integer(v)
}

fn nonce() -> Result<String> {
    Ok(crate::random_bytes()?[..12]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Limits baked into a member grant.
#[derive(Debug, Clone)]
pub struct Caps {
    pub cpu_milli: i64,
    pub mem_mib: i64,
    pub timeout_secs: i64,
    pub net: bool,
    /// Destinations a task may be limited to (exact `host` or `host:port` strings).
    pub hosts: Vec<String>,
}

/// Steering limits baked into a member grant (see `docs/steering.md`). A grant without these has no
/// steering rights at all.
#[derive(Debug, Clone)]
pub struct SteerCaps {
    /// Weights manifest hashes (hex) the member may steer. Must not be empty.
    pub models: Vec<String>,
    pub layer_min: i64,
    pub layer_max: i64,
    /// Largest strength, in thousandths of the mean residual norm (absolute value).
    pub max_coef_milli: i64,
    /// Subset of `add`, `clamp`, `ablate`. Must not be empty.
    pub modes: Vec<String>,
    /// Direction hashes (hex) the member may use; `None` means any direction.
    pub directions: Option<Vec<String>>,
    /// Largest number of tokens one run may generate (prompts times new tokens, steered and clean).
    pub max_tokens_per_run: i64,
    /// Guard classes the member may steer along despite a guard hit. Empty: none.
    pub classes: Vec<String>,
    /// Whether the member may also run read-only probes.
    pub probe: bool,
    /// Total runs and generated tokens the host will serve this member. The host counts these; they are
    /// bounded by what the host issued, and a member's own narrowing of them is not enforced.
    pub max_runs: u64,
    pub max_tokens_total: u64,
}

impl SteerCaps {
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn validate(&self) -> Result<()> {
        let hex64 =
            |s: &String| s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if self.models.is_empty() || !self.models.iter().all(hex64) {
            return Err(anyhow!(
                "steer models must be a non-empty list of 64-char lowercase hex hashes"
            ));
        }
        if let Some(d) = &self.directions {
            if !d.iter().all(hex64) {
                return Err(anyhow!(
                    "steer directions must be 64-char lowercase hex hashes"
                ));
            }
        }
        if self.modes.is_empty()
            || !self
                .modes
                .iter()
                .all(|m| matches!(m.as_str(), "add" | "clamp" | "ablate"))
        {
            return Err(anyhow!(
                "steer modes must be a non-empty subset of add, clamp, ablate"
            ));
        }
        if self.layer_min < 0
            || self.layer_max < self.layer_min
            || self.max_coef_milli < 0
            || self.max_tokens_per_run < 0
        {
            return Err(anyhow!("steer bounds are inconsistent"));
        }
        if !self
            .classes
            .iter()
            .all(|c| !c.is_empty() && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
        {
            return Err(anyhow!("guard classes must be lowercase words"));
        }
        Ok(())
    }
}

/// What one steering request needs, derived by the host from the verified spec and the prompt set.
#[derive(Debug, Clone)]
pub struct SteerNeeds {
    /// Weights manifest hash, hex.
    pub model: String,
    pub layer_lo: i64,
    pub layer_hi: i64,
    pub coef_abs_milli: i64,
    pub mode: String,
    /// Direction hash, hex.
    pub direction: String,
    pub tokens: i64,
}

/// What one request needs. Zero for operations that run nothing.
#[derive(Debug, Clone, Default)]
pub struct Needs {
    /// Present only for `steer` and `steer_probe`.
    pub steer: Option<SteerNeeds>,
    /// Present only for the `steer_class` question: may this member steer along a guard of this class?
    pub class: Option<String>,
    pub cpu_milli: i64,
    pub mem_mib: i64,
    pub timeout_secs: i64,
    pub net: bool,
    /// Destinations the task asks to be limited to. Every one must be listed in the grant.
    pub hosts: Vec<String>,
    pub cmd: String,
}

pub struct Authority {
    /// The signing half. `None` on a replica, which can verify tokens but never issue them.
    root: Option<KeyPair>,
    public: PublicKey,
    /// Signs revocation notices. The same Ed25519 key as `root` (a test checks that they match).
    notice_key: Option<ed25519_dalek::SigningKey>,
    room: String,
}

impl Authority {
    /// # Panics
    ///
    /// Panics if the operating system cannot supply randomness.
    #[must_use]
    pub fn new(room: &str) -> Self {
        let seed = crate::random_bytes().expect("OS randomness");
        Self::from_seed(room, &seed).expect("a random seed is a valid key")
    }

    /// An authority whose root key comes from a stored 32-byte seed, so tokens survive restarts.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn from_seed(room: &str, seed: &[u8; 32]) -> Result<Self> {
        let root = KeyPair::from_bytes(seed, Algorithm::Ed25519.into())
            .map_err(|e| anyhow!("bad root key: {e}"))?;
        let public = root.public();
        Ok(Self {
            root: Some(root),
            public,
            notice_key: Some(ed25519_dalek::SigningKey::from_bytes(seed)),
            room: room.to_string(),
        })
    }

    /// An authority that only knows the room's public key: it checks tokens and revocation notices
    /// but cannot mint either.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn verifier(room: &str, root_public: &[u8; 32]) -> Result<Self> {
        let public = PublicKey::from_bytes(root_public, Algorithm::Ed25519)
            .map_err(|e| anyhow!("bad root public key: {e}"))?;
        Ok(Self {
            root: None,
            public,
            notice_key: None,
            room: room.to_string(),
        })
    }

    #[must_use]
    pub fn can_issue(&self) -> bool {
        self.root.is_some()
    }

    fn root(&self) -> Result<&KeyPair> {
        self.root.as_ref().ok_or_else(|| {
            anyhow!("this host only verifies tokens: it cannot issue invites or grants")
        })
    }

    #[must_use]
    pub fn public(&self) -> PublicKey {
        self.public
    }

    /// The room's root public key as raw bytes.
    #[must_use]
    pub fn public_bytes(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        out.copy_from_slice(&self.public.to_bytes());
        out
    }

    /// Sign `data` with the root key; `None` on a replica.
    #[must_use]
    pub fn sign_notice(&self, data: &[u8]) -> Option<[u8; 64]> {
        use ed25519_dalek::Signer;
        self.notice_key.as_ref().map(|k| k.sign(data).to_bytes())
    }

    /// Whether `sig` over `data` was made by the room's root key.
    #[must_use]
    pub fn verify_notice(&self, data: &[u8], sig: &[u8; 64]) -> bool {
        use ed25519_dalek::Verifier;
        ed25519_dalek::VerifyingKey::from_bytes(&self.public_bytes())
            .and_then(|k| k.verify(data, &ed25519_dalek::Signature::from_bytes(sig)))
            .is_ok()
    }

    /// A bearer invite good for `join` until `now + ttl_secs`. Returns the token and its id.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub fn mint_invite(&self, ttl_secs: i64) -> Result<(String, String)> {
        let token = Biscuit::builder()
            .code_with_params(
                r#"
                room({room});
                nonce({nonce});
                check if operation("join");
                check if now($t), $t < {exp};
                "#,
                params(vec![
                    ("room", s(&self.room)),
                    ("nonce", s(&nonce()?)),
                    ("exp", i(now_secs() + ttl_secs)),
                ]),
                HashMap::new(),
            )?
            .build(self.root()?)?;
        Ok((token.to_base64()?, hex(&token.revocation_identifiers()[0])))
    }

    /// Check an invite; returns its id (the authority block's revocation id).
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn verify_invite(&self, token: &str) -> Result<String> {
        let b = Biscuit::from_base64(token, self.public())
            .context("invite is not valid for this room")?;
        self.run(&b, "join", "", &Needs::default())?;
        Ok(hex(&b.revocation_identifiers()[0]))
    }

    /// A member grant bound to `subject` (their endpoint id). Returns the token and its id.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub fn mint_grant(
        &self,
        subject: &str,
        ttl_secs: i64,
        caps: &Caps,
    ) -> Result<(String, String)> {
        self.mint_grant_with(subject, ttl_secs, caps, None)
    }

    /// Like [`Authority::mint_grant`], optionally with steering rights.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub fn mint_grant_with(
        &self,
        subject: &str,
        ttl_secs: i64,
        caps: &Caps,
        steer: Option<&SteerCaps>,
    ) -> Result<(String, String)> {
        let mut code = String::new();
        let mut ps = params(vec![
            ("room", s(&self.room)),
            ("nonce", s(&nonce()?)),
            ("subject", s(subject)),
            ("exp", i(now_secs() + ttl_secs)),
            ("cpu", i(caps.cpu_milli)),
            ("mem", i(caps.mem_mib)),
            ("timeout", i(caps.timeout_secs)),
            ("net", i(i64::from(caps.net))),
        ]);
        // `check all` fails with nothing to iterate over, so the empty string is always both requested and
        // allowed. It is not a valid host, so it never names a real destination.
        let mut allowed: std::collections::BTreeSet<Term> =
            caps.hosts.iter().map(|h| s(h)).collect();
        allowed.insert(s(""));
        ps.insert("allowed".into(), Term::Set(allowed));
        if let Some(sc) = steer {
            sc.validate()?;
            let set = |v: &[String]| Term::Set(v.iter().map(|x| s(x)).collect());
            let sops = r#"operation($o), !["steer", "steer_probe"].contains($o)"#;
            code.push_str("right(\"steer\");\nright(\"handoff\");\n");
            if sc.probe {
                code.push_str("right(\"steer_probe\");\n");
            }
            code.push_str(&format!(
                "check if {sops} or req_model($m), {{smodels}}.contains($m);\n"
            ));
            code.push_str(&format!("check if {sops} or req_layer_lo($l), req_layer_hi($h), $l >= {{slmin}}, $h <= {{slmax}};\n"));
            code.push_str(&format!(
                "check if {sops} or req_coef($c), $c <= {{scoef}};\n"
            ));
            code.push_str(&format!(
                "check if {sops} or req_tokens($t), $t <= {{stok}};\n"
            ));
            code.push_str(r#"check if operation($o), !["steer"].contains($o) or req_mode($m), {smodes}.contains($m);"#);
            code.push('\n');
            ps.insert("smodels".into(), set(&sc.models));
            ps.insert("slmin".into(), i(sc.layer_min));
            ps.insert("slmax".into(), i(sc.layer_max));
            ps.insert("scoef".into(), i(sc.max_coef_milli));
            ps.insert("stok".into(), i(sc.max_tokens_per_run));
            ps.insert("smodes".into(), set(&sc.modes));
            if let Some(d) = &sc.directions {
                if d.is_empty() {
                    return Err(anyhow!("an empty direction allow-list would allow nothing; leave it unset or list directions"));
                }
                code.push_str(&format!(
                    "check if {sops} or req_direction($d), {{sdirs}}.contains($d);\n"
                ));
                ps.insert("sdirs".into(), set(d));
            }
            if !sc.classes.is_empty() {
                code.push_str("right(\"steer_class\");\n");
                code.push_str(r#"check if operation($o), !["steer_class"].contains($o) or req_class($c), {sclasses}.contains($c);"#);
                code.push('\n');
                ps.insert("sclasses".into(), set(&sc.classes));
            }
        }
        let token = Biscuit::builder()
            .code_with_params(
                &(code
                    + r#"
                room({room});
                nonce({nonce});
                right("echo"); right("pull"); right("push"); right("run"); right("tasks"); right("browse");
                check if subject({subject});
                check if now($t), $t < {exp};
                check if operation($op), right($op);
                check if req_cpu($c), $c <= {cpu};
                check if req_mem($m), $m <= {mem};
                check if req_timeout($t), $t <= {timeout};
                check if req_net($n), $n <= {net};
                check all req_host($h), {allowed}.contains($h);
                "#),
                ps,
                HashMap::new(),
            )?
            .build(self.root()?)?;
        Ok((token.to_base64()?, hex(&token.revocation_identifiers()[0])))
    }

    /// Authorize `op` for `subject`. `revoked` holds revoked authority-block ids.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn authorize(
        &self,
        grant: &str,
        subject: &str,
        op: &str,
        needs: &Needs,
        revoked: &HashSet<String>,
    ) -> Result<()> {
        let b = Biscuit::from_base64(grant, self.public())
            .context("grant is not valid for this room")?;
        // Any block's id being revoked kills the token; the authority id revokes every descendant.
        if b.revocation_identifiers()
            .iter()
            .any(|id| revoked.contains(&hex(id)))
        {
            return Err(anyhow!("grant revoked"));
        }
        self.run(&b, op, subject, needs)
    }

    fn run(&self, b: &Biscuit, op: &str, subject: &str, n: &Needs) -> Result<()> {
        let mut code = String::new();
        let mut ps = params(vec![
            ("now", i(now_secs())),
            ("subject", s(subject)),
            ("op", s(op)),
            ("cpu", i(n.cpu_milli)),
            ("mem", i(n.mem_mib)),
            ("timeout", i(n.timeout_secs)),
            ("net", i(i64::from(n.net))),
            ("cmd", s(&n.cmd)),
        ]);
        if let Some(st) = &n.steer {
            code.push_str("req_model({s_model}); req_layer_lo({s_lo}); req_layer_hi({s_hi}); req_coef({s_coef}); req_mode({s_mode}); req_direction({s_dir}); req_tokens({s_tok});\n");
            ps.insert("s_model".into(), s_(&st.model));
            ps.insert("s_lo".into(), i(st.layer_lo));
            ps.insert("s_hi".into(), i(st.layer_hi));
            ps.insert("s_coef".into(), i(st.coef_abs_milli));
            ps.insert("s_mode".into(), s_(&st.mode));
            ps.insert("s_dir".into(), s_(&st.direction));
            ps.insert("s_tok".into(), i(st.tokens));
        }
        if let Some(c) = &n.class {
            code.push_str("req_class({s_class});\n");
            ps.insert("s_class".into(), s_(c));
        }
        code.push_str("req_host(\"\");\n");
        for (k, h) in n.hosts.iter().enumerate() {
            code.push_str(&format!("req_host({{r{k}}});\n"));
            ps.insert(format!("r{k}"), s(h));
        }
        let mut a = AuthorizerBuilder::new()
            .code_with_params(
                &(code + r"
                now({now}); subject({subject}); operation({op});
                req_cpu({cpu}); req_mem({mem}); req_timeout({timeout}); req_net({net}); req_cmd({cmd});
                allow if true;
                "),
                ps,
                HashMap::new(),
            )?
            .build(b)?;
        // The library's default budget is one millisecond of evaluation. Under load (six steering shards plus their
        // containers on one machine) that produced a spurious "Reached Datalog execution limits" refusal of a valid
        // request. A refusal fails closed, so it was never unsafe, but it was wrong. Time is relaxed; the fact and
        // iteration caps that bound what a hostile token can make us do stay at their defaults.
        let limits = biscuit_auth::datalog::RunLimits {
            max_time: std::time::Duration::from_millis(250),
            ..Default::default()
        };
        a.authorize_with_limits(limits)
            .map(|_| ())
            .map_err(|e| anyhow!("not authorized: {e}"))
    }
}

/// Biscuit code that narrows only the steering limits of a grant, for [`attenuate`]. Each clause applies to
/// steering operations alone, so the narrowed grant still serves every other operation. Layers and the
/// strength cap can only shrink; `modes` can only be a subset of what the grant already allows (a mode it
/// does not allow stays refused, since both checks must hold).
///
/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
pub fn steer_narrowing(
    layer_max: Option<i64>,
    max_coef_milli: Option<i64>,
    modes: Option<&[&str]>,
    max_tokens: Option<i64>,
) -> Result<String> {
    for m in modes.unwrap_or_default() {
        if !matches!(*m, "add" | "clamp" | "ablate") {
            return Err(anyhow!("unknown steer mode {m:?}"));
        }
    }
    let sops = r#"operation($o), !["steer", "steer_probe"].contains($o)"#;
    let mut out = String::new();
    if let Some(l) = layer_max {
        out.push_str(&format!(
            "check if {sops} or req_layer_hi($h), $h <= {l};\n"
        ));
    }
    if let Some(c) = max_coef_milli {
        out.push_str(&format!("check if {sops} or req_coef($c), $c <= {c};\n"));
    }
    if let Some(t) = max_tokens {
        out.push_str(&format!("check if {sops} or req_tokens($t), $t <= {t};\n"));
    }
    if let Some(ms) = modes {
        let list = ms
            .iter()
            .map(|m| format!("\"{m}\""))
            .collect::<Vec<_>>()
            .join(", ");
        // An empty list allows no mode: `[""]` contains nothing a request can ask for.
        let list = if list.is_empty() {
            "\"\"".to_string()
        } else {
            list
        };
        out.push_str(&format!(
            "check if operation($o), !{} or req_mode($m), [{list}].contains($m);\n",
            r#"["steer"].contains($o)"#
        ));
    }
    Ok(out)
}

/// Narrow a token without the root key. Blocks added this way can only add checks that further
/// restrict what the token allows.
///
/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
pub fn attenuate(token: &str, code: &str) -> Result<String> {
    let block = BlockBuilder::new().code(code)?;
    let t = UnverifiedBiscuit::from_base64(token)?.append(block)?;
    Ok(t.to_base64()?)
}
