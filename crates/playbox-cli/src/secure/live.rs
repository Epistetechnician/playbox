//! Live checks against a running hub: auth, headers, framing and error handling.

use super::http::{build, get, h, lan_ip, raw};
use super::machine::{mode_of, out_of};
use super::{Check, Level};
use crate::hub::Reach;
use crate::service;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// Read-only probes of the running hub: nothing here changes its state, and only one request counts toward its sign-in throttle.
pub(super) fn live_checks(port: u16, reach: &Reach, out: &mut Vec<Check>) {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let (running, detail) = service::status();
    if service::installed() {
        let u = service::unit_path();
        let open = mode_of(&u).is_some_and(|m| m & 0o022 != 0);
        out.push(if open {
            Check::new(
                "hub",
                "service-file",
                "Service definition",
                Level::Bad,
                format!(
                    "{} can be changed by other accounts; they could make the hub run anything",
                    u.display()
                ),
            )
            .hint(format!("chmod go-w {}", u.display()))
        } else {
            Check::new(
                "hub",
                "service-file",
                "Service definition",
                Level::Ok,
                format!("{} is only writable by you", u.display()),
            )
        });
        out.push(
            Check::new(
                "hub",
                "service",
                "Background service",
                if running { Level::Ok } else { Level::Warn },
                format!("{detail} (starts at login)"),
            )
            .hint(if running {
                ""
            } else {
                "playbox hub --service install"
            }),
        );
    } else {
        out.push(
            Check::new(
                "hub",
                "service",
                "Background service",
                Level::Info,
                "not installed: the hub only runs while you keep it open",
            )
            .hint("playbox init"),
        );
    }
    if !service::wait_healthy(port, Duration::from_millis(800)) {
        out.push(
            Check::new(
                "hub",
                "running",
                "Hub",
                Level::Info,
                format!("not running on port {port}; live probes skipped"),
            )
            .hint("playbox hub"),
        );
        return;
    }
    out.push(Check::new(
        "hub",
        "running",
        "Hub",
        Level::Ok,
        format!("answering on 127.0.0.1:{port}"),
    ));
    let Some(root) = get(addr, "/", &[]) else {
        return;
    };
    let missing: Vec<&str> = [
        "content-security-policy",
        "x-frame-options: deny",
        "x-content-type-options: nosniff",
        "referrer-policy",
    ]
    .into_iter()
    .filter(|k| !root.head.contains(k))
    .collect();
    out.push(
        if missing.is_empty() && !root.head.contains("access-control-allow-origin") {
            Check::new(
                "hub",
                "headers",
                "Browser protections",
                Level::Ok,
                "CSP, frame, sniffing and referrer protections are sent; no CORS opening",
            )
        } else {
            Check::new(
                "hub",
                "headers",
                "Browser protections",
                Level::Bad,
                format!("missing or unsafe headers: {}", missing.join(", ")),
            )
            .hint("update the hub")
        },
    );
    let mut leaks = Vec::new();
    for p in [
        "/api/me",
        "/api/rooms",
        "/api/system",
        "/api/integrations",
        "/api/invites",
        "/x/grants",
    ] {
        match get(addr, p, &[]) {
            Some(r) if r.status == 401 => {}
            Some(r) => leaks.push(format!("{p} -> {}", r.status)),
            None => leaks.push(format!("{p} -> no answer")),
        }
    }
    out.push(if leaks.is_empty() {
        Check::new(
            "hub",
            "unauth",
            "Closed to strangers",
            Level::Ok,
            "every private route answers 401 without a session",
        )
    } else {
        Check::new(
            "hub",
            "unauth",
            "Closed to strangers",
            Level::Bad,
            format!("unexpected answers without a session: {}", leaks.join(", ")),
        )
        .hint("do not expose this hub; update it")
    });
    let evil = raw(
        addr,
        &build(
            "POST",
            "/api/rooms",
            &[
                h("Host", &format!("127.0.0.1:{port}")),
                h("Origin", "https://evil.example"),
                h("Content-Type", "application/json"),
                h("Content-Length", "2"),
            ],
            b"{}",
        ),
    );
    out.push(match evil {
        Ok(r) if r.status == 403 => Check::new(
            "hub",
            "origin",
            "Cross-site requests",
            Level::Ok,
            "a request from another site is refused",
        ),
        Ok(r) => Check::new(
            "hub",
            "origin",
            "Cross-site requests",
            Level::Bad,
            format!("a cross-site POST was answered {}", r.status),
        )
        .hint("update the hub"),
        Err(e) => Check::new(
            "hub",
            "origin",
            "Cross-site requests",
            Level::Warn,
            format!("no answer ({e})"),
        ),
    });
    // What the network can see. A hub meant for this computer should not answer on the LAN address.
    if let Some(ip) = lan_ip() {
        let exposed =
            TcpStream::connect_timeout(&SocketAddr::new(ip, port), Duration::from_millis(800))
                .is_ok();
        out.push(match (exposed, reach) {
            (true, Reach::Local) => Check::new(
                "hub",
                "lan",
                "Network exposure",
                Level::Warn,
                format!("the hub answers on {ip}:{port}, so anyone on this network can reach it"),
            )
            .hint("restart without --lan"),
            (true, _) => Check::new(
                "hub",
                "lan",
                "Network exposure",
                Level::Info,
                format!("also answers on {ip}:{port} (--lan); pairing there is plain HTTP"),
            ),
            (false, _) => Check::new(
                "hub",
                "lan",
                "Network exposure",
                Level::Ok,
                "only answers on this computer",
            ),
        });
    }
    if let Reach::Named { hostname, .. } = reach {
        let code = out_of(
            "curl",
            &[
                "-sS",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "--max-time",
                "8",
                &format!("https://{hostname}/api/me"),
            ],
        );
        out.push(match code.as_deref().map(str::trim) {
            Some("401") => Check::new(
                "hub",
                "public",
                "Public address",
                Level::Ok,
                format!("https://{hostname} reaches the hub and private routes still answer 401"),
            ),
            Some(c @ ("302" | "403")) => Check::new(
                "hub",
                "public",
                "Public address",
                Level::Ok,
                format!("https://{hostname} is behind an access gate ({c})"),
            ),
            Some(c) => Check::new(
                "hub",
                "public",
                "Public address",
                Level::Warn,
                format!("https://{hostname}/api/me answered {c}, not 401"),
            ),
            None => Check::new(
                "hub",
                "public",
                "Public address",
                Level::Info,
                "couldn't test the public address (curl missing?)",
            ),
        });
    }
}

// ---------- the fuzzer ----------
