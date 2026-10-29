use playbox_core::attest::{Policy, Tier};
use playbox_net::{new_secret_key, Client, Host, HostConfig};
use playbox_runner::ProcessRunner;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn host_attests_and_client_verifies_it() {
    let cfg = HostConfig { min_isolation: playbox_runner::Isolation::None, runner: Some(Arc::new(ProcessRunner)), ..Default::default() };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg).await.unwrap();
    // No invite needed: attestation is public, like the agent card.
    let c = Client::connect(host.addr(), new_secret_key().unwrap(), true).await.unwrap();
    let v = c.verify_host(Tier::T0, &[]).await.unwrap();
    assert_eq!(v.achieved, Tier::T0);
    assert_eq!(v.statement.subject, *host.addr().id.as_bytes());
    assert_eq!(v.statement.measurements.runner, "process-UNSAFE");
    assert_eq!(v.statement.measurements.binary.len(), 64);
    assert_eq!(v.statement.measurements.os, std::env::consts::OS);

    // The room's policy can ask for more than a self-signed statement can give; it is refused.
    let e = c.verify_host(Tier::T2, &[]).await.unwrap_err();
    assert!(format!("{e:#}").contains("achieves T0 but T2 was required"), "{e:#}");
    assert!(Policy::default().require("merge", Tier::T2).check("merge", v.achieved).is_err());

    // Each call is fresh: nonces differ, so an old statement cannot be replayed.
    let a = c.call("playbox.GetAttestation", json!({"nonce": "aa"})).await.unwrap();
    let b = c.call("playbox.GetAttestation", json!({"nonce": "bb"})).await.unwrap();
    assert_ne!(a["statement"], b["statement"]);
    // Bad nonces are refused rather than put into a signed statement.
    assert!(c.call("playbox.GetAttestation", json!({"nonce": "not hex\nsig x"})).await.is_err());
    assert!(c.call("playbox.GetAttestation", json!({"nonce": "a".repeat(65)})).await.is_err());
}

#[tokio::test]
async fn the_policy_hash_changes_when_the_policy_does() {
    let mk = |cpus: f32| async move {
        let cfg = HostConfig { min_isolation: playbox_runner::Isolation::None, limits: playbox_runner::Limits { cpus, ..Default::default() }, ..Default::default() };
        let h = Host::start_with(new_secret_key().unwrap(), true, cfg).await.unwrap();
        let c = Client::connect(h.addr(), new_secret_key().unwrap(), true).await.unwrap();
        let v = c.verify_host(Tier::T0, &[]).await.unwrap();
        (v.statement.measurements.policy, h)
    };
    let (p1, _h1) = mk(1.0).await;
    let (p2, _h2) = mk(2.0).await;
    assert_ne!(p1, p2);
}

// ---- platform evidence over the wire -------------------------------------------------------------

mod platform {
    use playbox_core::attest::{Measurements, PlatformProvider, PlatformVerdict, PlatformVerifier, Produced, Statement, Tier};
    use playbox_net::{new_secret_key, Client, Host, HostConfig};
    use std::sync::Arc;

    /// Evidence that is just the binding itself: enough to prove the host binds evidence to the
    /// nonce-specific statement and the client checks it, without any hardware.
    struct EchoBinding;
    impl PlatformProvider for EchoBinding {
        fn kind(&self) -> &str {
            "mock-hw"
        }
        fn produce(&self, binding: &[u8; 32], _: &Measurements) -> Result<Produced, String> {
            Ok(Produced { evidence: format!("bound {}\n", binding.iter().map(|b| format!("{b:02x}")).collect::<String>()), claimed: Tier::T1 })
        }
    }

    struct CheckBinding;
    impl PlatformVerifier for CheckBinding {
        fn kind(&self) -> &str {
            "mock-hw"
        }
        fn verify(&self, st: &Statement) -> Result<PlatformVerdict, String> {
            let ev = String::from_utf8((0..st.platform_evidence.len() / 2).map(|i| u8::from_str_radix(&st.platform_evidence[i * 2..i * 2 + 2], 16).unwrap()).collect()).unwrap();
            let want: String = st.binding().iter().map(|b| format!("{b:02x}")).collect();
            if ev.trim() == format!("bound {want}") {
                Ok(PlatformVerdict { tier: Tier::T1, notes: vec![], device: Some("mock-device".into()), challenge: None })
            } else {
                Err("evidence is not bound to this statement".into())
            }
        }
    }

    /// Hardware that can answer a challenge: it reverses the request.
    struct Answers(bool);
    impl PlatformProvider for Answers {
        fn kind(&self) -> &str {
            "mock-hw"
        }
        fn produce(&self, _: &[u8; 32], _: &Measurements) -> Result<Produced, String> {
            Ok(Produced { evidence: "x\n".into(), claimed: Tier::T1 })
        }
        fn respond(&self, request: &[u8]) -> Result<Vec<u8>, String> {
            if self.0 { Ok(request.iter().rev().copied().collect()) } else { Err("no".into()) }
        }
    }

    struct Asks;
    impl PlatformVerifier for Asks {
        fn kind(&self) -> &str {
            "mock-hw"
        }
        fn verify(&self, _: &Statement) -> Result<PlatformVerdict, String> {
            Ok(PlatformVerdict {
                tier: Tier::T0,
                notes: vec![],
                device: None,
                challenge: Some(playbox_core::attest::Challenge { payload: b"ping".to_vec(), expect: b"gnip".to_vec(), tier_on_success: Tier::T1 }),
            })
        }
    }

    #[tokio::test]
    async fn the_hardware_challenge_is_asked_over_the_wire() {
        let (_h, c) = host_with(Some(Arc::new(Answers(true)))).await;
        let v = c.verify_host(Tier::T1, &[&Asks]).await.unwrap();
        assert_eq!(v.achieved, Tier::T1);
        assert!(v.notes.iter().any(|n| n.contains("challenge answered")), "{:?}", v.notes);
        // Hardware that refuses to answer cannot reach T1.
        let (_h2, c2) = host_with(Some(Arc::new(Answers(false)))).await;
        let e = c2.verify_host(Tier::T1, &[&Asks]).await.unwrap_err();
        assert!(format!("{e:#}").contains("could not answer the hardware challenge"), "{e:#}");
        assert_eq!(c2.verify_host(Tier::T0, &[&Asks]).await.unwrap().achieved, Tier::T0);
        // A host with no hardware at all cannot answer either; malformed requests are refused.
        let (_h3, c3) = host_with(None).await;
        assert!(c3.call("playbox.ActivateCredential", serde_json::json!({"request": "00"})).await.is_err());
        assert!(c.call("playbox.ActivateCredential", serde_json::json!({"request": "zz"})).await.is_err());
    }

    struct Broken;
    impl PlatformProvider for Broken {
        fn kind(&self) -> &str {
            "mock-hw"
        }
        fn produce(&self, _: &[u8; 32], _: &Measurements) -> Result<Produced, String> {
            Err("tpm is on fire".into())
        }
    }

    async fn host_with(p: Option<Arc<dyn PlatformProvider>>) -> (Host, Client) {
        let host = Host::start_with(new_secret_key().unwrap(), true, HostConfig { platform: p, ..Default::default() }).await.unwrap();
        let c = Client::connect(host.addr(), new_secret_key().unwrap(), true).await.unwrap();
        (host, c)
    }

    #[tokio::test]
    async fn host_attaches_platform_evidence_and_client_checks_it() {
        let (_h, c) = host_with(Some(Arc::new(EchoBinding))).await;
        // With a verifier that understands the evidence, the host reaches T1.
        let v = c.verify_host(Tier::T1, &[&CheckBinding]).await.unwrap();
        assert_eq!(v.achieved, Tier::T1);
        assert_eq!(v.statement.platform, "mock-hw");
        assert_eq!(v.device.as_deref(), Some("mock-hw:mock-device"));
        // Without one, the same evidence is not believed and the T1 requirement fails.
        let e = c.verify_host(Tier::T1, &[]).await.unwrap_err();
        assert!(format!("{e:#}").contains("no verifier available"), "{e:#}");
        // T0 is still fine without a verifier.
        assert_eq!(c.verify_host(Tier::T0, &[]).await.unwrap().achieved, Tier::T0);
    }

    #[tokio::test]
    async fn a_failing_provider_degrades_to_a_plain_statement() {
        let (_h, c) = host_with(Some(Arc::new(Broken))).await;
        let v = c.verify_host(Tier::T0, &[&CheckBinding]).await.unwrap();
        assert_eq!(v.statement.platform, "none");
        assert_eq!(v.achieved, Tier::T0);
        assert!(c.verify_host(Tier::T1, &[&CheckBinding]).await.is_err());
    }

    #[tokio::test]
    async fn a_host_without_a_provider_claims_no_platform() {
        let (_h, c) = host_with(None).await;
        assert_eq!(c.verify_host(Tier::T0, &[&CheckBinding]).await.unwrap().statement.platform, "none");
    }
}

#[cfg(target_os = "macos")]
mod secure_enclave {
    use playbox_core::attest::Tier;
    use playbox_core::platform::{PinStore, SecureEnclaveProvider, SecureEnclaveVerifier, SE_KIND};
    use playbox_net::{new_secret_key, Client, Host, HostConfig};
    use std::{process::Command, sync::Arc};

    #[tokio::test]
    async fn a_host_with_a_real_secure_enclave_key_reaches_t1_once_pinned() {
        let dir = std::env::temp_dir().join(format!("playbox-se-net-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/playbox-se/playbox-se.swift");
        let helper = dir.join("playbox-se");
        if !Command::new("swiftc").arg("-O").arg(&src).arg("-o").arg(&helper).status().map(|s| s.success()).unwrap_or(false) {
            eprintln!("skipping: swiftc not available");
            return;
        }
        let provider = SecureEnclaveProvider { helper, key_file: dir.join("se.key") };
        let host = Host::start_with(new_secret_key().unwrap(), true, HostConfig { platform: Some(Arc::new(provider)), ..Default::default() }).await.unwrap();
        let c = Client::connect(host.addr(), new_secret_key().unwrap(), true).await.unwrap();

        // First contact: valid, untrusted, and the fingerprint to pin is reported.
        let first = c.verify_host(Tier::T0, &[&SecureEnclaveVerifier::new(PinStore::in_memory())]).await.unwrap();
        assert_eq!(first.achieved, Tier::T0);
        let fp = first.device.clone().unwrap().strip_prefix("secure-enclave:").unwrap().to_string();
        assert!(c.verify_host(Tier::T1, &[&SecureEnclaveVerifier::new(PinStore::in_memory())]).await.is_err());

        // After the operator pins it, the host reaches T1, every time, with fresh nonces.
        let mut pins = PinStore::in_memory();
        pins.add(SE_KIND, &fp).unwrap();
        let trusting = SecureEnclaveVerifier::new(pins);
        for _ in 0..2 {
            let v = c.verify_host(Tier::T1, &[&trusting]).await.unwrap();
            assert_eq!(v.achieved, Tier::T1);
            assert_eq!(v.statement.platform, "secure-enclave");
        }
    }
}

#[tokio::test]
async fn the_host_list_is_part_of_the_signed_policy() {
    let mk = |hosts: Vec<String>| async move {
        let cfg = HostConfig { min_isolation: playbox_runner::Isolation::None, limits: playbox_runner::Limits { allowed_hosts: hosts, ..Default::default() }, ..Default::default() };
        let h = Host::start_with(new_secret_key().unwrap(), true, cfg).await.unwrap();
        let c = Client::connect(h.addr(), new_secret_key().unwrap(), true).await.unwrap();
        let v = c.verify_host(Tier::T0, &[]).await.unwrap();
        (v.statement.measurements.policy, h)
    };
    let (none, _a) = mk(vec![]).await;
    let (some, _b) = mk(vec!["example.com".into()]).await;
    assert_ne!(none, some);
}

/// A Docker runner whose daemon check is skipped: only its attested description matters here.
struct NoDaemon(playbox_runner::DockerRunner);

#[async_trait::async_trait]
impl playbox_runner::Runner for NoDaemon {
    fn name(&self) -> &'static str {
        "docker"
    }
    fn isolation(&self) -> playbox_runner::Isolation {
        self.0.isolation()
    }
    fn attestation(&self) -> String {
        self.0.attestation()
    }
    async fn preflight(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn run(&self, spec: &playbox_runner::TaskSpec, ws: &std::path::Path) -> anyhow::Result<playbox_runner::TaskOutput> {
        self.0.run(spec, ws).await
    }
}

#[tokio::test]
async fn a_host_that_allows_a_different_sandbox_image_attests_a_different_policy() {
    use playbox_runner::{DockerRunner, SandboxPolicy};
    let mk = |image: &'static str| async move {
        let runner = NoDaemon(DockerRunner::new(SandboxPolicy { allowed_images: vec![image.into()] }));
        let cfg = HostConfig { runner: Some(std::sync::Arc::new(runner)), ..Default::default() };
        let h = Host::start_with(new_secret_key().unwrap(), true, cfg).await.unwrap();
        let c = Client::connect(h.addr(), new_secret_key().unwrap(), true).await.unwrap();
        let v = c.verify_host(Tier::T0, &[]).await.unwrap();
        (v.statement.measurements.policy, h)
    };
    let (expected, _a) = mk("alpine:3").await;
    let (swapped, _b) = mk("evil:latest").await;
    let (same, _c) = mk("alpine:3").await;
    assert_ne!(expected, swapped, "swapping the image must change the signed policy measurement");
    assert_eq!(expected, same);
}
