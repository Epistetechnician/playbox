//! Tests for the browse policy, the visit loop and the rate limits.

use super::policy::{bare_host, is_public, Policy};
use super::{clip_url, BrowseConfig, BrowseError, Browser, Mode};
use base64::Engine as _;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use url::Url;

use crate::ws::server;

fn pol(a: &[&str]) -> Policy {
    Policy::new(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>(), false).unwrap()
}

fn ok(p: &Policy, u: &str) -> bool {
    p.check(&Url::parse(u).unwrap()).is_ok()
}

#[test]
fn the_allowlist_matches_hosts_ports_and_subdomains() {
    let p = pol(&["example.com", "*.docs.rs", "tools.test:8080"]);
    assert!(ok(&p, "https://example.com/x"));
    assert!(ok(&p, "http://EXAMPLE.com./x"));
    assert!(ok(&p, "https://example.com:443/"));
    assert!(
        !ok(&p, "https://example.com:8443/"),
        "a pattern without a port allows 80 and 443 only"
    );
    assert!(ok(&p, "https://a.docs.rs/"));
    assert!(ok(&p, "https://a.b.docs.rs/"));
    assert!(!ok(&p, "https://docs.rs/"), "*.docs.rs is the subdomains");
    assert!(!ok(&p, "https://evildocs.rs/"));
    assert!(!ok(&p, "https://docs.rs.evil.com/"));
    assert!(!ok(&p, "https://example.com.evil.com/"));
    assert!(!ok(&p, "https://notexample.com/"));
    assert!(ok(&p, "http://tools.test:8080/"));
    assert!(!ok(&p, "http://tools.test/"));
    // Credentials, other schemes and tricks in the authority.
    assert!(!ok(&p, "https://user:pw@example.com/"));
    assert!(!ok(&p, "https://example.com@evil.com/"));
    assert!(!ok(&p, "https://evil.com#@example.com/"));
    assert!(!ok(&p, "https://evil.com\\@example.com/"));
    for bad in [
        "file:///etc/passwd",
        "ftp://example.com/",
        "javascript:alert(1)",
        "data:text/html,hi",
        "chrome://gpu",
        "view-source:https://example.com/",
    ] {
        assert!(
            !Url::parse(bad)
                .map(|u| p.check(&u).is_ok())
                .unwrap_or(false),
            "{bad}"
        );
    }
    // Unicode and encoded hosts are compared as the browser will see them.
    assert!(
        !ok(&p, "https://exаmple.com/"),
        "a Cyrillic a is another host"
    );
    assert!(!ok(&p, "https://example.com%2eevil.com/"));
    assert!(
        !ok(&p, "https://%2edocs.rs/"),
        "an empty label is not a subdomain"
    );
    assert!(!ok(&p, "https://a..docs.rs/"));
}

/// Whatever string a page or a member offers, a URL the check accepts is http or https, has no credentials, and is on a
/// listed host and port as the browser will read it. Seeded, so a failure reproduces.
#[test]
fn nothing_odd_passes_the_check() {
    let p = pol(&["example.com", "*.docs.rs"]);
    let parts = [
        "http",
        "https",
        "ftp",
        "file",
        "javascript",
        "://",
        ":",
        "//",
        "/",
        "\\",
        "@",
        "#",
        "?",
        ".",
        "..",
        "%2e",
        "%2f",
        "%40",
        "%00",
        "example.com",
        "EXAMPLE.COM",
        "docs.rs",
        "a.docs.rs",
        "evil.com",
        "127.0.0.1",
        "[::1]",
        "0x7f.1",
        "2130706433",
        "443",
        "80",
        "8080",
        " ",
        "\t",
        "\n",
        "\u{202e}",
        "\u{0430}",
        "xn--",
        "user",
        "pw",
    ];
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut accepted = 0;
    for _ in 0..200_000 {
        let n = 2 + (next() % 7) as usize;
        let s: String = (0..n)
            .map(|_| parts[(next() % parts.len() as u64) as usize])
            .collect();
        let Ok(u) = Url::parse(&s) else { continue };
        if p.check(&u).is_ok() {
            accepted += 1;
            let host = bare_host(u.host_str().unwrap());
            assert!(matches!(u.scheme(), "http" | "https"), "{s:?}");
            assert!(u.username().is_empty() && u.password().is_none(), "{s:?}");
            assert!(
                host == "example.com"
                    || (host.ends_with(".docs.rs") && host.len() > ".docs.rs".len()),
                "{s:?} -> {host}"
            );
            assert!(matches!(u.port_or_known_default(), Some(80 | 443)), "{s:?}");
        }
    }
    assert!(
        accepted > 0,
        "the generator must produce some allowed URLs, or this test proves nothing"
    );
}

#[test]
fn bad_patterns_are_refused() {
    for bad in [
        "",
        "*",
        "*.com",
        "https://example.com",
        "example.com/path",
        "user@example.com",
        "ex ample.com",
        "example.com:notaport",
        "a*b.com",
        "*.*.com",
    ] {
        assert!(Policy::new(&[bad.to_string()], false).is_err(), "{bad:?}");
    }
    assert!(Policy::new(&[], false).is_err());
    assert!(Policy::new(
        &[
            "127.0.0.1:8080".into(),
            "[::1]:9".into(),
            "localhost".into()
        ],
        true
    )
    .is_ok());
}

#[test]
fn private_and_special_addresses_are_not_public() {
    for ip in [
        "127.0.0.1",
        "10.1.2.3",
        "172.16.0.1",
        "172.31.255.255",
        "192.168.0.1",
        "169.254.169.254",
        "100.64.0.1",
        "0.0.0.0",
        "255.255.255.255",
        "224.0.0.1",
        "192.0.2.1",
        "198.18.0.1",
        "240.0.0.1",
        "::1",
        "::",
        "fe80::1",
        "fc00::1",
        "fd12::1",
        "::ffff:127.0.0.1",
        "::ffff:10.0.0.1",
        "64:ff9b::a00:1",
        "2001:db8::1",
        "ff02::1",
    ] {
        assert!(!is_public(&ip.parse().unwrap()), "{ip}");
    }
    for ip in [
        "8.8.8.8",
        "1.1.1.1",
        "93.184.216.34",
        "172.32.0.1",
        "100.63.255.255",
        "2606:4700:4700::1111",
        "::ffff:8.8.8.8",
    ] {
        assert!(is_public(&ip.parse().unwrap()), "{ip}");
    }
}

#[tokio::test]
async fn a_name_that_resolves_to_a_private_address_is_refused() {
    let p = pol(&["localhost", "127.0.0.1", "8.8.8.8"]);
    assert!(p
        .check_resolved(&Url::parse("http://localhost/").unwrap())
        .await
        .is_err());
    assert!(p
        .check_resolved(&Url::parse("http://127.0.0.1/").unwrap())
        .await
        .is_err());
    assert!(p
        .check_resolved(&Url::parse("http://8.8.8.8/").unwrap())
        .await
        .is_ok());
    let open = Policy::new(&["127.0.0.1".into()], true).unwrap();
    assert!(open
        .check_resolved(&Url::parse("http://127.0.0.1/").unwrap())
        .await
        .is_ok());
}

#[test]
fn urls_in_messages_carry_no_query() {
    assert_eq!(
        clip_url("https://a.example/p?token=secret#frag"),
        "https://a.example/p"
    );
    assert_eq!(clip_url("https://u:p@a.example/p"), "https://a.example/p");
}

/// A scripted browser: answers the commands a visit sends, pauses two requests (one allowed, one not), and records
/// what the host did with each.
async fn fake_browser(
    l: tokio::net::TcpListener,
    page_text: &'static str,
) -> tokio::task::JoinHandle<Vec<String>> {
    tokio::spawn(async move {
        let mut p = server::accept(&l).await;
        let mut log = Vec::new();
        while let Some(m) = p.recv().await {
            let v: Value = serde_json::from_str(&m).unwrap();
            let id = v["id"].as_u64().unwrap();
            let method = v["method"].as_str().unwrap().to_string();
            let sess = v.get("sessionId").cloned();
            log.push(
                format!(
                    "{method} {}",
                    v["params"]
                        .get("requestId")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                )
                .trim()
                .to_string(),
            );
            let result = match method.as_str() {
                "Target.createBrowserContext" => json!({"browserContextId": "ctx"}),
                "Target.createTarget" => json!({"targetId": "T1"}),
                "Target.attachToTarget" => json!({"sessionId": "S1"}),
                "Page.navigate" => json!({"frameId": "F"}),
                "Runtime.evaluate" => {
                    let e = v["params"]["expression"].as_str().unwrap();
                    if e.starts_with("JSON.stringify") {
                        json!({"result": {"type": "string", "value": "{\"u\":\"https://example.com/final\",\"t\":\"A title\"}"}})
                    } else {
                        json!({"result": {"type": "string", "value": page_text}})
                    }
                }
                "Page.captureScreenshot" => {
                    json!({"data": base64::engine::general_purpose::STANDARD.encode(b"PNGDATA")})
                }
                _ => json!({}),
            };
            let mut reply = json!({"id": id, "result": result});
            if let Some(s) = sess {
                reply["sessionId"] = s;
            }
            p.send(&reply.to_string()).await;
            if method == "Page.navigate" {
                for (rid, url) in [
                    ("r1", "https://example.com/style.css"),
                    ("r2", "http://169.254.169.254/latest/meta-data"),
                    ("r3", "https://evil.example/x.js"),
                ] {
                    p.send(&json!({"method": "Fetch.requestPaused", "sessionId": "S1", "params": {"requestId": rid, "request": {"url": url, "method": "GET"}}}).to_string()).await;
                }
                p.send(&json!({"method": "Network.responseReceived", "sessionId": "S1", "params": {"type": "Document", "response": {"status": 200}}}).to_string()).await;
                p.send(
                    &json!({"method": "Page.loadEventFired", "sessionId": "S1", "params": {}})
                        .to_string(),
                )
                .await;
            }
        }
        log
    })
}

fn browser_for(port: u16, allow: &[&str]) -> Browser {
    let mut c = BrowseConfig::new(
        &format!("ws://127.0.0.1:{port}/devtools/browser/x"),
        allow.iter().map(|s| s.to_string()).collect(),
    );
    c.timeout = Duration::from_secs(10);
    Browser::new(c).unwrap()
}

#[tokio::test]
async fn a_visit_checks_every_request_the_page_makes() {
    // `example.com` is a public name; the test must not depend on the network, so allow-private skips only the DNS step.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let srv = fake_browser(l, "Hello from the page").await;
    let mut c = BrowseConfig::new(
        &format!("ws://127.0.0.1:{port}/"),
        vec!["example.com".into()],
    );
    c.allow_private = true;
    c.timeout = Duration::from_secs(10);
    let b = Browser::new(c).unwrap();
    let r = b
        .browse(
            "alice",
            "https://example.com/start?secret=1",
            Mode::Text,
            None,
        )
        .await
        .unwrap();
    assert_eq!(r.text.as_deref(), Some("Hello from the page"));
    assert_eq!(r.title, "A title");
    assert_eq!(r.status, Some(200));
    assert_eq!(r.requests, 3);
    assert_eq!(r.blocked.len(), 2, "{:?}", r.blocked);
    assert!(
        r.blocked.iter().any(|x| x.contains("169.254.169.254"))
            || r.blocked
                .iter()
                .any(|x| x.contains("not on this room's browser allowlist")),
        "{:?}",
        r.blocked
    );
    assert!(r.blocked.iter().any(|x| x.contains("evil.example")));
    let log = srv.await.unwrap();
    assert!(
        log.contains(&"Fetch.continueRequest r1".to_string()),
        "{log:?}"
    );
    assert!(log.contains(&"Fetch.failRequest r2".to_string()), "{log:?}");
    assert!(log.contains(&"Fetch.failRequest r3".to_string()), "{log:?}");
    let at = |s: &str| {
        log.iter()
            .position(|x| x.starts_with(s))
            .unwrap_or_else(|| panic!("{s} not in {log:?}"))
    };
    assert!(
        at("Fetch.enable") < at("Page.navigate"),
        "interception must be on before the page is asked to load"
    );
    assert!(at("Emulation.setScriptExecutionDisabled") < at("Page.navigate"));
}

#[tokio::test]
async fn the_start_url_is_refused_before_any_browser_is_contacted() {
    // No browser is listening at this port: a refusal must come first.
    let b = browser_for(9, &["example.com"]);
    for u in [
        "https://evil.example/",
        "file:///etc/passwd",
        "http://169.254.169.254/",
        "https://user:pw@example.com/",
        "not a url",
        "https://example.com:8443/",
    ] {
        match b.browse("alice", u, Mode::Text, None).await {
            Err(BrowseError::Refused(_)) => {}
            other => panic!("{u}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn text_is_capped_and_a_screenshot_comes_back_as_bytes() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let _srv = fake_browser(l, "héllo wörld, héllo wörld, héllo wörld").await;
    let mut c = BrowseConfig::new(
        &format!("ws://127.0.0.1:{port}/"),
        vec!["example.com".into()],
    );
    c.allow_private = true;
    c.max_text_bytes = 9;
    let b = Browser::new(c).unwrap();
    let r = b
        .browse("alice", "https://example.com/", Mode::Text, None)
        .await
        .unwrap();
    assert!(
        r.truncated
            && r.text.as_ref().unwrap().len() <= 9
            && r.text
                .as_ref()
                .unwrap()
                .is_char_boundary(r.text.as_ref().unwrap().len())
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let _srv = fake_browser(l, "x").await;
    let mut c = BrowseConfig::new(
        &format!("ws://127.0.0.1:{port}/"),
        vec!["example.com".into()],
    );
    c.allow_private = true;
    let b = Browser::new(c).unwrap();
    let r = b
        .browse("alice", "https://example.com/", Mode::Screenshot, None)
        .await
        .unwrap();
    assert_eq!(r.screenshot.as_deref(), Some(&b"PNGDATA"[..]));
}

#[tokio::test]
async fn visits_are_rate_limited_per_member_and_bounded_at_once() {
    let mut c = BrowseConfig::new("ws://127.0.0.1:9/", vec!["example.com".into()]);
    c.allow_private = true;
    c.visits_per_minute = 2;
    c.timeout = Duration::from_secs(2);
    let b = Browser::new(c).unwrap();
    // Nothing listens at port 9, so each visit fails after passing the limits: two failures, then a refusal.
    for _ in 0..2 {
        assert!(matches!(
            b.browse("alice", "https://example.com/", Mode::Text, None)
                .await,
            Err(BrowseError::Failed(_))
        ));
    }
    assert!(matches!(
        b.browse("alice", "https://example.com/", Mode::Text, None)
            .await,
        Err(BrowseError::Refused(_))
    ));
    assert!(
        matches!(
            b.browse("bob", "https://example.com/", Mode::Text, None)
                .await,
            Err(BrowseError::Failed(_))
        ),
        "another member has their own budget"
    );
}

#[tokio::test]
async fn a_browser_that_goes_silent_ends_the_visit_at_the_deadline() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut p = server::accept(&l).await;
        let _ = p.recv().await; // read one command and never answer
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let mut c = BrowseConfig::new(
        &format!("ws://127.0.0.1:{port}/"),
        vec!["example.com".into()],
    );
    c.allow_private = true;
    c.timeout = Duration::from_millis(800);
    let b = Browser::new(c).unwrap();
    let t0 = Instant::now();
    assert!(matches!(
        b.browse("alice", "https://example.com/", Mode::Text, None)
            .await,
        Err(BrowseError::Failed(_))
    ));
    assert!(t0.elapsed() < Duration::from_secs(5));
}
