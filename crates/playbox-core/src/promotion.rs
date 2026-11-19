//! Promotion as a quorum rule (M5 of `docs/host-less-rooms.md`).
//!
//! A challenge problem is registered in the room log with a rule: at least `j >= 2` distinct hosts must each
//! have run the evaluation themselves and signed a [`Receipt`], the receipts must agree within `noise_ppm`, and
//! the claim (the **lowest** measured metric, as in `docs/challenge.md`) must beat the frontier by `min_gain_ppm`.
//! A `promotion` entry carries the receipts. Every host replaying the log re-checks all of it, so a leader
//! cannot promote on its own say-so, and the strength of a promotion is the lowest attestation tier among its signers.
//!
//! Metrics are integers (micro-units, higher is better) so every host computes the same verdict.

use crate::roomlog::{hex, unhex, Entry, Hash, Key, LogError};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

const RECEIPT_DOMAIN: &[u8] = b"playbox.roomlog.receipt.v1";
/// The fewest distinct hosts a rule may ask for. One host's word is never enough.
pub const MIN_J: usize = 2;
const MAX_RECEIPTS: usize = 16;

/// What a promotion of one problem needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    /// Distinct hosts that must have measured the candidate.
    pub j: usize,
    /// The most the receipts may differ, as parts per million of the largest metric.
    pub noise_ppm: u64,
    /// How far above the frontier the lowest metric must be, in parts per million of the frontier.
    pub min_gain_ppm: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Register {
    pub problem: Hash,
    pub rule: Rule,
    /// The metric of the pinned baseline: the first frontier.
    pub baseline: i64,
}

/// One host's own measurement of a candidate, signed with its host key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub host: Key,
    /// Hash of the environment this host measured in (software and hardware); hosts may differ.
    pub env: Hash,
    /// The host's attestation tier, 0 (none) and up.
    pub tier: u8,
    /// The measured metric in micro-units, higher is better.
    pub metric: i64,
    pub sig: [u8; 64],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promotion {
    pub problem: Hash,
    pub candidate: Hash,
    /// The model weights every receipt was measured with.
    pub weights: Hash,
    pub receipts: Vec<Receipt>,
}

/// The best promoted candidate of a problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frontier {
    pub candidate: Hash,
    pub metric: i64,
    /// The lowest tier among the hosts that signed the promotion (0 for the baseline).
    pub strength: u8,
    /// Index of the log entry that promoted it (0 for the baseline).
    pub index: u64,
}

pub type State = BTreeMap<Hash, (Rule, Frontier)>;

impl Receipt {
    fn signing_bytes(
        &self,
        room: &Key,
        problem: &Hash,
        candidate: &Hash,
        weights: &Hash,
    ) -> Vec<u8> {
        let mut b = Vec::with_capacity(RECEIPT_DOMAIN.len() + 200);
        b.extend_from_slice(RECEIPT_DOMAIN);
        b.extend_from_slice(room);
        b.extend_from_slice(&self.host);
        b.extend_from_slice(problem);
        b.extend_from_slice(candidate);
        b.extend_from_slice(weights);
        b.extend_from_slice(&self.env);
        b.push(self.tier);
        b.extend_from_slice(&self.metric.to_be_bytes());
        b
    }

    /// A host's receipt for `candidate` on `problem`, as measured with `weights` in `env`.
    #[must_use]
    #[allow(clippy::too_many_arguments)] // every field is part of what the signature binds
    pub fn sign(
        room: &Key,
        key: &SigningKey,
        problem: &Hash,
        candidate: &Hash,
        weights: &Hash,
        env: Hash,
        tier: u8,
        metric: i64,
    ) -> Self {
        let mut r = Self {
            host: key.verifying_key().to_bytes(),
            env,
            tier,
            metric,
            sig: [0; 64],
        };
        r.sig = key
            .sign(&r.signing_bytes(room, problem, candidate, weights))
            .to_bytes();
        r
    }

    fn verifies(&self, room: &Key, p: &Promotion) -> bool {
        VerifyingKey::from_bytes(&self.host)
            .and_then(|k| {
                k.verify(
                    &self.signing_bytes(room, &p.problem, &p.candidate, &p.weights),
                    &Signature::from_bytes(&self.sig),
                )
            })
            .is_ok()
    }
}

fn bad(e: &Entry, m: impl Into<String>) -> LogError {
    LogError::Promotion(e.index, m.into())
}

fn json_of(e: &Entry) -> Result<Value, LogError> {
    serde_json::from_slice(&e.payload).map_err(|_| bad(e, "payload is not json"))
}

fn hash(e: &Entry, v: &Value, what: &str) -> Result<Hash, LogError> {
    v.as_str()
        .and_then(unhex::<32>)
        .ok_or_else(|| bad(e, format!("{what} is not 64 hex characters")))
}

/// The payload of a `problem.register` entry.
#[must_use]
pub fn register_payload(r: &Register) -> Value {
    json!({
        "problem": hex(&r.problem), "baseline": r.baseline,
        "j": r.rule.j, "noise_ppm": r.rule.noise_ppm, "min_gain_ppm": r.rule.min_gain_ppm,
    })
}

/// The payload of a `promotion` entry.
#[must_use]
pub fn promotion_payload(p: &Promotion) -> Value {
    json!({
        "problem": hex(&p.problem), "candidate": hex(&p.candidate), "weights": hex(&p.weights),
        "receipts": p.receipts.iter().map(|r| json!({
            "host": hex(&r.host), "env": hex(&r.env), "tier": r.tier, "metric": r.metric, "sig": hex(&r.sig),
        })).collect::<Vec<_>>(),
    })
}

/// # Errors
///
/// Fails if the payload is not a well-formed registration with a sane rule.
pub fn parse_register(e: &Entry) -> Result<Register, LogError> {
    let v = json_of(e)?;
    let num = |k: &str| {
        v[k].as_u64()
            .ok_or_else(|| bad(e, format!("{k} is missing")))
    };
    let j = usize::try_from(num("j")?).map_err(|_| bad(e, "j"))?;
    let rule = Rule {
        j,
        noise_ppm: num("noise_ppm")?,
        min_gain_ppm: num("min_gain_ppm")?,
    };
    if rule.j < MIN_J || rule.j > MAX_RECEIPTS {
        return Err(bad(
            e,
            format!("j must be {MIN_J} to {MAX_RECEIPTS}: one host's word is not enough"),
        ));
    }
    if rule.noise_ppm > 1_000_000 || rule.min_gain_ppm > 100_000_000 {
        return Err(bad(e, "noise_ppm or min_gain_ppm is out of range"));
    }
    let baseline = v["baseline"]
        .as_i64()
        .filter(|b| *b > 0)
        .ok_or_else(|| bad(e, "baseline must be a positive metric"))?;
    Ok(Register {
        problem: hash(e, &v["problem"], "problem")?,
        rule,
        baseline,
    })
}

/// # Errors
///
/// Fails if the payload is not a well-formed promotion.
pub fn parse_promotion(e: &Entry) -> Result<Promotion, LogError> {
    let v = json_of(e)?;
    let mut receipts = Vec::new();
    for r in v["receipts"]
        .as_array()
        .ok_or_else(|| bad(e, "no receipts"))?
        .iter()
        .take(MAX_RECEIPTS + 1)
    {
        receipts.push(Receipt {
            host: hash(e, &r["host"], "host")?,
            env: hash(e, &r["env"], "env")?,
            tier: u8::try_from(r["tier"].as_u64().ok_or_else(|| bad(e, "tier"))?)
                .map_err(|_| bad(e, "tier"))?,
            metric: r["metric"].as_i64().ok_or_else(|| bad(e, "metric"))?,
            sig: r["sig"]
                .as_str()
                .and_then(unhex::<64>)
                .ok_or_else(|| bad(e, "receipt signature"))?,
        });
    }
    if receipts.len() > MAX_RECEIPTS {
        return Err(bad(e, "too many receipts"));
    }
    Ok(Promotion {
        problem: hash(e, &v["problem"], "problem")?,
        candidate: hash(e, &v["candidate"], "candidate")?,
        weights: hash(e, &v["weights"], "weights")?,
        receipts,
    })
}

/// Check a `problem.register` against what the log already registered.
///
/// # Errors
///
/// Fails if the problem is already registered or `j` is more than the hosts there are.
pub fn check_register(e: &Entry, hosts: &BTreeSet<Key>, state: &State) -> Result<(), LogError> {
    let r = parse_register(e)?;
    if state.contains_key(&r.problem) {
        return Err(bad(e, "that problem is already registered"));
    }
    if r.rule.j > hosts.len() {
        return Err(bad(
            e,
            format!("j={} is more than the {} hosts", r.rule.j, hosts.len()),
        ));
    }
    Ok(())
}

/// Check a `promotion` entry: the rule of its problem, distinct current hosts with valid receipts, agreement
/// within the noise, and a gain over the frontier.
///
/// # Errors
///
/// Fails with the first rule the promotion breaks.
pub fn check_promotion(
    room: &Key,
    e: &Entry,
    hosts: &BTreeSet<Key>,
    state: &State,
) -> Result<(), LogError> {
    let p = parse_promotion(e)?;
    let (rule, front) = state
        .get(&p.problem)
        .ok_or_else(|| bad(e, "that problem is not registered"))?;
    if front.candidate == p.candidate {
        return Err(bad(e, "that candidate is already the frontier"));
    }
    let mut seen = BTreeSet::new();
    let mut metrics = Vec::new();
    for r in &p.receipts {
        if !hosts.contains(&r.host) {
            return Err(bad(
                e,
                "a receipt is from a key that is not a host of this epoch",
            ));
        }
        if !seen.insert(r.host) {
            return Err(bad(
                e,
                "two receipts from one host count once; send distinct hosts",
            ));
        }
        if !r.verifies(room, &p) {
            return Err(bad(e, "a receipt's signature does not verify"));
        }
        if r.metric <= 0 {
            return Err(bad(e, "a receipt reports no positive metric"));
        }
        metrics.push(r.metric);
    }
    if seen.len() < rule.j {
        return Err(bad(
            e,
            format!(
                "{} distinct host(s) measured it; the rule needs {}",
                seen.len(),
                rule.j
            ),
        ));
    }
    let (lo, hi) = (
        i128::from(*metrics.iter().min().unwrap_or(&0)),
        i128::from(*metrics.iter().max().unwrap_or(&0)),
    );
    if (hi - lo) * 1_000_000 > hi * i128::from(rule.noise_ppm) {
        return Err(bad(
            e,
            format!("the receipts differ by more than {} ppm", rule.noise_ppm),
        ));
    }
    // The claim is the lowest measurement: a host that measured less is not talked over by one that measured more.
    if lo * 1_000_000 < i128::from(front.metric) * (1_000_000 + i128::from(rule.min_gain_ppm)) {
        return Err(bad(
            e,
            format!(
                "the lowest metric {lo} does not beat the frontier {} by {} ppm",
                front.metric, rule.min_gain_ppm
            ),
        ));
    }
    Ok(())
}

/// The frontier of every registered problem after replaying `entries` (already checked).
#[must_use]
pub fn replay(entries: &[Entry]) -> State {
    let mut st = State::new();
    for e in entries {
        match e.kind.as_str() {
            "problem.register" => {
                if let Ok(r) = parse_register(e) {
                    st.entry(r.problem).or_insert((
                        r.rule,
                        Frontier {
                            candidate: [0; 32],
                            metric: r.baseline,
                            strength: 0,
                            index: 0,
                        },
                    ));
                }
            }
            "promotion" => {
                if let Ok(p) = parse_promotion(e) {
                    if let Some((_, f)) = st.get_mut(&p.problem) {
                        *f = Frontier {
                            candidate: p.candidate,
                            metric: p
                                .receipts
                                .iter()
                                .map(|r| r.metric)
                                .min()
                                .unwrap_or(f.metric),
                            strength: p.receipts.iter().map(|r| r.tier).min().unwrap_or(0),
                            index: e.index,
                        };
                    }
                }
            }
            _ => {}
        }
    }
    st
}
