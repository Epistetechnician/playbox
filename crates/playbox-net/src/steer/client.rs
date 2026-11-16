//! Client calls for `steer`, `steer_probe` and `handoff`.

use super::files::{unhex32, verify_decision};
use crate::{
    wire::{self, Op},
    Client,
};
use a2a::Part;
use anyhow::{anyhow, bail, Result};
use ed25519_dalek::SigningKey;
use playbox_core::steer::{SteerReceipt, SteerSpec};
use serde_json::{json, Value};

/// What a client gets back from a completed steer run.
#[derive(Debug, Clone)]
pub struct SteerResult {
    /// The engine's JSON output, exactly the bytes the receipt covers.
    pub output: String,
    /// The host's receipt, already checked against the spec, the output and the host's key.
    pub receipt: SteerReceipt,
    /// The receipt record exactly as the host signed it: what an evidence record's digest covers.
    pub receipt_text: String,
    pub task_id: String,
}

impl Client {
    /// Hand the host a signed handoff envelope (`PBH1`) for the hosted model `weights` (hex manifest hash).
    /// Returns the host's decision JSON; `Err` if the host refused or the cache was rejected.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn handoff(&self, weights: &str, envelope: &[u8]) -> Result<Value> {
        let op = Op::Handoff {
            weights: weights.to_string(),
        };
        let t = self
            .send_task("handoff", &op, vec![Part::raw(envelope.to_vec())])
            .await?;
        let d = wire::artifact_text(&t, "decision")
            .ok_or_else(|| anyhow!("the host sent no decision"))?;
        let receipt = wire::artifact_text(&t, "receipt")
            .ok_or_else(|| anyhow!("the host sent no signed decision"))?;
        let me = self.endpoint.id();
        let accepted = verify_decision(
            &receipt,
            self.host_id().as_bytes(),
            me.as_bytes(),
            weights,
            envelope,
        )?;
        let v: Value = serde_json::from_str(&d)?;
        if !accepted || v["accepted"] != json!(true) {
            bail!("handoff rejected: {}", v["reasons"]);
        }
        Ok(v)
    }

    /// Sign `spec` with this client's key and run it on the host. `direction` is the unit-norm float32
    /// file and `prompts` the prompt-set JSON; both must hash to what `spec` names.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn steer(
        &self,
        spec: &SteerSpec,
        direction: &[u8],
        prompts: &[u8],
    ) -> Result<SteerResult> {
        self.steer_op(spec, direction, prompts, false).await
    }

    /// Read-only: project the prompts' activations onto the direction.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub async fn steer_probe(
        &self,
        spec: &SteerSpec,
        direction: &[u8],
        prompts: &[u8],
    ) -> Result<SteerResult> {
        self.steer_op(spec, direction, prompts, true).await
    }

    async fn steer_op(
        &self,
        spec: &SteerSpec,
        direction: &[u8],
        prompts: &[u8],
        probe: bool,
    ) -> Result<SteerResult> {
        let key = SigningKey::from_bytes(&self.endpoint.secret_key().to_bytes());
        let text = spec.sign(&key).map_err(|e| anyhow!(e))?;
        let op = Op::Steer { spec: text, probe };
        let extra = vec![Part::raw(direction.to_vec()), Part::raw(prompts.to_vec())];
        let t = self.send_task("steer", &op, extra).await?;
        let output =
            wire::artifact_text(&t, "output").ok_or_else(|| anyhow!("the host sent no output"))?;
        let receipt_text = wire::artifact_text(&t, "receipt")
            .ok_or_else(|| anyhow!("the host sent no receipt"))?;
        let receipt = SteerReceipt::open(&receipt_text).map_err(|e| anyhow!("bad receipt: {e}"))?;
        if &receipt.host != self.host_id().as_bytes() {
            bail!("the receipt is signed by a key other than the host this client is connected to");
        }
        receipt
            .matches(spec, output.as_bytes())
            .map_err(|e| anyhow!("{e}"))?;
        let _ = unhex32; // kept for callers that decode hashes from task metadata
        Ok(SteerResult {
            output,
            receipt,
            receipt_text,
            task_id: t.id,
        })
    }
}
