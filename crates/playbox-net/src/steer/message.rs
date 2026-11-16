//! The room handler side of `steer` and `handoff` messages.

use super::files::{decision_body, hex, read_output, PromptSet, Scratch};
use super::host::SteerHost;
use super::{GUARDS_AT, MAX_DIRECTION_BYTES, MAX_HANDOFF_BYTES, MAX_PROMPT_BYTES, WEIGHTS_AT};
use crate::sync::LockExt;
use crate::{
    grants::{Needs, SteerNeeds},
    wire::{self},
    EndpointId, RoomHandler,
};
use a2a::{Message, SendMessageResponse, Task, TaskState};
use anyhow::{anyhow, Result};
use playbox_core::steer::{Ceiling, Mode, SteerReceipt, SteerSpec};
use playbox_runner::{Mount, TaskSpec};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

impl RoomHandler {
    /// Handle a `steer` or `steer_probe` message. The route has already checked that the grant has the
    /// right; this authorizes the specifics and runs.
    pub(crate) async fn steer_message(
        &self,
        from: EndpointId,
        grant: Option<&str>,
        text: String,
        probe: bool,
        msg: &Message,
    ) -> Result<Value, a2a::JsonRpcError> {
        let t0 = Instant::now();
        let prepared = match self.steer_prepare(from, grant, &text, probe, msg) {
            Ok(p) => p,
            Err(Prep::Unauthorized(e)) => {
                self.audit.record(
                    "steer_denied",
                    Some(&from.to_string()),
                    json!({ "reason": crate::audit::clip(&format!("{e:#}")) }),
                );
                return Err(crate::unauthorized(e));
            }
            Err(Prep::Refused(e)) => {
                let task = crate::finish(Err(e));
                self.remember(from, &task);
                return serde_json::to_value(SendMessageResponse::Task(task))
                    .map_err(|e| a2a::A2AError::internal(e.to_string()).to_jsonrpc_error());
            }
        };
        let details = json!({
            "kind": if probe { "probe" } else { "run" },
            "model": crate::audit::clip(&prepared.spec.model),
            "weights": hex(&prepared.spec.weights)[..16].to_string(),
            "spec": hex(&prepared.spec.digest().unwrap_or([0; 32]))[..16].to_string(),
            "tokens": prepared.tokens,
        });
        let r = self.steer_run(from, prepared).await;
        self.audit_op(from, "steer", &r, t0, details);
        let task = crate::finish(r);
        self.remember(from, &task);
        serde_json::to_value(SendMessageResponse::Task(task))
            .map_err(|e| a2a::A2AError::internal(e.to_string()).to_jsonrpc_error())
    }

    /// Handle a `handoff` message: check a signed KV-cache record from `from` against a hosted model, with a
    /// random recompute audit. Needs the `handoff` right (issued with steering) and a hosted model.
    pub(crate) async fn handoff_message(
        &self,
        from: EndpointId,
        weights: String,
        msg: &Message,
    ) -> Result<Value, a2a::JsonRpcError> {
        let t0 = Instant::now();
        let r = self.handoff_run(from, &weights, msg).await;
        let details = json!({ "weights": crate::audit::clip(&weights) });
        self.audit_op(from, "handoff", &r, t0, details);
        let task = crate::finish(r);
        self.remember(from, &task);
        serde_json::to_value(SendMessageResponse::Task(task))
            .map_err(|e| a2a::A2AError::internal(e.to_string()).to_jsonrpc_error())
    }

    async fn handoff_run(&self, from: EndpointId, weights: &str, msg: &Message) -> Result<Task> {
        let host = self
            .cfg
            .steer
            .clone()
            .ok_or_else(|| crate::refuse("this room does not host steering"))?;
        let runner = self
            .cfg
            .runner
            .clone()
            .ok_or_else(|| crate::refuse("this room runs no tasks"))?;
        if host.quarantined.lock_recover().contains(&from) {
            return Err(crate::refuse(
                "you are quarantined: an earlier handoff of yours failed the audit",
            ));
        }
        let weights_dir = host
            .weights
            .get(weights)
            .ok_or_else(|| crate::refuse(format!("this host does not serve model {weights}")))?
            .clone();
        let parts = wire::raw_parts(msg);
        let [data] = parts.as_slice() else {
            return Err(crate::refuse(
                "a handoff message needs exactly one raw part",
            ));
        };
        if data.is_empty() || data.len() > MAX_HANDOFF_BYTES {
            return Err(crate::refuse("the handoff is empty or too large"));
        }
        let _permit = self.tasks.acquire().await?;
        let scratch = Scratch::new()?;
        let dir = &scratch.0;
        std::fs::write(dir.join("handoff.bin"), data)?;
        std::fs::write(dir.join("signer.txt"), hex(from.as_bytes()))?;
        let mounts = vec![Mount {
            host: std::fs::canonicalize(&weights_dir)?,
            container: WEIGHTS_AT.into(),
        }];
        let mut task = TaskSpec::new(&[]);
        task.cmd = [
            "python",
            "-m",
            "playbox_steer",
            "handoff",
            "--weights",
            WEIGHTS_AT,
            "--task",
            "/workspace",
            "--expected",
            weights,
            "--audit",
            &host.audit_prob.to_string(),
            "--out",
            "/workspace/out.json",
        ]
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
        task.image = host.image.clone();
        task.cpus = host.limits.cpus;
        task.mem_mib = host.limits.mem_mib;
        task.timeout_secs = host.limits.timeout_secs;
        let output = tokio::select! {
            r = runner.run_with_mounts(&task, dir, &mounts) => r?,
            why = playbox_runner::quota_exceeded(dir, self.cfg.scratch_quota) => return Err(why),
            () = self.until_banned(from) => return Err(crate::refuse("cancelled: your access to this room was revoked")),
        };
        let out = read_output(&dir.join("out.json"));
        match (output.exit_code, out) {
            (Some(code @ (0 | 5)), Ok(bytes)) => {
                if code == 5
                    && serde_json::from_slice::<Value>(&bytes)?["quarantine"] == json!(true)
                {
                    host.quarantine(from);
                }
                let state = if code == 0 {
                    TaskState::Completed
                } else {
                    TaskState::Failed
                };
                let v: Value = serde_json::from_slice(&bytes)?;
                let body = decision_body(
                    self.attest_key.verifying_key().as_bytes(),
                    from.as_bytes(),
                    weights,
                    data,
                    code == 0,
                    v["audited"] == json!(true),
                );
                let sig = ed25519_dalek::Signer::sign(&*self.attest_key, body.as_bytes());
                let receipt = format!("{body}sig {}", hex(&sig.to_bytes()));
                Ok(wire::task(
                    state,
                    (code == 5).then(|| "the handoff was rejected".to_string()),
                    vec![
                        wire::text_artifact(
                            "decision",
                            String::from_utf8_lossy(&bytes).into_owned(),
                        ),
                        wire::text_artifact("receipt", receipt),
                    ],
                    vec![("kind", json!("handoff"))],
                ))
            }
            (code, _) => Err(anyhow!(
                "handoff check failed (exit {code:?}): {}",
                output
                    .stderr
                    .lines()
                    .rev()
                    .take(4)
                    .collect::<Vec<_>>()
                    .join(" | ")
            )),
        }
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "the value is bounded, or truncated on purpose"
    )]
    fn steer_prepare(
        &self,
        from: EndpointId,
        grant: Option<&str>,
        text: &str,
        probe: bool,
        msg: &Message,
    ) -> Result<Prepared, Prep> {
        let host = self
            .cfg
            .steer
            .clone()
            .ok_or_else(|| Prep::Refused(crate::refuse("this room does not host steering")))?;
        let refuse = |m: String| Prep::Refused(crate::refuse(m));
        let (spec, signer) = SteerSpec::open(text).map_err(|e| refuse(format!("bad spec: {e}")))?;
        if &signer != from.as_bytes() {
            return Err(refuse(
                "the spec must be signed by the key that sends it".into(),
            ));
        }
        let parts = wire::raw_parts(msg);
        let [direction, prompts] = parts.as_slice() else {
            return Err(refuse(
                "a steer message needs two raw parts: the direction file, then the prompt set"
                    .into(),
            ));
        };
        if direction.is_empty() || direction.len() > MAX_DIRECTION_BYTES || direction.len() % 4 != 0
        {
            return Err(refuse(
                "the direction must be a non-empty float32 array under 1 MiB".into(),
            ));
        }
        if prompts.len() > MAX_PROMPT_BYTES {
            return Err(refuse("the prompt set is too large".into()));
        }
        if blake3::hash(direction).as_bytes() != &spec.direction {
            return Err(refuse(
                "the direction file does not hash to the spec's direction".into(),
            ));
        }
        if blake3::hash(prompts).as_bytes() != &spec.prompts {
            return Err(refuse(
                "the prompt set does not hash to the spec's prompts".into(),
            ));
        }
        let ps = PromptSet::parse(prompts).map_err(|e| refuse(format!("{e:#}")))?;
        let weights_hex = hex(&spec.weights);
        let weights_dir = host
            .weights
            .get(&weights_hex)
            .ok_or_else(|| refuse(format!("this host does not serve model {weights_hex}")))?
            .clone();

        let (Some(&first), Some(&last)) = (spec.layers.first(), spec.layers.last()) else {
            return Err(refuse("the steering spec names no layers".to_string()));
        };
        let (lo, hi) = (i64::from(first), i64::from(last));
        let op = if probe { "steer_probe" } else { "steer" };
        let tokens = if probe { 0 } else { ps.tokens() };
        let l = &host.limits;
        let needs = Needs {
            cpu_milli: (l.cpus * 1000.0).ceil() as i64,
            mem_mib: i64::from(l.mem_mib),
            timeout_secs: i64::try_from(l.timeout_secs).unwrap_or(i64::MAX),
            steer: Some(SteerNeeds {
                model: weights_hex.clone(),
                layer_lo: lo,
                layer_hi: hi,
                coef_abs_milli: spec.coef_milli.abs(),
                mode: mode_name(spec.mode).into(),
                direction: hex(&spec.direction),
                tokens: i64::try_from(tokens).unwrap_or(i64::MAX),
            }),
            ..Needs::default()
        };
        self.authorize(grant, from, op, &needs)
            .map_err(|e| Prep::Unauthorized(anyhow!("{}", e.message)))?;

        // Which guard classes this member may not steer along: every class the host defines that the grant
        // does not name.
        let mut deny = Vec::new();
        if let Some((_, classes)) = host.guards.get(&weights_hex) {
            for c in classes {
                let q = Needs {
                    class: Some(c.clone()),
                    ..Needs::default()
                };
                if self.authorize(grant, from, "steer_class", &q).is_err() {
                    deny.push(c.clone());
                }
            }
        }
        host.charge(from, tokens)
            .map_err(|e| refuse(format!("{e:#}")))?;
        Ok(Prepared {
            host,
            spec,
            probe,
            direction: direction.to_vec(),
            prompts: prompts.to_vec(),
            weights_dir,
            deny,
            tokens,
        })
    }

    async fn steer_run(&self, from: EndpointId, p: Prepared) -> Result<Task> {
        let runner = self
            .cfg
            .runner
            .clone()
            .ok_or_else(|| crate::refuse("this room runs no tasks"))?;
        let Prepared {
            host,
            spec,
            probe,
            direction,
            prompts,
            weights_dir,
            deny,
            tokens: _,
        } = p;
        let _permit = self.tasks.acquire().await?;
        let scratch = Scratch::new()?;
        let dir = &scratch.0;
        std::fs::write(
            dir.join("spec.txt"),
            spec.canonical_body().map_err(|e| anyhow!(e))?,
        )?;
        std::fs::write(dir.join("direction.f32"), &direction)?;
        std::fs::write(dir.join("prompts.json"), &prompts)?;

        let mut mounts = vec![Mount {
            host: std::fs::canonicalize(&weights_dir)?,
            container: WEIGHTS_AT.into(),
        }];
        let mut cmd: Vec<String> = [
            "python",
            "-m",
            "playbox_steer",
            if probe { "probe" } else { "run" },
            "--weights",
            WEIGHTS_AT,
            "--task",
            "/workspace",
            "--out",
            "/workspace/out.json",
        ]
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
        if !probe {
            if let Some((gdir, _)) = host.guards.get(&hex(&spec.weights)) {
                mounts.push(Mount {
                    host: std::fs::canonicalize(gdir)?,
                    container: GUARDS_AT.into(),
                });
                cmd.extend([
                    "--guards".into(),
                    GUARDS_AT.into(),
                    "--deny".into(),
                    deny.join(","),
                ]);
            }
        }
        let mut task = TaskSpec::new(&[]);
        task.cmd = cmd;
        task.image = host.image.clone();
        task.cpus = host.limits.cpus;
        task.mem_mib = host.limits.mem_mib;
        task.timeout_secs = host.limits.timeout_secs;

        let image = runner
            .image_digest(&host.image)
            .await
            .map_err(|e| crate::refuse(format!("cannot identify the engine image: {e:#}")))?;
        let output = tokio::select! {
            r = runner.run_with_mounts(&task, dir, &mounts) => r?,
            why = playbox_runner::quota_exceeded(dir, self.cfg.scratch_quota) => return Err(why),
            () = self.until_banned(from) => return Err(crate::refuse("cancelled: your access to this room was revoked")),
        };

        let kind = if probe { "probe" } else { "run" };
        let out_json = read_output(&dir.join("out.json"));
        let meta = |extra: Vec<(&'static str, Value)>| {
            let mut m = vec![
                ("runner", json!(runner.name())),
                ("specDigest", json!(hex(&spec.digest().unwrap_or([0; 32])))),
                ("kind", json!(kind)),
            ];
            if let Some(c) = output.exit_code {
                m.push(("exitCode", json!(c)));
            }
            m.extend(extra);
            m
        };
        let tail = |s: &str| {
            s.lines()
                .rev()
                .take(6)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        };
        match (output.exit_code, out_json) {
            (Some(0), Ok(bytes)) => {
                let request = format!(
                    "{}kind {kind}\n",
                    spec.canonical_body().map_err(|e| anyhow!(e))?
                );
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis().try_into().unwrap_or(u64::MAX))
                    .unwrap_or(0);
                let receipt = SteerReceipt::sign(
                    &spec,
                    image,
                    request.as_bytes(),
                    &bytes,
                    host.ceiling,
                    now,
                    &self.attest_key,
                )
                .map_err(|e| anyhow!(e))?;
                let m = meta(vec![
                    ("ceiling", json!(ceiling_name(host.ceiling))),
                    ("imageDigest", json!(hex(&image))),
                ]);
                Ok(wire::task(
                    TaskState::Completed,
                    None,
                    vec![
                        wire::text_artifact("output", String::from_utf8_lossy(&bytes).into_owned()),
                        wire::text_artifact("receipt", receipt),
                    ],
                    m,
                ))
            }
            // A failed gate still has an output that says which one.
            (Some(4), Ok(bytes)) => Ok(wire::task(
                TaskState::Failed,
                Some("a pre-flight gate failed; no steered text was produced".into()),
                vec![wire::text_artifact(
                    "output",
                    String::from_utf8_lossy(&bytes).into_owned(),
                )],
                meta(vec![]),
            )),
            (Some(3), _) => Err(crate::refuse(format!(
                "refused by a safety guard: {}",
                tail(&output.stderr)
            ))),
            (Some(2), _) => Err(crate::refuse(format!(
                "the engine rejected the request: {}",
                tail(&output.stderr)
            ))),
            (code, _) if output.timed_out => Err(anyhow!(
                "steering timed out after {}s (exit {code:?})",
                host.limits.timeout_secs
            )),
            (code, out) => Err(anyhow!(
                "steering failed (exit {code:?}, output {}): {}",
                if out.is_ok() { "present" } else { "missing" },
                tail(&output.stderr)
            )),
        }
    }

    /// Resolves once `who` is banned, here or by a notice that reached this host.
    async fn until_banned(&self, who: EndpointId) {
        loop {
            if self.state.lock_recover().banned.contains(&who) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

pub(super) enum Prep {
    Unauthorized(anyhow::Error),
    Refused(anyhow::Error),
}

pub(super) struct Prepared {
    host: std::sync::Arc<SteerHost>,
    spec: SteerSpec,
    probe: bool,
    direction: Vec<u8>,
    prompts: Vec<u8>,
    weights_dir: PathBuf,
    deny: Vec<String>,
    tokens: u64,
}

pub(super) fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Add => "add",
        Mode::Clamp => "clamp",
        Mode::Ablate => "ablate",
    }
}

pub(super) fn ceiling_name(c: Ceiling) -> &'static str {
    match c {
        Ceiling::HostClaim => "host_claim",
        Ceiling::LocalInternalExploratory => "local_internal_exploratory",
        Ceiling::Attested => "attested",
    }
}
