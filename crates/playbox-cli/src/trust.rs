use crate::cli::{PinCmd, ReceiptCmd};
use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use playbox_core::{
    attest,
    history::Log,
    platform, pq,
    record::{hex, unhex},
    store,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub(crate) const RECEIPT_SCHEMA: &str = "playbox.attested_gate_receipt.v1";
const RECEIPT_DOMAIN: &str = "playbox.attested_gate_receipt.v1";
const RECEIPT_PQ_DOMAIN: &str = "playbox.attested_gate_receipt.pq.v1";
const RECEIPT_PQ_CTX: &[u8] = b"playbox.attested_gate_receipt.pq.v1";

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
    let e = mem
        .entry(&full)
        .ok_or_else(|| anyhow::anyhow!("entry {full} disappeared while reading it"))?;
    eprintln!(
        "{} {:?} {}: {}",
        if accept { "accepting" } else { "rejecting" },
        e.kind,
        e.topic,
        e.body
    );
    let tok = mem
        .review(&full, accept, &reviewer)
        .map_err(anyhow::Error::msg)?;
    std::fs::write(home.join("memory.json"), serde_json::to_vec(mem.log())?)?;
    println!("{tok}");
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
    let s = playbox_net::audit::verify(p).with_context(|| format!("cannot verify {path}"))?;
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

pub fn pin_cmd(cmd: PinCmd) -> Result<()> {
    let path = platform::pins_path(&store::home());
    let mut pins = platform::PinStore::load(&path)?;
    match cmd {
        PinCmd::List => {
            if pins.list().is_empty() {
                println!("no pinned device keys ({})", path.display());
            }
            for (k, f) in pins.list() {
                println!("{k} {f}");
            }
        }
        PinCmd::Add { kind, fingerprint } => {
            pins.add(&kind, &fingerprint)?;
            println!("pinned {kind} {fingerprint}");
        }
        PinCmd::Remove { kind, fingerprint } => println!(
            "{}",
            if pins.remove(&kind, &fingerprint)? {
                "removed"
            } else {
                "was not pinned"
            }
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
    let r = store::verify_deep(std::path::Path::new(path), &refs)
        .with_context(|| format!("cannot verify {path}"))?;
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

fn canonical_json_bytes(v: &Value) -> Result<Vec<u8>> {
    let raw = serde_json::to_string(v)?;
    let mut out = String::with_capacity(raw.len());
    let mut utf16 = [0u16; 2];
    for ch in raw.chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else {
            for unit in ch.encode_utf16(&mut utf16) {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{unit:04x}");
            }
        }
    }
    Ok(out.into_bytes())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex(&digest)
}

fn unhex_vec(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        bail!("must be even-length hex");
    }
    (0..s.len() / 2)
        .map(|i| {
            u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
                .map_err(|_| anyhow::anyhow!("invalid hex byte"))
        })
        .collect()
}

pub fn sign_receipt_value(doc: &Value, key: &SigningKey) -> Result<Value> {
    let payload = doc
        .get("payload")
        .filter(|v| v.is_object())
        .unwrap_or(doc)
        .clone();
    if payload.get("schema").and_then(Value::as_str) != Some(RECEIPT_SCHEMA) {
        bail!(
            "unexpected receipt schema: {:?}",
            payload.get("schema").and_then(Value::as_str)
        );
    }
    let payload_sha256 = sha256_hex(&canonical_json_bytes(&payload)?);
    if let Some(claimed) = doc.get("payload_sha256").and_then(Value::as_str) {
        if !claimed.is_empty() && claimed != payload_sha256 {
            bail!("payload_sha256 mismatch: receipt payload does not match claimed hash");
        }
    }
    let msg = format!("{RECEIPT_DOMAIN}\n{payload_sha256}").into_bytes();
    let ed_pk = key.verifying_key().to_bytes();
    let ed_sig = key.sign(&msg).to_bytes();
    let (pq_pk, pq_sk) = pq::derive_mldsa65_from_ed25519_seed(&key.to_bytes());
    let pq_sig = pq::sign_mldsa65(&pq_sk, &msg, RECEIPT_PQ_CTX)?;

    Ok(json!({
        "payload": payload,
        "payload_sha256": payload_sha256,
        "signature": {
            "alg": "Ed25519+ML-DSA-65",
            "domain": RECEIPT_DOMAIN,
            "pq_alg": "ML-DSA-65",
            "pq_domain": RECEIPT_PQ_DOMAIN,
            "signed": true,
            "pubkey_hex": hex(&ed_pk),
            "sig_hex": hex(&ed_sig),
            "pq_pubkey_hex": hex(&pq_pk),
            "pq_sig_hex": hex(&pq_sig),
        }
    }))
}

pub fn verify_receipt_value(
    doc: &Value,
    expected_pubkey: Option<&str>,
    expected_pq_pubkey: Option<&str>,
    require_pq: bool,
) -> Result<Value> {
    let payload = doc
        .get("payload")
        .filter(|v| v.is_object())
        .ok_or_else(|| anyhow::anyhow!("malformed receipt: missing payload object"))?;
    if payload.get("schema").and_then(Value::as_str) != Some(RECEIPT_SCHEMA) {
        bail!(
            "unexpected receipt schema: {:?}",
            payload.get("schema").and_then(Value::as_str)
        );
    }
    let computed_sha = sha256_hex(&canonical_json_bytes(payload)?);
    let claimed_sha = doc
        .get("payload_sha256")
        .and_then(Value::as_str)
        .unwrap_or("");
    if computed_sha != claimed_sha {
        bail!("payload_sha256 mismatch (receipt modified)");
    }
    let sig = doc
        .get("signature")
        .filter(|v| v.is_object())
        .ok_or_else(|| anyhow::anyhow!("malformed receipt: missing signature object"))?;
    if sig.get("signed").and_then(Value::as_bool) != Some(true) {
        bail!("receipt is not signed");
    }
    if sig.get("domain").and_then(Value::as_str) != Some(RECEIPT_DOMAIN) {
        bail!("unexpected signature domain: {:?}", sig.get("domain"));
    }
    let alg = sig.get("alg").and_then(Value::as_str).unwrap_or("");
    if alg != "Ed25519+ML-DSA-65" && alg != "Ed25519" {
        bail!("unsupported signature algorithm: {alg:?}");
    }
    let pk_hex = sig.get("pubkey_hex").and_then(Value::as_str).unwrap_or("");
    let sig_hex = sig.get("sig_hex").and_then(Value::as_str).unwrap_or("");
    if let Some(exp) = expected_pubkey.filter(|s| !s.trim().is_empty()) {
        if !pk_hex.eq_ignore_ascii_case(exp.trim()) {
            bail!("signer Ed25519 pubkey does not match expected --pubkey");
        }
    }
    let pk_bytes: [u8; 32] =
        unhex(pk_hex).map_err(|e| anyhow::anyhow!("Ed25519 pubkey_hex: {e}"))?;
    let sig_bytes: [u8; 64] =
        unhex(sig_hex).map_err(|e| anyhow::anyhow!("Ed25519 sig_hex: {e}"))?;
    let vk = VerifyingKey::from_bytes(&pk_bytes).context("invalid Ed25519 public key")?;
    let msg = format!("{RECEIPT_DOMAIN}\n{computed_sha}").into_bytes();
    vk.verify(&msg, &Signature::from_bytes(&sig_bytes))
        .context("Ed25519 signature verification failed")?;

    let has_pq_fields = sig
        .get("pq_sig_hex")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    let check_pq = alg == "Ed25519+ML-DSA-65"
        || has_pq_fields
        || require_pq
        || expected_pq_pubkey.is_some_and(|s| !s.trim().is_empty());
    let mut pq_verified = false;
    if check_pq {
        if sig.get("pq_alg").and_then(Value::as_str) != Some("ML-DSA-65") {
            bail!("missing or unsupported post-quantum algorithm (expected ML-DSA-65)");
        }
        if sig.get("pq_domain").and_then(Value::as_str) != Some(RECEIPT_PQ_DOMAIN) {
            bail!(
                "unexpected post-quantum signature domain: {:?}",
                sig.get("pq_domain")
            );
        }
        let pq_pk_hex = sig
            .get("pq_pubkey_hex")
            .and_then(Value::as_str)
            .unwrap_or("");
        let pq_sig_hex = sig.get("pq_sig_hex").and_then(Value::as_str).unwrap_or("");
        if let Some(exp_pq) = expected_pq_pubkey.filter(|s| !s.trim().is_empty()) {
            if !pq_pk_hex.eq_ignore_ascii_case(exp_pq.trim()) {
                bail!("signer ML-DSA-65 pubkey does not match expected --pq-pubkey");
            }
        }
        let pq_pk = unhex_vec(pq_pk_hex).context("ML-DSA-65 pq_pubkey_hex")?;
        let pq_sig = unhex_vec(pq_sig_hex).context("ML-DSA-65 pq_sig_hex")?;
        if !pq::verify_mldsa65(&pq_pk, &msg, &pq_sig, RECEIPT_PQ_CTX) {
            bail!("ML-DSA-65 signature verification failed");
        }
        pq_verified = true;
    }

    Ok(json!({
        "valid": true,
        "alg": alg,
        "ed25519_verified": true,
        "pq_verified": pq_verified,
        "payload_sha256": computed_sha,
        "pubkey_hex": pk_hex,
    }))
}

pub fn receipt_cmd(cmd: ReceiptCmd) -> Result<()> {
    match cmd {
        ReceiptCmd::Sign { file, key, out } => {
            let key_path =
                key.map_or_else(|| store::home().join("attested_gate.key"), PathBuf::from);
            let sk = store::load_or_create_key(&key_path)?;
            let raw = std::fs::read_to_string(&file).with_context(|| format!("reading {file}"))?;
            let doc: Value =
                serde_json::from_str(&raw).with_context(|| format!("parsing {file} as JSON"))?;
            let signed = sign_receipt_value(&doc, &sk)?;
            let target = out.as_deref().unwrap_or(&file);
            std::fs::write(target, serde_json::to_string_pretty(&signed)? + "\n")
                .with_context(|| format!("writing {target}"))?;
            println!(
                "{}",
                json!({
                    "signed": true,
                    "alg": "Ed25519+ML-DSA-65",
                    "payload_sha256": signed["payload_sha256"],
                    "pubkey_hex": signed["signature"]["pubkey_hex"],
                })
            );
            Ok(())
        }
        ReceiptCmd::Verify {
            file,
            pubkey,
            pq_pubkey,
            require_pq,
        } => {
            let raw = std::fs::read_to_string(&file).with_context(|| format!("reading {file}"))?;
            let doc: Value =
                serde_json::from_str(&raw).with_context(|| format!("parsing {file} as JSON"))?;
            let summary =
                verify_receipt_value(&doc, pubkey.as_deref(), pq_pubkey.as_deref(), require_pq)?;
            println!("{summary}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod receipt_tests {
    use super::*;

    #[test]
    fn hybrid_ed25519_mldsa65_receipt_sign_and_verify_fail_closed() {
        let sk = SigningKey::from_bytes(&[77u8; 32]);
        let doc = json!({
            "payload": {
                "schema": RECEIPT_SCHEMA,
                "instance": "django-11848",
                "base_commit": "f0adf3b9",
                "runtime": "openshell",
                "note": "unicode check — café",
            }
        });
        let signed = sign_receipt_value(&doc, &sk).unwrap();
        assert_eq!(signed["signature"]["alg"], "Ed25519+ML-DSA-65");
        assert_eq!(signed["signature"]["pq_alg"], "ML-DSA-65");

        let ok = verify_receipt_value(&signed, None, None, true).unwrap();
        assert_eq!(ok["valid"], true);
        assert_eq!(ok["pq_verified"], true);

        // Tamper with payload -> fails closed.
        let mut bad_payload = signed.clone();
        bad_payload["payload"]["runtime"] = json!("tampered");
        assert!(verify_receipt_value(&bad_payload, None, None, true).is_err());

        // Tamper with ML-DSA-65 signature -> fails closed even though Ed25519 is intact.
        let mut bad_pq = signed.clone();
        let orig_pq = bad_pq["signature"]["pq_sig_hex"]
            .as_str()
            .unwrap()
            .to_string();
        let flipped = format!(
            "{:02x}{}",
            u8::from_str_radix(&orig_pq[..2], 16).unwrap() ^ 1,
            &orig_pq[2..]
        );
        bad_pq["signature"]["pq_sig_hex"] = json!(flipped);
        assert!(verify_receipt_value(&bad_pq, None, None, false).is_err());
    }
}
