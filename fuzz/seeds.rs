//! Writes valid inputs into fuzz/corpus/<target>/ so the fuzzer starts from real structure and mutates it.
//! Run: `cd fuzz && cargo +nightly run --bin seeds`
use base64::{engine::general_purpose::URL_SAFE, Engine};
use ed25519_dalek::SigningKey;
use playbox_core::{attest, history};
use playbox_net::{
    grants::{Authority, Caps},
    revoke::Notice,
    wire::{Frame, Op},
};
use std::{fs, path::Path};

fn put(target: &str, name: &str, bytes: &[u8]) {
    let dir = Path::new("corpus").join(target);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), bytes).unwrap();
}

fn main() {
    let auth = Authority::from_seed("fuzz", &[7u8; 32]).unwrap();
    let member = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]).verifying_key();
    let member_id = iroh_like_id(&member.to_bytes());

    // wire_frame: a real request carrying a run operation, a pull, and an echo.
    for (n, op) in [
        Op::Echo { text: "hi".into() },
        Op::Pull { refname: "main".into() },
        Op::Run { base: "main".into(), spec: playbox_runner_spec() },
    ]
    .iter()
    .enumerate()
    {
        let req = a2a::SendMessageRequest { message: op.message(vec![]), configuration: None, metadata: None, tenant: None };
        let rpc = a2a::JsonRpcRequest::new(1.into(), "SendMessage", Some(serde_json::to_value(req).unwrap()));
        put("wire_frame", &format!("op{n}.json"), &serde_json::to_vec(&Frame::new(rpc, Some("grant-token"))).unwrap());
    }

    // grant_token: the raw bytes of a real grant and a real invite.
    let (grant, _) = auth.mint_grant(&member_id, 3600, &Caps { cpu_milli: 1000, mem_mib: 256, timeout_secs: 30, net: false, hosts: vec![] }).unwrap();
    let (invite, _) = auth.mint_invite(3600).unwrap();
    for (name, tok) in [("grant", grant), ("invite", invite)] {
        put("grant_token", name, &URL_SAFE.decode(tok.trim_end_matches('=')).unwrap_or_else(|_| tok.into_bytes()));
    }

    // revocation_notice
    let notice = Notice { grant_id: "ab12cd34".into(), member: member_id.clone(), issued_ms: 5 };
    put("revocation_notice", "valid", notice.sign(&auth).unwrap().as_bytes());

    // attestation_statement
    let key = SigningKey::from_bytes(&[3u8; 32]);
    let st = attest::new_statement(&key, "aa", "", 1, attest::measure_self("none", "seed")).sign(&key).unwrap();
    put("attestation_statement", "valid", st.as_bytes());

    // history_and_audit
    let mut log = history::Log::new();
    log.append(&key, "session_start", b"x", 1);
    log.append(&key, "tool_call", b"y", 2);
    put("history_and_audit", "history", history::text::to_text(&log).unwrap().as_bytes());

    // links_and_pem
    put("links_and_pem", "link", b"playbox://eyJhZGRyIjp7ImlkIjoiYWFhIiwiYWRkcnMiOltdfX0#invite");
    put("links_and_pem", "pem", b"-----BEGIN CERTIFICATE-----\nAAEC\n-----END CERTIFICATE-----\n");
    put("links_and_pem", "transport", b"relay:https://relay.example.com");

    // latent_envelope: a signed envelope without and with a commitment, a reveal that opens it, and a host receipt shape.
    let sentences = vec!["Waves rolled across the open sea.".to_string()];
    let salt = "00112233445566778899aabbccddeeff";
    put("latent_envelope", "plain", &playbox_net::latent::pack(&key, "0123456789abcdef", "x", 0.5, &[3u8; 16]));
    let commit = playbox_net::latent::commit_of(salt, &sentences);
    let committed = playbox_net::latent::pack_with(&key, "0123456789abcdef", "x", 0.5, &[3u8; 16], Some(&commit));
    put("latent_envelope", "committed", &committed);
    let reveal = serde_json::to_vec(&serde_json::json!({ "salt": salt, "sentences": sentences })).unwrap();
    put("latent_envelope", "reveal", &reveal);
    put("latent_envelope", "receipt", b"playbox.latent-receipt.v1\nhost 01\nsigner 02\nhub 0123456789abcdef\nenvelope 00\nseq 1\nsig 00");

    // latent_store: a saved mailbox with one committed message and its reveal.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let store = serde_json::json!({ "seq": 1, "entries": [{
        "seq": 1, "from": iroh_like_id(&key.verifying_key().to_bytes()), "envelope": base64::engine::general_purpose::STANDARD.encode(&committed),
        "reveal": base64::engine::general_purpose::STANDARD.encode(&reveal), "at_ms": now }] });
    put("latent_store", "mailbox", store.to_string().as_bytes());

    println!("seeds written under fuzz/corpus/");
}

fn playbox_runner_spec() -> playbox_runner::TaskSpec {
    playbox_runner::TaskSpec::new(&["sh", "-c", "true"])
}

/// An endpoint id is the key's hex.
fn iroh_like_id(key: &[u8; 32]) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}
