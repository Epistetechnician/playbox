//! Delegated work: pulling, delegating, steering, pushing, verifying, auditing and revoking.

use crate::args::{b, req, s};
use crate::{Server, SHOWN_PATCH_BYTES};
use anyhow::{anyhow, bail, Context, Result};
use playbox_core::{platform, store, taint::Source};
use playbox_net::EndpointId;
use playbox_runner::TaskSpec;
use serde_json::{json, Value};
use std::path::PathBuf;

impl Server {
    pub(crate) async fn pull(&self, a: &Value) -> Result<Value> {
        let r = req(a, "ref")?;
        let ws = self.workspace()?;
        let (head, bundle) = self.guest()?.pull(r).await?;
        let dest = format!("refs/remotes/playbox/{r}");
        let src = format!("refs/heads/{r}");
        let got = match playbox_git::bundle_import(ws, &bundle, &src, &dest).await {
            Ok(h) => h,
            // The ref may be a tag or commit id rather than a branch.
            Err(_) => playbox_git::bundle_import(ws, &bundle, r, &dest).await?,
        };
        if b(a, "checkout") {
            playbox_git::checkout_branch(ws, r, &dest)
                .await
                .context("checkout failed")?;
        }
        Ok(
            json!({"ref": dest, "head": got, "host_head": head, "bytes": bundle.len(), "checked_out": b(a, "checkout")}),
        )
    }

    pub(crate) async fn delegate(&mut self, a: &Value) -> Result<Value> {
        let base = req(a, "base")?;
        let command = req(a, "command")?;
        let admission = self.admit("playbox_delegate", &[command], s(a, "capability"))?;
        let mut spec = TaskSpec::new(&["sh", "-c", command]);
        if let Some(i) = s(a, "image") {
            spec.image = i.to_string();
        }
        if let Some(v) = a.get("cpus").and_then(Value::as_f64) {
            spec.cpus = v as f32;
        }
        if let Some(v) = a.get("mem_mib").and_then(Value::as_u64) {
            spec.mem_mib = v as u32;
        }
        if let Some(v) = a.get("timeout_secs").and_then(Value::as_u64) {
            spec.timeout_secs = v;
        }
        spec.network = b(a, "network");
        if let Some(v) = a.get("allowed_hosts").and_then(Value::as_array) {
            spec.allowed_hosts = v
                .iter()
                .filter_map(|h| h.as_str().map(str::to_string))
                .collect();
        }
        if let Some(v) = a.get("secrets").and_then(Value::as_array) {
            spec.secrets = v
                .iter()
                .filter_map(|h| h.as_str().map(str::to_string))
                .collect();
        }
        let run = self
            .guest()?
            .run_task_with(base, spec, b(a, "warm"), b(a, "keep"))
            .await?;
        let (runner, out, patch) = (run.runner.clone(), run.output.clone(), run.patch.clone());
        // Everything a remote task printed or changed is untrusted text from now on.
        let origin = Source::untrusted(format!("task:{}", run.task_id));
        self.guard.tracker.register(&out.stdout, &origin);
        self.guard.tracker.register(&out.stderr, &origin);
        self.guard
            .tracker
            .register(&String::from_utf8_lossy(&patch), &origin);
        let mut applied = false;
        if b(a, "apply") && !patch.is_empty() {
            playbox_git::apply_patch(self.workspace()?, &patch)
                .await
                .context("applying patch")?;
            applied = true;
        }
        let shown =
            String::from_utf8_lossy(&patch[..patch.len().min(SHOWN_PATCH_BYTES)]).into_owned();
        Ok(json!({
            "runner": runner,
            "exit_code": out.exit_code,
            "timed_out": out.timed_out,
            "stdout": out.stdout,
            "stderr": out.stderr,
            "patch_bytes": patch.len(),
            "patch": shown,
            "patch_truncated": patch.len() > SHOWN_PATCH_BYTES,
            "patch_applied": applied,
            "task_id": run.task_id,
            "host_branch": run.branch,
            "host_commit": run.commit,
            "changed": run.changed,
            "admission": admission,
        }))
    }

    pub(crate) async fn steer(&mut self, a: &Value) -> Result<Value> {
        use playbox_core::steer::{Mode, Recipe, Site, SteerSpec, Tokens};
        let weights = playbox_core::record::unhex::<32>(req(a, "weights")?)
            .map_err(|e| anyhow!("weights: {e}"))?;
        let prompts: Vec<&str> = a
            .get("prompts")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("prompts must be a list of strings"))?
            .iter()
            .map(|p| {
                p.as_str()
                    .ok_or_else(|| anyhow!("prompts must be a list of strings"))
            })
            .collect::<Result<_>>()?;
        let admission = self.admit("playbox_steer", &prompts, s(a, "capability"))?;
        let layers: Vec<u32> = a
            .get("layers")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("layers must be a list of integers"))?
            .iter()
            .map(|l| {
                l.as_u64()
                    .and_then(|l| u32::try_from(l).ok())
                    .ok_or_else(|| anyhow!("layers must be non-negative integers"))
            })
            .collect::<Result<_>>()?;
        let file = req(a, "direction_file")?;
        let direction = std::fs::read(file).with_context(|| format!("reading {file}"))?;
        let mut set = serde_json::Map::new();
        set.insert("prompts".into(), json!(prompts));
        set.insert(
            "max_new_tokens".into(),
            json!(a
                .get("max_new_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(32)),
        );
        set.insert(
            "temperature".into(),
            json!(a.get("temperature").and_then(Value::as_f64).unwrap_or(0.0)),
        );
        let prompt_bytes = serde_json::to_vec(&Value::Object(set))?;
        let pick = |key: &str, default: &str| s(a, key).unwrap_or(default).to_string();
        let mode = pick("mode", "add");
        let coef = a.get("coef").and_then(Value::as_f64).unwrap_or(1.0);
        if !coef.is_finite() || coef.abs() > 100.0 {
            bail!("coef must be a finite number within 100 norms");
        }
        let spec = SteerSpec {
            model: pick("model", "unspecified"),
            weights,
            site: match pick("site", "resid_post").as_str() {
                "resid_post" => Site::ResidPost,
                "attn_out" => Site::AttnOut,
                "mlp_out" => Site::MlpOut,
                o => bail!("unknown site {o:?}"),
            },
            layers,
            direction: *blake3::hash(&direction).as_bytes(),
            recipe: match pick("recipe", "diffmean").as_str() {
                "diffmean" => Recipe::DiffMean,
                "probe" => Recipe::Probe,
                "sae_feature" => Recipe::SaeFeature,
                "reft_r1" => Recipe::ReftR1,
                o => bail!("unknown recipe {o:?}"),
            },
            mode: match mode.as_str() {
                "add" => Mode::Add,
                "clamp" => Mode::Clamp,
                "ablate" => Mode::Ablate,
                o => bail!("unknown mode {o:?}"),
            },
            coef_milli: if mode == "ablate" {
                0
            } else {
                (coef * 1000.0).round() as i64
            },
            tokens: match pick("tokens", "all").as_str() {
                "prompt" => Tokens::Prompt,
                "generated" => Tokens::Generated,
                "all" => Tokens::All,
                o => bail!("unknown tokens value {o:?}"),
            },
            prompts: *blake3::hash(&prompt_bytes).as_bytes(),
            seed: a.get("seed").and_then(Value::as_u64).unwrap_or(0),
        };
        let probe = b(a, "probe");
        let r = if probe {
            self.guest()?
                .steer_probe(&spec, &direction, &prompt_bytes)
                .await?
        } else {
            self.guest()?
                .steer(&spec, &direction, &prompt_bytes)
                .await?
        };
        // Model output produced by a host another member runs: untrusted from here on.
        self.guard.tracker.register(
            &r.output,
            &Source::untrusted(format!("steer:{}", r.task_id)),
        );
        let output: Value = serde_json::from_str(&r.output).unwrap_or(Value::Null);
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        Ok(json!({
            "kind": if probe { "probe" } else { "steer" },
            "output": output,
            "receipt": {
                "spec_digest": hex(&r.receipt.spec_digest),
                "weights": hex(&r.receipt.weights),
                "prompts": hex(&r.receipt.prompts),
                "image": hex(&r.receipt.image),
                "output_hash": hex(&r.receipt.output_hash),
                "host": hex(&r.receipt.host),
                "ceiling": format!("{:?}", r.receipt.ceiling),
                "time_ms": r.receipt.time_ms,
                "verified": "signed by the connected host and matches the spec and output bytes"
            },
            "receipt_record": r.receipt_text,
            "output_raw": b(a, "include_raw").then(|| r.output.clone()),
            "task_id": r.task_id,
            "admission": admission,
        }))
    }

    pub(crate) async fn push(&mut self, a: &Value) -> Result<Value> {
        let branch = req(a, "branch")?;
        let admission = self.admit("playbox_push", &[branch], s(a, "capability"))?;
        let bundle = playbox_git::bundle_create(self.workspace()?, branch).await?;
        let reference = self.guest()?.push(branch, &bundle).await?;
        Ok(json!({"stored_as": reference, "bytes": bundle.len(), "admission": admission}))
    }

    pub(crate) fn verify(&self, a: &Value) -> Result<Value> {
        let path = s(a, "path")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.log_path.clone());
        // Re-check platform evidence (Secure Enclave, TPM) against the pin store as it is now, exactly as
        // `playbox verify` does, so the history is judged the same way it was at join.
        let pins = platform::PinStore::load(&platform::pins_path(&store::home()))?;
        let vs = platform::verifiers(
            pins.clone(),
            platform::boot_reference_from_env().map_err(|e| anyhow!(e))?,
        )
        .map_err(|e| anyhow!(e))?;
        let refs: Vec<&dyn playbox_core::attest::PlatformVerifier> =
            vs.iter().map(|v| v.as_ref()).collect();
        let r = store::verify_deep(&path, &refs)?;
        let atts: Vec<Value> = r
            .attestations
            .iter()
            .map(|x| {
                let unpinned = x.device.as_deref().and_then(|d| d.split_once(':')).filter(|(k, fp)| !pins.contains(k, fp)).is_some();
                json!({"entry": x.seq, "about": x.about, "subject": x.subject, "claimed": x.claimed.to_string(), "verified": x.achieved.to_string(), "device": x.device, "device_pinned": x.device.is_some().then_some(!unpinned), "notes": x.notes})
            })
            .collect();
        Ok(json!({
            "valid": r.ok(),
            "entries": r.entries,
            "head": r.head,
            "path": path.display().to_string(),
            "error": r.chain_error,
            "problems": r.problems,
            "attestations": atts,
        }))
    }

    pub(crate) fn audit(&self, a: &Value) -> Result<Value> {
        let host = self
            .host
            .as_ref()
            .ok_or_else(|| anyhow!("this sidecar is not hosting a room"))?;
        let path = host.audit_path().ok_or_else(|| {
            anyhow!("this room keeps no audit log file (start it with a named room)")
        })?;
        let tail = a.get("tail").and_then(Value::as_u64).unwrap_or(20).min(200) as usize;
        let s = playbox_net::audit::verify(&path)?;
        let recent: Vec<String> = playbox_net::audit::tail(&path, tail)?
            .iter()
            .map(playbox_net::audit::describe)
            .collect();
        Ok(
            json!({"path": path.display().to_string(), "valid": true, "entries": s.entries, "head": s.head, "host_key": s.host_key, "recent": recent}),
        )
    }

    pub(crate) fn revoke(&mut self, a: &Value) -> Result<Value> {
        let member = req(a, "member")?;
        let admission = self.admit("playbox_revoke", &[member], s(a, "capability"))?;
        let host = self
            .host
            .as_ref()
            .ok_or_else(|| anyhow!("this sidecar is not hosting a room"))?;
        let id: EndpointId = req(a, "member")?
            .parse()
            .map_err(|e| anyhow!("not a member id: {e}"))?;
        Ok(json!({"revoked": host.revoke_member(&id), "admission": admission}))
    }
}
