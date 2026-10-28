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
