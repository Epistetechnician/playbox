//! End-to-end tests for `playbox gate export` and `playbox gate verify`.

use ed25519_dalek::SigningKey;
use playbox_net::{join_link, new_secret_key, Host, HostConfig, Transport};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_playbox");

fn fresh(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("playbox-gate-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run_cli(home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .env("PLAYBOX_HOME", home)
        .args(args)
        .output()
        .unwrap()
}

#[tokio::test]
async fn export_and_verify_five_gate_compliance_bundle_and_reject_tampering() {
    let root_dir = fresh("e2e");
    let home = root_dir.join("home");
    let state_p = root_dir.join("state_p");
    let state_r = root_dir.join("state_r");
    std::fs::create_dir_all(&home).unwrap();

    // 1. Real primary + replica hosts with replicated RoomLog (Ed25519 + ML-DSA-65 checkpoints), audit.log, join & task refusal.
    let p = Host::open(&state_p, Transport::Local, HostConfig::default())
        .await
        .unwrap();
    let r = Host::open_replica(
        &state_r,
        Transport::Local,
        HostConfig::default(),
        p.root_public(),
    )
    .await
    .unwrap();
    p.start_room_log(&[r.addr().id], 2).unwrap();

    let link = p.create_link(Duration::from_secs(60), 2).unwrap().encode();
    let guest = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let _ = guest.echo("hello gate").await;
    r.pull_room_log(&p.addr()).await.unwrap();
    let committed = p.commit_room_log(&[r.addr()]).await.unwrap();
    assert!(committed > 0);
    drop(guest);
    p.shutdown().await.unwrap();
    r.shutdown().await.unwrap();

    // 2. Real BOX-7 egress metadata JSONL (no secrets/tokens).
    let egress_log = root_dir.join("egress.jsonl");
    std::fs::write(
        &egress_log,
        concat!(
            r#"{"ts_ms":100,"upstream":"anthropic","method":"POST","path":"/v1/messages","status":200,"req_bytes":128,"resp_bytes":512,"duration_ms":42}"#,
            "\n",
            r#"{"ts_ms":101,"upstream":"anthropic","method":"GET","path":"/admin","status":403,"req_bytes":0,"resp_bytes":0,"duration_ms":1,"denied":"path_not_allowed"}"#,
            "\n"
        ),
    )
    .unwrap();

    // 3. Challenge gate receipts JSONL (explicitly unsigned metadata).
    let challenge_log = root_dir.join("challenge_receipts.jsonl");
    std::fs::write(
        &challenge_log,
        concat!(
            r#"{"verdict":"promote","env_hash":"abc","cand_hash":"def","seed_commit":{"primary":"111","replica":"222"}}"#,
            "\n",
            r#"{"verdict":"reject","env_hash":"abc","cand_hash":"ghi","seed_commit":{"primary":"333","replica":"444"}}"#,
            "\n"
        ),
    )
    .unwrap();

    // 4. Signed T0 host attestation created by `playbox attest` inside PLAYBOX_HOME/history/default.log.
    let attest_log = home.join("history").join("default.log");
    let att_out = run_cli(&home, &["attest", "--platform", "none"]);
    assert!(
        att_out.status.success(),
        "playbox attest failed: {}",
        String::from_utf8_lossy(&att_out.stderr)
    );
    assert!(attest_log.is_file());

    let bundle_path = root_dir.join("compliance_bundle.json");
    let exp = run_cli(
        &home,
        &[
            "gate",
            "export",
            "--state",
            state_p.to_str().unwrap(),
            "--egress-log",
            egress_log.to_str().unwrap(),
            "--challenge-log",
            challenge_log.to_str().unwrap(),
            "--attestation",
            attest_log.to_str().unwrap(),
            "--out",
            bundle_path.to_str().unwrap(),
        ],
    );
    assert!(
        exp.status.success(),
        "gate export failed: {}",
        String::from_utf8_lossy(&exp.stderr)
    );

    let ep_seed = playbox_net::load_seed_hex(&state_p.join("endpoint.key")).unwrap();
    let ep_pub_hex: String = SigningKey::from_bytes(&ep_seed)
        .verifying_key()
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    // 5. Verify offline with --require-signature, --require-pq, --require-tier t0, --pubkey, and --json.
    let ver = run_cli(
        &home,
        &[
            "gate",
            "verify",
            bundle_path.to_str().unwrap(),
            "--pubkey",
            &ep_pub_hex,
            "--require-signature",
            "--require-pq",
            "--require-tier",
            "t0",
            "--json",
        ],
    );
    assert!(
        ver.status.success(),
        "gate verify failed: {}",
        String::from_utf8_lossy(&ver.stderr)
    );
    let report: Value = serde_json::from_slice(&ver.stdout).unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(report["signed"], true);
    assert_eq!(report["pq_signed"], true);
    assert_eq!(report["enforced_gates_count"], 5);
    assert_eq!(report["claims_full_84_permf"], false);
    assert_eq!(report["egress_requests"], 2);
    assert_eq!(report["achieved_tier"], "T0");
    assert!(report["room_log_pq_committed"].as_u64().unwrap() > 0);

    // 6. Fail-closed checks: tampering with body, signature, or claim boundary must fail.
    let mut tampered: Value =
        serde_json::from_slice(&std::fs::read(&bundle_path).unwrap()).unwrap();
    tampered["body"]["claim_boundary"]["claims_full_84_permf"] = Value::Bool(true);
    let tampered_path = root_dir.join("tampered_claim.json");
    std::fs::write(&tampered_path, serde_json::to_vec(&tampered).unwrap()).unwrap();
    let bad = run_cli(&home, &["gate", "verify", tampered_path.to_str().unwrap()]);
    assert!(
        !bad.status.success(),
        "tampered bundle must fail verification"
    );

    let mut bad_sig: Value = serde_json::from_slice(&std::fs::read(&bundle_path).unwrap()).unwrap();
    let orig_pq = bad_sig["signature"]["pq_sig"].as_str().unwrap().to_string();
    let flipped = format!(
        "{}{}",
        if orig_pq.starts_with('0') { "1" } else { "0" },
        &orig_pq[1..]
    );
    bad_sig["signature"]["pq_sig"] = Value::String(flipped);
    let bad_sig_path = root_dir.join("bad_pq.json");
    std::fs::write(&bad_sig_path, serde_json::to_vec(&bad_sig).unwrap()).unwrap();
    let bad2 = run_cli(
        &home,
        &[
            "gate",
            "verify",
            bad_sig_path.to_str().unwrap(),
            "--require-pq",
        ],
    );
    assert!(
        !bad2.status.success(),
        "forged ML-DSA-65 signature must fail verification"
    );

    // 7. Requiring T1 when only T0 self-attestation exists must fail closed.
    let low_tier = run_cli(
        &home,
        &[
            "gate",
            "verify",
            bundle_path.to_str().unwrap(),
            "--require-tier",
            "t1",
        ],
    );
    assert!(
        !low_tier.status.success(),
        "T0 must not satisfy --require-tier t1"
    );

    // 8. Export must refuse an egress log containing a leaked capability token path `/t/`.
    let dirty_egress = root_dir.join("dirty_egress.jsonl");
    std::fs::write(
        &dirty_egress,
        r#"{"ts_ms":1,"upstream":"m","method":"GET","path":"/t/secret123/v1","status":200,"req_bytes":0,"resp_bytes":0,"duration_ms":1}"#,
    )
    .unwrap();
    let dirty_exp = run_cli(
        &home,
        &[
            "gate",
            "export",
            "--egress-log",
            dirty_egress.to_str().unwrap(),
        ],
    );
    assert!(
        !dirty_exp.status.success(),
        "export must refuse leaked /t/ capability path in egress log"
    );

    let _ = std::fs::remove_dir_all(&root_dir);
}

#[test]
fn unsigned_bundle_verifies_from_stdin_and_fails_require_signature() {
    let root_dir = fresh("stdin");
    let home = root_dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let egress_log = root_dir.join("egress.jsonl");
    std::fs::write(
        &egress_log,
        r#"{"ts_ms":1,"upstream":"openai","method":"POST","path":"/v1/responses","status":200,"req_bytes":64,"resp_bytes":128,"duration_ms":12}"#,
    )
    .unwrap();
    let exp = run_cli(
        &home,
        &[
            "gate",
            "export",
            "--egress-log",
            egress_log.to_str().unwrap(),
            "--no-sign",
        ],
    );
    assert!(exp.status.success());

    let mut verify_stdin = Command::new(BIN)
        .env("PLAYBOX_HOME", &home)
        .args(["gate", "verify", "-", "--json"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    verify_stdin
        .stdin
        .take()
        .unwrap()
        .write_all(&exp.stdout)
        .unwrap();
    let out = verify_stdin.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stdin verify failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(report["signed"], false);
    assert_eq!(report["egress_requests"], 1);

    let bundle_path = root_dir.join("unsigned.json");
    std::fs::write(&bundle_path, &exp.stdout).unwrap();
    let req_sig = run_cli(
        &home,
        &[
            "gate",
            "verify",
            bundle_path.to_str().unwrap(),
            "--require-signature",
        ],
    );
    assert!(
        !req_sig.status.success(),
        "unsigned bundle must fail --require-signature"
    );

    let _ = std::fs::remove_dir_all(&root_dir);
}

#[test]
fn gate_sign_verify_and_export_unify_run_receipts() {
    let root_dir = fresh("unified_receipt");
    let home = root_dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let rcpt = root_dir.join("run.receipt.json");
    std::fs::write(
        &rcpt,
        r#"{"payload":{"schema":"playbox.attested_gate_receipt.v1","instance":"django-11848","base_commit":"f0adf3b9","runtime":"openshell"}}"#,
    )
    .unwrap();

    // Sign directly via unified `playbox gate sign`, then verify directly via `playbox gate verify`.
    let sign_out = run_cli(&home, &["gate", "sign", rcpt.to_str().unwrap()]);
    assert!(
        sign_out.status.success(),
        "gate sign failed: {}",
        String::from_utf8_lossy(&sign_out.stderr)
    );

    let direct_ver = run_cli(
        &home,
        &[
            "gate",
            "verify",
            rcpt.to_str().unwrap(),
            "--require-pq",
            "--json",
        ],
    );
    assert!(
        direct_ver.status.success(),
        "gate verify on run receipt failed: {}",
        String::from_utf8_lossy(&direct_ver.stderr)
    );
    let dv: Value = serde_json::from_slice(&direct_ver.stdout).unwrap();
    assert_eq!(dv["valid"], true);
    assert_eq!(dv["pq_verified"], true);

    // Export into a 5-gate compliance bundle via `playbox gate export --receipt` and verify with `--require-pq`.
    let bundle_path = root_dir.join("bundle.json");
    let exp = run_cli(
        &home,
        &[
            "gate",
            "export",
            "--receipt",
            rcpt.to_str().unwrap(),
            "--no-sign",
            "--out",
            bundle_path.to_str().unwrap(),
        ],
    );
    assert!(
        exp.status.success(),
        "gate export --receipt failed: {}",
        String::from_utf8_lossy(&exp.stderr)
    );

    let bundle_ver = run_cli(
        &home,
        &[
            "gate",
            "verify",
            bundle_path.to_str().unwrap(),
            "--require-pq",
            "--json",
        ],
    );
    assert!(
        bundle_ver.status.success(),
        "gate verify on receipt bundle failed: {}",
        String::from_utf8_lossy(&bundle_ver.stderr)
    );
    let bv: Value = serde_json::from_slice(&bundle_ver.stdout).unwrap();
    assert_eq!(bv["ok"], true);
    assert_eq!(bv["attested_receipts"], 1);

    // Tampering with embedded run receipt inside bundle must fail verification.
    let mut bad_bundle: Value =
        serde_json::from_slice(&std::fs::read(&bundle_path).unwrap()).unwrap();
    bad_bundle["body"]["gates"]["gate_4_sealed_evaluation_gates"]["attested_receipts"][0]
        ["receipt"]["payload"]["runtime"] = Value::String("tampered".into());
    let bad_path = root_dir.join("bad_bundle.json");
    std::fs::write(&bad_path, serde_json::to_vec(&bad_bundle).unwrap()).unwrap();
    let bad_ver = run_cli(&home, &["gate", "verify", bad_path.to_str().unwrap()]);
    assert!(!bad_ver.status.success());

    let _ = std::fs::remove_dir_all(&root_dir);
}

#[cfg(unix)]
#[test]
fn published_t3_confidential_space_bundle_verifies_offline() {
    let root_dir = fresh("t3_cs_bundle");
    let home = root_dir.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let gz_path = repo_root.join("docs/reports/playbox-t3-confidential-space-5gate-bundle.json.gz");
    let jwks_path = repo_root.join("docs/reports/google-confidential-space-jwks.json");
    let gunzip = Command::new("gzip")
        .args(["-dc", gz_path.to_str().unwrap()])
        .output()
        .expect("gzip -dc");
    assert!(gunzip.status.success(), "gzip -dc failed");

    let mut verify = Command::new(BIN)
        .env("PLAYBOX_HOME", &home)
        .env("PLAYBOX_CS_JWKS", &jwks_path)
        .env("PLAYBOX_CS_AUDIENCE", "playbox-t3-gate-v0.4.5")
        .env(
            "PLAYBOX_CS_IMAGES",
            "sha256:67cec5f5e7189c7735f0bef95c69876fdbac00ec026195dfd985d3236be413af",
        )
        .env("PLAYBOX_CS_PROJECTS", "secops1219")
        .args([
            "gate",
            "verify",
            "-",
            "--pubkey",
            "e909cef9782c42082e7e0f8c2e2954346f3588395e33e4075fddf00938b487d2",
            "--require-signature",
            "--require-pq",
            "--require-tier",
            "t3",
            "--json",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    verify
        .stdin
        .take()
        .unwrap()
        .write_all(&gunzip.stdout)
        .unwrap();
    let out = verify.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "T3 Confidential Space bundle verify failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(report["signed"], true);
    assert_eq!(report["pq_signed"], true);
    assert_eq!(report["achieved_tier"], "T3");
    assert_eq!(report["enforced_gates_count"], 5);
    assert_eq!(report["claims_full_84_permf"], false);
    assert_eq!(
        report["bundle_digest"],
        "206e4ad33be0bcf42a5f7ce268d7ba504bd86b58302fd005000b51b1683f617f"
    );

    let _ = std::fs::remove_dir_all(&root_dir);
}
