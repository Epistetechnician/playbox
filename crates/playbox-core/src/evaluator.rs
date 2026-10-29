//! Verifiable self-improvement: a signed evaluator, a sealed holdout, and a lineage of variants.
//!
//! "My agent improved itself" is only a claim until three things hold, and this module is those three:
//!
//! 1. **The evaluator is fixed and was not the variant's to change.** An [`Evaluator`] is a manifest
//!    signed by a *custodian* (not the agent). It names the harness hash, the holdout commitment, the
//!    command, the paths no variant may touch, and how many runs a claim needs. Changing it takes a new
//!    custodian-signed manifest that names the one it supersedes. This is the opposite choice to
//!    co-evolving the evaluator inside the loop: here the loop cannot move the goalposts, and a human
//!    rotates them on purpose.
//! 2. **The holdout was sealed before the agent worked.** The custodian publishes only a Merkle root over
//!    salted items ([`Holdout::commit`]). Items are opened one at a time with a proof against that root,
//!    so a grader can show an item belongs to the committed set without revealing the others.
//! 3. **A claim of improvement is a signed, linked, statistically supported record.** A [`Variant`] says
//!    which parent it improves on, which evaluator scored it, the per-run scores, the compute spent and
//!    a hash of the patch. [`Lineage::add`] rejects it if the patch touches a protected path, the
//!    evaluator changed without a rotation, the parent is unknown, or the patch is not the one signed.
//!    [`Lineage::improvement`] reports a one-sided lower confidence bound on the score gain, not just a
//!    difference of means, because LLM runs are noisy.
//!
//! # What this does and does not prove
//! It proves the *procedure*: this variant, produced by this patch on that parent, scored these numbers
//! under that evaluator, and the evaluator was not edited by the agent. It does not prove the scores
//! were honestly produced unless the custodian (or an attested runner, see [`crate::confidential`]) ran
//! the evaluation and signed them; a variant author who runs their own evaluator can report anything.
//! It does not detect a model that games the evaluator's *content* (reward hacking inside the allowed
//! files), only edits to the evaluator itself. And a fixed holdout is eventually overfit by repeated
//! queries, so `min_runs` and a query budget (the lineage's total cost) matter.

use crate::record::{self, hash_parts, hex};
use ed25519_dalek::SigningKey;
use std::collections::BTreeMap;

const EVAL_DOMAIN: &str = "playbox.evaluator.v1";
const VARIANT_DOMAIN: &str = "playbox.variant.v1";

/// One-sided 95% normal quantile.
pub const Z_95: f64 = 1.645;

/// A custodian-signed evaluator manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluator {
    /// Identity: blake3 of the signed text, hex.
    pub id: String,
    pub text: String,
    pub name: String,
    pub version: u64,
    /// Hash of the evaluation harness (code and config), hex.
    pub harness: String,
    /// Merkle root of the sealed holdout, hex.
    pub holdout: String,
    pub command: String,
    /// Path prefixes a variant must not modify (the harness, its config, the grader).
    pub protected: Vec<String>,
    pub min_runs: u64,
    /// The evaluator this one replaces, if it is a rotation.
    pub supersedes: Option<String>,
    pub custodian: [u8; 32],
}

pub struct EvaluatorSpec<'a> {
    pub name: &'a str,
    pub version: u64,
    pub harness: &'a str,
    pub holdout: [u8; 32],
    pub command: &'a str,
    pub protected: &'a [&'a str],
    pub min_runs: u64,
    pub supersedes: Option<&'a Evaluator>,
}

impl Evaluator {
    pub fn issue(spec: EvaluatorSpec, custodian: &SigningKey) -> Result<Evaluator, String> {
        if spec.protected.iter().any(|p| p.contains(',') || p.is_empty()) {
            return Err("protected paths must be non-empty and contain no commas".into());
        }
        let text = record::sign(
            EVAL_DOMAIN,
            &[
                ("name", spec.name.into()),
                ("version", spec.version.to_string()),
                ("harness", spec.harness.into()),
                ("holdout", hex(&spec.holdout)),
                ("command", spec.command.into()),
                ("protected", spec.protected.join(",")),
                ("min_runs", spec.min_runs.to_string()),
                ("supersedes", spec.supersedes.map(|e| e.id.clone()).unwrap_or_default()),
            ],
            custodian,
        )?;
        Self::open(&text, &[custodian.verifying_key().to_bytes()])
    }

    /// Parse and check a manifest signed by one of `custodians`.
    pub fn open(text: &str, custodians: &[[u8; 32]]) -> Result<Evaluator, String> {
        let r = record::open(EVAL_DOMAIN, text)?;
        if !custodians.contains(&r.signer) {
            return Err("evaluator is not signed by a trusted custodian".into());
        }
        Ok(Evaluator {
            id: hex(&hash_parts("playbox.evaluator.id", &[text.as_bytes()])),
            text: text.to_string(),
            name: r.req("name")?.to_string(),
            version: r.num("version")?,
            harness: r.req("harness")?.to_string(),
            holdout: r.req("holdout")?.to_string(),
            command: r.req("command")?.to_string(),
            protected: r.list("protected")?.into_iter().map(String::from).collect(),
            min_runs: r.num("min_runs")?,
            supersedes: Some(r.req("supersedes")?).filter(|s| !s.is_empty()).map(String::from),
            custodian: r.signer,
        })
    }
}

// ---- sealed holdout ---------------------------------------------------------------------------------

fn leaf(blind: &[u8; 32], index: u64, item: &[u8]) -> [u8; 32] {
    hash_parts("playbox.holdout.leaf", &[blind, &index.to_be_bytes(), item])
}

fn node(l: &[u8; 32], r: &[u8; 32]) -> [u8; 32] {
    hash_parts("playbox.holdout.node", &[l, r])
}

fn blind(salt: &[u8; 32], index: u64) -> [u8; 32] {
    hash_parts("playbox.holdout.blind", &[salt, &index.to_be_bytes()])
}

/// One level up the tree. An odd node out is carried up unchanged.
fn up(level: &[[u8; 32]]) -> Vec<[u8; 32]> {
    level.chunks(2).map(|c| if let [l, r] = c { node(l, r) } else { c[0] }).collect()
}

/// The custodian's side of a sealed holdout.
pub struct Holdout {
    salt: [u8; 32],
    items: Vec<Vec<u8>>,
}

/// Proof that `item` is entry `index` of the set a root commits to. Reveals nothing about other items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemProof {
    pub index: u64,
    pub total: u64,
    pub blind: [u8; 32],
    pub item: Vec<u8>,
    /// Sibling hashes from the leaf to the root; `None` where the node had no sibling.
    pub path: Vec<Option<[u8; 32]>>,
}

impl Holdout {
    /// `salt` must be secret random bytes: it keeps low-entropy items from being guessed from the root.
    pub fn new(salt: [u8; 32], items: Vec<Vec<u8>>) -> Result<Holdout, String> {
        if items.is_empty() {
            return Err("a holdout needs at least one item".into());
        }
        Ok(Holdout { salt, items })
    }

    fn leaves(&self) -> Vec<[u8; 32]> {
        self.items.iter().enumerate().map(|(i, it)| leaf(&blind(&self.salt, i as u64), i as u64, it)).collect()
    }

    /// The root to publish (and to put in the evaluator manifest).
    pub fn commit(&self) -> [u8; 32] {
        let mut level = self.leaves();
        while level.len() > 1 {
            level = up(&level);
        }
        level[0]
    }

    pub fn open(&self, index: u64) -> Result<ItemProof, String> {
        let i = usize::try_from(index).ok().filter(|i| *i < self.items.len()).ok_or("index out of range")?;
        let (mut level, mut pos, mut path) = (self.leaves(), i, Vec::new());
        while level.len() > 1 {
            path.push(level.get(pos ^ 1).copied());
            level = up(&level);
            pos /= 2;
        }
        Ok(ItemProof { index, total: self.items.len() as u64, blind: blind(&self.salt, index), item: self.items[i].clone(), path })
    }
}

impl ItemProof {
    /// Does this proof tie `item` to the committed `root`?
    pub fn verify(&self, root: &[u8; 32]) -> bool {
        let (mut h, mut pos, mut len) = (leaf(&self.blind, self.index, &self.item), self.index, self.total);
        if self.index >= self.total {
            return false;
        }
        for sibling in &self.path {
            let has_sibling = (pos ^ 1) < len;
            match (sibling, has_sibling) {
                (Some(s), true) => h = if pos % 2 == 0 { node(&h, s) } else { node(s, &h) },
                (None, false) => {}
                _ => return false,
            }
            pos /= 2;
            len = len.div_ceil(2);
        }
        len == 1 && h == *root
    }
}

// ---- variants and lineage ---------------------------------------------------------------------------

/// A signed claim: "this patch on that parent scored these numbers under that evaluator".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub id: String,
    pub text: String,
    pub parent: Option<String>,
    pub evaluator: String,
    /// blake3 of the patch, hex.
    pub patch: String,
    /// Per-run scores, in millionths.
    pub scores: Vec<i64>,
    /// Compute or query units spent producing and scoring it.
    pub cost: u64,
    pub author: [u8; 32],
}

pub struct VariantSpec<'a> {
    pub parent: Option<&'a Variant>,
    pub evaluator: &'a Evaluator,
    pub patch: &'a str,
    pub scores: &'a [f64],
    pub cost: u64,
}

fn micro(x: f64) -> i64 {
    (x * 1e6).round() as i64
}

impl Variant {
    pub fn sign(spec: VariantSpec, key: &SigningKey) -> Result<Variant, String> {
        let text = record::sign(
            VARIANT_DOMAIN,
            &[
                ("parent", spec.parent.map(|p| p.id.clone()).unwrap_or_default()),
                ("evaluator", spec.evaluator.id.clone()),
                ("patch", hex(blake3::hash(spec.patch.as_bytes()).as_bytes())),
                ("scores", spec.scores.iter().map(|s| micro(*s).to_string()).collect::<Vec<_>>().join(",")),
                ("cost", spec.cost.to_string()),
            ],
            key,
        )?;
        Self::open(&text)
    }

    pub fn open(text: &str) -> Result<Variant, String> {
        let r = record::open(VARIANT_DOMAIN, text)?;
        Ok(Variant {
            id: hex(&hash_parts("playbox.variant.id", &[text.as_bytes()])),
            text: text.to_string(),
            parent: Some(r.req("parent")?).filter(|s| !s.is_empty()).map(String::from),
            evaluator: r.req("evaluator")?.to_string(),
            patch: r.req("patch")?.to_string(),
            scores: r.list("scores")?.into_iter().map(|s| s.parse::<i64>().map_err(|_| "bad score".to_string())).collect::<Result<_, _>>()?,
            cost: r.num("cost")?,
            author: r.signer,
        })
    }
}

/// The paths a unified diff changes, including both sides of a rename, as the host sees them.
pub fn touched_paths(patch: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in patch.lines() {
        for prefix in ["+++ ", "--- ", "rename from ", "rename to ", "copy to "] {
            if let Some(p) = line.strip_prefix(prefix) {
                let p = p.split('\t').next().unwrap_or(p).trim_matches('"');
                if p != "/dev/null" {
                    out.push(p.strip_prefix("a/").or_else(|| p.strip_prefix("b/")).unwrap_or(p).to_string());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn protected_hit(protected: &[String], path: &str) -> bool {
    let norm = path.trim_start_matches("./");
    norm.split('/').any(|s| s == "..")
        || protected.iter().any(|p| {
            let p = p.trim_end_matches('/');
            norm == p || norm.starts_with(&format!("{p}/"))
        })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Improvement {
    pub parent_mean: f64,
    pub mean: f64,
    pub delta: f64,
    /// One-sided lower confidence bound on `delta` at the requested z.
    pub lower_bound: f64,
    pub runs: (usize, usize),
}

impl Improvement {
    /// Supported gain: the lower bound is above zero.
    pub fn significant(&self) -> bool {
        self.lower_bound > 0.0
    }
}

fn mean_var(xs: &[i64]) -> (f64, f64) {
    let n = xs.len() as f64;
    let m = xs.iter().map(|x| *x as f64 / 1e6).sum::<f64>() / n;
    let v = if xs.len() > 1 { xs.iter().map(|x| (*x as f64 / 1e6 - m).powi(2)).sum::<f64>() / (n - 1.0) } else { 0.0 };
    (m, v)
}

#[derive(Default)]
pub struct Lineage {
    evaluators: BTreeMap<String, Evaluator>,
    variants: BTreeMap<String, Variant>,
    order: Vec<String>,
}

impl Lineage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_evaluator(&mut self, e: Evaluator) {
        self.evaluators.insert(e.id.clone(), e);
    }

    pub fn get(&self, id: &str) -> Option<&Variant> {
        self.variants.get(id)
    }

    /// Admit a variant. `patch` is the diff the host actually computed; it must be the one signed.
    pub fn add(&mut self, variant: Variant, patch: &str) -> Result<(), String> {
        if self.variants.contains_key(&variant.id) {
            return Err("variant already recorded".into());
        }
        if hex(blake3::hash(patch.as_bytes()).as_bytes()) != variant.patch {
            return Err("the patch is not the one this variant signed".into());
        }
        let ev = self.evaluators.get(&variant.evaluator).ok_or("variant names an evaluator this lineage does not trust")?;
        if variant.scores.len() as u64 > 0 && (variant.scores.len() as u64) < ev.min_runs {
            return Err(format!("{} runs, the evaluator needs at least {}", variant.scores.len(), ev.min_runs));
        }
        if let Some(bad) = touched_paths(patch).iter().find(|p| protected_hit(&ev.protected, p)) {
            return Err(format!("the patch touches protected path {bad:?}: a variant may not edit its own evaluator"));
        }
        match &variant.parent {
            None => {}
            Some(pid) => {
                let parent = self.variants.get(pid).ok_or("parent is not in the lineage")?;
                if parent.evaluator != variant.evaluator && ev.supersedes.as_deref() != Some(parent.evaluator.as_str()) {
                    return Err("evaluator changed without a custodian-signed rotation naming the parent's evaluator".into());
                }
            }
        }
        self.order.push(variant.id.clone());
        self.variants.insert(variant.id.clone(), variant);
        Ok(())
    }

    /// Gain of `id` over its parent with a one-sided lower confidence bound (`z` normal quantile).
    /// Both must be scored by the same evaluator; after a rotation, re-score the parent first.
    pub fn improvement(&self, id: &str, z: f64) -> Result<Improvement, String> {
        let v = self.variants.get(id).ok_or("unknown variant")?;
        let p = self.variants.get(v.parent.as_deref().ok_or("a root has no parent to improve on")?).ok_or("parent missing")?;
        if p.evaluator != v.evaluator {
            return Err("scores come from different evaluators and are not comparable".into());
        }
        let min = self.evaluators[&v.evaluator].min_runs.max(2) as usize;
        if v.scores.len() < min || p.scores.len() < min {
            return Err(format!("need at least {min} runs on both sides, have {} and {}", v.scores.len(), p.scores.len()));
        }
        let ((mc, vc), (mp, vp)) = (mean_var(&v.scores), mean_var(&p.scores));
        let se = (vc / v.scores.len() as f64 + vp / p.scores.len() as f64).sqrt();
        Ok(Improvement { parent_mean: mp, mean: mc, delta: mc - mp, lower_bound: mc - mp - z * se, runs: (v.scores.len(), p.scores.len()) })
    }

    /// Ancestors of `id`, nearest first, ending at the root.
    pub fn ancestry(&self, id: &str) -> Vec<&Variant> {
        let mut out = Vec::new();
        let mut cur = self.variants.get(id);
        while let Some(v) = cur {
            out.push(v);
            cur = v.parent.as_ref().and_then(|p| self.variants.get(p));
        }
        out
    }

    /// Total cost on the path from the root to `id`: the budget spent reaching it.
    pub fn cost_to(&self, id: &str) -> u64 {
        self.ancestry(id).iter().map(|v| v.cost).sum()
    }

    /// Total cost of everything recorded, including abandoned branches. This is what the holdout has paid
    /// in queries, so it bounds how much the lineage could have overfit it.
    pub fn total_cost(&self) -> u64 {
        self.variants.values().map(|v| v.cost).sum()
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custodian() -> SigningKey {
        SigningKey::from_bytes(&[11; 32])
    }
    fn agent() -> SigningKey {
        SigningKey::from_bytes(&[12; 32])
    }

    fn items(n: usize) -> Vec<Vec<u8>> {
        (0..n).map(|i| format!("task-{i}").into_bytes()).collect()
    }

    fn evaluator(version: u64, from: Option<&Evaluator>, holdout: [u8; 32]) -> Evaluator {
        Evaluator::issue(
            EvaluatorSpec { name: "bench", version, harness: "aa", holdout, command: "make eval", protected: &["eval", "Makefile"], min_runs: 5, supersedes: from },
            &custodian(),
        )
        .unwrap()
    }

    const PATCH: &str = "--- a/src/agent.py\n+++ b/src/agent.py\n@@ -1 +1 @@\n-x\n+y\n";

    fn lineage() -> (Lineage, Evaluator, Variant) {
        let ev = evaluator(1, None, [1; 32]);
        let mut l = Lineage::new();
        l.add_evaluator(ev.clone());
        let root = Variant::sign(VariantSpec { parent: None, evaluator: &ev, patch: "", scores: &[0.50, 0.52, 0.49, 0.51, 0.50, 0.48], cost: 10 }, &agent()).unwrap();
        l.add(root.clone(), "").unwrap();
        (l, ev, root)
    }

    #[test]
    fn holdout_proofs_verify_for_every_size_and_reveal_only_their_item() {
        for n in 1..=9 {
            let h = Holdout::new([7; 32], items(n)).unwrap();
            let root = h.commit();
            for i in 0..n as u64 {
                let p = h.open(i).unwrap();
                assert!(p.verify(&root), "n={n} i={i}");
                let mut wrong = p.clone();
                wrong.item = b"task-evil".to_vec();
                assert!(!wrong.verify(&root), "swapped item must fail (n={n} i={i})");
            }
            assert!(h.open(n as u64).is_err());
        }
        let a = Holdout::new([7; 32], items(4)).unwrap().commit();
        assert_ne!(a, Holdout::new([8; 32], items(4)).unwrap().commit(), "salt changes the root");
        let h = Holdout::new([7; 32], items(4)).unwrap();
        let mut p = h.open(1).unwrap();
        p.index = 2;
        assert!(!p.verify(&h.commit()), "a proof cannot be moved to another index");
    }

    #[test]
    fn evaluator_needs_a_trusted_custodian_and_rotation_names_its_predecessor() {
        let e1 = evaluator(1, None, [1; 32]);
        assert!(Evaluator::open(&e1.text, &[agent().verifying_key().to_bytes()]).is_err(), "agent is not a custodian");
        let e2 = evaluator(2, Some(&e1), [2; 32]);
        assert_eq!(e2.supersedes.as_deref(), Some(e1.id.as_str()));
        assert_ne!(e1.id, e2.id);
    }

    #[test]
    fn real_gain_is_significant_and_noise_is_not() {
        let (mut l, ev, root) = lineage();
        let good = Variant::sign(VariantSpec { parent: Some(&root), evaluator: &ev, patch: PATCH, scores: &[0.60, 0.62, 0.59, 0.61, 0.60, 0.63], cost: 20 }, &agent()).unwrap();
        let noisy = Variant::sign(VariantSpec { parent: Some(&root), evaluator: &ev, patch: PATCH, scores: &[0.70, 0.30, 0.65, 0.35, 0.60, 0.45], cost: 20 }, &agent()).unwrap();
        l.add(good.clone(), PATCH).unwrap();
        l.add(noisy.clone(), PATCH).unwrap();
        let g = l.improvement(&good.id, Z_95).unwrap();
        assert!(g.significant() && g.delta > 0.09, "{g:?}");
        let n = l.improvement(&noisy.id, Z_95).unwrap();
        assert!(n.delta > 0.0 && !n.significant(), "higher mean but not supported: {n:?}");
        assert_eq!(l.total_cost(), 50);
        assert_eq!(l.cost_to(&good.id), 30);
        assert_eq!(l.ancestry(&good.id).len(), 2);
    }

    #[test]
    fn a_variant_cannot_edit_its_own_evaluator() {
        let (mut l, ev, root) = lineage();
        for bad in ["eval/grader.py", "Makefile", "./eval/data/x.json", "src/../eval/grader.py"] {
            let patch = format!("--- a/{bad}\n+++ b/{bad}\n@@ -1 +1 @@\n-x\n+y\n");
            let v = Variant::sign(VariantSpec { parent: Some(&root), evaluator: &ev, patch: &patch, scores: &[0.9; 6], cost: 1 }, &agent()).unwrap();
            assert!(l.add(v, &patch).unwrap_err().contains("protected"), "{bad}");
        }
        let rename = "rename from src/a.py\nrename to eval/a.py\n";
        let v = Variant::sign(VariantSpec { parent: Some(&root), evaluator: &ev, patch: rename, scores: &[0.9; 6], cost: 1 }, &agent()).unwrap();
        assert!(l.add(v, rename).is_err(), "rename into a protected directory");
    }

    #[test]
    fn host_computed_patch_must_match_what_was_signed() {
        let (mut l, ev, root) = lineage();
        let v = Variant::sign(VariantSpec { parent: Some(&root), evaluator: &ev, patch: PATCH, scores: &[0.6; 6], cost: 1 }, &agent()).unwrap();
        let evil = "--- a/eval/grader.py\n+++ b/eval/grader.py\n";
        assert!(l.add(v.clone(), evil).unwrap_err().contains("not the one"));
        l.add(v.clone(), PATCH).unwrap();
        assert!(l.add(v, PATCH).is_err(), "no duplicates");
    }

    #[test]
    fn swapping_the_evaluator_needs_a_rotation_and_breaks_comparability() {
        let (mut l, ev1, root) = lineage();
        let sneaky = evaluator(9, None, [9; 32]); // custodian-signed but does not supersede ev1
        l.add_evaluator(sneaky.clone());
        let v = Variant::sign(VariantSpec { parent: Some(&root), evaluator: &sneaky, patch: PATCH, scores: &[0.9; 6], cost: 1 }, &agent()).unwrap();
        assert!(l.add(v, PATCH).unwrap_err().contains("rotation"));

        let ev2 = evaluator(2, Some(&ev1), [2; 32]);
        l.add_evaluator(ev2.clone());
        let v = Variant::sign(VariantSpec { parent: Some(&root), evaluator: &ev2, patch: PATCH, scores: &[0.4; 6], cost: 1 }, &agent()).unwrap();
        l.add(v.clone(), PATCH).unwrap();
        assert!(l.improvement(&v.id, Z_95).unwrap_err().contains("not comparable"));
    }

    #[test]
    fn too_few_runs_unknown_parents_and_untrusted_evaluators_are_rejected() {
        let (mut l, ev, root) = lineage();
        let few = Variant::sign(VariantSpec { parent: Some(&root), evaluator: &ev, patch: PATCH, scores: &[0.9, 0.9], cost: 1 }, &agent()).unwrap();
        assert!(l.add(few, PATCH).unwrap_err().contains("runs"));
        let orphan = Variant::sign(VariantSpec { parent: Some(&Variant { id: "ff".into(), ..root.clone() }), evaluator: &ev, patch: PATCH, scores: &[0.9; 6], cost: 1 }, &agent()).unwrap();
        assert!(l.add(orphan, PATCH).unwrap_err().contains("parent"));
        let foreign = evaluator(1, None, [5; 32]);
        let v = Variant::sign(VariantSpec { parent: None, evaluator: &foreign, patch: "", scores: &[0.9; 6], cost: 1 }, &agent()).unwrap();
        assert!(l.add(v, "").unwrap_err().contains("trust"));
    }

    #[test]
    fn touched_paths_reads_diff_headers() {
        let p = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@\n--- /dev/null\n+++ b/new.txt\n";
        assert_eq!(touched_paths(p), vec!["new.txt".to_string(), "x".to_string()]);
    }
}
