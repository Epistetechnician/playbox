//! `browse`: members read a web page through a browser the host runs, limited to the hosts the host allows.
//!
//! The host drives any browser that speaks the Chrome DevTools Protocol (headless Chrome, Kitesurf, a hosted CDP service) over
//! a `ws://` endpoint on a trusted address. A member names a URL and what it wants back: the page's text, its HTML, or a
//! screenshot. The member never touches the browser.
//!
//! What keeps the browser on the allowlist:
//! * The URL is checked first: `http` or `https` only, no credentials in it, a host (and port) the host's allowlist names.
//! * Every visit gets its own browser context, so no cookies, storage or cache pass between visits or members. The context is
//!   disposed when the connection closes.
//! * Every request the page makes (subresources, redirects, frames, workers) is paused at the browser and checked against the
//!   same allowlist; a request that fails the check is failed with `BlockedByClient`. Frames in other processes and popups
//!   are attached before they run and get the same treatment; if the browser cannot intercept one, it is closed.
//! * A host name is resolved here and refused if any address is private, loopback, link-local (cloud metadata) or otherwise
//!   not public, unless `allow_private` is set. The browser resolves the name again when it connects, so this narrows a DNS
//!   rebinding attack and does not remove it: put the browser where it cannot reach the internal network at all.
//! * Page scripts are off unless `javascript` is set; with them on, `WebSocket`, `WebTransport` and WebRTC are removed from
//!   the page because the interception does not see them.
//! * Not covered: speculative connections. A page can ask the browser to preconnect, and the browser may open a socket to a
//!   host before the request it would send is paused. No request or data goes to the host (tested), but the connection
//!   itself can happen, so a port on an internal host can be probed blindly. Keep the browser on a network that reaches only
//!   what it should.
//! * Downloads are denied; a visit has a time limit, a request limit and size limits on what it returns; the number of
//!   visits at once and per member per minute is bounded.
//!
//! What comes back is text from a web page: untrusted, like anything another party wrote.

use crate::ws::WebSocket;
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// The page's visible text (`document.documentElement.innerText`).
    #[default]
    Text,
    Html,
    Screenshot,
}

/// What a visit returns.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Browsed {
    /// Where the browser ended up (after redirects).
    pub url: String,
    pub title: String,
    pub status: Option<u16>,
    pub text: Option<String>,
    pub html: Option<String>,
    #[serde(skip)]
    pub screenshot: Option<Vec<u8>>,
    /// Requests the allowlist stopped, as `url (reason)`.
    pub blocked: Vec<String>,
    /// How many requests the page made.
    pub requests: usize,
    /// The page had not finished loading when the visit's time was up.
    pub timed_out: bool,
    /// The text or HTML was cut to the size limit.
    pub truncated: bool,
}

#[derive(Debug)]
pub enum BrowseError {
    /// The request was refused (not allowed, too many, busy).
    Refused(String),
    /// The visit failed (the browser did not answer, the page did not load).
    Failed(anyhow::Error),
}

impl std::fmt::Display for BrowseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BrowseError::Refused(m) => f.write_str(m),
            BrowseError::Failed(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for BrowseError {}

// ---- the allowlist ----

#[derive(Debug, Clone)]
struct Pattern {
    /// `*.example.com`: any subdomain of `example.com`, not `example.com` itself.
    suffix: bool,
    host: String,
    port: Option<u16>,
}

/// Which hosts a visit may reach.
#[derive(Debug, Clone)]
pub struct Policy {
    patterns: Vec<Pattern>,
    allow_private: bool,
}

fn bare_host(h: &str) -> String {
    h.trim_start_matches('[').trim_end_matches(']').trim_end_matches('.').to_ascii_lowercase()
}

impl Policy {
    /// Patterns are `host`, `*.domain` or `host:port`. A pattern without a port allows 80 and 443.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty list or a pattern that is not one of those forms (a wildcard over a whole top-level
    /// domain, a scheme, a path, credentials).
    pub fn new(allow: &[String], allow_private: bool) -> Result<Self> {
        if allow.is_empty() {
            bail!("the browser needs at least one allowed host");
        }
        let mut patterns = Vec::new();
        for raw in allow {
            let p = raw.trim().to_ascii_lowercase();
            if p.is_empty() || p.contains(|c: char| c.is_whitespace() || c == '/' || c == '@' || c == '?' || c == '#') || p.contains("://") {
                bail!("bad allowed host {raw:?}: write a host name, *.domain, or host:port");
            }
            let (hostpart, port) = match p.rsplit_once(':') {
                Some((h, port)) if !h.is_empty() && (!h.contains(':') || h.ends_with(']')) => (h.to_string(), Some(port.parse::<u16>().map_err(|_| anyhow!("bad port in {raw:?}"))?)),
                _ => (p.clone(), None),
            };
            let (suffix, host) = match hostpart.strip_prefix("*.") {
                Some(rest) => (true, bare_host(rest)),
                None => (false, bare_host(&hostpart)),
            };
            if host.is_empty() || host.contains('*') || (suffix && !host.contains('.')) {
                bail!("bad allowed host {raw:?}: a wildcard must cover a domain of its own, such as *.example.com");
            }
            patterns.push(Pattern { suffix, host, port });
        }
        Ok(Self { patterns, allow_private })
    }

    /// The first check: scheme, credentials, and the allowlist. No network.
    ///
    /// # Errors
    ///
    /// Returns the reason the URL is not allowed.
    pub fn check(&self, url: &Url) -> std::result::Result<(), String> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(format!("the {} scheme is not allowed", url.scheme()));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("a URL with credentials in it is not allowed".into());
        }
        let host = bare_host(url.host_str().ok_or("the URL has no host")?);
        let port = url.port_or_known_default().ok_or("the URL has no port")?;
        if host.is_empty() || host.starts_with('.') || host.contains("..") {
            return Err("the host name is malformed".into());
        }
        let ok = self.patterns.iter().any(|p| {
            let host_ok = if p.suffix { host.len() > p.host.len() && host.ends_with(&p.host) && host.as_bytes()[host.len() - p.host.len() - 1] == b'.' } else { host == p.host };
            host_ok && p.port.map_or(port == 80 || port == 443, |pp| pp == port)
        });
        if ok {
            Ok(())
        } else {
            Err(format!("{host}:{port} is not on this room's browser allowlist"))
        }
    }

    /// [`Policy::check`], then resolve the host and refuse a private address.
    ///
    /// # Errors
    ///
    /// Returns the reason the URL is not allowed.
    pub async fn check_resolved(&self, url: &Url) -> std::result::Result<(), String> {
        self.check(url)?;
        if self.allow_private {
            return Ok(());
        }
        let port = url.port_or_known_default().unwrap_or(80);
        let host = bare_host(url.host_str().unwrap_or_default());
        let addrs: Vec<IpAddr> = if let Ok(ip) = host.parse::<IpAddr>() {
            vec![ip]
        } else {
            match tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host((host.as_str(), port))).await {
                Ok(Ok(it)) => it.map(|a| a.ip()).collect(),
                _ => return Err(format!("{host} did not resolve")),
            }
        };
        if addrs.is_empty() {
            return Err(format!("{host} did not resolve"));
        }
        match addrs.iter().find(|a| !is_public(a)) {
            Some(a) => Err(format!("{host} resolves to {a}, which is not a public address")),
            None => Ok(()),
        }
    }
}

/// Whether an address is one a page on the internet could reasonably have. Everything else is where internal services,
/// cloud metadata and the host itself live.
#[must_use]
pub fn is_public(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => is_public_v4(*a),
        IpAddr::V6(a) => {
            if let Some(v4) = a.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let s = a.segments();
            !(a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local
                || (s[0] & 0xffc0) == 0xfe80 // link local
                || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
                || (s[0] == 0x64 && s[1] == 0xff9b) // NAT64: can embed a private v4 address
                || *a == Ipv6Addr::LOCALHOST)
        }
    }
}

fn is_public_v4(a: Ipv4Addr) -> bool {
    let o = a.octets();
    !(a.is_private()
        || a.is_loopback()
        || a.is_link_local()
        || a.is_unspecified()
        || a.is_broadcast()
        || a.is_multicast()
        || a.is_documentation()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // carrier-grade NAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // benchmarking
        || o[0] >= 240) // reserved
}

// ---- the browser ----

#[derive(Debug, Clone)]
pub struct BrowseConfig {
    /// `ws://host:port/devtools/browser/<id>`, or `http://host:port` (the host asks `/json/version` for the socket).
    pub endpoint: String,
    pub allow: Vec<String>,
    /// Let pages and the start URL reach private addresses. Default false.
    pub allow_private: bool,
    /// Run the page's scripts. Default false.
    pub javascript: bool,
    /// The whole visit. Default 30 s.
    pub timeout: Duration,
    pub max_text_bytes: usize,
    pub max_screenshot_bytes: usize,
    /// Requests one visit may make. Default 300.
    pub max_requests: usize,
    pub max_concurrent: usize,
    /// Visits one member may start per minute. Default 6.
    pub visits_per_minute: u32,
}

impl BrowseConfig {
    #[must_use]
    pub fn new(endpoint: &str, allow: Vec<String>) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            allow,
            allow_private: false,
            javascript: false,
            timeout: Duration::from_secs(30),
            max_text_bytes: 256 * 1024,
            max_screenshot_bytes: 2 << 20,
            max_requests: 300,
            max_concurrent: 2,
            visits_per_minute: 6,
        }
    }
}

pub struct Browser {
    cfg: BrowseConfig,
    policy: Policy,
    slots: tokio::sync::Semaphore,
    visits: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl Browser {
    /// # Errors
    ///
    /// Returns an error if the allowlist or the endpoint is not acceptable.
    pub fn new(cfg: BrowseConfig) -> Result<Self> {
        let policy = Policy::new(&cfg.allow, cfg.allow_private)?;
        if !(cfg.endpoint.starts_with("ws://") || cfg.endpoint.starts_with("http://")) {
            bail!("the browser endpoint must be ws://host:port/... or http://host:port");
        }
        let slots = tokio::sync::Semaphore::new(cfg.max_concurrent.max(1));
        Ok(Self { cfg, policy, slots, visits: Mutex::default() })
    }

    #[must_use]
    pub fn allowlist(&self) -> &[String] {
        &self.cfg.allow
    }

    fn rate_ok(&self, who: &str) -> bool {
        let mut g = self.visits.lock().expect("visit lock");
        let q = g.entry(who.to_string()).or_default();
        let now = Instant::now();
        while q.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60)) {
            q.pop_front();
        }
        if q.len() >= self.cfg.visits_per_minute as usize {
            return false;
        }
        q.push_back(now);
        true
    }

    /// Visit `url` for `who` (a member's id).
    ///
    /// # Errors
    ///
    /// [`BrowseError::Refused`] if the URL is not allowed or the member or the browser is busy; [`BrowseError::Failed`]
    /// if the browser could not do it.
    pub async fn browse(&self, who: &str, url: &str, mode: Mode, wait_ms: Option<u64>) -> std::result::Result<Browsed, BrowseError> {
        let parsed = Url::parse(url).map_err(|e| BrowseError::Refused(format!("not a URL: {e}")))?;
        self.policy.check_resolved(&parsed).await.map_err(BrowseError::Refused)?;
        if !self.rate_ok(who) {
            return Err(BrowseError::Refused("too many page visits this minute".into()));
        }
        let Ok(_slot) = self.slots.try_acquire() else {
            return Err(BrowseError::Refused("the browser is busy; try again shortly".into()));
        };
        let wait = Duration::from_millis(wait_ms.unwrap_or(0).min(5000));
        match tokio::time::timeout(self.cfg.timeout, self.visit(&parsed, mode, wait)).await {
            Ok(r) => r.map_err(BrowseError::Failed),
            Err(_) => Err(BrowseError::Failed(anyhow!("the visit took longer than {} s", self.cfg.timeout.as_secs()))),
        }
    }

    async fn socket_url(&self) -> Result<String> {
        let e = &self.cfg.endpoint;
        if e.starts_with("ws://") {
            return Ok(e.clone());
        }
        let rest = e.strip_prefix("http://").unwrap_or(e);
        let hostport = rest.split('/').next().unwrap_or(rest);
        let (host, port) = hostport.rsplit_once(':').map_or((hostport, 80u16), |(h, p)| (h, p.parse().unwrap_or(80)));
        let mut s = tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect((host.trim_matches(|c| c == '[' || c == ']'), port))).await.map_err(|_| anyhow!("the browser did not answer"))?.context("connect to the browser")?;
        s.write_all(format!("GET /json/version HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nAccept: application/json\r\n\r\n").as_bytes()).await?;
        // The browser may keep the connection open after answering, so read what Content-Length says and stop.
        let mut buf = Vec::new();
        loop {
            let mut chunk = [0u8; 4096];
            let n = s.read(&mut chunk).await?;
            buf.extend_from_slice(&chunk[..n]);
            if n == 0 || buf.len() > 1 << 20 {
                break;
            }
            if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..at]).to_ascii_lowercase();
                let len = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok());
                if len.is_some_and(|l| buf.len() >= at + 4 + l) {
                    break;
                }
            }
        }
        let (code, body) = crate::shared_memory::parse_response(&buf)?;
        if code != 200 {
            bail!("the browser's /json/version answered {code}");
        }
        let v: Value = serde_json::from_slice(&body).context("/json/version")?;
        let ws = v.get("webSocketDebuggerUrl").and_then(Value::as_str).ok_or_else(|| anyhow!("the browser gave no webSocketDebuggerUrl"))?;
        Ok(ws.to_string())
    }

    async fn visit(&self, url: &Url, mode: Mode, wait: Duration) -> Result<Browsed> {
        let ws = WebSocket::connect(&self.socket_url().await?).await?;
        let mut v = Visit::new(ws, &self.policy, &self.cfg, Instant::now() + self.cfg.timeout);
        let r = v.run(url, mode, wait).await;
        // Closing the connection disposes the browser context.
        v.ws.close().await;
        r
    }
}

#[derive(Debug)]
enum Pending {
    /// A command whose answer a caller is waiting for.
    Caller,
    /// `Fetch.enable` on a child target: resume it when it succeeds, close it when it does not.
    FetchEnable { session: String, target: String },
    /// A command whose answer does not matter.
    Ignore,
}

struct Visit<'a> {
    ws: WebSocket,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    answers: HashMap<u64, std::result::Result<Value, String>>,
    policy: &'a Policy,
    cfg: &'a BrowseConfig,
    deadline: Instant,
    main: Option<String>,
    main_target: Option<String>,
    loaded: bool,
    crashed: bool,
    status: Option<u16>,
    blocked: Vec<String>,
    requests: usize,
    verdicts: HashMap<String, std::result::Result<(), String>>,
}

/// Source run in every document before its own scripts when page scripts are on.
const HARDEN: &str = "for (const k of ['WebSocket','WebTransport','RTCPeerConnection','webkitRTCPeerConnection']) { try { Object.defineProperty(window, k, {value: undefined, configurable: false, writable: false}); } catch (e) {} }";

impl<'a> Visit<'a> {
    fn new(ws: WebSocket, policy: &'a Policy, cfg: &'a BrowseConfig, deadline: Instant) -> Self {
        Self { ws, next_id: 0, pending: HashMap::new(), answers: HashMap::new(), policy, cfg, deadline, main: None, main_target: None, loaded: false, crashed: false, status: None, blocked: Vec::new(), requests: 0, verdicts: HashMap::new() }
    }

    async fn send(&mut self, method: &str, params: Value, session: Option<&str>, kind: Pending) -> Result<u64> {
        self.next_id += 1;
        let id = self.next_id;
        let mut m = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            m["sessionId"] = json!(s);
        }
        self.pending.insert(id, kind);
        self.ws.send(&m.to_string()).await?;
        Ok(id)
    }

    async fn call(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        let id = self.send(method, params, session, Pending::Caller).await?;
        loop {
            if let Some(r) = self.answers.remove(&id) {
                return r.map_err(|e| anyhow!("{method}: {e}"));
            }
            self.pump().await?;
        }
    }

    /// Read one message from the browser and act on it.
    async fn pump(&mut self) -> Result<()> {
        if self.crashed {
            bail!("the page crashed");
        }
        let msg = match tokio::time::timeout_at(tokio::time::Instant::from_std(self.deadline), self.ws.recv()).await {
            Err(_) => bail!("the visit ran out of time"),
            Ok(r) => r?.ok_or_else(|| anyhow!("the browser closed the connection"))?,
        };
        let v: Value = serde_json::from_str(&msg).context("a message from the browser")?;
        if let Some(id) = v.get("id").and_then(Value::as_u64) {
            let result = match v.get("error") {
                Some(e) => Err(e.get("message").and_then(Value::as_str).unwrap_or("error").to_string()),
                None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
            };
            match self.pending.remove(&id) {
                Some(Pending::Caller) => {
                    self.answers.insert(id, result);
                }
                Some(Pending::FetchEnable { session, target }) => {
                    if result.is_ok() {
                        self.send("Runtime.runIfWaitingForDebugger", json!({}), Some(&session), Pending::Ignore).await?;
                    } else {
                        // The browser cannot intercept this target, so it does not get to run.
                        self.blocked.push(format!("a target the browser could not intercept (closed): {}", clip(&target, 16)));
                        self.send("Target.closeTarget", json!({"targetId": target}), None, Pending::Ignore).await?;
                    }
                }
                Some(Pending::Ignore) | None => {}
            }
            return Ok(());
        }
        let method = v.get("method").and_then(Value::as_str).unwrap_or("");
        let session = v.get("sessionId").and_then(Value::as_str).map(str::to_string);
        let params = v.get("params").cloned().unwrap_or(Value::Null);
        self.on_event(method, &params, session.as_deref()).await
    }

    async fn vet(&mut self, raw: &str) -> std::result::Result<(), String> {
        let lower = raw.to_ascii_lowercase();
        if lower.starts_with("data:") || lower.starts_with("blob:") || lower == "about:blank" {
            return Ok(());
        }
        let url = Url::parse(raw).map_err(|_| "not a URL".to_string())?;
        let key = format!("{}://{}:{}", url.scheme(), url.host_str().unwrap_or(""), url.port_or_known_default().unwrap_or(0));
        if let Some(v) = self.verdicts.get(&key) {
            return v.clone();
        }
        let v = self.policy.check_resolved(&url).await;
        self.verdicts.insert(key, v.clone());
        v
    }

    async fn on_event(&mut self, method: &str, p: &Value, session: Option<&str>) -> Result<()> {
        match method {
            "Fetch.requestPaused" => {
                let (Some(rid), Some(url)) = (p.get("requestId").and_then(Value::as_str), p.pointer("/request/url").and_then(Value::as_str)) else { return Ok(()) };
                let (rid, url) = (rid.to_string(), url.to_string());
                if p.get("responseStatusCode").is_some() || p.get("responseErrorReason").is_some() {
                    return self.send("Fetch.continueRequest", json!({"requestId": rid}), session, Pending::Ignore).await.map(drop);
                }
                self.requests += 1;
                let verdict = if self.requests > self.cfg.max_requests { Err("too many requests for one visit".to_string()) } else { self.vet(&url).await };
                match verdict {
                    Ok(()) => self.send("Fetch.continueRequest", json!({"requestId": rid}), session, Pending::Ignore).await?,
                    Err(reason) => {
                        if self.blocked.len() < 50 {
                            self.blocked.push(format!("{} ({reason})", clip_url(&url)));
                        }
                        self.send("Fetch.failRequest", json!({"requestId": rid, "errorReason": "BlockedByClient"}), session, Pending::Ignore).await?
                    }
                };
            }
            "Target.attachedToTarget" => {
                let (Some(child), Some(target)) = (p.get("sessionId").and_then(Value::as_str), p.pointer("/targetInfo/targetId").and_then(Value::as_str)) else { return Ok(()) };
                let (child, target) = (child.to_string(), target.to_string());
                // The page this visit created is set up by `run`, in order.
                if self.main_target.as_deref() == Some(target.as_str()) {
                    return Ok(());
                }
                let kind = p.pointer("/targetInfo/type").and_then(Value::as_str).unwrap_or("").to_string();
                self.send("Target.setAutoAttach", json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}), Some(&child), Pending::Ignore).await?;
                if !self.cfg.javascript && matches!(kind.as_str(), "page" | "iframe") {
                    self.send("Emulation.setScriptExecutionDisabled", json!({"value": true}), Some(&child), Pending::Ignore).await?;
                }
                if self.cfg.javascript && matches!(kind.as_str(), "page" | "iframe") {
                    self.send("Page.addScriptToEvaluateOnNewDocument", json!({"source": HARDEN}), Some(&child), Pending::Ignore).await?;
                }
                self.send("Fetch.enable", json!({"patterns": [{"urlPattern": "*"}]}), Some(&child), Pending::FetchEnable { session: child.clone(), target }).await?;
            }
            "Page.loadEventFired" if session == self.main.as_deref() => self.loaded = true,
            "Network.responseReceived" if session == self.main.as_deref() && p.get("type").and_then(Value::as_str) == Some("Document") => {
                if let Some(s) = p.pointer("/response/status").and_then(Value::as_u64) {
                    self.status = u16::try_from(s).ok();
                }
            }
            "Page.javascriptDialogOpening" => {
                self.send("Page.handleJavaScriptDialog", json!({"accept": false}), session, Pending::Ignore).await?;
            }
            "Inspector.targetCrashed" | "Target.targetCrashed" => self.crashed = true,
            _ => {}
        }
        Ok(())
    }

    async fn pump_for(&mut self, d: Duration) -> Result<()> {
        let until = Instant::now() + d;
        while Instant::now() < until {
            let left = until - Instant::now();
            match tokio::time::timeout(left, self.pump()).await {
                Ok(r) => r?,
                Err(_) => break,
            }
        }
        Ok(())
    }

    async fn eval(&mut self, session: &str, expr: &str) -> Result<String> {
        let r = self.call("Runtime.evaluate", json!({"expression": expr, "returnByValue": true}), Some(session)).await?;
        if r.get("exceptionDetails").is_some() {
            bail!("the page could not be read");
        }
        Ok(r.pointer("/result/value").and_then(Value::as_str).unwrap_or_default().to_string())
    }

    async fn run(&mut self, url: &Url, mode: Mode, wait: Duration) -> Result<Browsed> {
        let ctx = self.call("Target.createBrowserContext", json!({"disposeOnDetach": true}), None).await?;
        let ctx = ctx.get("browserContextId").and_then(Value::as_str).ok_or_else(|| anyhow!("no browser context"))?.to_string();
        // Best effort: a browser that does not know this command still has downloads off in headless mode by default.
        let _ = self.call("Browser.setDownloadBehavior", json!({"behavior": "deny", "browserContextId": ctx}), None).await;
        let t = self.call("Target.createTarget", json!({"url": "about:blank", "browserContextId": ctx}), None).await?;
        let target = t.get("targetId").and_then(Value::as_str).ok_or_else(|| anyhow!("no target"))?.to_string();
        self.main_target = Some(target.clone());
        let a = self.call("Target.attachToTarget", json!({"targetId": target, "flatten": true}), None).await?;
        let s = a.get("sessionId").and_then(Value::as_str).ok_or_else(|| anyhow!("no session"))?.to_string();
        self.main = Some(s.clone());

        self.call("Page.enable", json!({}), Some(&s)).await?;
        self.call("Network.enable", json!({}), Some(&s)).await?;
        self.call("Network.setBypassServiceWorker", json!({"bypass": true}), Some(&s)).await.ok();
        self.call("Emulation.setDeviceMetricsOverride", json!({"width": 1024, "height": 768, "deviceScaleFactor": 1, "mobile": false}), Some(&s)).await?;
        if self.cfg.javascript {
            self.call("Page.addScriptToEvaluateOnNewDocument", json!({"source": HARDEN}), Some(&s)).await?;
        } else {
            self.call("Emulation.setScriptExecutionDisabled", json!({"value": true}), Some(&s)).await?;
        }
        self.call("Target.setAutoAttach", json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}), Some(&s)).await?;
        self.call("Fetch.enable", json!({"patterns": [{"urlPattern": "*"}]}), Some(&s)).await?;

        let nav = self.call("Page.navigate", json!({"url": url.as_str()}), Some(&s)).await?;
        if let Some(e) = nav.get("errorText").and_then(Value::as_str) {
            // Let the blocked list explain a failure the allowlist caused.
            self.pump_for(Duration::from_millis(100)).await.ok();
            bail!("the page did not load: {e}{}", if self.blocked.is_empty() { String::new() } else { format!(" (stopped: {})", self.blocked.join("; ")) });
        }
        let stop = (Instant::now() + Duration::from_secs(20)).min(self.deadline.checked_sub(Duration::from_secs(3)).unwrap_or_else(Instant::now));
        let mut timed_out = false;
        while !self.loaded {
            if Instant::now() >= stop {
                timed_out = true;
                break;
            }
            // A quiet half second is not an error; the browser may simply be loading.
            if let Ok(r) = tokio::time::timeout(Duration::from_millis(500), self.pump()).await {
                r?;
            }
        }
        if !wait.is_zero() {
            self.pump_for(wait).await?;
        }

        let info = self.eval(&s, "JSON.stringify({u: location.href, t: document.title})").await?;
        let info: Value = serde_json::from_str(&info).unwrap_or(Value::Null);
        let mut out = Browsed {
            url: info.get("u").and_then(Value::as_str).unwrap_or(url.as_str()).to_string(),
            title: clip(info.get("t").and_then(Value::as_str).unwrap_or(""), 300),
            status: self.status,
            blocked: self.blocked.clone(),
            requests: self.requests,
            timed_out,
            ..Browsed::default()
        };
        match mode {
            Mode::Text | Mode::Html => {
                let expr = if mode == Mode::Text { "document.documentElement ? document.documentElement.innerText : ''" } else { "document.documentElement ? document.documentElement.outerHTML : ''" };
                let mut text = self.eval(&s, expr).await?;
                if text.len() > self.cfg.max_text_bytes {
                    let mut cut = self.cfg.max_text_bytes;
                    while !text.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    text.truncate(cut);
                    out.truncated = true;
                }
                if mode == Mode::Text {
                    out.text = Some(text);
                } else {
                    out.html = Some(text);
                }
            }
            Mode::Screenshot => {
                let mut shot = self.call("Page.captureScreenshot", json!({"format": "png"}), Some(&s)).await?;
                let mut bytes = base64::engine::general_purpose::STANDARD.decode(shot.get("data").and_then(Value::as_str).unwrap_or("")).context("screenshot")?;
                if bytes.len() > self.cfg.max_screenshot_bytes {
                    shot = self.call("Page.captureScreenshot", json!({"format": "jpeg", "quality": 55}), Some(&s)).await?;
                    bytes = base64::engine::general_purpose::STANDARD.decode(shot.get("data").and_then(Value::as_str).unwrap_or("")).context("screenshot")?;
                    if bytes.len() > self.cfg.max_screenshot_bytes {
                        bail!("the screenshot is larger than the {} KiB limit", self.cfg.max_screenshot_bytes >> 10);
                    }
                }
                out.screenshot = Some(bytes);
            }
        }
        // What the page ended on must be allowed too: a navigation the interception somehow missed is not returned.
        if let Ok(end) = Url::parse(&out.url) {
            if end.scheme() != "about" && self.policy.check(&end).is_err() {
                bail!("the page ended on a host that is not allowed");
            }
        }
        out.blocked = self.blocked.clone();
        out.requests = self.requests;
        Ok(out)
    }
}

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// A URL for logs and messages: no query or fragment (they can carry tokens), and not too long.
#[must_use]
pub fn clip_url(raw: &str) -> String {
    match Url::parse(raw) {
        Ok(mut u) => {
            u.set_query(None);
            u.set_fragment(None);
            let _ = u.set_password(None);
            let _ = u.set_username("");
            clip(u.as_str(), 200)
        }
        Err(_) => clip(raw, 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(!ok(&p, "https://example.com:8443/"), "a pattern without a port allows 80 and 443 only");
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
        for bad in ["file:///etc/passwd", "ftp://example.com/", "javascript:alert(1)", "data:text/html,hi", "chrome://gpu", "view-source:https://example.com/"] {
            assert!(!Url::parse(bad).map(|u| p.check(&u).is_ok()).unwrap_or(false), "{bad}");
        }
        // Unicode and encoded hosts are compared as the browser will see them.
        assert!(!ok(&p, "https://exаmple.com/"), "a Cyrillic a is another host");
        assert!(!ok(&p, "https://example.com%2eevil.com/"));
        assert!(!ok(&p, "https://%2edocs.rs/"), "an empty label is not a subdomain");
        assert!(!ok(&p, "https://a..docs.rs/"));
    }

    /// Whatever string a page or a member offers, a URL the check accepts is http or https, has no credentials, and is on a
    /// listed host and port as the browser will read it. Seeded, so a failure reproduces.
    #[test]
    fn nothing_odd_passes_the_check() {
        let p = pol(&["example.com", "*.docs.rs"]);
        let parts = ["http", "https", "ftp", "file", "javascript", "://", ":", "//", "/", "\\", "@", "#", "?", ".", "..", "%2e", "%2f", "%40", "%00", "example.com", "EXAMPLE.COM", "docs.rs", "a.docs.rs", "evil.com", "127.0.0.1", "[::1]", "0x7f.1", "2130706433", "443", "80", "8080", " ", "\t", "\n", "\u{202e}", "\u{0430}", "xn--", "user", "pw"];
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
            let s: String = (0..n).map(|_| parts[(next() % parts.len() as u64) as usize]).collect();
            let Ok(u) = Url::parse(&s) else { continue };
            if p.check(&u).is_ok() {
                accepted += 1;
                let host = bare_host(u.host_str().unwrap());
                assert!(matches!(u.scheme(), "http" | "https"), "{s:?}");
                assert!(u.username().is_empty() && u.password().is_none(), "{s:?}");
                assert!(host == "example.com" || (host.ends_with(".docs.rs") && host.len() > ".docs.rs".len()), "{s:?} -> {host}");
                assert!(matches!(u.port_or_known_default(), Some(80 | 443)), "{s:?}");
            }
        }
        assert!(accepted > 0, "the generator must produce some allowed URLs, or this test proves nothing");
    }

    #[test]
    fn bad_patterns_are_refused() {
        for bad in ["", "*", "*.com", "https://example.com", "example.com/path", "user@example.com", "ex ample.com", "example.com:notaport", "a*b.com", "*.*.com"] {
            assert!(Policy::new(&[bad.to_string()], false).is_err(), "{bad:?}");
        }
        assert!(Policy::new(&[], false).is_err());
        assert!(Policy::new(&["127.0.0.1:8080".into(), "[::1]:9".into(), "localhost".into()], true).is_ok());
    }

    #[test]
    fn private_and_special_addresses_are_not_public() {
        for ip in ["127.0.0.1", "10.1.2.3", "172.16.0.1", "172.31.255.255", "192.168.0.1", "169.254.169.254", "100.64.0.1", "0.0.0.0", "255.255.255.255", "224.0.0.1", "192.0.2.1", "198.18.0.1", "240.0.0.1", "::1", "::", "fe80::1", "fc00::1", "fd12::1", "::ffff:127.0.0.1", "::ffff:10.0.0.1", "64:ff9b::a00:1", "2001:db8::1", "ff02::1"] {
            assert!(!is_public(&ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "93.184.216.34", "172.32.0.1", "100.63.255.255", "2606:4700:4700::1111", "::ffff:8.8.8.8"] {
            assert!(is_public(&ip.parse().unwrap()), "{ip}");
        }
    }

    #[tokio::test]
    async fn a_name_that_resolves_to_a_private_address_is_refused() {
        let p = pol(&["localhost", "127.0.0.1", "8.8.8.8"]);
        assert!(p.check_resolved(&Url::parse("http://localhost/").unwrap()).await.is_err());
        assert!(p.check_resolved(&Url::parse("http://127.0.0.1/").unwrap()).await.is_err());
        assert!(p.check_resolved(&Url::parse("http://8.8.8.8/").unwrap()).await.is_ok());
        let open = Policy::new(&["127.0.0.1".into()], true).unwrap();
        assert!(open.check_resolved(&Url::parse("http://127.0.0.1/").unwrap()).await.is_ok());
    }

    #[test]
    fn urls_in_messages_carry_no_query() {
        assert_eq!(clip_url("https://a.example/p?token=secret#frag"), "https://a.example/p");
        assert_eq!(clip_url("https://u:p@a.example/p"), "https://a.example/p");
    }

    /// A scripted browser: answers the commands a visit sends, pauses two requests (one allowed, one not), and records
    /// what the host did with each.
    async fn fake_browser(l: tokio::net::TcpListener, page_text: &'static str) -> tokio::task::JoinHandle<Vec<String>> {
        tokio::spawn(async move {
            let mut p = server::accept(&l).await;
            let mut log = Vec::new();
            while let Some(m) = p.recv().await {
                let v: Value = serde_json::from_str(&m).unwrap();
                let id = v["id"].as_u64().unwrap();
                let method = v["method"].as_str().unwrap().to_string();
                let sess = v.get("sessionId").cloned();
                log.push(format!("{method} {}", v["params"].get("requestId").and_then(Value::as_str).unwrap_or("")).trim().to_string());
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
                    "Page.captureScreenshot" => json!({"data": base64::engine::general_purpose::STANDARD.encode(b"PNGDATA")}),
                    _ => json!({}),
                };
                let mut reply = json!({"id": id, "result": result});
                if let Some(s) = sess {
                    reply["sessionId"] = s;
                }
                p.send(&reply.to_string()).await;
                if method == "Page.navigate" {
                    for (rid, url) in [("r1", "https://example.com/style.css"), ("r2", "http://169.254.169.254/latest/meta-data"), ("r3", "https://evil.example/x.js")] {
                        p.send(&json!({"method": "Fetch.requestPaused", "sessionId": "S1", "params": {"requestId": rid, "request": {"url": url, "method": "GET"}}}).to_string()).await;
                    }
                    p.send(&json!({"method": "Network.responseReceived", "sessionId": "S1", "params": {"type": "Document", "response": {"status": 200}}}).to_string()).await;
                    p.send(&json!({"method": "Page.loadEventFired", "sessionId": "S1", "params": {}}).to_string()).await;
                }
            }
            log
        })
    }

    fn browser_for(port: u16, allow: &[&str]) -> Browser {
        let mut c = BrowseConfig::new(&format!("ws://127.0.0.1:{port}/devtools/browser/x"), allow.iter().map(|s| s.to_string()).collect());
        c.timeout = Duration::from_secs(10);
        Browser::new(c).unwrap()
    }

    #[tokio::test]
    async fn a_visit_checks_every_request_the_page_makes() {
        // `example.com` is a public name; the test must not depend on the network, so allow-private skips only the DNS step.
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let srv = fake_browser(l, "Hello from the page").await;
        let mut c = BrowseConfig::new(&format!("ws://127.0.0.1:{port}/"), vec!["example.com".into()]);
        c.allow_private = true;
        c.timeout = Duration::from_secs(10);
        let b = Browser::new(c).unwrap();
        let r = b.browse("alice", "https://example.com/start?secret=1", Mode::Text, None).await.unwrap();
        assert_eq!(r.text.as_deref(), Some("Hello from the page"));
        assert_eq!(r.title, "A title");
        assert_eq!(r.status, Some(200));
        assert_eq!(r.requests, 3);
        assert_eq!(r.blocked.len(), 2, "{:?}", r.blocked);
        assert!(r.blocked.iter().any(|x| x.contains("169.254.169.254")) || r.blocked.iter().any(|x| x.contains("not on this room's browser allowlist")), "{:?}", r.blocked);
        assert!(r.blocked.iter().any(|x| x.contains("evil.example")));
        let log = srv.await.unwrap();
        assert!(log.contains(&"Fetch.continueRequest r1".to_string()), "{log:?}");
        assert!(log.contains(&"Fetch.failRequest r2".to_string()), "{log:?}");
        assert!(log.contains(&"Fetch.failRequest r3".to_string()), "{log:?}");
        let at = |s: &str| log.iter().position(|x| x.starts_with(s)).unwrap_or_else(|| panic!("{s} not in {log:?}"));
        assert!(at("Fetch.enable") < at("Page.navigate"), "interception must be on before the page is asked to load");
        assert!(at("Emulation.setScriptExecutionDisabled") < at("Page.navigate"));
    }

    #[tokio::test]
    async fn the_start_url_is_refused_before_any_browser_is_contacted() {
        // No browser is listening at this port: a refusal must come first.
        let b = browser_for(9, &["example.com"]);
        for u in ["https://evil.example/", "file:///etc/passwd", "http://169.254.169.254/", "https://user:pw@example.com/", "not a url", "https://example.com:8443/"] {
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
        let mut c = BrowseConfig::new(&format!("ws://127.0.0.1:{port}/"), vec!["example.com".into()]);
        c.allow_private = true;
        c.max_text_bytes = 9;
        let b = Browser::new(c).unwrap();
        let r = b.browse("alice", "https://example.com/", Mode::Text, None).await.unwrap();
        assert!(r.truncated && r.text.as_ref().unwrap().len() <= 9 && r.text.as_ref().unwrap().is_char_boundary(r.text.as_ref().unwrap().len()));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let _srv = fake_browser(l, "x").await;
        let mut c = BrowseConfig::new(&format!("ws://127.0.0.1:{port}/"), vec!["example.com".into()]);
        c.allow_private = true;
        let b = Browser::new(c).unwrap();
        let r = b.browse("alice", "https://example.com/", Mode::Screenshot, None).await.unwrap();
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
            assert!(matches!(b.browse("alice", "https://example.com/", Mode::Text, None).await, Err(BrowseError::Failed(_))));
        }
        assert!(matches!(b.browse("alice", "https://example.com/", Mode::Text, None).await, Err(BrowseError::Refused(_))));
        assert!(matches!(b.browse("bob", "https://example.com/", Mode::Text, None).await, Err(BrowseError::Failed(_))), "another member has their own budget");
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
        let mut c = BrowseConfig::new(&format!("ws://127.0.0.1:{port}/"), vec!["example.com".into()]);
        c.allow_private = true;
        c.timeout = Duration::from_millis(800);
        let b = Browser::new(c).unwrap();
        let t0 = Instant::now();
        assert!(matches!(b.browse("alice", "https://example.com/", Mode::Text, None).await, Err(BrowseError::Failed(_))));
        assert!(t0.elapsed() < Duration::from_secs(5));
    }

}
