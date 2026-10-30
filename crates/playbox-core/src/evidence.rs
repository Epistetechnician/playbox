//! Binding a variant's scores to the runs that produced them.
//!
//! A [`Variant`] says "this patch scored these numbers under that evaluator", and the numbers are its author's claim.
//! The custodian who ran the evaluation knows more: the scores were computed from the output of particular steering
//! runs, each of which the host signed a [`SteerReceipt`] for. [`ScoreEvidence`] is the custodian's signed statement of
//! which runs those were. [`verify`] then lets a third party check, without trusting the author:
//!
//! 1. the evidence is signed by the key that signed the evaluator (the custodian), and names this variant, this
//!    evaluator and this scoring harness;
//! 2. the receipts handed over are exactly the runs the evidence lists, in order, none repeated;
//! 3. every receipt is signed by a host the verifier pins, ran on the weights and engine image the verifier expects,
//!    and states at least the ceiling the verifier requires.
//!
//! # What this does and does not prove
//! It proves the scores were claimed by the custodian over runs that those hosts say they executed on those weights.
//! It does not prove the scores follow from the runs' outputs: the receipts bind the output *bytes* by hash
//! ([`Backing::outputs`]), so a verifier who has the outputs and the scoring harness can recompute the scores itself
//! (see `judge/experiments/verify_e4.py`); this module does not. It does not prove the custodian is honest, only that
//! a custodian who lies about the runs is contradicted by the host's receipts, and one who lies about the scoring is
//! contradicted by a replay. A host that lies in its receipts is only as trustworthy as its ceiling says.

use crate::{
    evaluator::{Evaluator, Variant},
    record::{self, hex, RecordError},
    steer::{Ceiling, SteerReceipt},
};
use ed25519_dalek::SigningKey;

const DOMAIN: &str = "playbox.score-evidence.v1";

fn digest(receipt_text: &str) -> [u8; 32] {
    *blake3::hash(receipt_text.as_bytes()).as_bytes()
}

fn reject<T>(why: impl Into<String>) -> Result<T, RecordError> {
    Err(RecordError::Rejected(why.into()))
}

/// The custodian's signed list of the runs behind a variant's scores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoreEvidence {
    pub variant: String,
    pub evaluator: String,
    /// The scoring harness (the evaluator's `harness`), hex.
    pub scorer: String,
    /// blake3 of each receipt's text, in the order the scores were produced.
    pub runs: Vec<[u8; 32]>,
    pub custodian: [u8; 32],
}

impl ScoreEvidence {
    /// Sign the list of `receipts` (their exact texts) as the runs behind `variant`'s scores.
    ///
    /// # Errors
    ///
    /// Returns a [`RecordError`] if there are no receipts or the record cannot be built.
    pub fn sign(variant: &Variant, evaluator: &Evaluator, receipts: &[&str], custodian: &SigningKey) -> Result<String, RecordError> {
        if receipts.is_empty() {
            return reject("evidence needs at least one run");
        }
        record::sign(
            DOMAIN,
            &[
                ("variant", variant.id.clone()),
                ("evaluator", evaluator.id.clone()),
                ("scorer", evaluator.harness.clone()),
                ("runs", receipts.iter().map(|r| hex(&digest(r))).collect::<Vec<_>>().join(",")),
            ],
            custodian,
        )
    }

    /// Parse and check the signature. Whether the signer is the right custodian is [`verify`]'s question.
    ///
    /// # Errors
    ///
    /// Returns a [`RecordError`] if the text is malformed or its signature does not verify.
    pub fn open(text: &str) -> Result<ScoreEvidence, RecordError> {
        let r = record::open(DOMAIN, text)?;
        let runs = r.list("runs")?.into_iter().map(record::unhex::<32>).collect::<Result<Vec<_>, _>>()?;
        if runs.is_empty() {
            return reject("evidence lists no runs");
        }
        Ok(ScoreEvidence {
            variant: r.req("variant")?.to_string(),
            evaluator: r.req("evaluator")?.to_string(),
            scorer: r.req("scorer")?.to_string(),
            runs,
            custodian: r.signer,
        })
    }
}

/// What a verifier insists on.
#[derive(Debug, Clone)]
pub struct Expect<'a> {
    /// Host keys whose receipts count. A receipt from any other key is refused.
    pub hosts: &'a [[u8; 32]],
    /// The weights manifest every run must have used, if the verifier names one.
    pub weights: Option<[u8; 32]>,
    /// The engine image every run must have used, if the verifier names one.
    pub image: Option<[u8; 32]>,
    /// The weakest ceiling a receipt may state.
    pub min_ceiling: Ceiling,
}

/// What a verified piece of evidence establishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backing {
    pub runs: usize,
    pub weights: [u8; 32],
    pub image: [u8; 32],
    /// The weakest ceiling among the receipts.
    pub weakest: Ceiling,
    /// Each run's host key, in order.
    pub hosts: Vec<[u8; 32]>,
    /// Each run's output hash, in order: what a replay must match.
    pub outputs: Vec<[u8; 32]>,
}

/// Check `evidence` and the `receipts` it lists against `variant`, `evaluator` and what the verifier expects.
///
/// # Errors
///
/// Returns a [`RecordError`] naming the first check that failed.
pub fn verify(evidence: &str, variant: &Variant, evaluator: &Evaluator, receipts: &[&str], expect: &Expect) -> Result<Backing, RecordError> {
    let ev = ScoreEvidence::open(evidence)?;
    if ev.custodian != evaluator.custodian {
        return reject("the evidence is not signed by the evaluator's custodian");
    }
    if variant.evaluator != evaluator.id || ev.evaluator != evaluator.id {
        return reject("the evidence, the variant and the evaluator do not name the same evaluator");
    }
    if ev.variant != variant.id {
        return reject("the evidence is for a different variant: its scores are not the ones signed here");
    }
    if ev.scorer != evaluator.harness {
        return reject("the evidence names a different scoring harness than the evaluator");
    }
    if receipts.len() != ev.runs.len() {
        return reject(format!("{} receipts given, the evidence lists {} runs", receipts.len(), ev.runs.len()));
    }
    let mut parsed = Vec::with_capacity(receipts.len());
    for (i, (text, want)) in receipts.iter().zip(&ev.runs).enumerate() {
        if &digest(text) != want {
            return reject(format!("receipt {i} is not the run the evidence lists"));
        }
        if ev.runs[..i].contains(want) {
            return reject(format!("run {i} is listed twice: one run cannot back two score batches"));
        }
        let r = SteerReceipt::open(text)?;
        if !expect.hosts.contains(&r.host) {
            return reject(format!("receipt {i} is signed by a host this verifier does not pin"));
        }
        if expect.weights.is_some_and(|w| w != r.weights) {
            return reject(format!("receipt {i} ran on weights other than the ones expected"));
        }
        if expect.image.is_some_and(|im| im != r.image) {
            return reject(format!("receipt {i} ran in an engine image other than the one expected"));
        }
        if r.ceiling < expect.min_ceiling {
            return reject(format!("receipt {i} states a weaker ceiling than required"));
        }
        parsed.push(r);
    }
    let (weights, image) = (parsed[0].weights, parsed[0].image);
    if parsed.iter().any(|r| r.weights != weights || r.image != image) {
        return reject("the runs behind one variant used different weights or engine images");
    }
    Ok(Backing {
        runs: parsed.len(),
        weights,
        image,
        weakest: parsed.iter().map(|r| r.ceiling).min().expect("non-empty"),
        hosts: parsed.iter().map(|r| r.host).collect(),
        outputs: parsed.iter().map(|r| r.output_hash).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        evaluator::{EvaluatorSpec, VariantSpec},
        steer::{Mode, Recipe, Site, SteerSpec, Tokens},
    };

    fn key(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }

    fn evaluator(custodian: &SigningKey) -> Evaluator {
        Evaluator::issue(
            EvaluatorSpec { name: "steer", version: 1, harness: "ab", holdout: [1; 32], command: "score", protected: &["harness/"], min_runs: 2, supersedes: None },
            custodian,
        )
        .unwrap()
    }

    fn spec(coef_milli: i64) -> SteerSpec {
        SteerSpec {
            model: "tiny".into(), weights: [5; 32], site: Site::ResidPost, layers: vec![6], direction: [2; 32], recipe: Recipe::DiffMean,
            mode: Mode::Add, coef_milli, tokens: Tokens::All, prompts: [3; 32], seed: 0,
        }
    }

    fn receipt(host: &SigningKey, coef: i64, output: &[u8], image: [u8; 32], ceiling: Ceiling) -> String {
        SteerReceipt::sign(&spec(coef), image, b"req", output, ceiling, 1, host).unwrap()
    }

    struct World {
        ev: Evaluator,
        variant: Variant,
        evidence: String,
        receipts: Vec<String>,
        host: [u8; 32],
    }

    fn world() -> World {
        let (custodian, host, agent) = (key(1), key(2), key(3));
        let ev = evaluator(&custodian);
        let variant = Variant::sign(VariantSpec { parent: None, evaluator: &ev, patch: "", scores: &[0.3, 0.4, 0.5], cost: 1 }, &agent).unwrap();
        let receipts = vec![receipt(&host, 200, b"out-1", [7; 32], Ceiling::LocalInternalExploratory)];
        let refs: Vec<&str> = receipts.iter().map(String::as_str).collect();
        let evidence = ScoreEvidence::sign(&variant, &ev, &refs, &custodian).unwrap();
        World { ev, variant, evidence, receipts, host: host.verifying_key().to_bytes() }
    }

    fn check(w: &World, receipts: &[String], hosts: &[[u8; 32]], tweak: impl FnOnce(&mut Expect)) -> Result<Backing, RecordError> {
        let mut e = Expect { hosts, weights: Some([5; 32]), image: Some([7; 32]), min_ceiling: Ceiling::LocalInternalExploratory };
        tweak(&mut e);
        let refs: Vec<&str> = receipts.iter().map(String::as_str).collect();
        verify(&w.evidence, &w.variant, &w.ev, &refs, &e)
    }

    fn why(r: Result<Backing, RecordError>) -> String {
        r.unwrap_err().to_string()
    }

    #[test]
    fn honest_evidence_verifies_and_reports_what_it_establishes() {
        let w = world();
        let b = check(&w, &w.receipts, &[w.host], |_| {}).unwrap();
        assert_eq!((b.runs, b.weights, b.image, b.weakest), (1, [5; 32], [7; 32], Ceiling::LocalInternalExploratory));
        assert_eq!(b.hosts, vec![w.host]);
        assert_eq!(b.outputs, vec![*blake3::hash(b"out-1").as_bytes()]);
        // A verifier that names no weights or image accepts any, as long as the runs agree with each other.
        assert!(check(&w, &w.receipts, &[w.host], |e| {
            e.weights = None;
            e.image = None;
        })
        .is_ok());
    }

    #[test]
    fn evidence_for_other_scores_is_refused_because_the_variant_id_covers_them() {
        let w = world();
        let forged = Variant::sign(VariantSpec { parent: None, evaluator: &w.ev, patch: "", scores: &[0.9, 0.9, 0.9], cost: 1 }, &key(3)).unwrap();
        assert_ne!(forged.id, w.variant.id);
        let refs: Vec<&str> = w.receipts.iter().map(String::as_str).collect();
        let e = Expect { hosts: &[w.host], weights: None, image: None, min_ceiling: Ceiling::HostClaim };
        let err = verify(&w.evidence, &forged, &w.ev, &refs, &e).unwrap_err().to_string();
        assert!(err.contains("different variant"), "{err}");
    }

    #[test]
    fn receipts_must_be_exactly_the_listed_runs() {
        let w = world();
        let other = receipt(&key(2), 200, b"out-2", [7; 32], Ceiling::LocalInternalExploratory);
        assert!(why(check(&w, &[other.clone()], &[w.host], |_| {})).contains("not the run the evidence lists"));
        assert!(why(check(&w, &[], &[w.host], |_| {})).contains("0 receipts"));
        let two = vec![w.receipts[0].clone(), other];
        assert!(why(check(&w, &two, &[w.host], |_| {})).contains("2 receipts"));
        let tampered = w.receipts[0].replace("ceiling local_internal_exploratory", "ceiling attested");
        assert!(check(&w, &[tampered], &[w.host], |_| {}).is_err(), "an edited receipt breaks its signature or its digest");
    }

    #[test]
    fn only_pinned_hosts_the_expected_weights_image_and_ceiling_count() {
        let w = world();
        assert!(why(check(&w, &w.receipts, &[key(9).verifying_key().to_bytes()], |_| {})).contains("does not pin"));
        assert!(why(check(&w, &w.receipts, &[w.host], |e| e.weights = Some([6; 32]))).contains("weights other"));
        assert!(why(check(&w, &w.receipts, &[w.host], |e| e.image = Some([8; 32]))).contains("image other"));
        assert!(why(check(&w, &w.receipts, &[w.host], |e| e.min_ceiling = Ceiling::Attested)).contains("weaker ceiling"));
    }

    #[test]
    fn only_the_evaluators_custodian_can_vouch_and_only_for_its_own_evaluator() {
        let w = world();
        let refs: Vec<&str> = w.receipts.iter().map(String::as_str).collect();
        let by_agent = ScoreEvidence::sign(&w.variant, &w.ev, &refs, &key(3)).unwrap();
        let e = Expect { hosts: &[w.host], weights: None, image: None, min_ceiling: Ceiling::HostClaim };
        assert!(verify(&by_agent, &w.variant, &w.ev, &refs, &e).unwrap_err().to_string().contains("custodian"));
        let other_ev = Evaluator::issue(
            EvaluatorSpec { name: "other", version: 1, harness: "ab", holdout: [1; 32], command: "score", protected: &[], min_runs: 2, supersedes: None },
            &key(1),
        )
        .unwrap();
        assert!(verify(&w.evidence, &w.variant, &other_ev, &refs, &e).unwrap_err().to_string().contains("same evaluator"));
        let other_scorer = Evaluator::issue(
            EvaluatorSpec { name: "steer", version: 1, harness: "cd", holdout: [1; 32], command: "score", protected: &["harness/"], min_runs: 2, supersedes: None },
            &key(1),
        )
        .unwrap();
        let v2 = Variant::sign(VariantSpec { parent: None, evaluator: &other_scorer, patch: "", scores: &[0.3, 0.4, 0.5], cost: 1 }, &key(3)).unwrap();
        let ev2 = ScoreEvidence::sign(&v2, &other_scorer, &refs, &key(1)).unwrap();
        assert!(verify(&ev2, &v2, &w.ev, &refs, &e).is_err());
    }

    #[test]
    fn runs_cannot_be_repeated_or_mixed_across_weights() {
        let (custodian, host, agent) = (key(1), key(2), key(3));
        let ev = evaluator(&custodian);
        let variant = Variant::sign(VariantSpec { parent: None, evaluator: &ev, patch: "", scores: &[0.3, 0.4], cost: 1 }, &agent).unwrap();
        let hosts = [host.verifying_key().to_bytes()];
        let e = Expect { hosts: &hosts, weights: None, image: None, min_ceiling: Ceiling::HostClaim };
        let r1 = receipt(&host, 200, b"a", [7; 32], Ceiling::HostClaim);
        let dup = ScoreEvidence::sign(&variant, &ev, &[&r1, &r1], &custodian).unwrap();
        assert!(verify(&dup, &variant, &ev, &[&r1, &r1], &e).unwrap_err().to_string().contains("twice"));
        let r2 = receipt(&host, 200, b"b", [8; 32], Ceiling::HostClaim);
        let mixed = ScoreEvidence::sign(&variant, &ev, &[&r1, &r2], &custodian).unwrap();
        assert!(verify(&mixed, &variant, &ev, &[&r1, &r2], &e).unwrap_err().to_string().contains("different weights or engine images"));
        // Several runs on the same weights and image are fine, and the weakest ceiling is reported.
        let r3 = receipt(&host, 200, b"c", [7; 32], Ceiling::LocalInternalExploratory);
        let ok = ScoreEvidence::sign(&variant, &ev, &[&r1, &r3], &custodian).unwrap();
        let b = verify(&ok, &variant, &ev, &[&r1, &r3], &e).unwrap();
        assert_eq!((b.runs, b.weakest), (2, Ceiling::HostClaim));
    }

    #[test]
    fn evidence_text_is_signed_and_needs_at_least_one_run() {
        let w = world();
        assert!(ScoreEvidence::open(&w.evidence.replace("scorer ab", "scorer cd")).is_err(), "edited field");
        assert!(ScoreEvidence::open(&format!("{}x 1\n", w.evidence)).is_err(), "trailing data");
        let refs: [&str; 0] = [];
        assert!(ScoreEvidence::sign(&w.variant, &w.ev, &refs, &key(1)).is_err());
        let o = ScoreEvidence::open(&w.evidence).unwrap();
        assert_eq!((o.variant.as_str(), o.runs.len(), o.custodian), (w.variant.id.as_str(), 1, key(1).verifying_key().to_bytes()));
    }
}
