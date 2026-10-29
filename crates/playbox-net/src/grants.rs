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

pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn params(pairs: Vec<(&str, Term)>) -> HashMap<String, Term> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn s(v: &str) -> Term {
    Term::Str(v.to_string())
}

fn i(v: i64) -> Term {
    Term::Integer(v)
}

fn nonce() -> Result<String> {
    Ok(crate::random_bytes()?[..12].iter().map(|b| format!("{b:02x}")).collect())
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

/// What one request needs. Zero for operations that run nothing.
#[derive(Debug, Clone, Default)]
pub struct Needs {
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
    pub fn new(room: &str) -> Self {
        let seed = crate::random_bytes().expect("OS randomness");
        Self::from_seed(room, &seed).expect("a random seed is a valid key")
    }

    /// An authority whose root key comes from a stored 32-byte seed, so tokens survive restarts.
    pub fn from_seed(room: &str, seed: &[u8; 32]) -> Result<Self> {
        let root = KeyPair::from_bytes(seed, Algorithm::Ed25519.into()).map_err(|e| anyhow!("bad root key: {e}"))?;
        let public = root.public();
        Ok(Self { root: Some(root), public, notice_key: Some(ed25519_dalek::SigningKey::from_bytes(seed)), room: room.to_string() })
    }

    /// An authority that only knows the room's public key: it checks tokens and revocation notices
    /// but cannot mint either.
    pub fn verifier(room: &str, root_public: &[u8; 32]) -> Result<Self> {
        let public = PublicKey::from_bytes(root_public, Algorithm::Ed25519.into()).map_err(|e| anyhow!("bad root public key: {e}"))?;
        Ok(Self { root: None, public, notice_key: None, room: room.to_string() })
    }

    pub fn can_issue(&self) -> bool {
        self.root.is_some()
    }

    fn root(&self) -> Result<&KeyPair> {
        self.root.as_ref().ok_or_else(|| anyhow!("this host only verifies tokens: it cannot issue invites or grants"))
    }

    pub fn public(&self) -> PublicKey {
        self.public.clone()
    }

    /// The room's root public key as raw bytes.
    pub fn public_bytes(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        out.copy_from_slice(&self.public.to_bytes());
        out
    }

    /// Sign `data` with the root key; `None` on a replica.
    pub fn sign_notice(&self, data: &[u8]) -> Option<[u8; 64]> {
        use ed25519_dalek::Signer;
        self.notice_key.as_ref().map(|k| k.sign(data).to_bytes())
    }

    /// Whether `sig` over `data` was made by the room's root key.
    pub fn verify_notice(&self, data: &[u8], sig: &[u8; 64]) -> bool {
        use ed25519_dalek::Verifier;
        ed25519_dalek::VerifyingKey::from_bytes(&self.public_bytes())
            .and_then(|k| k.verify(data, &ed25519_dalek::Signature::from_bytes(sig)))
            .is_ok()
    }

    /// A bearer invite good for `join` until `now + ttl_secs`. Returns the token and its id.
    pub fn mint_invite(&self, ttl_secs: i64) -> Result<(String, String)> {
        let token = Biscuit::builder()
            .code_with_params(
                r#"
                room({room});
                nonce({nonce});
                check if operation("join");
                check if now($t), $t < {exp};
                "#,
                params(vec![("room", s(&self.room)), ("nonce", s(&nonce()?)), ("exp", i(now_secs() + ttl_secs))]),
                HashMap::new(),
            )?
            .build(self.root()?)?;
        Ok((token.to_base64()?, hex(&token.revocation_identifiers()[0])))
    }

    /// Check an invite; returns its id (the authority block's revocation id).
    pub fn verify_invite(&self, token: &str) -> Result<String> {
        let b = Biscuit::from_base64(token, self.public()).context("invite is not valid for this room")?;
        self.run(&b, "join", "", &Needs::default())?;
        Ok(hex(&b.revocation_identifiers()[0]))
    }

    /// A member grant bound to `subject` (their endpoint id). Returns the token and its id.
    pub fn mint_grant(&self, subject: &str, ttl_secs: i64, caps: &Caps) -> Result<(String, String)> {
        let code = String::new();
        let mut ps = params(vec![
            ("room", s(&self.room)),
            ("nonce", s(&nonce()?)),
            ("subject", s(subject)),
            ("exp", i(now_secs() + ttl_secs)),
            ("cpu", i(caps.cpu_milli)),
            ("mem", i(caps.mem_mib)),
            ("timeout", i(caps.timeout_secs)),
            ("net", i(caps.net as i64)),
        ]);
        // `check all` fails with nothing to iterate over, so the empty string is always both requested and
        // allowed. It is not a valid host, so it never names a real destination.
        let mut allowed: std::collections::BTreeSet<Term> = caps.hosts.iter().map(|h| s(h)).collect();
        allowed.insert(s(""));
        ps.insert("allowed".into(), Term::Set(allowed));
        let token = Biscuit::builder()
            .code_with_params(
                &(code + r#"
                room({room});
                nonce({nonce});
                right("echo"); right("pull"); right("push"); right("run"); right("tasks");
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
    pub fn authorize(&self, grant: &str, subject: &str, op: &str, needs: &Needs, revoked: &HashSet<String>) -> Result<()> {
        let b = Biscuit::from_base64(grant, self.public()).context("grant is not valid for this room")?;
        // Any block's id being revoked kills the token; the authority id revokes every descendant.
        if b.revocation_identifiers().iter().any(|id| revoked.contains(&hex(id))) {
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
            ("net", i(n.net as i64)),
            ("cmd", s(&n.cmd)),
        ]);
        code.push_str("req_host(\"\");\n");
        for (k, h) in n.hosts.iter().enumerate() {
            code.push_str(&format!("req_host({{r{k}}});\n"));
            ps.insert(format!("r{k}"), s(h));
        }
        let mut a = AuthorizerBuilder::new()
            .code_with_params(
                &(code + r#"
                now({now}); subject({subject}); operation({op});
                req_cpu({cpu}); req_mem({mem}); req_timeout({timeout}); req_net({net}); req_cmd({cmd});
                allow if true;
                "#),
                ps,
                HashMap::new(),
            )?
            .build(b)?;
        a.authorize().map(|_| ()).map_err(|e| anyhow!("not authorized: {e}"))
    }
}

/// Narrow a token without the root key. Blocks added this way can only add checks that further
/// restrict what the token allows.
pub fn attenuate(token: &str, code: &str) -> Result<String> {
    let block = BlockBuilder::new().code(code)?;
    let t = UnverifiedBiscuit::from_base64(token)?.append(block)?;
    Ok(t.to_base64()?)
}
