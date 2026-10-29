//! Steering end to end: a real host, real clients, and the real Python engine on a tiny random GPT-2.
//! The sandbox is replaced by a shim runner that runs the engine on the host with the mounts mapped to
//! real paths, so no Docker is needed; `docker_engine_image` (ignored) covers the real image.
//! Skipped, loudly, when `steer/.venv` has not been created (`cd steer && uv sync --python 3.12`).

use async_trait::async_trait;
use playbox_core::steer::{Ceiling, Mode, Recipe, Site, SteerSpec, Tokens};
use playbox_net::{
    grants::{steer_narrowing, SteerCaps},
    join_link, new_secret_key,
    steer::{SteerHost, SteerLimits},
    Client, Host, HostConfig,
};
use playbox_runner::{Isolation, Mount, Runner, TaskOutput, TaskSpec};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
    time::Duration,
};

fn steer_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../steer")
}

fn python() -> Option<PathBuf> {
    let p = steer_dir().join(".venv/bin/python");
    p.exists().then_some(p)
}

macro_rules! need_python {
    () => {
        match python() {
            Some(p) => p,
            None => {
                eprintln!("SKIPPED: steer/.venv is missing; run `cd steer && uv sync --python 3.12` to run the steering tests");
                return;
            }
        }
    };
}

fn run_py(py: &Path, args: &[&str]) -> String {
    let out = Command::new(py)
        .args(["-m", "playbox_steer"])
        .args(args)
        .env("PYTHONPATH", steer_dir())
        .env("HF_HUB_OFFLINE", "1")
        .output()
        .expect("python");
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The engine, run on the host, with sandbox paths mapped to real ones.
struct PyRunner {
    py: PathBuf,
}

#[async_trait]
impl Runner for PyRunner {
    fn name(&self) -> &'static str {
        "py-shim"
    }
    fn isolation(&self) -> Isolation {
        Isolation::None
    }
    async fn run(&self, _: &TaskSpec, _: &Path) -> anyhow::Result<TaskOutput> {
        anyhow::bail!("the shim only runs steering")
    }
    async fn image_digest(&self, _: &str) -> anyhow::Result<[u8; 32]> {
        Ok([7; 32])
    }
    async fn run_with_mounts(&self, spec: &TaskSpec, workspace: &Path, mounts: &[Mount]) -> anyhow::Result<TaskOutput> {
        let map = |a: &str| -> String {
            if let Some(rest) = a.strip_prefix("/workspace") {
                return format!("{}{rest}", workspace.display());
            }
            for m in mounts {
                if a == m.container {
                    return m.host.display().to_string();
                }
            }
            a.to_string()
        };
        let args: Vec<String> = spec.cmd[1..].iter().map(|a| map(a)).collect();
        let out = tokio::process::Command::new(&self.py)
            .args(&args)
            .current_dir(workspace)
            .env("PYTHONPATH", steer_dir())
            .env("HF_HUB_OFFLINE", "1")
            .output()
            .await?;
        Ok(TaskOutput {
            exit_code: out.status.code(),
            timed_out: false,
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            policy_digest: None,
        })
    }
}

static MODEL: OnceLock<PathBuf> = OnceLock::new();

fn base_model(py: &Path) -> PathBuf {
    MODEL
        .get_or_init(|| {
            let dir = std::env::temp_dir().join(format!("pb-steer-model-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let out = Command::new(py).arg(steer_dir().join("tools/make_tiny_model.py")).arg(&dir).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            dir
        })
        .clone()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_file() {
            std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    }
}

struct Env {
    host: Host,
    link: String,
    weights: [u8; 32],
    direction: Vec<u8>,
    prompts: Vec<u8>,
}

fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn base_caps(weights: &[u8; 32]) -> SteerCaps {
    SteerCaps {
        models: vec![hexs(weights)],
        layer_min: 0,
        layer_max: 3,
        max_coef_milli: 10_000,
        modes: vec!["add".into(), "clamp".into()],
        directions: None,
        max_tokens_per_run: 400,
        classes: vec![],
        probe: true,
        max_runs: 5,
        max_tokens_total: 10_000,
    }
}

const PROMPTS: &str = r#"{"prompts":["The weather today is","I think that"],"max_new_tokens":4,"temperature":0}"#;

async fn env(name: &str, py: &Path, guard: bool, tweak: impl FnOnce(&mut SteerCaps, &[u8; 32], &Path)) -> Env {
    env_with(name, py, guard, Arc::new(PyRunner { py: py.to_path_buf() }), "playbox-steer:test", Isolation::None, tweak).await
}

async fn env_with(name: &str, py: &Path, guard: bool, runner: Arc<dyn Runner>, image: &str, isolation: Isolation, tweak: impl FnOnce(&mut SteerCaps, &[u8; 32], &Path)) -> Env {
    let store = std::env::temp_dir().join(format!("pb-steer-store-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&store);
    let staged = store.join("staged");
    copy_dir(&base_model(py), &staged);
    let weights = playbox_net::steer::weights_manifest(&staged).unwrap();
    let wdir = store.join("weights").join(hexs(&weights));
    std::fs::create_dir_all(wdir.parent().unwrap()).unwrap();
    std::fs::rename(&staged, &wdir).unwrap();

    let dd = store.join("dir");
    run_py(py, &["random", "--dim", "32", "--seed", "5", "--out", dd.to_str().unwrap()]);
    let direction = std::fs::read(dd.join("direction.f32")).unwrap();
    if guard {
        let g = store.join("guards").join(hexs(&weights));
        std::fs::create_dir_all(&g).unwrap();
        std::fs::write(g.join("refusal.f32"), &direction).unwrap();
        std::fs::write(g.join("guards.json"), r#"[{"class":"refusal","site":"resid_post","layer":2,"file":"refusal.f32"}]"#).unwrap();
    }
    let mut caps = base_caps(&weights);
    tweak(&mut caps, &weights, &store);
    let steer = SteerHost::open(&store, image, SteerLimits { cpus: 1.0, mem_mib: 512, timeout_secs: 120 }, caps, Ceiling::LocalInternalExploratory).unwrap();
    let cfg = HostConfig {
        min_isolation: isolation,
        runner: Some(runner),
        steer: Some(Arc::new(steer)),
        limits: playbox_runner::Limits { cpus: 2.0, mem_mib: 1024, timeout_secs: 300, network: false, allowed_hosts: vec![] },
        ..Default::default()
    };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg).await.unwrap();
    let link = host.create_link(Duration::from_secs(120), 10).unwrap().encode();
    Env { host, link, weights, direction, prompts: PROMPTS.as_bytes().to_vec() }
}

impl Env {
    fn spec(&self) -> SteerSpec {
        SteerSpec {
            model: "tiny-gpt2".into(),
            weights: self.weights,
            site: Site::ResidPost,
            layers: vec![1, 2],
            direction: *blake3::hash(&self.direction).as_bytes(),
            recipe: Recipe::DiffMean,
            mode: Mode::Add,
            coef_milli: 4000,
            tokens: Tokens::All,
            prompts: *blake3::hash(&self.prompts).as_bytes(),
            seed: 1,
        }
    }
    async fn member(&self) -> Client {
        join_link(&self.link, new_secret_key().unwrap(), true).await.unwrap()
    }
}

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[tokio::test]
async fn a_member_steers_a_hosted_model_and_gets_a_verified_receipt() {
    let py = need_python!();
    let e = env("happy", &py, false, |_, _, _| {}).await;
    let c = e.member().await;
    let spec = e.spec();
    let r = c.steer(&spec, &e.direction, &e.prompts).await.unwrap();

    let out: serde_json::Value = serde_json::from_str(&r.output).unwrap();
    assert_eq!(out["gates"]["pass"], true);
    assert_eq!(out["results"].as_array().unwrap().len(), 2);
    assert_eq!(r.receipt.spec_digest, spec.digest().unwrap());
    assert_eq!(r.receipt.weights, e.weights);
    assert_eq!(r.receipt.image, [7; 32]);
    assert_eq!(r.receipt.ceiling, Ceiling::LocalInternalExploratory);
    assert_eq!(&r.receipt.host, e.host.addr().id.as_bytes());
    r.receipt.matches(&spec, r.output.as_bytes()).unwrap();
    assert!(r.receipt.matches(&spec, b"other").is_err());

    // Same request, same bytes: the engine is deterministic and the receipt covers them.
    let again = c.steer(&spec, &e.direction, &e.prompts).await.unwrap();
    assert_eq!(again.output, r.output);
    assert_eq!(again.receipt.output_hash, r.receipt.output_hash);
}

#[tokio::test]
async fn probing_reads_without_editing_and_needs_the_right() {
    let py = need_python!();
    let e = env("probe", &py, false, |_, _, _| {}).await;
    let c = e.member().await;
    let r = c.steer_probe(&e.spec(), &e.direction, &e.prompts).await.unwrap();
    let out: serde_json::Value = serde_json::from_str(&r.output).unwrap();
    assert_eq!(out["probe"].as_array().unwrap().len(), 2);
    assert!(out.get("results").is_none());

    let e2 = env("noprobe", &py, false, |c, _, _| c.probe = false).await;
    let c2 = e2.member().await;
    let msg = err(c2.steer_probe(&e2.spec(), &e2.direction, &e2.prompts).await.unwrap_err());
    assert!(msg.contains("not authorized"), "{msg}");
    c2.steer(&e2.spec(), &e2.direction, &e2.prompts).await.unwrap();
}

#[tokio::test]
async fn the_grant_bounds_model_layers_strength_mode_direction_and_tokens() {
    let py = need_python!();
    let e = env("bounds", &py, false, |c, _, _| {
        c.layer_max = 2;
        c.max_coef_milli = 5000;
        c.max_tokens_per_run = 16;
        c.modes = vec!["add".into()];
    })
    .await;
    let c = e.member().await;
    let ok = c.steer(&e.spec(), &e.direction, &e.prompts).await;
    assert!(ok.is_ok(), "{:?}", ok.err().map(err));

    let denied = |label: &'static str, spec: SteerSpec, prompts: Vec<u8>| {
        let (c, d) = (&c, e.direction.clone());
        async move {
            let mut spec = spec;
            spec.prompts = *blake3::hash(&prompts).as_bytes();
            let m = err(c.steer(&spec, &d, &prompts).await.unwrap_err());
            assert!(m.contains("not authorized"), "{label}: {m}");
        }
    };
    let base = e.spec();
    denied("layer above the range", SteerSpec { layers: vec![1, 3], ..base.clone() }, e.prompts.clone()).await;
    denied("strength above the cap", SteerSpec { coef_milli: 5001, ..base.clone() }, e.prompts.clone()).await;
    denied("negative strength counts by magnitude", SteerSpec { coef_milli: -5001, ..base.clone() }, e.prompts.clone()).await;
    denied("mode not allowed", SteerSpec { mode: Mode::Clamp, ..base.clone() }, e.prompts.clone()).await;
    denied("ablate not allowed", SteerSpec { mode: Mode::Ablate, coef_milli: 0, ..base.clone() }, e.prompts.clone()).await;
    // 2 prompts x 5 tokens x 2 = 20 > 16.
    let bigger = br#"{"prompts":["The weather today is","I think that"],"max_new_tokens":5,"temperature":0}"#.to_vec();
    denied("too many tokens", base.clone(), bigger).await;
    // A boundary value is fine.
    let at_cap = SteerSpec { coef_milli: -5000, layers: vec![0, 2], ..base };
    c.steer(&at_cap, &e.direction, &e.prompts).await.unwrap();
}

#[tokio::test]
async fn a_model_or_direction_outside_the_grant_is_refused() {
    let py = need_python!();
    let e = env("dirs", &py, false, |c, _, _| c.directions = Some(vec!["00".repeat(32)])).await;
    let c = e.member().await;
    let m = err(c.steer(&e.spec(), &e.direction, &e.prompts).await.unwrap_err());
    assert!(m.contains("not authorized"), "direction not on the list: {m}");

    let e2 = env("model", &py, false, |_, _, _| {}).await;
    let c2 = e2.member().await;
    let other = SteerSpec { weights: [9; 32], ..e2.spec() };
    let m = err(c2.steer(&other, &e2.direction, &e2.prompts).await.unwrap_err());
    assert!(m.contains("does not serve model"), "{m}");
}

#[tokio::test]
async fn files_must_hash_to_what_the_spec_names() {
    let py = need_python!();
    let e = env("hash", &py, false, |_, _, _| {}).await;
    let c = e.member().await;
    let mut wrong_dir = e.direction.clone();
    wrong_dir[0] ^= 1;
    assert!(err(c.steer(&e.spec(), &wrong_dir, &e.prompts).await.unwrap_err()).contains("does not hash"));
    let wrong_prompts = br#"{"prompts":["tampered"]}"#;
    assert!(err(c.steer(&e.spec(), &e.direction, wrong_prompts).await.unwrap_err()).contains("does not hash"));
    // Not a float32 array.
    let odd = vec![1u8; 7];
    let mut s = e.spec();
    s.direction = *blake3::hash(&odd).as_bytes();
    assert!(err(c.steer(&s, &odd, &e.prompts).await.unwrap_err()).contains("float32"));
}

#[tokio::test]
async fn a_spec_signed_by_someone_else_is_refused() {
    let py = need_python!();
    let e = env("signer", &py, false, |_, _, _| {}).await;
    let mallory = e.member().await;
    let other = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
    let text = e.spec().sign(&other).unwrap();
    let op = playbox_net::wire::Op::Steer { spec: text, probe: false };
    let msg = op.message(vec![a2a::Part::raw(e.direction.clone()), a2a::Part::raw(e.prompts.clone())]);
    let req = a2a::SendMessageRequest { message: msg, configuration: None, metadata: None, tenant: None };
    let v = mallory.call(a2a::methods::SEND_MESSAGE, serde_json::to_value(req).unwrap()).await.unwrap();
    assert!(v.to_string().contains("signed by the key that sends it"), "{v}");
}

#[tokio::test]
async fn a_member_without_steering_rights_cannot_steer() {
    let py = need_python!();
    let e = env("norights", &py, false, |_, _, _| {}).await;
    let c = e.member().await;
    // Narrow to echo only: the steer right is gone.
    c.attenuate(r#"check if operation("echo");"#).unwrap();
    assert!(err(c.steer(&e.spec(), &e.direction, &e.prompts).await.unwrap_err()).contains("not authorized"));
    assert_eq!(c.echo("still here").await.unwrap(), "still here");
}

#[tokio::test]
async fn members_can_narrow_their_own_steering_limits_offline() {
    let py = need_python!();
    let e = env("narrow", &py, false, |_, _, _| {}).await;
    let c = e.member().await;
    c.steer(&e.spec(), &e.direction, &e.prompts).await.unwrap();

    c.attenuate(&steer_narrowing(Some(1), Some(3000), Some(&["add"]), None).unwrap()).unwrap();
    // Layers [1, 2] now exceed layer 1, and strength 4000 exceeds 3000.
    assert!(err(c.steer(&e.spec(), &e.direction, &e.prompts).await.unwrap_err()).contains("not authorized"));
    let ok = SteerSpec { layers: vec![1], coef_milli: 3000, ..e.spec() };
    c.steer(&ok, &e.direction, &e.prompts).await.unwrap();
    // The narrowing is about steering only: other operations are untouched.
    assert_eq!(c.echo("fine").await.unwrap(), "fine");
    // Narrowing cannot widen: clamp was never allowed in the narrowed grant's modes.
    let clamp = SteerSpec { mode: Mode::Clamp, layers: vec![1], coef_milli: 1000, ..e.spec() };
    assert!(err(c.steer(&clamp, &e.direction, &e.prompts).await.unwrap_err()).contains("not authorized"));
}

#[tokio::test]
async fn the_budget_is_counted_per_member() {
    let py = need_python!();
    let e = env("budget", &py, false, |c, _, _| c.max_runs = 2).await;
    let a = e.member().await;
    a.steer(&e.spec(), &e.direction, &e.prompts).await.unwrap();
    a.steer(&e.spec(), &e.direction, &e.prompts).await.unwrap();
    assert!(err(a.steer(&e.spec(), &e.direction, &e.prompts).await.unwrap_err()).contains("budget used up"));
    // Another member has their own budget.
    let b = e.member().await;
    b.steer(&e.spec(), &e.direction, &e.prompts).await.unwrap();

    let e2 = env("tokbudget", &py, false, |c, _, _| c.max_tokens_total = 20).await;
    let m = e2.member().await;
    m.steer(&e2.spec(), &e2.direction, &e2.prompts).await.unwrap(); // 16 tokens
    assert!(err(m.steer(&e2.spec(), &e2.direction, &e2.prompts).await.unwrap_err()).contains("budget used up"));
}

#[tokio::test]
async fn a_guarded_direction_is_refused_unless_the_grant_names_its_class() {
    let py = need_python!();
    let e = env("guard", &py, true, |_, _, _| {}).await;
    let c = e.member().await;
    // The guard sits at layer 2 of resid_post and the direction is the guard itself.
    let m = err(c.steer(&e.spec(), &e.direction, &e.prompts).await.unwrap_err());
    assert!(m.contains("safety guard"), "{m}");
    // The same direction at layers the guard does not cover is not guarded.
    c.steer(&SteerSpec { layers: vec![1], ..e.spec() }, &e.direction, &e.prompts).await.unwrap();
    c.steer(&SteerSpec { site: Site::MlpOut, ..e.spec() }, &e.direction, &e.prompts).await.unwrap();
    // Probing is read-only and is not guarded.
    c.steer_probe(&e.spec(), &e.direction, &e.prompts).await.unwrap();

    let allowed = env("guard-ok", &py, true, |c, _, _| c.classes = vec!["refusal".into()]).await;
    let c2 = allowed.member().await;
    let r = c2.steer(&allowed.spec(), &allowed.direction, &allowed.prompts).await.unwrap();
    let out: serde_json::Value = serde_json::from_str(&r.output).unwrap();
    assert_eq!(out["guard_hits"][0]["class"], "refusal");
    assert_eq!(out["guard_hits"][0]["denied"], false);
}

#[tokio::test]
async fn a_direction_of_the_wrong_width_is_rejected_by_the_engine() {
    let py = need_python!();
    // A direction whose width does not fit the model is rejected by the engine and yields no receipt.
    let e = env("bad-dim", &py, false, |_, _, _| {}).await;
    let c = e.member().await;
    let wrong = {
        let tmp = std::env::temp_dir().join(format!("pb-dim-{}", std::process::id()));
        run_py(&py, &["random", "--dim", "16", "--seed", "1", "--out", tmp.to_str().unwrap()]);
        std::fs::read(tmp.join("direction.f32")).unwrap()
    };
    let mut s = e.spec();
    s.direction = *blake3::hash(&wrong).as_bytes();
    let m = err(c.steer(&s, &wrong, &e.prompts).await.unwrap_err());
    assert!(m.contains("engine rejected") || m.contains("model width"), "{m}");
}

#[tokio::test]
async fn a_host_without_steering_refuses_politely() {
    let py = need_python!();
    let e = env("nohost", &py, false, |_, _, _| {}).await;
    let cfg = HostConfig { min_isolation: Isolation::None, runner: Some(Arc::new(PyRunner { py })), ..Default::default() };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 1).unwrap().encode();
    let c = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    let m = err(c.steer(&e.spec(), &e.direction, &e.prompts).await.unwrap_err());
    assert!(m.contains("does not host steering"), "{m}");
}

#[test]
fn a_steering_host_whose_limits_exceed_the_grant_refuses_to_start() {
    let py = need_python!();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let store = std::env::temp_dir().join(format!("pb-steer-store-limits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&store);
        let staged = store.join("staged");
        copy_dir(&base_model(&py), &staged);
        let w = playbox_net::steer::weights_manifest(&staged).unwrap();
        let wdir = store.join("weights").join(hexs(&w));
        std::fs::create_dir_all(wdir.parent().unwrap()).unwrap();
        std::fs::rename(&staged, &wdir).unwrap();
        let steer = SteerHost::open(&store, "img", SteerLimits { cpus: 1.0, mem_mib: 4096, timeout_secs: 60 }, base_caps(&w), Ceiling::HostClaim).unwrap();
        let cfg = HostConfig { min_isolation: Isolation::None, runner: Some(Arc::new(PyRunner { py })), steer: Some(Arc::new(steer)), ..Default::default() };
        let e = Host::start_with(new_secret_key().unwrap(), true, cfg).await.err().expect("must not start");
        assert!(err(e).contains("raise the limits"));
    });
}

#[test]
fn a_mislabeled_weights_store_is_refused() {
    let py = need_python!();
    let store = std::env::temp_dir().join(format!("pb-steer-store-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&store);
    copy_dir(&base_model(&py), &store.join("weights").join("00".repeat(32)));
    let e = SteerHost::open(&store, "img", SteerLimits::default(), base_caps(&[0; 32]), Ceiling::HostClaim).unwrap_err();
    assert!(err(e).contains("corrupt or mislabeled"));
}

/// The real thing: the Docker runner, the built image (`docker build -t playbox-steer:0.1 -f steer/Dockerfile steer`),
/// weights mounted read-only, no network. Run with `cargo test -p playbox-net --test steer -- --ignored`.
#[tokio::test]
#[ignore = "needs Docker and the playbox-steer:0.1 image"]
async fn docker_engine_image() {
    let py = need_python!();
    let image = "playbox-steer:0.1";
    let runner: Arc<dyn Runner> = Arc::new(playbox_runner::DockerRunner::new(playbox_runner::SandboxPolicy { allowed_images: vec![image.into()] }));
    let e = env_with("docker", &py, true, runner.clone(), image, Isolation::Container, |_, _, _| {}).await;
    let c = e.member().await;

    // Guarded direction: refused inside the container with exit code 3.
    let m = err(c.steer(&e.spec(), &e.direction, &e.prompts).await.unwrap_err());
    assert!(m.contains("safety guard"), "{m}");

    let spec = SteerSpec { layers: vec![1], ..e.spec() };
    let r = c.steer(&spec, &e.direction, &e.prompts).await.unwrap();
    let out: serde_json::Value = serde_json::from_str(&r.output).unwrap();
    assert_eq!(out["gates"]["pass"], true);
    let digest = runner.image_digest(image).await.unwrap();
    assert_eq!(r.receipt.image, digest, "the receipt names the image that ran");
    assert_ne!(r.receipt.image, [0; 32]);

    // The container has no network and a read-only root: the engine still ran, so it needs neither.
    let again = c.steer(&spec, &e.direction, &e.prompts).await.unwrap();
    assert_eq!(again.output, r.output, "deterministic inside the container too");
}
