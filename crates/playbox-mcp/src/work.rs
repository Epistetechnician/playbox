//! Delegated work: pulling, delegating, steering, pushing, verifying, auditing and revoking.

use crate::args::{b, req, s};
use crate::{Server, MAX_FULL_PATCH_BYTES, SHOWN_PATCH_BYTES};
use anyhow::{anyhow, bail, Context, Result};
use playbox_core::{platform, store, taint::Source};
use playbox_net::EndpointId;
use playbox_runner::{SyncMode, TaskSpec};
use serde_json::{json, Value};
use std::path::PathBuf;

impl Server {
    pub(crate) async fn pull(&mut self, a: &Value) -> Result<Value> {
        let r = req(a, "ref")?;
        let admission = self.admit("playbox_pull", &[r], s(a, "capability"))?;
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
            json!({"ref": dest, "head": got, "host_head": head, "bytes": bundle.len(), "checked_out": b(a, "checkout"), "admission": admission}),
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
        if let Some(v) = a.get("egress").and_then(Value::as_array) {
            spec.egress = v
                .iter()
                .filter_map(|h| h.as_str().map(str::to_string))
                .collect();
        }
        let keep = b(a, "keep");
        let sync = match s(a, "sync") {
            None | Some("full") => SyncMode::Full,
            Some("delta") => SyncMode::Delta,
            Some("none") => SyncMode::None,
            Some(other) => bail!("unknown sync mode {other:?}; use full, delta, or none"),
        };
        let mut h = blake3::Hasher::new();
        for arg in &spec.cmd {
            h.update(arg.as_bytes());
            h.update(&[0]);
        }
        let cmd_hash: String = h.finalize().to_hex().chars().take(16).collect();
        let offline_workspace_task = self.trust_offline_workspace
            && !spec.network
            && spec.allowed_hosts.is_empty()
            && spec.egress.is_empty();
        let run = self
            .guest()?
            .run_task_session(base, spec, b(a, "warm"), keep, sync)
            .await?;
        let (runner, out, patch) = (run.runner.clone(), run.output.clone(), run.patch.clone());
        let patch_hash = blake3::hash(&patch).to_hex().to_string();
        let stdout_hash = blake3::hash(out.stdout.as_bytes()).to_hex().to_string();
        // Everything a networked task (or any task when PLAYBOX_TRUST_OFFLINE_WORKSPACE is unset)
        // printed or changed is untrusted text from now on; isolated offline workspace tasks in
        // single-owner coding sessions do not self-taint benign own-repo code echoes.
        if !offline_workspace_task {
            let origin = Source::untrusted(format!("task:{}", run.task_id));
            self.guard.tracker.register(&out.stdout, &origin);
            self.guard.tracker.register(&out.stderr, &origin);
            self.guard
                .tracker
                .register(&String::from_utf8_lossy(&patch), &origin);
        }
        let mut applied = false;
        if b(a, "apply") && !patch.is_empty() {
            playbox_git::apply_patch(self.workspace()?, &patch)
                .await
                .context("applying patch")?;
            applied = true;
        }
        let patch_cap = if b(a, "full_patch") {
            MAX_FULL_PATCH_BYTES
        } else {
            SHOWN_PATCH_BYTES
        };
        let shown = String::from_utf8_lossy(&patch[..patch.len().min(patch_cap)]).into_owned();
        Ok(json!({
            "runner": runner,
            "exit_code": out.exit_code,
            "timed_out": out.timed_out,
            "stdout": out.stdout,
            "stderr": out.stderr,
            "patch_bytes": patch.len(),
            "patch": shown,
            "patch_truncated": patch.len() > patch_cap,
            "patch_applied": applied,
            "patch_hash": patch_hash,
            "stdout_hash": stdout_hash,
            "cmd_hash": cmd_hash,
            "policy_digest": out.policy_digest,
            "task_id": run.task_id,
            "base": base,
            "host_branch": run.branch,
            "host_commit": run.commit,
            "changed": run.changed,
            "keep": keep,
            "sync": sync.as_str(),
            "admission": admission,
        }))
    }

    pub(crate) async fn replay(&mut self, a: &Value) -> Result<Value> {
        let task_id = s(a, "task_id");
        let run = b(a, "run");
        let mut res = if self.client.is_some() {
            self.guest()?.replay(task_id, run).await?
        } else if let Some(host) = &self.host {
            host.replay(task_id, run).await?
        } else {
            bail!("not in a room: call playbox_join or playbox_create first");
        };
        // Register untrusted text from the inspected/replayed task.
        let origin = Source::untrusted(format!("replay:{}", task_id.unwrap_or("list")));
        let patch_text = res
            .pointer("/replay/patch")
            .or_else(|| res.pointer("/task/patch"))
            .or_else(|| res.pointer("/original/patch"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if !patch_text.is_empty() {
            self.guard.tracker.register(&patch_text, &origin);
        }
        for ptr in ["/replay/stdout", "/replay/stderr"] {
            if let Some(txt) = res.pointer(ptr).and_then(Value::as_str) {
                self.guard.tracker.register(txt, &origin);
            }
        }
        if b(a, "apply") && !patch_text.is_empty() {
            playbox_git::apply_patch(self.workspace()?, patch_text.as_bytes())
                .await
                .context("applying replay patch")?;
            if let Some(obj) = res.as_object_mut() {
                obj.insert("patch_applied".into(), json!(true));
            }
        }
        Ok(res)
    }

    pub(crate) async fn handoff(&mut self, a: &Value) -> Result<Value> {
        let summary = req(a, "summary")?.to_string();
        let str_list = |k: &str| -> Vec<String> {
            a.get(k)
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut branch = s(a, "branch").unwrap_or("main").to_string();
        let mut base_commit = s(a, "base_commit").unwrap_or("").to_string();
        let mut pushed_ref: Option<String> = None;
        if let Some(ws) = &self.workspace {
            if base_commit.is_empty() {
                if let Ok(head) = playbox_git::rev_parse(ws, "HEAD").await {
                    base_commit = head;
                }
            }
            if b(a, "push") && self.client.is_some() {
                let bundle = playbox_git::bundle_create(ws, &branch).await?;
                let stored = self.guest()?.push(&branch, &bundle).await?;
                pushed_ref = Some(stored.clone());
                branch = stored;
            }
        } else if let Some(repo) = &self.host_repo {
            if base_commit.is_empty() {
                if let Ok(head) = playbox_git::rev_parse(repo, &branch).await {
                    base_commit = head;
                } else if let Ok(head) = playbox_git::rev_parse(repo, "HEAD").await {
                    base_commit = head;
                }
            }
        }
        let policy_snap = if let Some(host) = &self.host {
            Some(host.policy_snapshot())
        } else if let Ok(g) = self.guest() {
            g.policy().await.ok()
        } else {
            None
        };
        let mut budget_remaining = a
            .get("budget_remaining")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if let (Some(obj), Some(ps)) = (budget_remaining.as_object_mut(), &policy_snap) {
            if let Some(b_obj) = ps.get("budget").and_then(Value::as_object) {
                for (k, v) in b_obj {
                    obj.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        }
        let policy_digest = s(a, "policy_digest")
            .map(str::to_string)
            .or_else(|| {
                policy_snap
                    .as_ref()
                    .and_then(|p| p.get("policy_digest"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let cp = playbox_net::card::HandoffCheckpoint {
            id: s(a, "id").unwrap_or("").to_string(),
            room: String::new(),
            from: String::new(),
            to: s(a, "to").unwrap_or("").to_string(),
            summary,
            base_commit,
            branch,
            open_cards: str_list("open_cards"),
            verified_artifacts: str_list("verified_artifacts"),
            failed_checks: str_list("failed_checks"),
            budget_remaining,
            policy_digest,
            issued_ms: store::now_ms(),
            sig: String::new(),
        };
        let mut out = if self.client.is_some() {
            self.guest()?.checkpoint_put(cp).await?
        } else if let Some(host) = &self.host {
            host.checkpoint_put(cp)?
        } else {
            bail!("not in a room: call playbox_join or playbox_create first");
        };
        if let (Some(obj), Some(pr)) = (out.as_object_mut(), pushed_ref) {
            obj.insert("pushed_ref".into(), json!(pr));
        }
        Ok(out)
    }

    pub(crate) async fn handoffs(&mut self, a: &Value) -> Result<Value> {
        let id = s(a, "id");
        let resume = b(a, "resume");
        let target_id = if resume && id.is_none() {
            Some("latest")
        } else {
            id
        };
        let mut res = if self.client.is_some() {
            self.guest()?.checkpoint_list(target_id).await?
        } else if let Some(host) = &self.host {
            host.checkpoint_list(target_id)
        } else {
            bail!("not in a room: call playbox_join or playbox_create first");
        };
        let me = self.member_id.clone().unwrap_or_default();
        for cp in res
            .get("checkpoints")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let from = cp.get("from").and_then(Value::as_str).unwrap_or("");
            if from != me {
                let cid = cp.get("id").and_then(Value::as_str).unwrap_or("handoff");
                let origin = Source::untrusted(format!("handoff:{cid}"));
                if let Some(sum) = cp.get("summary").and_then(Value::as_str) {
                    self.guard.tracker.register(sum, &origin);
                }
                for fc in cp
                    .get("failed_checks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    self.guard.tracker.register(fc, &origin);
                }
            }
        }
        if resume {
            let first = res
                .get("checkpoints")
                .and_then(Value::as_array)
                .and_then(|arr| arr.first())
                .cloned()
                .ok_or_else(|| anyhow!("no matching handoff checkpoint to resume"))?;
            let branch = first
                .get("branch")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    first
                        .get("base_commit")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or("main");
            if self.client.is_some() && self.workspace.is_some() {
                let pulled = self
                    .pull(&json!({ "ref": branch, "checkout": true }))
                    .await?;
                if let Some(obj) = res.as_object_mut() {
                    obj.insert(
                        "resumed".into(),
                        json!({
                            "checkpoint_id": first.get("id"),
                            "branch": branch,
                            "base_commit": first.get("base_commit"),
                            "pull": pulled,
                        }),
                    );
                }
            } else if let Some(obj) = res.as_object_mut() {
                obj.insert(
                    "resumed".into(),
                    json!({
                        "checkpoint_id": first.get("id"),
                        "branch": branch,
                        "base_commit": first.get("base_commit"),
                    }),
                );
            }
        }
        Ok(res)
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
