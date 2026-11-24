//! Native Rust CPU-ARC skill cache, Maverick Goldilocks RAA Verification-Only prover/verifier,
//! 64-byte int8 Swarm Telepathy descriptor encoder, and dual-signed (`Ed25519` + `ML-DSA-65`)
//! `RoomLog` receipt hook for the BOX-7 egress proxy (`egress.rs`).

use ed25519_dalek::SigningKey;
use playbox_core::roomlog::{Config, RoomLog, SignedCheckpoint};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Goldilocks prime modulus `q = 2^64 - 2^32 + 1` (strictly `> 2^31`, avoiding BabyBear overflow).
pub const GOLDILOCKS_Q: u64 = 0xFFFF_FFFF_0000_0001;

/// Exact element of the Goldilocks prime field `F_q`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Goldilocks(pub u64);

impl Goldilocks {
    #[must_use]
    pub const fn new(v: u64) -> Self {
        Self(v % GOLDILOCKS_Q)
    }

    #[must_use]
    pub fn from_i64(v: i64) -> Self {
        let q = i128::from(GOLDILOCKS_Q);
        let r = i128::from(v).rem_euclid(q);
        Self(u64::try_from(r).unwrap_or(0))
    }

    #[must_use]
    pub fn add_mod(self, rhs: Self) -> Self {
        let s = u128::from(self.0) + u128::from(rhs.0);
        let q = u128::from(GOLDILOCKS_Q);
        let r = if s >= q { s - q } else { s };
        Self(u64::try_from(r).unwrap_or(0))
    }

    #[must_use]
    pub fn sub_mod(self, rhs: Self) -> Self {
        if self.0 >= rhs.0 {
            Self(self.0 - rhs.0)
        } else {
            Self(GOLDILOCKS_Q - (rhs.0 - self.0))
        }
    }

    #[must_use]
    pub fn mul_mod(self, rhs: Self) -> Self {
        let p = (u128::from(self.0) * u128::from(rhs.0)) % u128::from(GOLDILOCKS_Q);
        Self(u64::try_from(p).unwrap_or(0))
    }
}

impl std::ops::Add for Goldilocks {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        self.add_mod(rhs)
    }
}

impl std::ops::Sub for Goldilocks {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        self.sub_mod(rhs)
    }
}

impl std::ops::Mul for Goldilocks {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        self.mul_mod(rhs)
    }
}

/// Deterministic `SplitMix64` PRNG for RAA code sampling and fresh sparse challenges.
#[derive(Clone, Debug)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn next_nonzero_field(&mut self) -> Goldilocks {
        loop {
            let v = self.next_u64() % GOLDILOCKS_Q;
            if v != 0 {
                return Goldilocks(v);
            }
        }
    }

    pub fn shuffle(&mut self, slice: &mut [usize]) {
        for i in (1..slice.len()).rev() {
            let j = usize::try_from(self.next_u64() % ((i as u64) + 1)).unwrap_or(0);
            slice.swap(i, j);
        }
    }
}

/// Exact Appendix-B Repeat-Accumulate-Accumulate (RAA) linear code over Goldilocks `F_q`.
///
/// Satisfies the exact adjoint identity `<G^T w, v> == <w, G v>` for all `w in F^n, v in F^N`.
#[derive(Clone, Debug)]
pub struct RaaCode {
    pub n: usize,
    pub r: usize,
    pub big_n: usize,
    perm1: Vec<usize>,
    perm2: Vec<usize>,
    v1: Vec<Goldilocks>,
    v2: Vec<Goldilocks>,
}

impl RaaCode {
    /// Sample a fresh RAA code with input dimension `n` and repetition factor `r`.
    #[must_use]
    pub fn sample(n: usize, r: usize, rng: &mut SplitMix64) -> Self {
        let big_n = n.saturating_mul(r);
        let mut perm1: Vec<usize> = (0..big_n).collect();
        let mut perm2: Vec<usize> = (0..big_n).collect();
        rng.shuffle(&mut perm1);
        rng.shuffle(&mut perm2);
        let v1 = (0..big_n).map(|_| rng.next_nonzero_field()).collect();
        let v2 = (0..big_n).map(|_| rng.next_nonzero_field()).collect();
        Self {
            n,
            r,
            big_n,
            perm1,
            perm2,
            v1,
            v2,
        }
    }

    /// Encode `z = G^T w` (`F^n -> F^N`).
    #[must_use]
    pub fn encode(&self, w: &[Goldilocks]) -> Vec<Goldilocks> {
        let big_n = self.big_n;
        let mut buf = vec![Goldilocks::default(); big_n];
        let mut tmp = vec![Goldilocks::default(); big_n];
        for (i, &wi) in w.iter().take(self.n).enumerate() {
            let base = i * self.r;
            for j in 0..self.r {
                buf[base + j] = wi;
            }
        }
        for (&p, &b) in self.perm1.iter().zip(&buf) {
            tmp[p] = b;
        }
        for (t, &v1i) in tmp.iter_mut().zip(&self.v1) {
            *t = *t * v1i;
        }
        for i in 1..big_n {
            tmp[i] = tmp[i] + tmp[i - 1];
        }
        for (&p, &t) in self.perm2.iter().zip(&tmp) {
            buf[p] = t;
        }
        for (b, &v2i) in buf.iter_mut().zip(&self.v2) {
            *b = *b * v2i;
        }
        for i in 1..big_n {
            buf[i] = buf[i] + buf[i - 1];
        }
        buf
    }

    /// Syndrome `x = G v` (`F^N -> F^n`).
    #[must_use]
    pub fn syndrome(&self, v: &[Goldilocks]) -> Vec<Goldilocks> {
        let big_n = self.big_n;
        let mut buf = v.to_vec();
        let mut tmp = vec![Goldilocks::default(); big_n];
        for i in (0..big_n.saturating_sub(1)).rev() {
            buf[i] = buf[i] + buf[i + 1];
        }
        for (b, &v2i) in buf.iter_mut().zip(&self.v2) {
            *b = *b * v2i;
        }
        for (t, &p) in tmp.iter_mut().zip(&self.perm2) {
            *t = buf[p];
        }
        for i in (0..big_n.saturating_sub(1)).rev() {
            tmp[i] = tmp[i] + tmp[i + 1];
        }
        for (t, &v1i) in tmp.iter_mut().zip(&self.v1) {
            *t = *t * v1i;
        }
        for (b, &p) in buf.iter_mut().zip(&self.perm1) {
            *b = tmp[p];
        }
        let mut out = vec![Goldilocks::default(); self.n];
        for (i, slot) in out.iter_mut().enumerate() {
            let base = i * self.r;
            let mut acc = Goldilocks::default();
            for j in 0..self.r {
                acc = acc + buf[base + j];
            }
            *slot = acc;
        }
        out
    }
}

/// Request payload for `POST /v1/maverick/verify` (1-RTT Verification-Only Mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaverickVerifyRequest {
    pub d_out: usize,
    pub d_in: usize,
    pub w_row_major: Vec<i64>,
    pub x: Vec<i64>,
    pub claimed_y: Vec<i64>,
    #[serde(default = "default_seed")]
    pub challenge_seed: u64,
    #[serde(default = "default_reps")]
    pub repetitions: usize,
}

const fn default_seed() -> u64 {
    0xA5A5_1337_CAFE_BABE
}

const fn default_reps() -> usize {
    4
}

/// Result of a Maverick Verification-Only check over Goldilocks `F_q`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaverickVerifyOutcome {
    pub verified: bool,
    pub field_modulus: u64,
    pub overflow_safe: bool,
    pub repetitions: usize,
    pub proof_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Verify `claimed_y == W * x` using 1-RTT Goldilocks Freivalds + RAA adjoint linear-code checks.
///
/// Explicitly rejects modular-wrap risk (`d_in * max|W| * max|x| >= q / 2`) and any 1-LSB cheat.
#[must_use]
pub fn verify_matvec_goldilocks(req: &MaverickVerifyRequest) -> MaverickVerifyOutcome {
    let digest_input = serde_json::to_vec(req).unwrap_or_default();
    let proof_digest = blake3::hash(&digest_input).to_hex().to_string();

    if req.d_out == 0
        || req.d_in == 0
        || req.w_row_major.len() != req.d_out.saturating_mul(req.d_in)
        || req.x.len() != req.d_in
        || req.claimed_y.len() != req.d_out
    {
        return MaverickVerifyOutcome {
            verified: false,
            field_modulus: GOLDILOCKS_Q,
            overflow_safe: false,
            repetitions: 0,
            proof_digest,
            reason: Some("dimension_mismatch".into()),
        };
    }

    let max_w = req
        .w_row_major
        .iter()
        .map(|&v| v.unsigned_abs())
        .max()
        .unwrap_or(0);
    let max_x = req.x.iter().map(|&v| v.unsigned_abs()).max().unwrap_or(0);
    let bound = u128::from(u64::try_from(req.d_in).unwrap_or(u64::MAX))
        .saturating_mul(u128::from(max_w))
        .saturating_mul(u128::from(max_x));
    let overflow_safe = bound < u128::from(GOLDILOCKS_Q / 2);
    if !overflow_safe {
        return MaverickVerifyOutcome {
            verified: false,
            field_modulus: GOLDILOCKS_Q,
            overflow_safe: false,
            repetitions: 0,
            proof_digest,
            reason: Some("field_overflow_risk".into()),
        };
    }

    let reps = req.repetitions.clamp(1, 16);
    let mut rng = SplitMix64::new(req.challenge_seed);
    let raa = RaaCode::sample(req.d_out, 4, &mut rng);
    let x_f: Vec<Goldilocks> = req.x.iter().map(|&v| Goldilocks::from_i64(v)).collect();
    let y_f: Vec<Goldilocks> = req
        .claimed_y
        .iter()
        .map(|&v| Goldilocks::from_i64(v))
        .collect();
    let y_code = raa.encode(&y_f);

    for _ in 0..reps {
        // Fresh sparse challenge vector v in F^N (weight = min(N, max(8, N/4))).
        let mut v = vec![Goldilocks::default(); raa.big_n];
        let weight = raa.big_n.min((raa.big_n / 4).max(8));
        for _ in 0..weight {
            let idx = usize::try_from(rng.next_u64() % (raa.big_n as u64)).unwrap_or(0);
            v[idx] = rng.next_nonzero_field();
        }
        // Left side: <G^T y, v>
        let mut lhs = Goldilocks::default();
        for (&yc, &vi) in y_code.iter().zip(&v) {
            if vi.0 != 0 {
                lhs = lhs + (yc * vi);
            }
        }
        // Adjoint syndrome: c = G v in F^{d_out}, right side: <(c^T W), x>
        let c = raa.syndrome(&v);
        let mut rhs = Goldilocks::default();
        for (j, &xj) in x_f.iter().enumerate() {
            let mut cw_j = Goldilocks::default();
            for (i, &ci) in c.iter().enumerate() {
                let wij = Goldilocks::from_i64(req.w_row_major[i * req.d_in + j]);
                cw_j = cw_j + (ci * wij);
            }
            rhs = rhs + (cw_j * xj);
        }
        if lhs != rhs {
            return MaverickVerifyOutcome {
                verified: false,
                field_modulus: GOLDILOCKS_Q,
                overflow_safe: true,
                repetitions: reps,
                proof_digest,
                reason: Some("maverick_verification_failed".into()),
            };
        }
    }

    MaverickVerifyOutcome {
        verified: true,
        field_modulus: GOLDILOCKS_Q,
        overflow_safe: true,
        repetitions: reps,
        proof_digest,
        reason: None,
    }
}

/// Deterministic 64-byte int8 Swarm Telepathy descriptor for a compiled CPU-ARC skill.
#[must_use]
pub fn encode_skill_descriptor_64b(skill_key: &str, response_body: &str) -> [u8; 64] {
    let mut out = [0u8; 64];
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"playbox.arc.skill.descriptor.v1");
    hasher.update(skill_key.as_bytes());
    hasher.update(&[0u8]);
    hasher.update(response_body.as_bytes());
    let mut reader = hasher.finalize_xof();
    reader.fill(&mut out);
    out
}

/// Cosine similarity between two 64-byte signed int8 Swarm Telepathy descriptors.
#[must_use]
pub fn descriptor_cosine_i8(a: &[u8; 64], b: &[u8; 64]) -> f64 {
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for (&xa, &xb) in a.iter().zip(b.iter()) {
        let fa = f64::from(i8::from_be_bytes([xa]));
        let fb = f64::from(i8::from_be_bytes([xb]));
        dot += fa * fb;
        na += fa * fa;
        nb += fb * fb;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

/// Novelty filter for Swarm Telepathy: accepts a 64-byte descriptor iff max cosine to prior descriptors `< threshold`.
#[must_use]
pub fn is_novel_descriptor(existing: &[[u8; 64]], candidate: &[u8; 64], max_cosine: f64) -> bool {
    existing
        .iter()
        .all(|prev| descriptor_cosine_i8(prev, candidate) < max_cosine)
}

/// Compiled CPU-ARC deterministic skill entry.
#[derive(Debug, Clone)]
pub struct CpuArcSkill {
    pub trigger: String,
    pub response_json: String,
    pub skill_hash: String,
    pub descriptor_64b: [u8; 64],
}

impl CpuArcSkill {
    #[must_use]
    pub fn compile(trigger: impl Into<String>, response_json: impl Into<String>) -> Self {
        let trigger = trigger.into();
        let response_json = response_json.into();
        let skill_hash = blake3::hash(format!("{trigger}\0{response_json}").as_bytes())
            .to_hex()
            .to_string();
        let descriptor_64b = encode_skill_descriptor_64b(&trigger, &response_json);
        Self {
            trigger,
            response_json,
            skill_hash,
            descriptor_64b,
        }
    }
}

/// Intercept outcome returned by [`ArcMaverickHook`] to the BOX-7 egress proxy.
#[derive(Debug, Clone)]
pub struct InterceptResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub denied: Option<String>,
}

struct HookState {
    skills: BTreeMap<String, CpuArcSkill>,
    log: RoomLog,
    checkpoints: Vec<SignedCheckpoint>,
    arc_hits: u64,
    upstream_misses: u64,
    maverick_verified: u64,
    maverick_cheats_caught: u64,
}

/// Host-side sovereign hook for BOX-7 `egress::Proxy`:
/// CPU-ARC local hit bypass + Maverick Goldilocks Verification-Only + dual-signed `Ed25519+ML-DSA-65` `RoomLog`.
pub struct ArcMaverickHook {
    leader_key: SigningKey,
    state: Mutex<HookState>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn hex_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

impl ArcMaverickHook {
    /// Create a hook backed by a fresh native `RoomLog` (single-host quorum `k=1` by default, or pass a prebuilt `RoomLog`).
    ///
    /// # Panics
    ///
    /// Never panics for a valid signing key (genesis config has `k=1` with `leader` in `hosts`).
    #[must_use]
    pub fn new(root_key: &SigningKey, leader_key: SigningKey) -> Self {
        let leader_pk = leader_key.verifying_key().to_bytes();
        let mut hosts = BTreeSet::new();
        hosts.insert(leader_pk);
        let cfg = Config {
            epoch: 0,
            leader: leader_pk,
            hosts,
            k: 1,
        };
        let log = RoomLog::genesis(root_key, cfg, now_ms()).expect("valid genesis config");
        Self {
            leader_key,
            state: Mutex::new(HookState {
                skills: BTreeMap::new(),
                log,
                checkpoints: Vec::new(),
                arc_hits: 0,
                upstream_misses: 0,
                maverick_verified: 0,
                maverick_cheats_caught: 0,
            }),
        }
    }

    /// Create a hook attached to an existing multi-host `RoomLog` (e.g., 2-node room with `k=2`).
    #[must_use]
    pub fn with_room_log(leader_key: SigningKey, log: RoomLog) -> Self {
        Self {
            leader_key,
            state: Mutex::new(HookState {
                skills: BTreeMap::new(),
                log,
                checkpoints: Vec::new(),
                arc_hits: 0,
                upstream_misses: 0,
                maverick_verified: 0,
                maverick_cheats_caught: 0,
            }),
        }
    }

    /// Register a compiled CPU-ARC skill and record a dual-signed `arc.receipt` entry.
    pub fn register_skill(&self, skill: CpuArcSkill) -> CpuArcSkill {
        if let Ok(mut st) = self.state.lock() {
            let hash = skill.skill_hash.clone();
            let trigger = skill.trigger.clone();
            st.skills.insert(trigger.clone(), skill.clone());
            self.append_and_checkpoint_locked(
                &mut st,
                "arc_skill_register",
                &hash,
                json!({"trigger": trigger}),
            );
        }
        skill
    }

    fn append_and_checkpoint_locked(
        &self,
        st: &mut HookState,
        event: &str,
        digest: &str,
        meta: serde_json::Value,
    ) {
        let payload = json!({
            "event": event,
            "digest": digest,
            "meta": meta,
        });
        if st
            .log
            .append(&self.leader_key, "arc.receipt", &payload, now_ms())
            .is_ok()
        {
            let cp = st.log.checkpoint();
            if let Ok(sc) = st.log.sign_checkpoint(&self.leader_key, cp) {
                st.checkpoints.push(sc);
            }
        }
    }

    /// Inspect an incoming BOX-7 request before upstream dispatch.
    ///
    /// Returns `Some(InterceptResponse)` on CPU-ARC local hit or `/v1/maverick/verify`,
    /// or `None` to allow normal BOX-7 upstream forwarding (while recording the miss receipt).
    pub fn intercept_request(
        &self,
        method: &str,
        clean_path: &str,
        body: &[u8],
    ) -> Option<InterceptResponse> {
        if method.eq_ignore_ascii_case("POST") && clean_path == "/v1/maverick/verify" {
            let Ok(req) = serde_json::from_slice::<MaverickVerifyRequest>(body) else {
                return Some(InterceptResponse {
                    status: 400,
                    headers: vec![("Content-Type".into(), "application/json".into())],
                    body: br#"{"error":"invalid_maverick_request"}"#.to_vec(),
                    denied: Some("invalid_maverick_request".into()),
                });
            };
            let outcome = verify_matvec_goldilocks(&req);
            let resp_body = serde_json::to_vec(&outcome).unwrap_or_default();
            if let Ok(mut st) = self.state.lock() {
                if outcome.verified {
                    st.maverick_verified += 1;
                    self.append_and_checkpoint_locked(
                        &mut st,
                        "maverick_verify_ok",
                        &outcome.proof_digest,
                        json!({"d_out": req.d_out, "d_in": req.d_in, "reps": outcome.repetitions}),
                    );
                } else {
                    st.maverick_cheats_caught += 1;
                    self.append_and_checkpoint_locked(
                        &mut st,
                        "maverick_cheat_caught",
                        &outcome.proof_digest,
                        json!({"reason": outcome.reason}),
                    );
                }
            }
            if outcome.verified {
                return Some(InterceptResponse {
                    status: 200,
                    headers: vec![
                        ("Content-Type".into(), "application/json".into()),
                        ("X-Playbox-Maverick-Verified".into(), "1".into()),
                    ],
                    body: resp_body,
                    denied: None,
                });
            }
            return Some(InterceptResponse {
                status: 502,
                headers: vec![
                    ("Content-Type".into(), "application/json".into()),
                    ("X-Playbox-Maverick-Cheat".into(), "1".into()),
                ],
                body: resp_body,
                denied: Some(
                    outcome
                        .reason
                        .unwrap_or_else(|| "maverick_verification_failed".into()),
                ),
            });
        }

        if method.eq_ignore_ascii_case("POST")
            && matches!(
                clean_path,
                "/v1/chat/completions" | "/v1/messages" | "/v1/responses"
            )
        {
            let prompt_text = extract_prompt_text(body);
            if let Ok(mut st) = self.state.lock() {
                let matched = st
                    .skills
                    .values()
                    .find(|s| prompt_text.contains(&s.trigger))
                    .cloned();
                if let Some(skill) = matched {
                    st.arc_hits += 1;
                    let req_digest = blake3::hash(body).to_hex().to_string();
                    self.append_and_checkpoint_locked(
                        &mut st,
                        "cpu_arc_hit",
                        &req_digest,
                        json!({"skill_hash": skill.skill_hash}),
                    );
                    return Some(InterceptResponse {
                        status: 200,
                        headers: vec![
                            ("Content-Type".into(), "application/json".into()),
                            ("X-Playbox-Arc-Hit".into(), "1".into()),
                            ("X-Playbox-Skill-Hash".into(), skill.skill_hash),
                            (
                                "X-Playbox-Latent-64B".into(),
                                hex_bytes(&skill.descriptor_64b),
                            ),
                        ],
                        body: skill.response_json.into_bytes(),
                        denied: None,
                    });
                }
                st.upstream_misses += 1;
                let req_digest = blake3::hash(body).to_hex().to_string();
                self.append_and_checkpoint_locked(
                    &mut st,
                    "upstream_miss",
                    &req_digest,
                    json!({"path": clean_path}),
                );
            }
        }
        None
    }

    /// Snapshot the `RoomLog` entries and dual-signed (`Ed25519 + ML-DSA-65`) checkpoints for offline verification.
    #[must_use]
    pub fn export_log_and_checkpoints(
        &self,
    ) -> (
        [u8; 32],
        Vec<playbox_core::roomlog::Entry>,
        Vec<SignedCheckpoint>,
    ) {
        let st = self.state.lock().expect("hook mutex");
        (
            st.log.room(),
            st.log.entries().to_vec(),
            st.checkpoints.clone(),
        )
    }

    /// Summary counters `(arc_hits, upstream_misses, maverick_verified, maverick_cheats_caught)`.
    #[must_use]
    pub fn stats(&self) -> (u64, u64, u64, u64) {
        let st = self.state.lock().expect("hook mutex");
        (
            st.arc_hits,
            st.upstream_misses,
            st.maverick_verified,
            st.maverick_cheats_caught,
        )
    }
}

fn extract_prompt_text(body: &[u8]) -> String {
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) {
        if let Some(arr) = v.get("messages").and_then(serde_json::Value::as_array) {
            let mut out = String::new();
            for msg in arr {
                if let Some(c) = msg.get("content").and_then(serde_json::Value::as_str) {
                    out.push_str(c);
                    out.push('\n');
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
        if let Some(inp) = v.get("input").and_then(serde_json::Value::as_str) {
            return inp.to_string();
        }
    }
    String::from_utf8_lossy(body).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raa_adjoint_and_maverick_cheat_detection() {
        let mut rng = SplitMix64::new(42);
        let code = RaaCode::sample(16, 8, &mut rng);
        let w: Vec<Goldilocks> = (0..16).map(|_| rng.next_nonzero_field()).collect();
        let v: Vec<Goldilocks> = (0..code.big_n).map(|_| rng.next_nonzero_field()).collect();
        let lhs = code
            .encode(&w)
            .iter()
            .zip(&v)
            .fold(Goldilocks::default(), |a, (&x, &y)| a + (x * y));
        let rhs = w
            .iter()
            .zip(code.syndrome(&v))
            .fold(Goldilocks::default(), |a, (&x, y)| a + (x * y));
        assert_eq!(lhs, rhs);

        let d_out = 8;
        let d_in = 12;
        let w_mat: Vec<i64> = (0..d_out * d_in)
            .map(|i| i64::try_from(i % 13).unwrap_or(0) - 6)
            .collect();
        let x: Vec<i64> = (0..d_in)
            .map(|i| i64::try_from(i % 7).unwrap_or(0) - 3)
            .collect();
        let mut y = vec![0i64; d_out];
        for i in 0..d_out {
            for j in 0..d_in {
                y[i] += w_mat[i * d_in + j] * x[j];
            }
        }
        let honest = MaverickVerifyRequest {
            d_out,
            d_in,
            w_row_major: w_mat.clone(),
            x: x.clone(),
            claimed_y: y.clone(),
            challenge_seed: 99,
            repetitions: 4,
        };
        assert!(verify_matvec_goldilocks(&honest).verified);

        let mut cheated_y = y;
        cheated_y[3] += 1; // 1-LSB cheat
        let cheat = MaverickVerifyRequest {
            d_out,
            d_in,
            w_row_major: w_mat,
            x,
            claimed_y: cheated_y,
            challenge_seed: 99,
            repetitions: 4,
        };
        let res = verify_matvec_goldilocks(&cheat);
        assert!(!res.verified);
        assert_eq!(res.reason.as_deref(), Some("maverick_verification_failed"));
    }

    #[test]
    fn descriptor_novelty_filter_works() {
        let d1 = encode_skill_descriptor_64b("skill-alpha", "result-a");
        let d2 = encode_skill_descriptor_64b("skill-beta", "result-b");
        assert!(!is_novel_descriptor(&[d1], &d1, 0.90));
        assert!(is_novel_descriptor(&[d1], &d2, 0.90));
    }
}
