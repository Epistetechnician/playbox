use anyhow::{bail, Result};
use ed25519_dalek::SigningKey;
use playbox_core::{attest, history::Log, platform, store};
use playbox_net::{join_link, new_secret_key, Host, HostConfig, Transport, TransportChoice};
use playbox_runner::{runner_by_name, SandboxPolicy, TaskSpec};
use std::{path::PathBuf, time::Duration};

const HELP: &str = "playbox (scaffold)
  up [--transport T] [--state DIR] [--repo DIR] [--runtime docker|libkrun|openshell] [--allow-host H]... [--steer-store DIR] [--steer-image IMG] [--steer-mem-mib N]
                            start a room and print a join link (valid 1h, 1 use).
                            T is `iroh` (public relays, default), `local` (same machine/LAN) or
                            `relay:<https url>` (your own iroh-relay; PLAYBOX_RELAY_CA=PEM for a private CA).
                            --state keeps the room's identity, members and bans across restarts.
                            --repo shares a git repo; --runtime lets members run tasks: libkrun = a microVM per task, docker = a container (--docker is shorthand), openshell = an NVIDIA OpenShell sandbox.
                            --allow-host H (repeatable; `host` or `host:port`) lets members' tasks reach just those hosts; only the openshell runtime enforces it
                            limits: --max-members N (64), --max-concurrent N tasks per member (2), --max-tasks-per-minute N per member (30),
                            --task-budget N tasks for the whole room (100000). Revoking a member also stops their running tasks.
  review <repo> [list | show REF | accept REF | reject REF] [--into BRANCH]
                            decide what members pushed. A push waits under refs/playbox/members/ and never moves a branch;
                            `accept` fast-forwards BRANCH (default main) to it, `reject` retires it. Run it on the host, by a person.
  join <link> [--transport T] [--task <shell cmd>] [--base REF] [--via <replica peer address>]
                            join a room (the link says which transport to use unless you override);
                            with --task, run it remotely on REF (default main) and print the patch
  (--local is shorthand for --transport local; PLAYBOX_TRANSPORT sets the default)
  (multi-host rooms: `up --state DIR` prints the room's root public key and a peer address. Start another host
   of the same room with `up --state DIR2 --replica-of ROOTKEY`, and give each host the other's address with
   `--sync-peer ADDR` (repeatable, `--sync-interval SECS`, default 15) so revocations reach every host.)
  bridge <link> [--listen ADDR] [--token T] [--transport T]
                            join a room and serve it to stock A2A HTTP clients (default 127.0.0.1:8787).
                            Callers act as this one member. A non-loopback --listen requires --token.
  record [--kind K] [--log F] [--key F]
                            append stdin as a signed entry to a history log (agent hook target)
  audit <file> [--tail N]   check a host's audit log (chain and signatures) and show its last N entries (default 20).
                            `playbox up --state DIR` keeps one at DIR/audit.log; `--audit-log FILE` chooses the file, `-` is stderr
  attest [--log F] [--key F] [--platform auto|none|se|tpm]
                            append a signed attestation of this binary to a history log, with hardware
                            evidence when available (Secure Enclave on macOS, TPM 2.0 on Linux).
                            Whether anyone believes the hardware part depends on their pins (see `pin`)
  pin list | add <kind> <fingerprint> | remove <kind> <fingerprint>
                            the device keys you trust (kind: secure-enclave or tpm). `verify` and `join`
                            report fingerprints; a device only reaches tier T1 once pinned
  approver-key [--key F]    create (or show) your approver key; put its public key in the agent's PLAYBOX_APPROVERS
  approve --action A --arg TEXT [--key F] [--ttl SECS]
                            approve one action your agent was refused because its arguments copy another
                            member's output. Prints a single-use capability bound to exactly this action
                            and these arguments (--arg repeatable); give it to the agent as `capability`
  memory-review <entry id> --accept|--reject [--key F]
                            sign a reviewer's decision on a quarantined memory entry in $PLAYBOX_HOME/memory.json with
                            your approver key; give the printed event to the agent's `playbox_memory_ingest`
  verify <log>              verify a history log: chain, signatures and attestations
  demo                      build and verify a sample history log
planned: status | delegate | verify | revoke (see PRD.md)";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let local = args.iter().any(|a| a == "--local");
    let transport = args.iter().position(|a| a == "--transport").and_then(|i| args.get(i + 1)).cloned();
    let state = args.iter().position(|a| a == "--state").and_then(|i| args.get(i + 1)).cloned();
    let runtime = args.iter().position(|a| a == "--runtime").and_then(|i| args.get(i + 1)).cloned()
        .or_else(|| args.iter().any(|a| a == "--docker").then(|| "docker".to_string()));
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
            if matches!(a.as_str(), "--repo" | "--task" | "--base" | "--kind" | "--log" | "--key" | "--transport" | "--state" | "--listen" | "--token" | "--platform" | "--runtime" | "--allow-host" | "--action" | "--arg" | "--ttl" | "--audit-log" | "--tail" | "--replica-of" | "--sync-peer" | "--sync-interval" | "--via" | "--into" | "--steer-store" | "--steer-image" | "--steer-mem-mib" | "--max-members" | "--max-concurrent" | "--max-tasks-per-minute" | "--task-budget") {
                skip = true;
                return false;
            }
            !a.starts_with("--")
        })
        .collect();
    match pos.first().map(|s| s.as_str()) {
        Some("up") => {
            let all = |name: &str| -> Vec<String> { args.windows(2).filter(|w| w[0] == name).map(|w| w[1].clone()).collect() };
            let sync = SyncArgs {
                replica_of: opt("--replica-of"),
                peers: all("--sync-peer"),
                interval: opt("--sync-interval").and_then(|v| v.parse().ok()).unwrap_or(15),
            };
            up(chosen(local, transport.as_deref())?, state, repo, runtime, sync).await
        }
        Some("join") => match pos.get(1) {
            Some(link) => join(link, join_choice(local, transport.as_deref())?, task, base, opt("--via")).await,
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
        Some("approver-key") => approver_key(key_path),
        Some("memory-review") => match pos.get(1) {
            Some(id) => memory_review(id, args.iter().any(|a| a == "--accept"), args.iter().any(|a| a == "--reject"), key_path),
            None => bail!("usage: playbox memory-review <entry id> --accept|--reject"),
        },
        Some("approve") => {
            let action = opt("--action").ok_or_else(|| anyhow::anyhow!("usage: playbox approve --action A --arg TEXT"))?;
            let call_args: Vec<&str> = args.iter().enumerate().filter(|(i, a)| a.as_str() == "--arg" && *i + 1 < args.len()).map(|(i, _)| args[i + 1].as_str()).collect();
            approve(&action, &call_args, key_path, opt("--ttl").and_then(|t| t.parse().ok()).unwrap_or(300))
        }
        Some("pin") => pin_cmd(&pos),
        Some("review") => review_cmd(&pos, opt("--into").unwrap_or_else(|| "main".into())).await,
        Some("audit") => match pos.get(1) {
            Some(path) => audit_cmd(path, opt("--tail").and_then(|n| n.parse().ok()).unwrap_or(20)),
            None => bail!("usage: playbox audit <file> [--tail N]"),
        },
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

struct SyncArgs {
    replica_of: Option<String>,
    peers: Vec<String>,
    interval: u64,
}

async fn up(transport: Transport, state: Option<String>, repo: Option<String>, runtime: Option<String>, sync: SyncArgs) -> Result<()> {
    let provider: Option<std::sync::Arc<dyn attest::PlatformProvider>> = platform::detect(&store::home()).map_err(anyhow::Error::msg)?.map(std::sync::Arc::from);
    println!("platform attestation: {}", provider.as_ref().map(|p| p.kind()).unwrap_or("none (self-signed only)"));
    let flag = |name: &str| std::env::args().skip_while(|a| a != name).nth(1);
    let steer_image = flag("--steer-image").unwrap_or_else(|| "playbox-steer:0.1".into());
    let steer_mem: u32 = flag("--steer-mem-mib").map(|v| v.parse()).transpose().map_err(|_| anyhow::anyhow!("--steer-mem-mib takes a whole number"))?.unwrap_or(4096);
    let mut limits = playbox_runner::Limits {
        allowed_hosts: std::env::args().collect::<Vec<_>>().windows(2).filter(|w| w[0] == "--allow-host").map(|w| w[1].clone()).collect(),
        ..Default::default()
    };
    if flag("--steer-store").is_some() {
        // The grant's task caps come from these limits, so steering's needs raise them.
        limits.mem_mib = limits.mem_mib.max(steer_mem);
        limits.timeout_secs = limits.timeout_secs.max(600);
    }
    let cfg = HostConfig {
        // A host with a state directory logs into it; one without logs to stderr, so activity is never invisible.
        audit_log: std::env::args().skip_while(|a| a != "--audit-log").nth(1).map(PathBuf::from).or_else(|| state.is_none().then(|| PathBuf::from("-"))),
        repo: repo.map(PathBuf::from),
        runner: match runtime {
            Some(name) => {
                let mut policy = SandboxPolicy::default();
                if flag("--steer-store").is_some() {
                    policy.allowed_images.push(steer_image.clone());
                }
                Some(runner_by_name(&name, policy)?)
            }
            None => None,
        },
        steer: match flag("--steer-store") {
            None => None,
            Some(store) => Some(std::sync::Arc::new(playbox_net::steer::SteerHost::open(
                std::path::Path::new(&store),
                &steer_image,
                playbox_net::steer::SteerLimits { cpus: limits.cpus, mem_mib: steer_mem, timeout_secs: limits.timeout_secs },
                playbox_net::grants::SteerCaps {
                    models: vec![],
                    layer_min: 0,
                    layer_max: 255,
                    max_coef_milli: 4000,
                    modes: vec!["add".into(), "clamp".into()],
                    directions: None,
                    max_tokens_per_run: 4096,
                    classes: vec![],
                    probe: true,
                    max_runs: 50,
                    max_tokens_total: 100_000,
                },
                playbox_core::steer::Ceiling::LocalInternalExploratory,
            )?)),
        },
        platform: provider,
        limits,
        abuse: {
            let n = |flag: &str| -> Result<Option<u64>> {
                std::env::args().skip_while(|a| a != flag).nth(1).map(|v| v.parse::<u64>().map_err(|_| anyhow::anyhow!("{flag} takes a whole number"))).transpose()
            };
            let mut a = playbox_net::limits::AbuseLimits::default();
            if let Some(v) = n("--max-members")? { a.max_members = v as usize }
            if let Some(v) = n("--max-concurrent")? { a.max_concurrent_per_member = v as usize }
            if let Some(v) = n("--max-tasks-per-minute")? { a.max_tasks_per_window = v as usize }
            if let Some(v) = n("--task-budget")? { a.task_budget = v }
            a
        },
        ..Default::default()
    };
    let host = match (&state, &sync.replica_of) {
        (Some(dir), Some(root)) => {
            let bytes = root.trim();
            if bytes.len() != 64 || !bytes.is_ascii() {
                bail!("--replica-of takes the room's root public key: 64 hex characters");
            }
            let mut key = [0u8; 32];
            for (i, b) in key.iter_mut().enumerate() {
                *b = u8::from_str_radix(&bytes[i * 2..i * 2 + 2], 16)?;
            }
            Host::open_replica(std::path::Path::new(dir), transport.clone(), cfg, key).await?
        }
        (None, Some(_)) => bail!("--replica-of needs --state DIR so the replica keeps its identity and revocations"),
        (Some(dir), None) => Host::open(std::path::Path::new(dir), transport.clone(), cfg).await?,
        (None, None) => Host::start_with(new_secret_key()?, transport.clone(), cfg).await?,
    };
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    println!("room root public key: {}", hex(&host.root_public()));
    println!("peer address (for other hosts' --sync-peer): {}", playbox_net::encode_peer(&host.addr()));
    if !sync.peers.is_empty() {
        let peers = sync.peers.iter().map(|p| playbox_net::decode_peer(p)).collect::<Result<Vec<_>>>()?;
        host.enable_revocation_sync(peers, Duration::from_secs(sync.interval.max(1))).await?;
        println!("revocation sync on with {} peer(s), pulling every {}s", sync.peers.len(), sync.interval.max(1));
    }
    if sync.replica_of.is_some() {
        println!("replica up ({}): serves the room's grants, cannot admit members; join through the primary.", transport.label());
    } else {
        let link = host.create_link(Duration::from_secs(3600), 1)?;
        println!("room is up ({}). share this link:\n{}", transport.label(), link.encode());
    }
    println!("(ctrl-c to stop)");
    tokio::signal::ctrl_c().await?;
    host.shutdown().await
}

async fn join(link: &str, choice: TransportChoice, task: Option<String>, base: Option<String>, via: Option<String>) -> Result<()> {
    let primary = join_link(link, new_secret_key()?, choice).await?;
    // With --via, do the work on another host of the room (a replica) as the same member.
    let (client, primary) = match via {
        Some(addr) => {
            let replica = primary.sibling(playbox_net::decode_peer(&addr)?).await?;
            replica.verify_host(attest::Tier::T0, &[]).await.map_err(|e| anyhow::anyhow!("not using that host: {e:#}"))?;
            println!("using replica {}", replica.host_id().fmt_short());
            (replica, Some(primary))
        }
        None => (primary, None),
    };
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
    if let Some(p) = primary {
        p.close().await;
    }
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

fn approver_key_path(key: Option<String>) -> PathBuf {
    key.map(PathBuf::from).unwrap_or_else(|| store::home().join("approver.key"))
}

fn approver_key(key: Option<String>) -> Result<()> {
    let k = store::load_or_create_key(&approver_key_path(key))?;
    println!("{}", playbox_core::record::hex(&k.verifying_key().to_bytes()));
    eprintln!("approver public key above; set PLAYBOX_APPROVERS to it for the agent's playbox-mcp");
    Ok(())
}

fn memory_review(id: &str, accept: bool, reject: bool, key: Option<String>) -> Result<()> {
    if accept == reject {
        bail!("give exactly one of --accept or --reject");
    }
    let home = store::home();
    let session = store::load_or_create_key(&home.join("session.key"))?;
    let reviewer = store::load_or_create_key(&approver_key_path(key))?;
    let log: Vec<String> = serde_json::from_slice(&std::fs::read(home.join("memory.json"))?)?;
    let owner = session.verifying_key().to_bytes();
    let mut mem = playbox_core::memory::MemoryStore::replay(&[owner], &[reviewer.verifying_key().to_bytes()], &log).map_err(anyhow::Error::msg)?;
    let full = mem.log().iter().filter_map(|t| mem.entry_for_event(t)).map(|e| e.id.clone()).find(|e| e.starts_with(id)).ok_or_else(|| anyhow::anyhow!("no entry starting with {id}"))?;
    let e = mem.entry(&full).unwrap();
    eprintln!("{} {:?} {}: {}", if accept { "accepting" } else { "rejecting" }, e.kind, e.topic, e.body);
    println!("{}", mem.review(&full, accept, &reviewer).map_err(anyhow::Error::msg)?);
    Ok(())
}

fn approve(action: &str, call_args: &[&str], key: Option<String>, ttl_secs: u64) -> Result<()> {
    let k = store::load_or_create_key(&approver_key_path(key))?;
    eprintln!("approving {action} with {} argument(s):", call_args.len());
    for a in call_args {
        eprintln!("  {a}");
    }
    let cap = playbox_core::taint::issue_capability(action, call_args, &k, store::now_ms(), ttl_secs * 1000).map_err(anyhow::Error::msg)?;
    println!("{cap}");
    Ok(())
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

fn audit_cmd(path: &str, tail: usize) -> Result<()> {
    let p = std::path::Path::new(path);
    let s = playbox_net::audit::verify(p)?;
    println!("audit log valid: {} entries, signed by host key {}…", s.entries, &s.host_key[..16]);
    println!("head {} (keep this elsewhere to detect later truncation)", s.head);
    for e in playbox_net::audit::tail(p, tail)? {
        println!("{}", playbox_net::audit::describe(&e));
    }
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

async fn review_cmd(pos: &[&String], into: String) -> Result<()> {
    use playbox_net::review;
    let Some(repo) = pos.get(1) else { bail!("usage: playbox review <repo> [list | show REF | accept REF | reject REF] [--into BRANCH]") };
    let repo = std::path::Path::new(repo.as_str());
    match (pos.get(2).map(|s| s.as_str()), pos.get(3)) {
        (None, _) | (Some("list"), _) => {
            let list = review::pending(repo).await?;
            if list.is_empty() {
                println!("nothing is waiting for review");
            }
            for p in list {
                println!("{}  {}", p.reference, &p.head[..p.head.len().min(12)]);
            }
            Ok(())
        }
        (Some("show"), Some(r)) => {
            println!("{}", review::show(repo, r, &into).await?);
            Ok(())
        }
        (Some("accept"), Some(r)) => {
            let tip = review::accept(repo, r, &into).await?;
            println!("accepted: {into} is now {tip}");
            Ok(())
        }
        (Some("reject"), Some(r)) => {
            review::reject(repo, r).await?;
            println!("rejected; the commits are kept under refs/playbox/rejected/");
            Ok(())
        }
        _ => bail!("usage: playbox review <repo> [list | show REF | accept REF | reject REF] [--into BRANCH]"),
    }
}
