use anyhow::{bail, Result};
use playbox_core::{attest, platform, store};
use playbox_net::{join_link, new_secret_key, Host, HostConfig, Transport, TransportChoice};
use playbox_runner::{runner_by_name, SandboxPolicy, TaskSpec};
use crate::cli::{JoinArgs, TransportArgs, UpArgs};
use std::{path::PathBuf, time::Duration};

pub fn chosen(t: &TransportArgs) -> Result<Transport> {
    let (local, t) = (t.local, t.transport.as_deref());
    match (t, local) {
        (Some(t), _) => Transport::parse(t),
        (None, true) => Ok(Transport::Local),
        (None, false) => Transport::from_env(),
    }
}

pub fn join_choice(t: &TransportArgs) -> Result<TransportChoice> {
    let (local, t) = (t.local, t.transport.as_deref());
    match (t, local) {
        (Some(t), _) => Ok(TransportChoice::Use(Transport::parse(t)?)),
        (None, true) => Ok(TransportChoice::Use(Transport::Local)),
        (None, false) => Ok(TransportChoice::FromLink),
    }
}

pub async fn up(a: UpArgs) -> Result<()> {
    let transport = chosen(&a.transport)?;
    let (state, repo) = (a.state.clone(), a.repo.clone());
    let runtime = a.runtime.clone().or_else(|| a.docker.then(|| "docker".to_string()));
    let provider: Option<std::sync::Arc<dyn attest::PlatformProvider>> = platform::detect(&store::home()).map_err(anyhow::Error::msg)?.map(std::sync::Arc::from);
    println!("platform attestation: {}", provider.as_ref().map(|p| p.kind()).unwrap_or("none (self-signed only)"));
    let steer_image = a.steer_image.clone();
    let steer_mem = a.steer_mem_mib;
    let has_steer = a.steer_store.is_some();
    let mut limits = playbox_runner::Limits {
        allowed_hosts: a.allow_host.clone(),
        ..Default::default()
    };
    if has_steer {
        // The grant's task caps come from these limits, so steering's needs raise them.
        limits.mem_mib = limits.mem_mib.max(steer_mem);
        limits.timeout_secs = limits.timeout_secs.max(600);
    }
    let cfg = HostConfig {
        // A host with a state directory logs into it; one without logs to stderr, so activity is never invisible.
        audit_log: a.audit_log.clone().map(PathBuf::from).or_else(|| state.is_none().then(|| PathBuf::from("-"))),
        repo: repo.map(PathBuf::from),
        runner: match runtime {
            Some(name) => {
                let mut policy = SandboxPolicy::default();
                if has_steer {
                    policy.allowed_images.push(steer_image.clone());
                }
                Some(runner_by_name(&name, policy)?)
            }
            None => None,
        },
        steer: match a.steer_store.clone() {
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
            let mut l = playbox_net::limits::AbuseLimits::default();
            if let Some(v) = a.max_members { l.max_members = v as usize }
            if let Some(v) = a.max_concurrent { l.max_concurrent_per_member = v as usize }
            if let Some(v) = a.max_tasks_per_minute { l.max_tasks_per_window = v as usize }
            if let Some(v) = a.task_budget { l.task_budget = v }
            l
        },
        ..Default::default()
    };
    let host = match (&state, &a.replica_of) {
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
    if !a.sync_peer.is_empty() {
        let peers = a.sync_peer.iter().map(|p| playbox_net::decode_peer(p)).collect::<Result<Vec<_>>>()?;
        host.enable_revocation_sync(peers, Duration::from_secs(a.sync_interval.max(1))).await?;
        println!("revocation sync on with {} peer(s), pulling every {}s", a.sync_peer.len(), a.sync_interval.max(1));
    }
    if a.replica_of.is_some() {
        println!("replica up ({}): serves the room's grants, cannot admit members; join through the primary.", transport.label());
    } else {
        let link = host.create_link(Duration::from_secs(3600), 1)?;
        println!("room is up ({}). share this link:\n{}", transport.label(), link.encode());
    }
    if a.console {
        println!("host console: link | members | audit [N] | revoke <member id> | quit");
        console(&host).await?;
    } else {
        println!("(ctrl-c to stop)");
        tokio::signal::ctrl_c().await?;
    }
    host.shutdown().await
}

pub async fn join(a: JoinArgs) -> Result<()> {
    let (link, choice, task, base, via) = (a.link.as_str(), join_choice(&a.transport)?, a.task, Some(a.base), a.via);
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

pub async fn bridge(link: &str, choice: TransportChoice, listen: Option<String>, token: Option<String>) -> Result<()> {
    let client = join_link(link, new_secret_key()?, choice).await?;
    let (listener, addr) = playbox_bridge::bind(listen.as_deref().unwrap_or("127.0.0.1:8787")).await?;
    let url = format!("http://{addr}");
    println!("bridge up: agent card at {url}/.well-known/agent-card.json, JSON-RPC at {url}/jsonrpc");
    println!("{}", if token.is_some() { "bearer token required" } else { "no token (loopback only)" });
    playbox_bridge::serve(client, listener, token, url).await
}


async fn console(host: &Host) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            line = lines.next_line() => {
                let Some(line) = line? else {
                    // stdin closed (daemonised): keep serving until ctrl-c.
                    tokio::signal::ctrl_c().await?;
                    return Ok(());
                };
                let mut w = line.split_whitespace();
                match (w.next(), w.next()) {
                    (None, _) => {}
                    (Some("quit" | "exit"), _) => return Ok(()),
                    (Some("link"), _) => println!("{}", host.create_link(Duration::from_secs(3600), 1)?.encode()),
                    (Some("members"), _) => {
                        let m = host.members();
                        println!("{} member(s)", m.len());
                        for id in m {
                            println!("  {id}");
                        }
                    }
                    (Some("audit"), n) => match host.audit_path() {
                        Some(p) => {
                            for e in playbox_net::audit::tail(&p, n.and_then(|n| n.parse().ok()).unwrap_or(20))? {
                                println!("{}", playbox_net::audit::describe(&e));
                            }
                        }
                        None => println!("no audit log file; start with --state DIR or --audit-log FILE"),
                    },
                    (Some("revoke"), Some(id)) => match id.parse::<playbox_net::EndpointId>() {
                        Ok(id) => println!("{}", if host.revoke_member(&id) { "revoked" } else { "not a current member" }),
                        Err(e) => println!("not a member id: {e}"),
                    },
                    _ => println!("link | members | audit [N] | revoke <member id> | quit"),
                }
            }
        }
    }
}
