//! A stand-in for NVIDIA's attestation service, for TESTING the GPU evidence path without a GPU.
//!
//! It signs tokens in the shape `gpu_cc` expects with a made-up P-384 key. **Nothing it prints is evidence of a real GPU.**
//! A verifier pinned to its key would believe any claim this tool makes; never pin that key outside a test.
//!
//! ```text
//! cargo run -q -p playbox-core --example fake_nras -- jwks  > gpu-jwks.json      # the public key to pin
//! PLAYBOX_NONCE=<hex> cargo run -q -p playbox-core --example fake_nras           # prints the gpu / gpu-detail lines
//! ```
//! `PLAYBOX_ATTEST=gpu PLAYBOX_GPU_EVIDENCE_CMD=<this binary>` makes the host use it. Environment: `FAKE_NRAS_SEED` (hex, 48
//! bytes; default fixed), `FAKE_NRAS_DRIVER`, `FAKE_NRAS_VBIOS`, `FAKE_NRAS_GPUS` (default 1) and `FAKE_NRAS_BREAK`, one of
//! `debug`, `secboot`, `measres`, `model`, `nonce`, `driver`, to produce a deliberately bad token.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use p384::ecdsa::{signature::Signer, Signature, SigningKey};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

fn key() -> SigningKey {
    let seed = std::env::var("FAKE_NRAS_SEED").unwrap_or_else(|_| "07".repeat(48));
    let bytes: Vec<u8> = (0..seed.len() / 2)
        .map(|i| u8::from_str_radix(&seed[i * 2..i * 2 + 2], 16).expect("FAKE_NRAS_SEED is hex"))
        .collect();
    SigningKey::from_slice(&bytes).expect("FAKE_NRAS_SEED is 48 bytes")
}

fn jwt(claims: &Value, key: &SigningKey) -> String {
    let h = B64.encode(json!({"alg": "ES384", "kid": "fake-nras"}).to_string());
    let p = B64.encode(claims.to_string());
    let sig: Signature = key.sign(format!("{h}.{p}").as_bytes());
    format!("{h}.{p}.{}", B64.encode(sig.to_bytes()))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn main() {
    let key = key();
    if std::env::args().nth(1).as_deref() == Some("jwks") {
        let pt = key.verifying_key().to_sec1_point(false);
        println!(
            "{}",
            json!({"keys": [{"kid": "fake-nras", "kty": "EC", "crv": "P-384",
                "x": B64.encode(pt.x().expect("x")), "y": B64.encode(pt.y().expect("y"))}]})
        );
        return;
    }
    let nonce =
        std::env::var("PLAYBOX_NONCE").expect("PLAYBOX_NONCE is the statement binding (hex)");
    let broken = std::env::var("FAKE_NRAS_BREAK").unwrap_or_default();
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        + 600;
    let gpus: usize = std::env::var("FAKE_NRAS_GPUS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let (mut details, mut submods) = (Vec::new(), Map::new());
    for i in 0..gpus {
        let mut d = json!({
            "iss": "https://nras.attestation.nvidia.com", "exp": exp,
            "eat_nonce": if broken == "nonce" { "00".repeat(32) } else { nonce.clone() },
            "hwmodel": if broken == "model" { "TU104" } else { "GH100 A01 GSP BROM" },
            "measres": if broken == "measres" { "fail" } else { "success" },
            "secboot": broken != "secboot",
            "dbgstat": if broken == "debug" { "enabled" } else { "disabled" },
            "x-nvidia-gpu-arch-check": true,
            "x-nvidia-gpu-attestation-report-signature-verified": true,
            "x-nvidia-gpu-attestation-report-nonce-match": true,
            "x-nvidia-gpu-attestation-report-cert-chain-validated": true,
            "x-nvidia-gpu-driver-version": if broken == "driver" { "535.0".into() } else { std::env::var("FAKE_NRAS_DRIVER").unwrap_or_else(|_| "550.90.07".into()) },
            "x-nvidia-gpu-vbios-version": std::env::var("FAKE_NRAS_VBIOS").unwrap_or_else(|_| "96.00.74.00.1A".into()),
        });
        d["ueid"] = json!(format!("fake-{i}"));
        let t = jwt(&d, &key);
        let name = format!("GPU-{i}");
        submods.insert(
            name.clone(),
            json!(["DIGEST", ["SHA256", hex(&Sha256::digest(t.as_bytes()))]]),
        );
        details.push(format!("gpu-detail {name} {t}"));
    }
    let overall = json!({
        "iss": "https://nras.attestation.nvidia.com", "exp": exp, "eat_nonce": nonce, "ueid": "fake-device",
        "x-nvidia-overall-att-result": true, "submods": submods
    });
    println!("gpu {}", jwt(&overall, &key));
    for d in details {
        println!("{d}");
    }
}
