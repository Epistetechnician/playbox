//! The Linux confinement around a real libkrun guest on KVM: jail, run cgroup, syscall filter and network filter all on,
//! and a VM still boots, runs and is held.
//!
//! Needs Linux, root (cgroups and nftables), `/dev/kvm`, the helper and Docker. Skipped with a message anywhere else;
//! `PLAYBOX_CONFINE_REQUIRE_ALL=1` turns a skip into a failure, as in `tests/confine.rs`.
#![cfg(target_os = "linux")]

use playbox_runner::{
    confine::Confinement,
    egress::{EgressConfig, EgressUpstream},
    find_helper,
    netpolicy::Level,
    runner_with,
    secrets::SecretStore,
    KrunRunner, Runner, RunnerOptions, SandboxPolicy, TaskSpec,
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
};

/// The tests share one cgroup root and one nftables table, and `preflight` rebuilds the table, so they run one at a time.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn skip(why: &str) -> Option<Arc<dyn Runner>> {
    eprintln!("skipping: {why}");
    assert!(
        std::env::var_os("PLAYBOX_CONFINE_REQUIRE_ALL").is_none(),
        "PLAYBOX_CONFINE_REQUIRE_ALL is set and this could not run: {why}"
    );
    None
}

fn says_yes(program: &str, args: &[&str], want: &str) -> bool {
    Command::new(program)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == want)
}

/// A confined runner whose tasks may have at most `max_net`.
fn confined(max_net: Level) -> Option<Arc<dyn Runner>> {
    if !says_yes("id", &["-u"], "0") {
        return skip("not root (the run cgroup and the network filter need it)");
    }
    if !std::path::Path::new("/dev/kvm").exists() {
        return skip("no /dev/kvm");
    }
    if find_helper().is_err() {
        return skip("playbox-krun-helper not installed (scripts/install-krun.sh)");
    }
    if !Command::new("docker")
        .arg("info")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return skip("docker is not running");
    }
    let opts = RunnerOptions {
        confine: Some((max_net, true)),
        egress: None,
    };
    Some(
        runner_with("libkrun", SandboxPolicy::default(), &opts).expect("a confined libkrun runner"),
    )
}

fn ws(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "playbox-confine-guest-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sh(script: &str) -> TaskSpec {
    TaskSpec {
        timeout_secs: 90,
        mem_mib: 256,
        ..TaskSpec::new(&["sh", "-c", script])
    }
}

// REQ: BOX-3
#[tokio::test]
async fn a_guest_boots_and_works_under_the_whole_confinement() {
    let _one = ONE_AT_A_TIME.lock().await;
    let Some(r) = confined(Level::None) else {
        return;
    };
    let att = r.attestation();
    assert!(
        att.contains("jail=") && att.contains("seccomp=deny:"),
        "{att}"
    );
    r.preflight().await.unwrap();
    let w = ws("boot");
    let out = r
        .run(
            &sh("echo out; echo err >&2; echo kept > /workspace/f; exit 7"),
            &w,
        )
        .await
        .unwrap();
    assert_eq!(out.exit_code, Some(7), "{out:?}");
    assert_eq!(out.stdout.trim(), "out", "{out:?}");
    assert!(out.stderr.contains("err"), "{out:?}");
    assert_eq!(std::fs::read_to_string(w.join("f")).unwrap().trim(), "kept");
}

// REQ: BOX-3
#[tokio::test]
async fn a_confined_guest_without_a_network_has_none() {
    let _one = ONE_AT_A_TIME.lock().await;
    let Some(r) = confined(Level::Internet) else {
        return;
    };
    r.preflight().await.unwrap(); // what a host does at start: the cgroup root and the fail-closed base rules
    let w = ws("nonet");
    // `which wget` proves the probe tool exists, so a failed fetch means no network and not a missing program.
    let out = r.run(&sh("which wget >/dev/null && echo tool; wget -q -T 3 -O /dev/null http://1.1.1.1/ && echo REACHED; true"), &w).await.unwrap();
    assert!(out.stdout.contains("tool"), "{out:?}");
    assert!(!out.stdout.contains("REACHED"), "{out:?}");
}

// REQ: BOX-3
#[tokio::test]
async fn a_confined_guest_with_a_network_reaches_the_internet_but_not_the_host() {
    let _one = ONE_AT_A_TIME.lock().await;
    let Some(r) = confined(Level::Internet) else {
        return;
    };
    r.preflight().await.unwrap();
    // Something only the host's loopback serves. TSI would hand it to an unfiltered guest.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let _ = s.write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok");
        }
    });
    // Plain TCP connects, so no redirect or name lookup decides the answer.
    let script = format!(
        "which nc >/dev/null && echo TOOL; \
         nc -w 5 1.1.1.1 80 </dev/null && echo INTERNET; \
         nc -w 3 127.0.0.1 {port} </dev/null && echo LOOPBACK; \
         nc -w 3 169.254.169.254 80 </dev/null && echo METADATA; \
         nslookup example.com >/dev/null 2>&1 && echo DNS; true"
    );
    let spec = TaskSpec {
        network: true,
        ..sh(&script)
    };

    // The control: the same task on an unfiltered runner does reach the host's loopback. Without this, a pass below could
    // mean only that TSI never forwarded the connection.
    let open = KrunRunner::locate(SandboxPolicy::default()).unwrap();
    std::env::set_var("PLAYBOX_ALLOW_UNFILTERED_NETWORK", "1");
    let unfiltered = open.run(&spec, &ws("open")).await;
    std::env::remove_var("PLAYBOX_ALLOW_UNFILTERED_NETWORK");
    let unfiltered = unfiltered.unwrap();
    assert!(
        unfiltered.stdout.contains("LOOPBACK"),
        "the control did not reach the host's loopback: {unfiltered:?}"
    );

    let out = r.run(&spec, &ws("net")).await.unwrap();
    assert!(out.stdout.contains("TOOL"), "{out:?}");
    // The canary: without it a guest with no network at all would pass the two checks below.
    assert!(
        out.stdout.contains("INTERNET"),
        "the guest has no network, so the filter was not tested: {out:?}"
    );
    assert!(
        !out.stdout.contains("LOOPBACK"),
        "the guest reached the host's loopback: {out:?}"
    );
    assert!(
        !out.stdout.contains("METADATA"),
        "the guest reached the metadata address: {out:?}"
    );
    // REQ: BOX-8 (the guest was given a resolver the filter lets it reach)
    assert!(
        out.stdout.contains("DNS"),
        "the guest could not resolve a name: {out:?}"
    );
}

// REQ: BOX-3
// REQ: BOX-8
#[tokio::test]
async fn a_confined_guest_with_a_host_list_reaches_those_hosts_by_name_and_nothing_else() {
    let _one = ONE_AT_A_TIME.lock().await;
    let Some(r) = confined(Level::Internet) else {
        return;
    };
    r.preflight().await.unwrap();
    let script = "grep -c example.com /etc/hosts; \
         nc -w 5 example.com 80 </dev/null && echo NAMED; \
         nc -w 3 1.1.1.1 80 </dev/null && echo OTHER; true";
    let spec = TaskSpec {
        network: true,
        allowed_hosts: vec!["example.com:80".into()],
        ..sh(script)
    };
    let out = r.run(&spec, &ws("hosts")).await.unwrap();
    assert!(
        out.stdout.contains("NAMED"),
        "the listed host was not reachable by name: {out:?}"
    );
    assert!(
        !out.stdout.contains("OTHER"),
        "the guest reached a host that is not on its list: {out:?}"
    );
}

// REQ: BOX-3, BOX-7
#[tokio::test]
async fn a_confined_guest_with_egress_reaches_proxy_only_without_leaking_token() {
    let _one = ONE_AT_A_TIME.lock().await;
    let Some(_) = confined(Level::None) else {
        return;
    };

    let base = ws("egress");
    let sec_dir = base.join("secrets");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&sec_dir)
        .unwrap();
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(sec_dir.join("model_key"))
        .unwrap();
    let fixture_val = ["pbtest", "box7", "demo", "value"].join("-");
    f.write_all(format!("{fixture_val}\n").as_bytes()).unwrap();
    drop(f);
    let log_path = base.join("egress.jsonl");

    // Local fake model upstream on host loopback: verifies host-injected secret, refuses guest creds.
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let up_port = upstream.local_addr().unwrap().port();
    let seen_auth: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = Arc::clone(&seen_auth);
    std::thread::spawn(move || {
        for mut s in upstream.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            seen_clone.lock().unwrap().push(req);
            let _ = s.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 7\r\nConnection: close\r\n\r\nOK_BOX7",
            );
        }
    });

    // Separate host loopback port that the guest must NOT be able to reach directly.
    let other_host = TcpListener::bind("127.0.0.1:0").unwrap();
    let other_port = other_host.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in other_host.incoming().flatten() {
            let _ = s.write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nno");
        }
    });

    let cfg = EgressConfig {
        upstreams: vec![EgressUpstream {
            name: "model".into(),
            base: format!("http://127.0.0.1:{up_port}"),
            auth_header: "x-api-key".into(),
            auth_prefix: String::new(),
            secret: "model_key".into(),
            allow_paths: vec!["/v1/models".into()],
            pass_headers: vec!["content-type".into()],
            extra_headers: std::collections::BTreeMap::new(),
            env_var: None,
            insecure_http: true,
            allow_private: false,
        }],
        log_file: Some(log_path.clone()),
        max_log_bytes: None,
    };
    let mut confine = Confinement::new(Level::None);
    confine.seccomp = true;
    confine.egress = true;
    let r = KrunRunner::locate(SandboxPolicy::default())
        .unwrap()
        .with_secrets(SecretStore::new(sec_dir))
        .with_egress(cfg)
        .with_confinement(confine);
    r.preflight().await.unwrap();

    let ws_dir = ws("egress-ws");
    let probe_script = format!(
        "echo CMDLINE=$(cat /proc/cmdline)\n\
         RESP=$(wget -q -O - \"$PLAYBOX_EGRESS_MODEL_URL/v1/models\" 2>/dev/null || true)\n\
         echo PROXY=$RESP\n\
         nc -w 2 127.0.0.1 {other_port} </dev/null && echo OTHER_LOOPBACK\n\
         nc -w 2 1.1.1.1 80 </dev/null && echo INTERNET\n\
         true\n"
    );
    std::fs::write(ws_dir.join("probe.sh"), probe_script).unwrap();
    let spec = TaskSpec {
        egress: vec!["model".into()],
        timeout_secs: 90,
        mem_mib: 256,
        ..TaskSpec::new(&["sh", "/workspace/probe.sh"])
    };
    let out = r.run(&spec, &ws_dir).await.unwrap();
    assert!(
        out.stdout.lines().any(|l| l == "PROXY=OK_BOX7"),
        "guest could not reach host BOX-7 egress proxy: {out:?}"
    );
    assert!(
        !out.stdout.lines().any(|l| l == "OTHER_LOOPBACK"),
        "guest reached an unpermitted host loopback port: {out:?}"
    );
    assert!(
        !out.stdout.lines().any(|l| l == "INTERNET"),
        "guest reached public internet at Level::None: {out:?}"
    );
    assert!(
        !out.stdout.contains(&fixture_val),
        "guest saw host secret: {out:?}"
    );
    let cmdline_line = out
        .stdout
        .lines()
        .find(|l| l.starts_with("CMDLINE="))
        .unwrap_or("");
    assert!(
        !cmdline_line.contains("/t/") && !cmdline_line.contains("PLAYBOX_EGRESS_MODEL_URL"),
        "egress capability token leaked into guest /proc/cmdline: {cmdline_line}"
    );

    let reqs = seen_auth.lock().unwrap();
    let expected_hdr = format!("x-api-key: {fixture_val}");
    assert!(
        reqs.iter()
            .any(|req| req.to_ascii_lowercase().contains(&expected_hdr)),
        "upstream did not receive host-injected secret: {reqs:?}"
    );
    let log = std::fs::read_to_string(&log_path).unwrap();
    assert!(log.contains("/v1/models") && !log.contains(&fixture_val));
}
