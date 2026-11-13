//! Mutation testing of a live host. Thousands of mangled requests go in over real connections, some of them
//! carrying a genuine grant so they get past authorization into the operation handlers. Afterwards the
//! host must still answer, every reply must be well-formed JSON-RPC, and its audit log must still verify.
//!
//! The host has no task runner, so no mutated request can ever execute a command. The generator is
//! seeded, so a failure reproduces: the failing input is printed as hex.

use a2a::{JsonRpcRequest, JsonRpcResponse, SendMessageRequest};
use iroh::{endpoint::presets, Endpoint};
use playbox_net::{
    audit,
    latent::{commit_of, pack_with},
    join_link, new_secret_key,
    wire::{Frame, Op},
    Client, Host, HostConfig, Transport,
};
use serde_json::{json, Value};
use std::{path::Path, process::Command, time::Duration};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the value is bounded, or truncated on purpose"
    )]
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const EDGES: &[&[u8]] = &[
    b"null",
    b"-1",
    b"0",
    b"99999999999999999999",
    b"1e999",
    b"\"\"",
    b"[]",
    b"{}",
    b"true",
    b"\\u0000",
    b"\\ud800",
    b"\xff\xfe\xfd",
    b"\"../../etc/passwd\"",
    b"\"-rf\"",
    b"\"main\\n\"",
    b"\"a b\"",
    b"\"\\r\\n\\r\\n\"",
];

/// Byte-level damage.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the value is bounded, or truncated on purpose"
)]
fn mutate_bytes(rng: &mut Rng, seeds: &[Vec<u8>]) -> Vec<u8> {
    let mut b = seeds[rng.below(seeds.len())].clone();
    for _ in 0..=rng.below(4) {
        if b.is_empty() {
            b.push(b'{');
        }
        let at = rng.below(b.len());
        match rng.below(7) {
            0 => b[at] ^= 1 << rng.below(8),
            1 => b.truncate(at),
            2 => {
                let end = (at + 1 + rng.below(32)).min(b.len());
                b.drain(at..end);
            }
            3 => {
                let end = (at + 1 + rng.below(64)).min(b.len());
                let chunk = b[at..end].to_vec();
                b.splice(at..at, chunk);
            }
            4 => {
                let n = 1 + rng.below(16);
                let junk: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                b.splice(at..at, junk);
            }
            5 => {
                let e = EDGES[rng.below(EDGES.len())];
                let end = (at + rng.below(12)).min(b.len());
                b.splice(at..end, e.iter().copied());
            }
            _ => {
                let other = &seeds[rng.below(seeds.len())];
                if !other.is_empty() {
                    let from = rng.below(other.len());
                    let to = (from + 1 + rng.below(48)).min(other.len());
                    b.splice(at..at, other[from..to].iter().copied());
                }
            }
        }
    }
    b
}

const STRINGS: &[&str] = &[
    "main",
    "feature",
    "HEAD",
    "refs/heads/main",
    "refs/playbox/members/x/y",
    "",
    "-x",
    "--upload-pack=x",
    "a..b",
    "a b",
    "x/y/z",
    "../../etc/passwd",
    "main\n",
    "$(id)",
    "\u{0}",
    "é",
    "🙂",
    "ROLE_AGENT",
    "ROLE_USER",
    "nonexistent",
];

/// Replace a node by something of a *different* shape: absurdly big, deep, or the wrong type.
fn reshape(rng: &mut Rng, v: &mut Value) {
    *v = match rng.below(10) {
        0 => Value::Null,
        1 => json!(true),
        2 => json!(-9_223_372_036_854_775_808i64),
        3 => json!(u64::MAX),
        4 => json!(0.5e308),
        5 => json!("A".repeat(60_000)),
        6 => json!(""),
        7 => {
            let mut deep = json!("x");
            for _ in 0..90 {
                deep = json!([deep]);
            }
            deep
        }
        8 => json!({}),
        _ => json!(["a", 1, null]),
    };
}

/// Structure-aware damage. Mostly it keeps every field the right *type* and only changes the value (a
/// different ref name, a bigger number, an empty string), so the request survives parsing and reaches the
/// operation handlers. Occasionally it changes a node's shape entirely.
fn mutate_json(rng: &mut Rng, v: &mut Value, depth: usize) {
    match v {
        Value::Object(m) if !m.is_empty() && depth < 12 => {
            if rng.below(12) == 0 {
                return reshape(rng, v);
            }
            let keys: Vec<String> = m.keys().cloned().collect();
            let k = keys[rng.below(keys.len())].clone();
            if rng.below(10) == 0 {
                m.remove(&k);
            } else if let Some(child) = m.get_mut(&k) {
                mutate_json(rng, child, depth + 1);
            }
        }
        Value::Array(a) if !a.is_empty() && depth < 12 => {
            if rng.below(12) == 0 {
                return reshape(rng, v);
            }
            let i = rng.below(a.len());
            mutate_json(rng, &mut a[i], depth + 1);
        }
        Value::String(s) => match rng.below(8) {
            0 => reshape(rng, v),
            1 => s.push_str(STRINGS[rng.below(STRINGS.len())]),
            _ => *s = STRINGS[rng.below(STRINGS.len())].to_string(),
        },
        Value::Number(_) => match rng.below(6) {
            0 => reshape(rng, v),
            1 => *v = json!(-1),
            2 => *v = json!(0),
            3 => *v = json!(1e9),
            4 => *v = json!(f64::MAX),
            _ => *v = json!(u32::MAX),
        },
        _ => reshape(rng, v),
    }
}

fn frame(method: &str, params: Option<Value>, grant: Option<&str>) -> Vec<u8> {
    serde_json::to_vec(&Frame::new(
        JsonRpcRequest::new(1.into(), method, params),
        grant,
    ))
    .unwrap()
}

fn message(op: Op, extra: Vec<a2a::Part>) -> Value {
    let req = SendMessageRequest {
        message: op.message(extra),
        configuration: None,
        metadata: None,
        tenant: None,
    };
    serde_json::to_value(req).unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn hex(b: &[u8]) -> String {
    b.iter().take(400).map(|x| format!("{x:02x}")).collect()
}

/// A raw connection as the given identity. A grant only works for the key it was issued to, so to get past
/// authorization the barrage has to come from the member's own key.
async fn connect(host: &Host, key: &iroh::SecretKey) -> (Endpoint, iroh::endpoint::Connection) {
    let ep = Endpoint::builder(presets::Minimal)
        .secret_key(key.clone())
        .bind()
        .await
        .unwrap();
    let conn = ep.connect(host.addr(), playbox_net::ALPN).await.unwrap();
    (ep, conn)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_live_host_survives_many_thousands_of_mangled_requests() {
    let base = std::env::temp_dir().join(format!("playbox-fuzzhost-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.com"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "i"]);

    // No runner: nothing a request says can run a command on this machine.
    let cfg = HostConfig {
        capabilities: vec![playbox_net::capabilities::Capability {
            name: "greet".into(),
            argv: vec!["echo".into(), "{who}".into()],
            params: [("who".to_string(), playbox_net::capabilities::ParamRule::default())].into(),
            approval: true,
            image: None,
            timeout_secs: None,
            mem_mib: None,
        }],
        repo: Some(repo.clone()),
        idle_timeout: Duration::from_secs(3),
        // An in-process index with a ledger in the state directory, so the memory operations are mutated too.
        browse: Some(std::sync::Arc::new(
            playbox_net::browse::Browser::new(playbox_net::browse::BrowseConfig::new("ws://127.0.0.1:9/", vec!["example.com".into()])).unwrap(),
        )),
        memory: Some(std::sync::Arc::new(
            playbox_net::shared_memory::SharedMemory::new({
                let mut c = playbox_net::shared_memory::MemoryConfig::new(std::sync::Arc::new(playbox_net::shared_memory::InMemory::default()), "fuzz");
                c.ledger = Some(base.join("memory.jsonl"));
                c
            })
            .unwrap(),
        )),
        ..Default::default()
    };
    let host = Host::open(&base.join("state"), Transport::Local, cfg)
        .await
        .unwrap();
    let link = host.create_link(Duration::from_secs(300), 1).unwrap();
    let invite = link.invite.clone();
    let member_key = new_secret_key().unwrap();
    let member = join_link(&link.encode(), member_key.clone(), true)
        .await
        .unwrap();
    let grant = member.grant().unwrap();
    let bundle = playbox_git::bundle_create(&repo, "main").await.unwrap();

    // Seeds: one genuine request of every kind, with and without the member's grant.
    let push_part = a2a::Part::raw(bundle).with_filename("p.bundle");
    let spec = playbox_runner::TaskSpec::new(&["true"]);
    // The room's talk and mailbox: a signed envelope that carries a commitment, and a reveal that opens it. The signer
    // is the member's own key, so an unmutated post is accepted and mutations of it reach the mailbox handlers.
    let latent_hub = "0123456789abcdef";
    let salt = "00112233445566778899aabbccddeeff";
    let sentences = vec!["Waves rolled across the open sea.".to_string()];
    let signer = ed25519_dalek::SigningKey::from_bytes(&member_key.to_bytes());
    let envelope = pack_with(
        &signer,
        latent_hub,
        "x",
        0.5,
        &[3u8; 16],
        Some(&commit_of(salt, &sentences)),
    );
    let reveal = serde_json::to_vec(&json!({ "salt": salt, "sentences": sentences })).unwrap();
    let mut seeds: Vec<Vec<u8>> = Vec::new();
    for g in [None, Some(grant.as_str())] {
        seeds.push(frame("playbox.GetAgentCard", None, g));
        seeds.push(frame(
            "playbox.GetAttestation",
            Some(json!({"nonce": "abcd"})),
            g,
        ));
        seeds.push(frame("playbox.Join", Some(json!({"invite": invite})), g));
        seeds.push(frame(
            "SendMessage",
            Some(message(Op::Echo { text: "hi".into() }, vec![])),
            g,
        ));
        seeds.push(frame(
            "SendMessage",
            Some(message(
                Op::Pull {
                    refname: "main".into(),
                },
                vec![],
            )),
            g,
        ));
        seeds.push(frame(
            "SendMessage",
            Some(message(
                Op::Push {
                    branch: "feature".into(),
                },
                vec![push_part.clone()],
            )),
            g,
        ));
        seeds.push(frame(
            "SendMessage",
            Some(message(
                Op::Run {
                    base: "main".into(),
                    spec: spec.clone(),
                },
                vec![],
            )),
            g,
        ));
        seeds.push(frame(
            "SendMessage",
            Some(message(
                Op::Say {
                    text: "hello".into(),
                    name: Some("x".into()),
                    request_id: Some("fuzz-1".into()),
                },
                vec![],
            )),
            g,
        ));
        seeds.push(frame(
            "SendMessage",
            Some(message(Op::Feed { since: 0 }, vec![])),
            g,
        ));
        // Capabilities: with no runner on this host a call is refused before anything could run.
        seeds.push(frame("SendMessage", Some(message(Op::CapList, vec![])), g));
        seeds.push(frame(
            "SendMessage",
            Some(message(
                Op::Cap { name: "greet".into(), args: [("who".to_string(), "world".to_string())].into(), call_id: "fuzz-cap".into() },
                vec![],
            )),
            g,
        ));
        seeds.push(frame("SendMessage", Some(message(Op::CapPoll { call_id: "fuzz-cap".into() }, vec![])), g));
        // Conversations: create (an unknown member is refused), post with a bad signature, read, list.
        seeds.push(frame("SendMessage", Some(message(Op::ConvNew { title: "t".into(), members: vec!["nobody".into()] }, vec![])), g));
        seeds.push(frame(
            "SendMessage",
            Some(message(Op::ConvPost { conv: "c-0000000000000000".into(), text: "x".into(), request_id: "r".into(), sig: "00".repeat(64) }, vec![])),
            g,
        ));
        seeds.push(frame("SendMessage", Some(message(Op::ConvRead { conv: "c-0000000000000000".into(), since: 0, mark: true }, vec![])), g));
        seeds.push(frame("SendMessage", Some(message(Op::ConvInvite { conv: "c-0".into(), members: vec![] }, vec![])), g));
        seeds.push(frame("SendMessage", Some(message(Op::ConvList, vec![])), g));
        seeds.push(frame(
            "SendMessage",
            Some(message(Op::Remember { text: "the staging VPN is needed".into(), key: Some("k".into()) }, vec![])),
            g,
        ));
        seeds.push(frame(
            "SendMessage",
            Some(message(Op::Browse { url: "https://example.com/page".into(), mode: playbox_net::browse::Mode::Text, wait_ms: Some(10) }, vec![])),
            g,
        ));
        seeds.push(frame(
            "SendMessage",
            Some(message(Op::Recall { query: "staging VPN".into(), max_tokens: Some(500), author: Some("x".into()) }, vec![])),
            g,
        ));
        seeds.push(frame(
            "SendMessage",
            Some(message(Op::Latent, vec![a2a::Part::raw(envelope.clone())])),
            g,
        ));
        seeds.push(frame(
            "SendMessage",
            Some(message(
                Op::LatentPull {
                    hub: latent_hub.into(),
                    since: 0,
                },
                vec![],
            )),
            g,
        ));
        seeds.push(frame(
            "SendMessage",
            Some(message(
                Op::LatentReveal { seq: 1 },
                vec![a2a::Part::raw(reveal.clone())],
            )),
            g,
        ));
        seeds.push(frame("GetTask", Some(json!({"id": "x"})), g));
        seeds.push(frame("ListTasks", Some(json!({})), g));
        seeds.push(frame("CancelTask", Some(json!({"id": "x"})), g));
        seeds.push(frame("GetExtendedAgentCard", Some(json!({})), g));
        seeds.push(frame("SendStreamingMessage", Some(json!({})), g));
    }

    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (mut ep, mut conn) = connect(&host, &member_key).await;
    let (mut sent, mut answered, mut dropped) = (0u32, 0u32, 0u32);
    for i in 0..15_000u32 {
        // Alternate byte-level and structure-aware damage.
        let input = if i % 2 == 0 {
            mutate_bytes(&mut rng, &seeds)
        } else {
            let mut f: Value = serde_json::from_slice(&seeds[rng.below(seeds.len())]).unwrap();
            for _ in 0..=rng.below(3) {
                mutate_json(&mut rng, &mut f, 0);
            }
            serde_json::to_vec(&f).unwrap()
        };
        sent += 1;
        let reply = tokio::time::timeout(Duration::from_secs(10), async {
            let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
            send.write_all(&input).await.map_err(|e| e.to_string())?;
            send.finish().map_err(|e| e.to_string())?;
            recv.read_to_end(8 << 20).await.map_err(|e| e.to_string())
        })
        .await;
        match reply {
            Err(_) => panic!(
                "the host stopped answering within 10 s (iteration {i}); input: {}",
                hex(&input)
            ),
            Ok(Ok(bytes)) => {
                answered += 1;
                // Every reply, to anything, is a well-formed JSON-RPC response.
                let parsed = serde_json::from_slice::<JsonRpcResponse>(&bytes);
                assert!(
                    parsed.is_ok(),
                    "iteration {i}: malformed reply {:?} to input {}",
                    String::from_utf8_lossy(&bytes[..bytes.len().min(200)]),
                    hex(&input)
                );
            }
            Ok(Err(_)) => {
                // The host hung up (oversized or too slow, by design). Take a fresh connection.
                dropped += 1;
                ep.close().await;
                (ep, conn) = connect(&host, &member_key).await;
            }
        }
        if i % 1500 == 1499 {
            // Still serving an honest member in the middle of it.
            assert_eq!(
                member
                    .echo("alive")
                    .await
                    .expect("an honest member is still served"),
                "alive"
            );
        }
    }
    eprintln!("fuzz_host: {sent} requests, {answered} answered, {dropped} connections dropped by the host");
    assert!(
        answered > 5000,
        "too few requests were answered ({answered}); the test is not exercising the handlers"
    );

    assert_eq!(member.echo("done").await.unwrap(), "done");
    let log = host.audit_path().unwrap();
    let summary = audit::verify(&log).expect("the audit log must still verify after the barrage");
    assert!(summary.entries > 10, "the barrage should have been logged");
    // The mutations must have reached the handlers, not just died in the parser: the host's own log shows
    // the operations it was asked to do and the denials it issued.
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for e in audit::tail(&log, usize::MAX).unwrap() {
        *counts
            .entry(e["event"].as_str().unwrap_or("?").to_string())
            .or_default() += 1;
    }
    eprintln!("fuzz_host: audit events by kind: {counts:?}");
    for kind in ["refused", "pull", "push", "task", "latent"] {
        assert!(
            counts.get(kind).copied().unwrap_or(0) > 0,
            "no `{kind}` event: the mutations never reached that handler ({counts:?})"
        );
    }
    ep.close().await;

    // The mailbox the barrage left on disk is still a valid store: a restarted host loads it, every message it serves
    // verifies, and an honest member can still post and read.
    member.close().await;
    host.shutdown().await.unwrap();
    let store = std::fs::read(base.join("state").join("latent.json"));
    if let Ok(bytes) = &store {
        serde_json::from_slice::<Value>(bytes).expect("the stored mailbox is valid JSON");
    }
    let host = Host::open(&base.join("state"), Transport::Local, HostConfig::default())
        .await
        .unwrap();
    let again = Client::connect(host.addr(), member_key.clone(), true)
        .await
        .unwrap();
    again.set_grant(grant);
    let (kept, _) = again.latent_pull(latent_hub, 0).await.unwrap();
    eprintln!("fuzz_host: {} latent messages survived the barrage and a restart", kept.len());
    let fresh = pack_with(&signer, latent_hub, "after", 0.5, &[9u8; 16], None);
    again.latent_send(&fresh).await.unwrap();
    host.shutdown().await.unwrap();
}
