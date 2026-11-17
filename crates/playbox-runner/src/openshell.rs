//! The NVIDIA `OpenShell` runner: each task gets its own `OpenShell` sandbox, driven through the
//! `openshell` CLI (<https://github.com/NVIDIA/OpenShell>).
//!
//! How a task runs:
//! 1. A policy file is generated per task: Landlock filesystem rules (`/sandbox` and `/tmp`
//!    writable, system paths read-only), non-root user, and no `network_policies`, so the
//!    sandbox's default-deny network applies.
//! 2. `openshell sandbox create --policy ... --detach` starts a sandbox from the (host-allowed) image.
//! 3. The workspace goes in as a tar stream on `exec`'s stdin (`create --upload` cannot be combined with
//!    a command, nests directories, and `download` needs SSH; `exec` takes at most 4 MiB on stdin, so a larger
//!    tar goes in as several chunks), the command runs with `exec`, and its exit code is the task's.
//! 4. The workspace comes back as a tar stream, is unpacked into an empty directory and mirrored over
//!    the host workspace; the sandbox is then deleted.
//!
//! Known gaps, deliberately not papered over:
//! - `OpenShell` policies carry no CPU, memory or timeout limits. The flags are passed, but its VM
//!   driver ignores them, so `preflight` fails closed unless `PLAYBOX_ALLOW_MISSING_LIMITS` is set
//!   (same rule as the Docker runner). Timeouts are enforced here by killing the CLI and deleting
//!   the sandbox.
//! - Network is per host: a task's `allowed_hosts` become `network_policies`; unrestricted network is refused.
//! - Isolation is reported as `Container`, the weakest claim that holds across its drivers.
//!   Tested against `OpenShell` 0.1.6 with the local VM driver (`tests/openshell.rs`, needs a gateway).

use super::{check_spec, run_child, Isolation, Runner, SandboxPolicy, TaskOutput, TaskSpec};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};

const BIN: &str = "openshell";
const SETUP_TIMEOUT: Duration = Duration::from_secs(180);

pub struct OpenShellRunner {
    policy: SandboxPolicy,
    /// `openshell --version`, read by `preflight`.
    version: std::sync::OnceLock<String>,
    /// One warm sandbox per session key (see [`Runner::run_warm`]).
    warm: tokio::sync::Mutex<std::collections::HashMap<String, Warm>>,
}

/// A sandbox kept running between one session's tasks.
struct Warm {
    name: String,
    /// Image, resources and policy digest it was created with; a task that differs gets a new sandbox.
    shape: String,
    idle_since: std::time::Instant,
}

/// A warm sandbox idle this long is deleted, and at most this many are kept (the oldest goes first).
const WARM_IDLE: Duration = Duration::from_secs(120);
const WARM_MAX: usize = 8;
const WARM_PREFIX: &str = "pbxw-";

impl OpenShellRunner {
    #[must_use]
    pub fn new(policy: SandboxPolicy) -> Self {
        Self {
            policy,
            version: std::sync::OnceLock::new(),
            warm: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }
}

/// A destination parsed from `host` or `host:port` (port 443 by default). Strict, because the values
/// are written into a YAML policy: anything outside plain DNS names and IPs is refused, so a peer cannot
/// inject policy text or a wildcard.
fn parse_host(h: &str) -> Result<(String, u16)> {
    let (name, port) = match h.rsplit_once(':') {
        Some((n, p)) => (
            n,
            p.parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .with_context(|| format!("bad port in {h:?}"))?,
        ),
        None => (h, 443),
    };
    let ok = !name.is_empty()
        && name.len() <= 253
        && name
            .split('.')
            .all(|l| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-'))
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if !ok {
        bail!("invalid host {h:?}: use a plain DNS name or IPv4 address, optionally `:port`");
    }
    Ok((name.to_ascii_lowercase(), port))
}

/// The `OpenShell` policy for one task. Pure, so it can be tested and hashed. With no hosts there are no
/// `network_policies`, so the sandbox's default-deny applies; with hosts, exactly those destinations
/// (any binary) are allowed and everything else stays denied.
///
/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
pub fn policy_yaml(hosts: &[String]) -> Result<String> {
    let mut y = String::from(
        "version: 1\n\
         filesystem_policy:\n  include_workdir: false\n  read_only: [/usr, /lib, /bin, /sbin, /etc]\n  read_write: [/sandbox, /tmp]\n\
         landlock:\n  compatibility: hard_requirement\n\
         process:\n  run_as_user: sandbox\n  run_as_group: sandbox\n",
    );
    if !hosts.is_empty() {
        let mut parsed = hosts
            .iter()
            .map(|h| parse_host(h))
            .collect::<Result<Vec<_>>>()?;
        parsed.sort();
        parsed.dedup();
        y.push_str(
            "network_policies:\n  playbox_hosts:\n    name: playbox-hosts\n    endpoints:\n",
        );
        for (h, p) in parsed {
            y.push_str(&format!("      - host: \"{h}\"\n        port: {p}\n"));
        }
        y.push_str("    binaries:\n      - path: \"/**\"\n");
    }
    Ok(y)
}

fn digest(policy: &str) -> String {
    blake3::hash(policy.as_bytes()).to_hex().to_string()
}

/// `OpenShell` takes Kubernetes-style quantities.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is bounded, or truncated on purpose; the value is clamped to be non-negative first"
)]
fn cpu_arg(cpus: f32) -> String {
    format!("{}m", (cpus * 1000.0).round() as u32)
}

fn mem_arg(mem_mib: u32) -> String {
    format!("{mem_mib}Mi")
}

fn random_name() -> Result<String> {
    let mut b = [0u8; 5]; // OpenShell names are at most 19 characters
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut b)?;
    Ok(format!(
        "playbox-{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    ))
}

fn warm_name() -> Result<String> {
    let mut b = [0u8; 5];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut b)?;
    Ok(format!(
        "{WARM_PREFIX}{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    ))
}

async fn delete_sandbox(name: &str) {
    let _ = cli(&["sandbox", "delete", name], &[], Duration::from_secs(60)).await;
}

async fn cli(
    args: &[&str],
    envs: &[(&str, &str)],
    timeout: Duration,
) -> Result<(TaskOutput, bool)> {
    let mut c = Command::new(BIN);
    c.args(args);
    for (k, v) in envs {
        c.env(k, v);
    }
    run_child(c, timeout).await
}

fn ok(out: &TaskOutput, what: &str) -> Result<()> {
    if out.exit_code != Some(0) {
        bail!("openshell {what} failed: {}", out.stderr.trim());
    }
    Ok(())
}

#[async_trait]
impl Runner for OpenShellRunner {
    fn name(&self) -> &'static str {
        "openshell"
    }

    fn isolation(&self) -> Isolation {
        Isolation::Container
    }

    fn enforces_host_list(&self) -> bool {
        true
    }

    /// The `OpenShell` version and the digest of the no-network base policy, so the host's signed
    /// statement changes when either changes.
    fn attestation(&self) -> String {
        let base = policy_yaml(&[]).unwrap_or_else(|e| format!("invalid:{e}"));
        format!(
            "{};base-policy={};{}",
            self.version.get().map_or("unknown", String::as_str),
            digest(&base),
            self.policy.describe()
        )
    }

    async fn preflight(&self) -> Result<()> {
        let (out, _) = cli(&["--version"], &[], Duration::from_secs(10))
            .await
            .context("openshell is not installed (https://github.com/NVIDIA/OpenShell)")?;
        ok(&out, "--version")?;
        let _ = self.version.set(out.stdout.trim().to_string());
        let (out, _) = cli(&["sandbox", "list"], &[], Duration::from_secs(20)).await?;
        if out.exit_code != Some(0) {
            bail!("openshell gateway is not reachable: {}", out.stderr.trim());
        }
        // Warm sandboxes outlive a host that stopped without cleaning up; nothing holds them now.
        for name in out
            .stdout
            .split_whitespace()
            .filter(|w| w.starts_with(WARM_PREFIX))
        {
            delete_sandbox(name).await;
        }
        if !crate::env_opt_in("PLAYBOX_ALLOW_MISSING_LIMITS") {
            bail!(
                "OpenShell policies have no CPU, memory or timeout limits and its VM driver ignores --cpu/--memory, \
                 so a task could exceed what its grant allows. Use a gateway driver that enforces them (e.g. Kubernetes) \
                 and set PLAYBOX_ALLOW_MISSING_LIMITS=1 to confirm, or use the docker or libkrun runner."
            );
        }
        Ok(())
    }

    async fn run(&self, spec: &TaskSpec, workspace: &Path) -> Result<TaskOutput> {
        self.execute(spec, workspace, None).await
    }

    async fn run_warm(
        &self,
        spec: &TaskSpec,
        workspace: &Path,
        session: &str,
    ) -> Result<TaskOutput> {
        self.execute(spec, workspace, Some(session)).await
    }

    async fn end_warm(&self, session: &str) {
        let w = self.warm.lock().await.remove(session);
        if let Some(w) = w {
            delete_sandbox(&w.name).await;
        }
    }
}

impl OpenShellRunner {
    /// Delete warm sandboxes idle too long, and the oldest ones beyond the cap (`room` makes space for one more).
    async fn reap(&self, room: bool) {
        let mut gone = Vec::new();
        {
            let mut m = self.warm.lock().await;
            let now = std::time::Instant::now();
            let stale: Vec<String> = m
                .iter()
                .filter(|(_, w)| now.duration_since(w.idle_since) > WARM_IDLE)
                .map(|(k, _)| k.clone())
                .collect();
            for k in stale {
                gone.extend(m.remove(&k));
            }
            while m.len() >= WARM_MAX && room {
                let Some(oldest) = m
                    .iter()
                    .min_by_key(|(_, w)| w.idle_since)
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                gone.extend(m.remove(&oldest));
            }
        }
        for w in gone {
            delete_sandbox(&w.name).await;
        }
    }

    async fn execute(
        &self,
        spec: &TaskSpec,
        workspace: &Path,
        session: Option<&str>,
    ) -> Result<TaskOutput> {
        // This runner is the one that honours a host list, so validate the rest of the spec without it.
        check_spec(&TaskSpec {
            allowed_hosts: Vec::new(),
            ..spec.clone()
        })?;
        self.policy.check(spec)?;
        if spec.network && spec.allowed_hosts.is_empty() {
            bail!("the openshell runner only allows network to named hosts (allowed_hosts); unrestricted network is refused");
        }
        let policy_text = policy_yaml(&spec.allowed_hosts)?;
        let ws = workspace.to_str().context("workspace path is not utf-8")?;
        let shape = format!(
            "{}|{}|{}|{}",
            spec.image,
            cpu_arg(spec.cpus),
            mem_arg(spec.mem_mib),
            digest(&policy_text)
        );

        let dir = tempdir()?;
        let policy_path = dir.join("policy.yaml");
        tokio::fs::write(&policy_path, &policy_text).await?;
        let policy_path = policy_path.to_str().context("temp path")?.to_string();

        // A warm sandbox is taken out of the map while it runs, so one session never runs two tasks in it at once.
        let mut name = None;
        if let Some(sess) = session {
            self.reap(false).await;
            let held = self.warm.lock().await.remove(sess);
            if let Some(w) = held {
                if w.shape == shape && self.reset(&w.name).await.is_ok() {
                    name = Some(w.name);
                } else {
                    delete_sandbox(&w.name).await;
                }
            }
        }
        let name = match name {
            Some(n) => n,
            None => {
                let n = if session.is_some() {
                    warm_name()?
                } else {
                    random_name()?
                };
                if let Err(e) = self.create(spec, &n, &policy_path).await {
                    delete_sandbox(&n).await;
                    let _ = tokio::fs::remove_dir_all(&dir).await;
                    return Err(e);
                }
                n
            }
        };

        let result = self
            .exec_in(spec, ws, &name, &dir)
            .await
            .map(|(mut o, timed_out)| {
                o.policy_digest = Some(digest(&policy_text));
                (o, timed_out)
            });
        // Keep the sandbox only after a clean run in a session; anything else (error, timeout, no session) deletes it,
        // because a leaked sandbox keeps the host's resources and a timed-out one is in an unknown state.
        let keep = session.is_some() && matches!(result, Ok((_, false)));
        if keep {
            self.reap(true).await;
            if let Some(sess) = session {
                self.warm.lock().await.insert(
                    sess.to_string(),
                    Warm {
                        name: name.clone(),
                        shape,
                        idle_since: std::time::Instant::now(),
                    },
                );
            }
        } else {
            delete_sandbox(&name).await;
        }
        let _ = tokio::fs::remove_dir_all(&dir).await;
        result.map(|(o, _)| o)
    }

    /// Empty what a previous task left in the places the task user can write, so the next one starts from the
    /// workspace it is given and nothing else. Processes the previous task left running are not touched.
    async fn reset(&self, name: &str) -> Result<()> {
        let (out, timed_out) = cli(
            &[
                "sandbox",
                "exec",
                "-n",
                name,
                "--no-tty",
                "--no-login-shell",
                "--",
                "sh",
                "-c",
                "find /sandbox /tmp -mindepth 1 -delete; true",
            ],
            &[],
            Duration::from_secs(60),
        )
        .await?;
        if timed_out {
            bail!("resetting the warm sandbox timed out");
        }
        ok(&out, "reset")
    }
}

/// Where the workspace lives inside the sandbox: its default working directory, which the `sandbox`
/// user can write (`/workspace` cannot be created by a non-root user).
const GUEST_DIR: &str = "/sandbox";

/// Run a command with `input` on stdin and return its raw stdout (a tar stream, so not lossy-decoded).
async fn piped(
    mut cmd: Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<(Option<i32>, Vec<u8>, String)> {
    use tokio::io::AsyncWriteExt;
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    let mut child = cmd.spawn().context("spawn")?;
    if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
        tokio::spawn(async move {
            let _ = stdin.write_all(&data).await;
        });
    }
    let mut stdout = child.stdout.take().context("child stdout was not piped")?;
    let mut stderr = child.stderr.take().context("child stderr was not piped")?;
    let run = async {
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let (a, b) = tokio::join!(stdout.read_to_end(&mut o), stderr.read_to_end(&mut e));
        a?;
        b?;
        let status = child.wait().await?;
        Ok::<_, anyhow::Error>((status.code(), o, String::from_utf8_lossy(&e).into_owned()))
    };
    tokio::time::timeout(timeout, run)
        .await
        .map_err(|_| anyhow::anyhow!("timed out"))?
}

/// `openshell sandbox exec` refuses more than 4 MiB on stdin ("use `sandbox upload` for larger input"), so a bigger
/// workspace goes in as several stdin chunks appended to a tar file in the sandbox's `/tmp`, which is unpacked and
/// removed before the task's command starts.
const STDIN_CHUNK: usize = 3 * 1024 * 1024;
const GUEST_TAR: &str = "/tmp/.pbx-workspace.tar";

impl OpenShellRunner {
    async fn exec_stdin(&self, name: &str, script: &[&str], data: Vec<u8>) -> Result<()> {
        let mut args = vec![
            "sandbox",
            "exec",
            "-n",
            name,
            "--no-tty",
            "--no-login-shell",
            "--",
        ];
        args.extend_from_slice(script);
        let (code, _, err) = piped(osh(&args), Some(data), SETUP_TIMEOUT).await?;
        if code != Some(0) {
            bail!("uploading the workspace failed: {}", err.trim());
        }
        Ok(())
    }

    async fn upload(&self, name: &str, tar: Vec<u8>) -> Result<()> {
        if tar.len() <= STDIN_CHUNK {
            return self
                .exec_stdin(name, &["tar", "-xf", "-", "-C", GUEST_DIR], tar)
                .await;
        }
        for (i, chunk) in tar.chunks(STDIN_CHUNK).enumerate() {
            let redirect = if i == 0 { ">" } else { ">>" };
            let script = format!("cat {redirect} {GUEST_TAR}");
            self.exec_stdin(name, &["sh", "-c", &script], chunk.to_vec())
                .await?;
        }
        let unpack =
            format!("tar -xf {GUEST_TAR} -C {GUEST_DIR}; rc=$?; rm -f {GUEST_TAR}; exit $rc");
        self.exec_stdin(name, &["sh", "-c", &unpack], Vec::new())
            .await
    }
}

fn osh(args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args);
    c
}

impl OpenShellRunner {
    async fn create(&self, spec: &TaskSpec, name: &str, policy: &str) -> Result<()> {
        let (cpu, mem) = (cpu_arg(spec.cpus), mem_arg(spec.mem_mib));
        let (out, timed_out) = cli(
            &[
                "sandbox",
                "create",
                "--name",
                name,
                "--from",
                &spec.image,
                "--cpu",
                &cpu,
                "--memory",
                &mem,
                "--policy",
                policy,
                "--detach",
            ],
            &[],
            SETUP_TIMEOUT,
        )
        .await?;
        if timed_out {
            bail!("openshell sandbox create timed out");
        }
        ok(&out, "sandbox create")
    }

    /// Upload the workspace, run the command, download the workspace. Returns the task output and whether it timed out.
    async fn exec_in(
        &self,
        spec: &TaskSpec,
        ws: &str,
        name: &str,
        tmp: &Path,
    ) -> Result<(TaskOutput, bool)> {
        // Workspace in: a tar stream on the exec's stdin.
        let packed = Command::new("tar")
            .env("COPYFILE_DISABLE", "1")
            .args(["-cf", "-", "-C", ws, "."])
            .stdin(Stdio::null())
            .output()
            .await?;
        if !packed.status.success() {
            bail!(
                "packing the workspace failed: {}",
                String::from_utf8_lossy(&packed.stderr).trim()
            );
        }
        self.upload(name, packed.stdout).await?;

        let secs = spec.timeout_secs.to_string();
        let mut args = vec![
            "sandbox",
            "exec",
            "-n",
            name,
            "--no-tty",
            "--no-login-shell",
            "--workdir",
            GUEST_DIR,
            "--timeout",
            &secs,
            "--",
        ];
        args.extend(spec.cmd.iter().map(String::as_str));
        // A little slack so OpenShell's own timeout reports first; ours is the backstop.
        let (task, timed_out) =
            cli(&args, &[], Duration::from_secs(spec.timeout_secs + 10)).await?;

        // Workspace out, so the host can diff it. Skipped on timeout: the state is partial.
        if !timed_out {
            let (code, tar, err) = piped(
                osh(&[
                    "sandbox",
                    "exec",
                    "-n",
                    name,
                    "--no-tty",
                    "--no-login-shell",
                    "--",
                    "tar",
                    "-cf",
                    "-",
                    "-C",
                    GUEST_DIR,
                    ".",
                ]),
                None,
                SETUP_TIMEOUT,
            )
            .await?;
            if code != Some(0) {
                bail!("downloading the workspace failed: {}", err.trim());
            }
            // Untrusted archive: unpack into an empty directory (tar refuses absolute and `..` paths),
            // then mirror it over the workspace so files the task deleted disappear too.
            let out_dir = tmp.join("out");
            tokio::fs::create_dir_all(&out_dir).await?;
            let mut un = Command::new("tar")
                .args(["-xf", "-", "-C"])
                .arg(&out_dir)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()?;
            {
                use tokio::io::AsyncWriteExt;
                let mut stdin = un.stdin.take().context("child stdin was not piped")?;
                stdin.write_all(&tar).await?;
            }
            let un = un.wait_with_output().await?;
            if !un.status.success() {
                bail!(
                    "unpacking the workspace failed: {}",
                    String::from_utf8_lossy(&un.stderr).trim()
                );
            }
            let sync = Command::new("rsync")
                .args(["-a", "--delete"])
                .arg(format!("{}/", out_dir.display()))
                .arg(format!("{ws}/"))
                .stdin(Stdio::null())
                .output()
                .await?;
            if !sync.status.success() {
                bail!(
                    "copying the workspace back failed: {}",
                    String::from_utf8_lossy(&sync.stderr).trim()
                );
            }
        }
        Ok((task, timed_out))
    }
}

fn tempdir() -> Result<std::path::PathBuf> {
    let p = std::env::temp_dir().join(random_name()?);
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_has_no_network_by_default_and_requires_landlock() {
        let p = policy_yaml(&[]).unwrap();
        assert!(p.starts_with("version: 1"));
        assert!(!p.contains("network_policies"));
        assert!(p.contains("hard_requirement") && p.contains("run_as_user: sandbox"));
    }

    #[test]
    fn hosts_become_exact_endpoints_and_cannot_inject_policy() {
        let p = policy_yaml(&[
            "API.github.com".into(),
            "example.com:8443".into(),
            "api.github.com".into(),
        ])
        .unwrap();
        assert!(p.contains("network_policies"));
        assert_eq!(p.matches("- host:").count(), 2, "{p}");
        assert!(p.contains("\"api.github.com\"") && p.contains("port: 8443"));
        for bad in [
            "*.example.com",
            "a.com\"\n  evil: x",
            "a b",
            "",
            "a..com",
            "-a.com",
            "a.com:0",
            "a.com:99999",
            "a.com:x",
            "a/b",
        ] {
            assert!(
                policy_yaml(&[bad.into()]).is_err(),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn digest_changes_with_hosts() {
        assert_ne!(
            digest(&policy_yaml(&[]).unwrap()),
            digest(&policy_yaml(&["a.com".into()]).unwrap())
        );
    }

    #[test]
    fn quantities_are_kubernetes_style() {
        assert_eq!(cpu_arg(0.5), "500m");
        assert_eq!(cpu_arg(2.0), "2000m");
        assert_eq!(mem_arg(512), "512Mi");
    }

    #[tokio::test]
    async fn refuses_unrestricted_network_and_unlisted_images() {
        let r = OpenShellRunner::new(SandboxPolicy::default());
        let mut spec = TaskSpec::new(&["true"]);
        spec.network = true;
        assert!(r.run(&spec, Path::new("/tmp")).await.is_err());
        let mut spec = TaskSpec::new(&["true"]);
        spec.image = "evil/miner".into();
        assert!(r.run(&spec, Path::new("/tmp")).await.is_err());
    }
}
