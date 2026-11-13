//! `browse` over a room: the wire, the allowlist, and (when Chrome is installed) a real headless browser that must stay on it.

use playbox_net::browse::{BrowseConfig, Browser, Mode};
use playbox_net::{join_link, new_secret_key, Host, HostConfig};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

async fn room(browser: Option<Browser>) -> (Host, playbox_net::Client) {
    let cfg = HostConfig { browse: browser.map(Arc::new), ..HostConfig::default() };
    let host = Host::start_with(new_secret_key().unwrap(), true, cfg).await.unwrap();
    let link = host.create_link(Duration::from_secs(60), 1).unwrap().encode();
    let a = join_link(&link, new_secret_key().unwrap(), true).await.unwrap();
    (host, a)
}

#[tokio::test]
async fn a_room_without_a_browser_says_so() {
    let (host, a) = room(None).await;
    let e = a.browse("https://example.com/", Mode::Text, None).await.unwrap_err().to_string();
    assert!(e.contains("no browser"), "{e}");
    a.close().await;
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_wire_refuses_what_the_allowlist_does_not_name() {
    // Nothing listens at the endpoint: every refusal below must happen before the browser is needed.
    let b = Browser::new(BrowseConfig::new("ws://127.0.0.1:9/", vec!["example.com".into()])).unwrap();
    let (host, a) = room(Some(b)).await;
    for u in ["https://evil.example/", "file:///etc/passwd", "http://169.254.169.254/latest/meta-data", "https://user:pw@example.com/", "https://example.com:8443/"] {
        let e = a.browse(u, Mode::Text, None).await.unwrap_err().to_string();
        assert!(!e.is_empty(), "{u}");
    }
    let e = a.browse("https://evil.example/", Mode::Text, None).await.unwrap_err().to_string();
    assert!(e.contains("allowlist"), "{e}");
    let log = std::fs::read_to_string(host.audit_path().unwrap_or_default()).unwrap_or_default();
    assert!(!log.contains("secret"), "no query strings in the log");
    a.close().await;
    host.shutdown().await.unwrap();
}

// ---- a real browser ----

fn chrome() -> Option<String> {
    let candidates = [
        std::env::var("CHROME_BIN").ok(),
        Some("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".to_string()),
        Some("/usr/bin/google-chrome".to_string()),
        Some("/usr/bin/chromium".to_string()),
        Some("/usr/bin/chromium-browser".to_string()),
    ];
    candidates.into_iter().flatten().find(|p| std::path::Path::new(p).exists())
}

struct Chrome {
    child: Child,
    port: u16,
    dir: std::path::PathBuf,
}

impl Drop for Chrome {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn start_chrome(bin: &str) -> Chrome {
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("pbx-chrome-{port}"));
    let log = std::env::temp_dir().join(format!("pbx-chrome-{port}.log"));
    let child = Command::new(bin)
        .args(["--headless=new", &format!("--remote-debugging-port={port}"), "--remote-debugging-address=127.0.0.1", &format!("--user-data-dir={}", dir.display()), "--no-first-run", "--no-default-browser-check", "--disable-gpu", "--no-sandbox", "--disable-dev-shm-usage", "--disable-extensions", "--disable-background-networking", "about:blank"])
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).expect("chrome log"))
        .spawn()
        .expect("start chrome");
    let mut c = Chrome { child, port, dir };
    let said = || std::fs::read_to_string(&log).unwrap_or_default().chars().take(800).collect::<String>();
    for _ in 0..300 {
        // A browser that already exited will never listen; say why instead of waiting it out.
        if let Ok(Some(status)) = c.child.try_wait() {
            panic!("chrome exited ({status}) before it listened: {}", said());
        }
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            tokio::time::sleep(Duration::from_millis(300)).await;
            return c;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("chrome did not listen on port {port} within 30 s: {}", said());
}

/// A web server for tests: `site` serves the pages; `off` is the host that must never be contacted and counts every connection.
type Route = (u16, String, Vec<(&'static str, String)>);

fn serve(routes: fn(&str, u16) -> Route, forbidden_port: u16) -> (u16, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (code, body, headers) = routes(&path, forbidden_port);
                let mut head = format!("HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: text/html\r\n", body.len());
                for (k, v) in headers {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                s.write_all(format!("{head}\r\n{body}").as_bytes()).ok();
            });
        }
    });
    (port, hits)
}

/// The forbidden server: counts connections, and records the first line of any request that arrives on them.
fn forbidden() -> (u16, Arc<AtomicUsize>, Arc<std::sync::Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let lines = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (h, ls) = (hits.clone(), lines.clone());
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            h.fetch_add(1, Ordering::SeqCst);
            let ls = ls.clone();
            std::thread::spawn(move || {
                s.set_read_timeout(Some(Duration::from_millis(1500))).ok();
                let mut buf = [0u8; 2048];
                if let Ok(n) = s.read(&mut buf) {
                    if n > 0 {
                        ls.lock().unwrap().push(String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or("").to_string());
                    }
                }
            });
        }
    });
    (port, hits, lines)
}

fn pages(path: &str, off: u16) -> Route {
    match path.split('?').next().unwrap() {
        "/" => (200, format!(r#"<html><head><title>Allowed page</title>
            <link rel="stylesheet" href="http://127.0.0.1:{off}/style.css">
            <link rel="preload" href="http://127.0.0.1:{off}/preload" as="fetch">
            <link rel="prefetch" href="http://127.0.0.1:{off}/prefetch">
            <link rel="preconnect" href="http://127.0.0.1:{off}">
            <link rel="dns-prefetch" href="//127.0.0.1:{off}">
            </head><body><h1>Hello from the allowed site</h1>
            <img src="http://127.0.0.1:{off}/pixel.png">
            <iframe src="http://localhost:{off}/frame"></iframe>
            <iframe src="http://127.0.0.1:{off}/frame2"></iframe>
            <form action="http://127.0.0.1:{off}/post" method="post" id="f"></form>
            <script>document.title = "script ran"; new Image().src = "http://127.0.0.1:{off}/beacon.png"; fetch("http://127.0.0.1:{off}/fetch"); try {{ new WebSocket("ws://127.0.0.1:{off}/ws"); }} catch (e) {{ document.title += " ws-blocked"; }} window.open("http://127.0.0.1:{off}/popup"); document.title += " " + typeof WebSocket;</script>
            </body></html>"#), vec![]),
        "/redirect" => (302, String::new(), vec![("Location", format!("http://127.0.0.1:{off}/secret"))]),
        "/meta" => (200, format!(r#"<html><head><meta http-equiv="refresh" content="0;url=http://127.0.0.1:{off}/meta-target"></head><body>redirecting</body></html>"#), vec![]),
        "/plain" => (200, "<html><head><title>Plain</title></head><body><p>Just text.</p></body></html>".into(), vec![]),
        _ => (404, "not found".into(), vec![]),
    }
}

#[tokio::test]
async fn a_real_browser_stays_on_the_allowlist() {
    let Some(bin) = chrome() else {
        eprintln!("no Chrome found; set CHROME_BIN to run this test");
        return;
    };
    let chrome = start_chrome(&bin).await;
    let (off, off_hits, off_lines) = forbidden();
    let (site, _) = serve(pages, off);
    let endpoint = format!("http://127.0.0.1:{}", chrome.port);
    let url = |p: &str| format!("http://127.0.0.1:{site}{p}");
    let cfg = |js: bool| {
        // The test servers are on 127.0.0.1, so private addresses are allowed here; only the host:port list is under test.
        let mut c = BrowseConfig::new(&endpoint, vec![format!("127.0.0.1:{site}")]);
        c.allow_private = true;
        c.javascript = js;
        c.timeout = Duration::from_secs(40);
        c
    };

    // Scripts off.
    let (host, a) = room(Some(Browser::new(cfg(false)).unwrap())).await;
    let r = a.browse(&url("/"), Mode::Text, None).await.unwrap();
    assert!(r.text.as_deref().unwrap().contains("Hello from the allowed site"), "{r:?}");
    assert_eq!(r.title, "Allowed page", "scripts must not run: {r:?}");
    assert_eq!(r.status, Some(200));
    assert!(r.blocked.len() >= 3, "the page's off-list requests are reported: {:?}", r.blocked);
    // A redirect off the list, and a meta refresh off the list.
    let e = a.browse(&url("/redirect"), Mode::Text, None).await;
    assert!(e.is_err(), "a redirect to a forbidden host must not be followed: {e:?}");
    let m = a.browse(&url("/meta"), Mode::Text, Some(1500)).await;
    if let Ok(m) = m {
        assert!(!m.url.contains(&off.to_string()), "{m:?}");
    }
    // HTML and a screenshot.
    let h = a.browse(&url("/plain"), Mode::Html, None).await.unwrap();
    assert!(h.html.unwrap().contains("<p>Just text.</p>"));
    let shot = a.browse(&url("/plain"), Mode::Screenshot, None).await.unwrap();
    let png = shot.screenshot.unwrap();
    assert!(png.starts_with(b"\x89PNG") || png.starts_with(&[0xff, 0xd8]), "an image comes back");
    // A host that is not listed, even one Chrome could reach.
    assert!(a.browse(&format!("http://127.0.0.1:{off}/"), Mode::Text, None).await.is_err());
    a.close().await;
    host.shutdown().await.unwrap();
    tokio::time::sleep(Duration::from_millis(1800)).await;
    let after_off = off_hits.load(Ordering::SeqCst);
    eprintln!("scripts off: connections to the forbidden host = {after_off}, requests = {:?}", off_lines.lock().unwrap());
    assert!(off_lines.lock().unwrap().is_empty(), "no request may reach the forbidden host: {:?}", off_lines.lock().unwrap());

    // Scripts on: the page tries a beacon, fetch, WebSocket, a popup and a form.
    let (host, a) = room(Some(Browser::new(cfg(true)).unwrap())).await;
    let r = a.browse(&url("/"), Mode::Text, Some(1500)).await.unwrap();
    assert!(r.title.starts_with("script ran"), "scripts run when the operator allows them: {r:?}");
    assert!(r.title.contains("undefined"), "WebSocket is removed from the page: {r:?}");
    a.close().await;
    host.shutdown().await.unwrap();
    tokio::time::sleep(Duration::from_millis(1800)).await;
    let total = off_hits.load(Ordering::SeqCst);
    eprintln!("scripts on: connections to the forbidden host = {total}, requests = {:?}", off_lines.lock().unwrap());
    // Preconnect and DNS prefetch open a socket without sending a request, so the interception cannot see them. What matters
    // is that no request, beacon, fetch, WebSocket upgrade, popup or form post arrives.
    assert!(off_lines.lock().unwrap().is_empty(), "no request may reach the forbidden host: {:?}", off_lines.lock().unwrap());
}
