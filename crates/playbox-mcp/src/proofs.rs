//! Receipts, checkpoints, holdouts, evaluator and evidence tools.

use crate::args::{b, req, s};
use crate::Server;
use anyhow::{anyhow, bail, Context, Result};
use playbox_core::{
    evaluator::{Evaluator, EvaluatorSpec, Holdout, ItemProof, Lineage, Variant, VariantSpec},
    evidence::{verify as verify_evidence, Expect, ScoreEvidence},
    receipt::{self, Level, Receipt, Trust},
    record::{hex, unhex},
    steer::Ceiling,
    store,
    witness::{self, Checkpoint},
};
use serde_json::{json, Value};
use std::path::PathBuf;

impl Server {
    pub(crate) fn receipts_check(&self, a: &Value) -> Result<Value> {
        let path = s(a, "log")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.log_path.clone());
        let log = store::load(&path).context("loading history log")?;
        let receipts = Self::strings(a, "receipts")
            .iter()
            .map(|t| Receipt::open(t))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow!("receipt: {e}"))?;
        let mut trust = Trust::default();
        if let Some(p) = a.get("providers").and_then(Value::as_object) {
            for (name, k) in p {
                trust = trust.provider(
                    name,
                    unhex::<32>(k.as_str().unwrap_or(""))
                        .map_err(|e| anyhow!("provider {name}: {e}"))?,
                );
            }
        }
        for k in Self::keys(a, "notaries")? {
            trust = trust.notary(k);
        }
        let c = receipt::coverage(&log, &receipts, &trust);
        let required = match s(a, "min") {
            Some(m) => {
                let min = Level::parse(m).map_err(|e| anyhow!(e))?;
                let (n, d) = (
                    a.get("numerator").and_then(Value::as_u64).unwrap_or(1) as usize,
                    a.get("denominator").and_then(Value::as_u64).unwrap_or(1) as usize,
                );
                Some(
                    receipt::require(&c, min, n, d.max(1))
                        .map(|_| "met".to_string())
                        .unwrap_or_else(|e| e.to_string()),
                )
            }
            None => None,
        };
        Ok(
            json!({"responses": c.responses, "provider_signed": c.provider_signed, "notarized": c.notarized, "self_claimed": c.self_claimed, "unbacked": c.unbacked, "mismatched": c.mismatched, "requirement": required}),
        )
    }

    pub(crate) fn checkpoint(&self) -> Result<Value> {
        let log = store::load(&self.log_path).context("loading history log")?;
        let cp = Checkpoint::sign(&log, &self.key, store::now_ms()).map_err(|e| anyhow!(e))?;
        Ok(
            json!({"checkpoint": cp.text, "size": cp.size, "head": hex(&cp.head), "log_key": hex(&cp.origin)}),
        )
    }

    pub(crate) fn checkpoint_verify(&self, a: &Value) -> Result<Value> {
        let m = a
            .get("m")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("m is required"))? as usize;
        let cp = witness::verify_quorum(
            req(a, "checkpoint")?,
            &Self::strings(a, "cosignatures"),
            &Self::keys(a, "witnesses")?,
            m,
        )
        .map_err(|e| anyhow!(e))?;
        Ok(json!({"ok": true, "size": cp.size, "head": hex(&cp.head), "log_key": hex(&cp.origin)}))
    }

    pub(crate) fn holdout_from(a: &Value, salt: [u8; 32]) -> Result<Holdout> {
        let items = Self::strings(a, "items")
            .into_iter()
            .map(String::into_bytes)
            .collect();
        Holdout::new(salt, items).map_err(|e| anyhow!(e))
    }

    pub(crate) fn holdout_commit(&self, a: &Value) -> Result<Value> {
        let salt = match s(a, "salt") {
            Some(h) => unhex::<32>(h).map_err(|e| anyhow!("salt: {e}"))?,
            None => playbox_net::random_bytes()?,
        };
        let h = Self::holdout_from(a, salt)?;
        Ok(
            json!({"root": hex(&h.commit()), "salt": hex(&salt), "items": Self::strings(a, "items").len()}),
        )
    }

    pub(crate) fn holdout_prove(&self, a: &Value) -> Result<Value> {
        let salt = unhex::<32>(req(a, "salt")?).map_err(|e| anyhow!("salt: {e}"))?;
        let index = a
            .get("index")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("index is required"))?;
        let p = Self::holdout_from(a, salt)?
            .open(index)
            .map_err(|e| anyhow!(e))?;
        Ok(json!({
            "index": p.index, "total": p.total, "blind": hex(&p.blind), "item": String::from_utf8_lossy(&p.item),
            "path": p.path.iter().map(|x| x.map(|h| hex(&h))).collect::<Vec<_>>(),
        }))
    }

    pub(crate) fn holdout_verify(&self, a: &Value) -> Result<Value> {
        let root = unhex::<32>(req(a, "root")?).map_err(|e| anyhow!("root: {e}"))?;
        let pr = a.get("proof").ok_or_else(|| anyhow!("proof is required"))?;
        let path = pr
            .get("path")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("proof.path must be a list"))?
            .iter()
            .map(|x| match x {
                Value::Null => Ok(None),
                Value::String(h) => unhex::<32>(h).map(Some).map_err(|e| anyhow!("path: {e}")),
                _ => Err(anyhow!("path entries are hex strings or null")),
            })
            .collect::<Result<Vec<_>>>()?;
        let proof = ItemProof {
            index: pr
                .get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow!("proof.index"))?,
            total: pr
                .get("total")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow!("proof.total"))?,
            blind: unhex::<32>(
                pr.get("blind")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("proof.blind"))?,
            )
            .map_err(|e| anyhow!("blind: {e}"))?,
            item: pr
                .get("item")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("proof.item"))?
                .as_bytes()
                .to_vec(),
            path,
        };
        Ok(json!({"valid": proof.verify(&root)}))
    }

    pub(crate) fn evaluator_issue(&self, a: &Value) -> Result<Value> {
        let own = [self.key.verifying_key().to_bytes()];
        let prior = s(a, "supersedes")
            .map(|t| Evaluator::open(t, &own).map_err(|e| anyhow!("supersedes: {e}")))
            .transpose()?;
        let protected = Self::strings(a, "protected");
        let protected: Vec<&str> = protected.iter().map(String::as_str).collect();
        let e = Evaluator::issue(
            EvaluatorSpec {
                name: req(a, "name")?,
                version: a
                    .get("version")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| anyhow!("version is required"))?,
                harness: req(a, "harness")?,
                holdout: unhex::<32>(req(a, "holdout")?).map_err(|e| anyhow!("holdout: {e}"))?,
                command: req(a, "command")?,
                protected: &protected,
                min_runs: a
                    .get("min_runs")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| anyhow!("min_runs is required"))?,
                supersedes: prior.as_ref(),
            },
            &self.key,
        )
        .map_err(|e| anyhow!(e))?;
        Ok(json!({"evaluator": e.text, "id": e.id, "custodian": hex(&e.custodian)}))
    }

    pub(crate) fn variant_sign(&self, a: &Value) -> Result<Value> {
        let custodians = Self::keys(a, "custodians")?;
        let evaluator = Evaluator::open(req(a, "evaluator")?, &custodians)
            .map_err(|e| anyhow!("evaluator: {e}"))?;
        let parent = s(a, "parent")
            .map(|t| Variant::open(t).map_err(|e| anyhow!("parent: {e}")))
            .transpose()?;
        let scores: Vec<f64> = a
            .get("scores")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("scores must be a list of numbers"))?
            .iter()
            .map(|x| {
                x.as_f64()
                    .filter(|f| f.is_finite())
                    .ok_or_else(|| anyhow!("scores must be finite numbers"))
            })
            .collect::<Result<_>>()?;
        let v = Variant::sign(
            VariantSpec {
                parent: parent.as_ref(),
                evaluator: &evaluator,
                patch: req(a, "patch")?,
                scores: &scores,
                cost: a
                    .get("cost")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| anyhow!("cost is required"))?,
            },
            &self.key,
        )
        .map_err(|e| anyhow!(e))?;
        Ok(json!({"variant": v.text, "id": v.id, "author": hex(&v.author)}))
    }

    pub(crate) fn evidence_sign(&self, a: &Value) -> Result<Value> {
        let own = [self.key.verifying_key().to_bytes()];
        let evaluator = Evaluator::open(req(a, "evaluator")?, &own)
            .map_err(|e| anyhow!("evaluator (you must be its custodian): {e}"))?;
        let variant = Variant::open(req(a, "variant")?).map_err(|e| anyhow!("variant: {e}"))?;
        if variant.evaluator != evaluator.id {
            bail!("the variant was scored under a different evaluator");
        }
        let receipts = Self::strings(a, "receipts");
        let refs: Vec<&str> = receipts.iter().map(String::as_str).collect();
        let text = ScoreEvidence::sign(&variant, &evaluator, &refs, &self.key)
            .map_err(|e| anyhow!("{e}"))?;
        Ok(json!({"evidence": text, "variant": variant.id, "runs": refs.len()}))
    }

    pub(crate) fn evidence_check(&self, a: &Value) -> Result<Value> {
        let custodians = Self::keys(a, "custodians")?;
        let evaluator = Evaluator::open(req(a, "evaluator")?, &custodians)
            .map_err(|e| anyhow!("evaluator: {e}"))?;
        let variant = Variant::open(req(a, "variant")?).map_err(|e| anyhow!("variant: {e}"))?;
        let hosts = Self::keys(a, "hosts")?;
        let opt32 = |k: &str| -> Result<Option<[u8; 32]>> {
            s(a, k)
                .map(|h| unhex::<32>(h).map_err(|e| anyhow!("{k}: {e}")))
                .transpose()
        };
        let expect = Expect {
            hosts: &hosts,
            weights: opt32("weights")?,
            image: opt32("image")?,
            min_ceiling: Ceiling::from_name(
                s(a, "min_ceiling").unwrap_or("local_internal_exploratory"),
            )
            .map_err(|e| anyhow!("min_ceiling: {e}"))?,
        };
        let receipts = Self::strings(a, "receipts");
        let refs: Vec<&str> = receipts.iter().map(String::as_str).collect();
        let b = verify_evidence(req(a, "evidence")?, &variant, &evaluator, &refs, &expect)
            .map_err(|e| anyhow!("{e}"))?;
        let hx = |v: &[[u8; 32]]| v.iter().map(|k| hex(k)).collect::<Vec<_>>();
        Ok(json!({
            "ok": true, "variant": variant.id, "runs": b.runs, "weights": hex(&b.weights), "image": hex(&b.image),
            "weakest_ceiling": b.weakest.as_str(), "hosts": hx(&b.hosts), "output_hashes": hx(&b.outputs),
        }))
    }

    pub(crate) fn lineage_check(&self, a: &Value) -> Result<Value> {
        let custodians = Self::keys(a, "custodians")?;
        let mut lineage = Lineage::new();
        for t in Self::strings(a, "evaluators") {
            lineage.add_evaluator(
                Evaluator::open(&t, &custodians).map_err(|e| anyhow!("evaluator: {e}"))?,
            );
        }
        let mut last = None;
        for (i, v) in a
            .get("variants")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("variants is required"))?
            .iter()
            .enumerate()
        {
            let variant =
                Variant::open(req(v, "variant")?).map_err(|e| anyhow!("variant {i}: {e}"))?;
            let id = variant.id.clone();
            lineage
                .add(
                    variant,
                    v.get("patch").and_then(Value::as_str).unwrap_or(""),
                )
                .map_err(|e| anyhow!("variant {i} refused: {e}"))?;
            last = Some(id);
        }
        let id = last.ok_or_else(|| anyhow!("no variants"))?;
        let z = a.get("z").and_then(Value::as_f64).unwrap_or(1.645);
        let paired = b(a, "paired");
        let imp = if paired {
            lineage.improvement_paired(&id, z)
        } else {
            lineage.improvement(&id, z)
        };
        Ok(json!({
            "last": id, "variants": lineage.len(), "cost_to_last": lineage.cost_to(&id), "total_cost": lineage.total_cost(),
            "improvement": imp.as_ref().ok().map(|i| json!({"parent_mean": i.parent_mean, "mean": i.mean, "delta": i.delta, "lower_bound": i.lower_bound, "significant": i.significant(), "paired": i.paired})),
            "improvement_error": imp.err().map(|e| e.to_string()),
        }))
    }
}
