//! One page visit over the Chrome DevTools protocol, vetting every request the page makes.

use super::policy::Policy;
use super::{clip, clip_url, BrowseConfig, Browsed, Mode};
use crate::ws::WebSocket;
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use url::Url;

#[derive(Debug)]
pub(super) enum Pending {
    /// A command whose answer a caller is waiting for.
    Caller,
    /// `Fetch.enable` on a child target: resume it when it succeeds, close it when it does not.
    FetchEnable { session: String, target: String },
    /// A command whose answer does not matter.
    Ignore,
}

pub(super) struct Visit<'a> {
    pub(super) ws: WebSocket,
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
pub(super) const HARDEN: &str = "for (const k of ['WebSocket','WebTransport','RTCPeerConnection','webkitRTCPeerConnection']) { try { Object.defineProperty(window, k, {value: undefined, configurable: false, writable: false}); } catch (e) {} }";

impl<'a> Visit<'a> {
    pub(super) fn new(
        ws: WebSocket,
        policy: &'a Policy,
        cfg: &'a BrowseConfig,
        deadline: Instant,
    ) -> Self {
        Self {
            ws,
            next_id: 0,
            pending: HashMap::new(),
            answers: HashMap::new(),
            policy,
            cfg,
            deadline,
            main: None,
            main_target: None,
            loaded: false,
            crashed: false,
            status: None,
            blocked: Vec::new(),
            requests: 0,
            verdicts: HashMap::new(),
        }
    }

    async fn send(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
        kind: Pending,
    ) -> Result<u64> {
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
        let msg = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(self.deadline),
            self.ws.recv(),
        )
        .await
        {
            Err(_) => bail!("the visit ran out of time"),
            Ok(r) => r?.ok_or_else(|| anyhow!("the browser closed the connection"))?,
        };
        let v: Value = serde_json::from_str(&msg).context("a message from the browser")?;
        if let Some(id) = v.get("id").and_then(Value::as_u64) {
            let result = match v.get("error") {
                Some(e) => Err(e
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("error")
                    .to_string()),
                None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
            };
            match self.pending.remove(&id) {
                Some(Pending::Caller) => {
                    self.answers.insert(id, result);
                }
                Some(Pending::FetchEnable { session, target }) => {
                    if result.is_ok() {
                        self.send(
                            "Runtime.runIfWaitingForDebugger",
                            json!({}),
                            Some(&session),
                            Pending::Ignore,
                        )
                        .await?;
                    } else {
                        // The browser cannot intercept this target, so it does not get to run.
                        self.blocked.push(format!(
                            "a target the browser could not intercept (closed): {}",
                            clip(&target, 16)
                        ));
                        self.send(
                            "Target.closeTarget",
                            json!({"targetId": target}),
                            None,
                            Pending::Ignore,
                        )
                        .await?;
                    }
                }
                Some(Pending::Ignore) | None => {}
            }
            return Ok(());
        }
        let method = v.get("method").and_then(Value::as_str).unwrap_or("");
        let session = v
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let params = v.get("params").cloned().unwrap_or(Value::Null);
        self.on_event(method, &params, session.as_deref()).await
    }

    async fn vet(&mut self, raw: &str) -> std::result::Result<(), String> {
        let lower = raw.to_ascii_lowercase();
        if lower.starts_with("data:") || lower.starts_with("blob:") || lower == "about:blank" {
            return Ok(());
        }
        let url = Url::parse(raw).map_err(|_| "not a URL".to_string())?;
        let key = format!(
            "{}://{}:{}",
            url.scheme(),
            url.host_str().unwrap_or(""),
            url.port_or_known_default().unwrap_or(0)
        );
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
                let (Some(rid), Some(url)) = (
                    p.get("requestId").and_then(Value::as_str),
                    p.pointer("/request/url").and_then(Value::as_str),
                ) else {
                    return Ok(());
                };
                let (rid, url) = (rid.to_string(), url.to_string());
                if p.get("responseStatusCode").is_some() || p.get("responseErrorReason").is_some() {
                    return self
                        .send(
                            "Fetch.continueRequest",
                            json!({"requestId": rid}),
                            session,
                            Pending::Ignore,
                        )
                        .await
                        .map(drop);
                }
                self.requests += 1;
                let verdict = if self.requests > self.cfg.max_requests {
                    Err("too many requests for one visit".to_string())
                } else {
                    self.vet(&url).await
                };
                match verdict {
                    Ok(()) => {
                        self.send(
                            "Fetch.continueRequest",
                            json!({"requestId": rid}),
                            session,
                            Pending::Ignore,
                        )
                        .await?
                    }
                    Err(reason) => {
                        if self.blocked.len() < 50 {
                            self.blocked.push(format!("{} ({reason})", clip_url(&url)));
                        }
                        self.send(
                            "Fetch.failRequest",
                            json!({"requestId": rid, "errorReason": "BlockedByClient"}),
                            session,
                            Pending::Ignore,
                        )
                        .await?
                    }
                };
            }
            "Target.attachedToTarget" => {
                let (Some(child), Some(target)) = (
                    p.get("sessionId").and_then(Value::as_str),
                    p.pointer("/targetInfo/targetId").and_then(Value::as_str),
                ) else {
                    return Ok(());
                };
                let (child, target) = (child.to_string(), target.to_string());
                // The page this visit created is set up by `run`, in order.
                if self.main_target.as_deref() == Some(target.as_str()) {
                    return Ok(());
                }
                let kind = p
                    .pointer("/targetInfo/type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                self.send(
                    "Target.setAutoAttach",
                    json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}),
                    Some(&child),
                    Pending::Ignore,
                )
                .await?;
                if !self.cfg.javascript && matches!(kind.as_str(), "page" | "iframe") {
                    self.send(
                        "Emulation.setScriptExecutionDisabled",
                        json!({"value": true}),
                        Some(&child),
                        Pending::Ignore,
                    )
                    .await?;
                }
                if self.cfg.javascript && matches!(kind.as_str(), "page" | "iframe") {
                    self.send(
                        "Page.addScriptToEvaluateOnNewDocument",
                        json!({"source": HARDEN}),
                        Some(&child),
                        Pending::Ignore,
                    )
                    .await?;
                }
                self.send(
                    "Fetch.enable",
                    json!({"patterns": [{"urlPattern": "*"}]}),
                    Some(&child),
                    Pending::FetchEnable {
                        session: child.clone(),
                        target,
                    },
                )
                .await?;
            }
            "Page.loadEventFired" if session == self.main.as_deref() => self.loaded = true,
            "Network.responseReceived"
                if session == self.main.as_deref()
                    && p.get("type").and_then(Value::as_str) == Some("Document") =>
            {
                if let Some(s) = p.pointer("/response/status").and_then(Value::as_u64) {
                    self.status = u16::try_from(s).ok();
                }
            }
            "Page.javascriptDialogOpening" => {
                self.send(
                    "Page.handleJavaScriptDialog",
                    json!({"accept": false}),
                    session,
                    Pending::Ignore,
                )
                .await?;
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
        let r = self
            .call(
                "Runtime.evaluate",
                json!({"expression": expr, "returnByValue": true}),
                Some(session),
            )
            .await?;
        if r.get("exceptionDetails").is_some() {
            bail!("the page could not be read");
        }
        Ok(r.pointer("/result/value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    pub(super) async fn run(&mut self, url: &Url, mode: Mode, wait: Duration) -> Result<Browsed> {
        let ctx = self
            .call(
                "Target.createBrowserContext",
                json!({"disposeOnDetach": true}),
                None,
            )
            .await?;
        let ctx = ctx
            .get("browserContextId")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("no browser context"))?
            .to_string();
        // Best effort: a browser that does not know this command still has downloads off in headless mode by default.
        let _ = self
            .call(
                "Browser.setDownloadBehavior",
                json!({"behavior": "deny", "browserContextId": ctx}),
                None,
            )
            .await;
        let t = self
            .call(
                "Target.createTarget",
                json!({"url": "about:blank", "browserContextId": ctx}),
                None,
            )
            .await?;
        let target = t
            .get("targetId")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("no target"))?
            .to_string();
        self.main_target = Some(target.clone());
        let a = self
            .call(
                "Target.attachToTarget",
                json!({"targetId": target, "flatten": true}),
                None,
            )
            .await?;
        let s = a
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("no session"))?
            .to_string();
        self.main = Some(s.clone());

        self.call("Page.enable", json!({}), Some(&s)).await?;
        self.call("Network.enable", json!({}), Some(&s)).await?;
        self.call(
            "Network.setBypassServiceWorker",
            json!({"bypass": true}),
            Some(&s),
        )
        .await
        .ok();
        self.call(
            "Emulation.setDeviceMetricsOverride",
            json!({"width": 1024, "height": 768, "deviceScaleFactor": 1, "mobile": false}),
            Some(&s),
        )
        .await?;
        if self.cfg.javascript {
            self.call(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({"source": HARDEN}),
                Some(&s),
            )
            .await?;
        } else {
            self.call(
                "Emulation.setScriptExecutionDisabled",
                json!({"value": true}),
                Some(&s),
            )
            .await?;
        }
        self.call(
            "Target.setAutoAttach",
            json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}),
            Some(&s),
        )
        .await?;
        self.call(
            "Fetch.enable",
            json!({"patterns": [{"urlPattern": "*"}]}),
            Some(&s),
        )
        .await?;

        let nav = self
            .call("Page.navigate", json!({"url": url.as_str()}), Some(&s))
            .await?;
        if let Some(e) = nav.get("errorText").and_then(Value::as_str) {
            // Let the blocked list explain a failure the allowlist caused.
            self.pump_for(Duration::from_millis(100)).await.ok();
            bail!(
                "the page did not load: {e}{}",
                if self.blocked.is_empty() {
                    String::new()
                } else {
                    format!(" (stopped: {})", self.blocked.join("; "))
                }
            );
        }
        let stop = (Instant::now() + Duration::from_secs(20)).min(
            self.deadline
                .checked_sub(Duration::from_secs(3))
                .unwrap_or_else(Instant::now),
        );
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

        let info = self
            .eval(&s, "JSON.stringify({u: location.href, t: document.title})")
            .await?;
        let info: Value = serde_json::from_str(&info).unwrap_or(Value::Null);
        let mut out = Browsed {
            url: info
                .get("u")
                .and_then(Value::as_str)
                .unwrap_or(url.as_str())
                .to_string(),
            title: clip(info.get("t").and_then(Value::as_str).unwrap_or(""), 300),
            status: self.status,
            blocked: self.blocked.clone(),
            requests: self.requests,
            timed_out,
            ..Browsed::default()
        };
        match mode {
            Mode::Text | Mode::Html => {
                let expr = if mode == Mode::Text {
                    "document.documentElement ? document.documentElement.innerText : ''"
                } else {
                    "document.documentElement ? document.documentElement.outerHTML : ''"
                };
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
                let mut shot = self
                    .call("Page.captureScreenshot", json!({"format": "png"}), Some(&s))
                    .await?;
                let mut bytes = base64::engine::general_purpose::STANDARD
                    .decode(shot.get("data").and_then(Value::as_str).unwrap_or(""))
                    .context("screenshot")?;
                if bytes.len() > self.cfg.max_screenshot_bytes {
                    shot = self
                        .call(
                            "Page.captureScreenshot",
                            json!({"format": "jpeg", "quality": 55}),
                            Some(&s),
                        )
                        .await?;
                    bytes = base64::engine::general_purpose::STANDARD
                        .decode(shot.get("data").and_then(Value::as_str).unwrap_or(""))
                        .context("screenshot")?;
                    if bytes.len() > self.cfg.max_screenshot_bytes {
                        bail!(
                            "the screenshot is larger than the {} KiB limit",
                            self.cfg.max_screenshot_bytes >> 10
                        );
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
