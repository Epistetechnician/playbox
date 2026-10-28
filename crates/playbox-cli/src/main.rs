use anyhow::{bail, Result};
use ed25519_dalek::SigningKey;
use playbox_core::{attest, history::Log, platform, store};
use playbox_net::{join_link, new_secret_key, Host, HostConfig, Transport, TransportChoice};
use playbox_runner::{DockerRunner, TaskSpec};
use std::{path::PathBuf, sync::Arc, time::Duration};

const HELP: &str = "playbox (scaffold)
  up [--transport T] [--state DIR] [--repo DIR] [--docker]
                            start a room and print a join link (valid 1h, 1 use).
                            T is `iroh` (public relays, default), `local` (same machine/LAN) or
                            `relay:<https url>` (your own iroh-relay; PLAYBOX_RELAY_CA=PEM for a private CA).
                            --state keeps the room's identity, members and bans across restarts.
                            --repo shares a git repo; --docker lets members run tasks in containers
  join <link> [--transport T] [--task <shell cmd>] [--base REF]
                            join a room (the link says which transport to use unless you override);
                            with --task, run it remotely on REF (default main) and print the patch
  (--local is shorthand for --transport local; PLAYBOX_TRANSPORT sets the default)
  bridge <link> [--listen ADDR] [--token T] [--transport T]
                            join a room and serve it to stock A2A HTTP clients (default 127.0.0.1:8787).
                            Callers act as this one member. A non-loopback --listen requires --token.
  record [--kind K] [--log F] [--key F]
                            append stdin as a signed entry to a history log (agent hook target)
  attest [--log F] [--key F] [--platform auto|none|se|tpm]
                            append a signed attestation of this binary to a history log, with hardware
                            evidence when available (Secure Enclave on macOS, TPM 2.0 on Linux).
                            Whether anyone believes the hardware part depends on their pins (see `pin`)
  pin list | add <kind> <fingerprint> | remove <kind> <fingerprint>
                            the device keys you trust (kind: secure-enclave or tpm). `verify` and `join`
                            report fingerprints; a device only reaches tier T1 once pinned
  verify <log>              verify a history log: chain, signatures and attestations
  demo                      build and verify a sample history log
planned: status | delegate | verify | revoke (see PRD.md)";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let local = args.iter().any(|a| a == "--local");
    let transport = args.iter().position(|a| a == "--transport").and_then(|i| args.get(i + 1)).cloned();
    let state = args.iter().position(|a| a == "--state").and_then(|i| args.get(i + 1)).cloned();
    let docker = args.iter().any(|a| a == "--docker");
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let (repo, task, base) = (opt("--repo"), opt("--task"), opt("--base"));
    let (kind, log_path, key_path) = (opt("--kind"), opt("--log"), opt("--key"));
    let platform_opt = opt("--platform");
    let mut skip = false;
    let pos: Vec<&String> = args
        .iter()
        .filter(|a| {
            if skip {
                skip = false;
                return false;
            }
            if matches!(a.as_str(), "--repo" | "--task" | "--base" | "--kind" | "--log" | "--key" | "--transport" | "--state" | "--listen" | "--token" | "--platform") {
                skip = true;
                return false;
            }
            !a.starts_with("--")
        })
        .collect();
    match pos.first().map(|s| s.as_str()) {
        Some("up") => up(chosen(local, transport.as_deref())?, state, repo, docker).await,
        Some("join") => match pos.get(1) {
            Some(link) => join(link, join_choice(local, transport.as_deref())?, task, base).await,
            None => bail!("usage: playbox join <link>"),
        },
        Some("bridge") => match pos.get(1) {
            Some(link) => {
                let opt = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
                bridge(link, join_choice(local, transport.as_deref())?, opt("--listen"), opt("--token").or_else(|| std::env::var("PLAYBOX_BRIDGE_TOKEN").ok())).await
            }
            None => bail!("usage: playbox bridge <link>"),
        },
        Some("record") => record(kind, log_path, key_path),
        Some("pin") => pin_cmd(&pos),
        Some("attest") => attest_cmd(log_path, key_path, platform_opt),
        Some("verify") => match pos.get(1) {
            Some(path) => verify(path),
            None => bail!("usage: playbox verify <log>"),
        },
        Some("demo") => demo(),
        _ => {
            println!("{HELP}");
            Ok(())
        }
    }
}

fn chosen(local: bool, t: Option<&str>) -> Result<Transport> {
    match (t, local) {
        (Some(t), _) => Transport::parse(t),
        (None, true) => Ok(Transport::Local),
        (None, false) => Transport::from_env(),
    }
}

fn join_choice(local: bool, t: Option<&str>) -> Result<TransportChoice> {
    match (t, local) {
        (Some(t), _) => Ok(TransportChoice::Use(Transport::parse(t)?)),
        (None, true) => Ok(TransportChoice::Use(Transport::Local)),
        (None, false) => Ok(TransportChoice::FromLink),
    }
}

async fn up(transport: Transport, state: Option<String>, repo: Option<String>, docker: bool) -> Result<()> {
    let provider: Option<std::sync::Arc<dyn attest::PlatformProvider>> = platform::detect(&store::home()).map_err(anyhow::Error::msg)?.map(std::sync::Arc::from);
    println!("platform attestation: {}", provider.as_ref().map(|p| p.kind()).unwrap_or("none (self-signed only)"));
    let cfg = HostConfig {
        repo: repo.map(PathBuf::from),
        runner: if docker { Some(Arc::new(DockerRunner::default())) } else { None },
        platform: provider,
        ..Default::default()
    };
    let host = match &state {
        Some(dir) => Host::open(std::path::Path::new(dir), transport.clone(), cfg).await?,
        None => Host::start_with(new_secret_key()?, transport.clone(), cfg).await?,
    };
    let link = host.create_link(Duration::from_secs(3600), 1)?;
    println!("room is up ({}). share this link:\n{}", transport.label(), link.encode());
    println!("(ctrl-c to stop)");
    tokio::signal::ctrl_c().await?;
    host.shutdown().await
}

async fn join(link: &str, choice: TransportChoice, task: Option<String>, base: Option<String>) -> Result<()> {
    let client = join_link(link, new_secret_key()?, choice).await?;
    if let Some(cmd) = task {
        let (runner, out, patch) = client
            .run_task(base.as_deref().unwrap_or("main"), TaskSpec::new(&["sh", "-c", &cmd]))
            .await?;
        println!("ran on {runner}: exit {:?}{}", out.exit_code, if out.timed_out { " (timed out)" } else { "" });
        print!("{}", out.stdout);
        eprint!("{}", out.stderr);
        println!("--- patch ({} bytes) ---\n{}", patch.len(), String::from_utf8_lossy(&patch));
    } else {
        let reply = client.echo("hello from a guest agent").await?;
        println!("joined. echo task returned: {reply}");
    }
    client.close().await;
    Ok(())
}

async fn bridge(link: &str, choice: TransportChoice, listen: Option<String>, token: Option<String>) -> Result<()> {
    let client = join_link(link, new_secret_key()?, choice).await?;
    let (listener, addr) = playbox_bridge::bind(listen.as_deref().unwrap_or("127.0.0.1:8787")).await?;
    let url = format!("http://{addr}");
    println!("bridge up: agent card at {url}/.well-known/agent-card.json, JSON-RPC at {url}/jsonrpc");
    println!("{}", if token.is_some() { "bearer token required" } else { "no token (loopback only)" });
    playbox_bridge::serve(client, listener, token, url).await
}

fn record(kind: Option<String>, log: Option<String>, key: Option<String>) -> Result<()> {
    let home = store::home();
    let key = store::load_or_create_key(&key.map(PathBuf::from).unwrap_or_else(|| home.join("session.key")))?;
    let log = log
        .or_else(|| std::env::var("PLAYBOX_LOG").ok())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("history").join("default.log"));
    let mut payload = Vec::new();
    std::io::Read::read_to_end(&mut std::io::stdin(), &mut payload)?;
    let e = store::append(&log, &key, kind.as_deref().unwrap_or("tool_call"), &payload, store::now_ms())?;
    eprintln!("recorded entry {} in {}", e.seq, log.display());
    Ok(())
}

fn attest_cmd(log: Option<String>, key: Option<String>, platform_mode: Option<String>) -> Result<()> {
    let home = store::home();
    let key = store::load_or_create_key(&key.map(PathBuf::from).unwrap_or_else(|| home.join("session.key")))?;
    let log = log
        .or_else(|| std::env::var("PLAYBOX_LOG").ok())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("history").join("default.log"));
    let head = if log.exists() { playbox_core::history::text::head_hex(&store::load(&log)?) } else { playbox_core::history::text::hex(&[0u8; 32]) };
    let provider = match &platform_mode {
        Some(m) => platform::detect_with(&home, m),
        None => platform::detect(&home),
    }
    .map_err(anyhow::Error::msg)?;
    let m = attest::measure_self("none", "cli");
    let text = match &provider {
        Some(p) => attest::new_platform_statement(&key, "", &head, store::now_ms(), m, p.as_ref()).map_err(anyhow::Error::msg)?,
        None => attest::new_statement(&key, "", &head, store::now_ms(), m).sign(&key).map_err(anyhow::Error::msg)?,
    };
    let claimed = attest::Statement::parse(&text).map_err(anyhow::Error::msg)?.claimed;
    let e = store::append_kept(&log, &key, "attestation", text.as_bytes(), store::now_ms())?;
    match &provider {
        Some(p) => println!("attested as entry {} in {} with {} evidence, claiming {claimed}", e.seq, log.display(), p.kind()),
        None => println!("attested as entry {} in {} (self-attested, T0)", e.seq, log.display()),
    }
    Ok(())
}

fn pin_cmd(pos: &[&String]) -> Result<()> {
    let path = platform::pins_path(&store::home());
    let mut pins = platform::PinStore::load(&path)?;
    match (pos.get(1).map(|s| s.as_str()), pos.get(2), pos.get(3)) {
        (Some("list"), _, _) => {
            if pins.list().is_empty() {
                println!("no pinned device keys ({})", path.display());
            }
            for (k, f) in pins.list() {
                println!("{k} {f}");
            }
        }
        (Some("add"), Some(k), Some(f)) => {
            pins.add(k, f)?;
            println!("pinned {k} {f}");
        }
        (Some("remove"), Some(k), Some(f)) => println!("{}", if pins.remove(k, f)? { "removed" } else { "was not pinned" }),
        _ => bail!("usage: playbox pin list | add <kind> <fingerprint> | remove <kind> <fingerprint>"),
    }
    Ok(())
}

fn verify(path: &str) -> Result<()> {
    let home = store::home();
    let pins = platform::PinStore::load(&platform::pins_path(&home))?;
    let vs = platform::verifiers(pins.clone(), platform::boot_reference_from_env().map_err(anyhow::Error::msg)?).map_err(anyhow::Error::msg)?;
    let refs: Vec<&dyn attest::PlatformVerifier> = vs.iter().map(|v| v.as_ref()).collect();
    let r = store::verify_deep(std::path::Path::new(path), &refs)?;
    if let Some(e) = &r.chain_error {
        bail!("INVALID chain: {e}");
    }
    println!("chain valid: {} entries, head {}", r.entries, r.head);
    for a in &r.attestations {
        println!("attestation at entry {} ({}): subject {}…, claims {}, verified {}", a.seq, a.about, &a.subject[..12], a.claimed, a.achieved);
        for n in &a.notes {
            println!("    {n}");
        }
        if let Some((kind, fp)) = a.device.as_deref().and_then(|d| d.split_once(':')) {
            if !pins.contains(kind, fp) {
                println!("    device key not pinned; to trust it: playbox pin add {kind} {fp}");
            }
        }
    }
    if r.attestations.is_empty() {
        println!("no attestations in this history");
    }
    if !r.problems.is_empty() {
        for p in &r.problems {
            eprintln!("PROBLEM: {p}");
        }
        bail!("{} problem(s) found", r.problems.len());
    }
    Ok(())
}

fn demo() -> Result<()> {
    let key = SigningKey::from_bytes(&playbox_net::random_bytes()?);
    let mut log = Log::new();
    for (kind, payload) in [("session_start", "demo"), ("tool_call", "cargo test"), ("commit", "tree:demo")] {
        log.append(&key, kind, payload.as_bytes(), 0);
    }
    match log.verify() {
        Ok(()) => println!("ok: {} entries verified", log.entries.len()),
        Err(e) => bail!("invalid: {e}"),
    }
    Ok(())
}
