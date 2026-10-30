use anyhow::{bail, Result};
use ed25519_dalek::SigningKey;
use playbox_core::{attest, history::Log, platform, store};
use std::path::PathBuf;

pub fn approver_key_path(key: Option<String>) -> PathBuf {
    key.map_or_else(|| store::home().join("approver.key"), PathBuf::from)
}

pub fn approver_key(key: Option<String>) -> Result<()> {
    let k = store::load_or_create_key(&approver_key_path(key))?;
    println!(
        "{}",
        playbox_core::record::hex(&k.verifying_key().to_bytes())
    );
    eprintln!("approver public key above; set PLAYBOX_APPROVERS to it for the agent's playbox-mcp");
    Ok(())
}

pub fn memory_review(id: &str, accept: bool, reject: bool, key: Option<String>) -> Result<()> {
    if accept == reject {
        bail!("give exactly one of --accept or --reject");
    }
    let home = store::home();
    let session = store::load_or_create_key(&home.join("session.key"))?;
    let reviewer = store::load_or_create_key(&approver_key_path(key))?;
    let log: Vec<String> = serde_json::from_slice(&std::fs::read(home.join("memory.json"))?)?;
    let owner = session.verifying_key().to_bytes();
    let mut mem = playbox_core::memory::MemoryStore::replay(
        &[owner],
        &[reviewer.verifying_key().to_bytes()],
        &log,
    )
    .map_err(anyhow::Error::msg)?;
    let full = mem
        .log()
        .iter()
        .filter_map(|t| mem.entry_for_event(t))
        .map(|e| e.id.clone())
        .find(|e| e.starts_with(id))
        .ok_or_else(|| anyhow::anyhow!("no entry starting with {id}"))?;
    let e = mem.entry(&full).unwrap();
    eprintln!(
        "{} {:?} {}: {}",
        if accept { "accepting" } else { "rejecting" },
        e.kind,
        e.topic,
        e.body
    );
    println!(
        "{}",
        mem.review(&full, accept, &reviewer)
            .map_err(anyhow::Error::msg)?
    );
    Ok(())
}

pub fn approve(action: &str, call_args: &[&str], key: Option<String>, ttl_secs: u64) -> Result<()> {
    let k = store::load_or_create_key(&approver_key_path(key))?;
    eprintln!("approving {action} with {} argument(s):", call_args.len());
    for a in call_args {
        eprintln!("  {a}");
    }
    let cap = playbox_core::taint::issue_capability(
        action,
        call_args,
        &k,
        store::now_ms(),
        ttl_secs * 1000,
    )
    .map_err(anyhow::Error::msg)?;
    println!("{cap}");
    Ok(())
}

pub fn record(kind: Option<String>, log: Option<String>, key: Option<String>) -> Result<()> {
    let home = store::home();
    let key =
        store::load_or_create_key(&key.map_or_else(|| home.join("session.key"), PathBuf::from))?;
    let log = log
        .or_else(|| std::env::var("PLAYBOX_LOG").ok())
        .map_or_else(|| home.join("history").join("default.log"), PathBuf::from);
    let mut payload = Vec::new();
    std::io::Read::read_to_end(&mut std::io::stdin(), &mut payload)?;
    let e = store::append(
        &log,
        &key,
        kind.as_deref().unwrap_or("tool_call"),
        &payload,
        store::now_ms(),
    )?;
    eprintln!("recorded entry {} in {}", e.seq, log.display());
    Ok(())
}

pub fn audit_cmd(path: &str, tail: usize) -> Result<()> {
    let p = std::path::Path::new(path);
    let s = playbox_net::audit::verify(p)?;
    println!(
        "audit log valid: {} entries, signed by host key {}…",
        s.entries,
        &s.host_key[..16]
    );
    println!(
        "head {} (keep this elsewhere to detect later truncation)",
        s.head
    );
    for e in playbox_net::audit::tail(p, tail)? {
        println!("{}", playbox_net::audit::describe(&e));
    }
    Ok(())
}

pub fn attest_cmd(
    log: Option<String>,
    key: Option<String>,
    platform_mode: Option<String>,
) -> Result<()> {
    let home = store::home();
    let key =
        store::load_or_create_key(&key.map_or_else(|| home.join("session.key"), PathBuf::from))?;
    let log = log
        .or_else(|| std::env::var("PLAYBOX_LOG").ok())
        .map_or_else(|| home.join("history").join("default.log"), PathBuf::from);
    let head = if log.exists() {
        playbox_core::history::text::head_hex(&store::load(&log)?)
    } else {
        playbox_core::history::text::hex(&[0u8; 32])
    };
    let provider = match &platform_mode {
        Some(m) => platform::detect_with(&home, m),
        None => platform::detect(&home),
    }
    .map_err(anyhow::Error::msg)?;
    let m = attest::measure_self("none", "cli");
    let text = match &provider {
        Some(p) => attest::new_platform_statement(&key, "", &head, store::now_ms(), m, p.as_ref())
            .map_err(anyhow::Error::msg)?,
        None => attest::new_statement(&key, "", &head, store::now_ms(), m)
            .sign(&key)
            .map_err(anyhow::Error::msg)?,
    };
    let claimed = attest::Statement::parse(&text)
        .map_err(anyhow::Error::msg)?
        .claimed;
    let e = store::append_kept(&log, &key, "attestation", text.as_bytes(), store::now_ms())?;
    match &provider {
        Some(p) => println!(
            "attested as entry {} in {} with {} evidence, claiming {claimed}",
            e.seq,
            log.display(),
            p.kind()
        ),
        None => println!(
            "attested as entry {} in {} (self-attested, T0)",
            e.seq,
            log.display()
        ),
    }
    Ok(())
}

pub fn pin_cmd(action: &str, kind: Option<&str>, fingerprint: Option<&str>) -> Result<()> {
    let path = platform::pins_path(&store::home());
    let mut pins = platform::PinStore::load(&path)?;
    match (action, kind, fingerprint) {
        ("list", _, _) => {
            if pins.list().is_empty() {
                println!("no pinned device keys ({})", path.display());
            }
            for (k, f) in pins.list() {
                println!("{k} {f}");
            }
        }
        ("add", Some(k), Some(f)) => {
            pins.add(k, f)?;
            println!("pinned {k} {f}");
        }
        ("remove", Some(k), Some(f)) => println!(
            "{}",
            if pins.remove(k, f)? {
                "removed"
            } else {
                "was not pinned"
            }
        ),
        _ => bail!(
            "usage: playbox pin list | add <kind> <fingerprint> | remove <kind> <fingerprint>"
        ),
    }
    Ok(())
}

pub fn verify(path: &str) -> Result<()> {
    let home = store::home();
    let pins = platform::PinStore::load(&platform::pins_path(&home))?;
    let vs = platform::verifiers(
        pins.clone(),
        platform::boot_reference_from_env().map_err(anyhow::Error::msg)?,
    )
    .map_err(anyhow::Error::msg)?;
    let refs: Vec<&dyn attest::PlatformVerifier> =
        vs.iter().map(std::convert::AsRef::as_ref).collect();
    let r = store::verify_deep(std::path::Path::new(path), &refs)?;
    if let Some(e) = &r.chain_error {
        bail!("INVALID chain: {e}");
    }
    println!("chain valid: {} entries, head {}", r.entries, r.head);
    for a in &r.attestations {
        println!(
            "attestation at entry {} ({}): subject {}…, claims {}, verified {}",
            a.seq,
            a.about,
            &a.subject[..12],
            a.claimed,
            a.achieved
        );
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

pub fn demo() -> Result<()> {
    let key = SigningKey::from_bytes(&playbox_net::random_bytes()?);
    let mut log = Log::new();
    for (kind, payload) in [
        ("session_start", "demo"),
        ("tool_call", "cargo test"),
        ("commit", "tree:demo"),
    ] {
        log.append(&key, kind, payload.as_bytes(), 0);
    }
    match log.verify() {
        Ok(()) => println!("ok: {} entries verified", log.entries.len()),
        Err(e) => bail!("invalid: {e}"),
    }
    Ok(())
}

pub async fn review_cmd(repo: &str, action: &[String], into: String) -> Result<()> {
    use playbox_net::review;
    let repo = std::path::Path::new(repo);
    match (action.first().map(std::string::String::as_str), action.get(1)) {
        (None | Some("list"), _) => {
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
