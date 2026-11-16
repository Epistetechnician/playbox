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

use crate::sync::LockExt;
use crate::ws::WebSocket;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

mod policy;
#[cfg(test)]
mod tests;
mod visit;
pub use policy::{is_public, Policy};
use visit::Visit;

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
        Ok(Self {
            cfg,
            policy,
            slots,
            visits: Mutex::default(),
        })
    }

    #[must_use]
    pub fn allowlist(&self) -> &[String] {
        &self.cfg.allow
    }

    fn rate_ok(&self, who: &str) -> bool {
        let mut g = self.visits.lock_recover();
        let q = g.entry(who.to_string()).or_default();
        let now = Instant::now();
        while q
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
        {
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
    pub async fn browse(
        &self,
        who: &str,
        url: &str,
        mode: Mode,
        wait_ms: Option<u64>,
    ) -> std::result::Result<Browsed, BrowseError> {
        let parsed =
            Url::parse(url).map_err(|e| BrowseError::Refused(format!("not a URL: {e}")))?;
        self.policy
            .check_resolved(&parsed)
            .await
            .map_err(BrowseError::Refused)?;
        if !self.rate_ok(who) {
            return Err(BrowseError::Refused(
                "too many page visits this minute".into(),
            ));
        }
        let Ok(_slot) = self.slots.try_acquire() else {
            return Err(BrowseError::Refused(
                "the browser is busy; try again shortly".into(),
            ));
        };
        let wait = Duration::from_millis(wait_ms.unwrap_or(0).min(5000));
        match tokio::time::timeout(self.cfg.timeout, self.visit(&parsed, mode, wait)).await {
            Ok(r) => r.map_err(BrowseError::Failed),
            Err(_) => Err(BrowseError::Failed(anyhow!(
                "the visit took longer than {} s",
                self.cfg.timeout.as_secs()
            ))),
        }
    }

    async fn socket_url(&self) -> Result<String> {
        let e = &self.cfg.endpoint;
        if e.starts_with("ws://") {
            return Ok(e.clone());
        }
        let rest = e.strip_prefix("http://").unwrap_or(e);
        let hostport = rest.split('/').next().unwrap_or(rest);
        let (host, port) = hostport
            .rsplit_once(':')
            .map_or((hostport, 80u16), |(h, p)| (h, p.parse().unwrap_or(80)));
        let mut s = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::TcpStream::connect((host.trim_matches(|c| c == '[' || c == ']'), port)),
        )
        .await
        .map_err(|_| anyhow!("the browser did not answer"))?
        .context("connect to the browser")?;
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
                let len = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok());
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
        let ws = v
            .get("webSocketDebuggerUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("the browser gave no webSocketDebuggerUrl"))?;
        Ok(ws.to_string())
    }

    async fn visit(&self, url: &Url, mode: Mode, wait: Duration) -> Result<Browsed> {
        let ws = WebSocket::connect(&self.socket_url().await?).await?;
        let mut v = Visit::new(
            ws,
            &self.policy,
            &self.cfg,
            Instant::now() + self.cfg.timeout,
        );
        let r = v.run(url, mode, wait).await;
        // Closing the connection disposes the browser context.
        v.ws.close().await;
        r
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
