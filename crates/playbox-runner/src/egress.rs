//! Host-side reverse proxy for model API egress (`BOX-7`).
//!
//! A task asks for named upstreams in [`crate::TaskSpec::egress`]; the host owner defines those upstreams in [`EgressConfig`]
//! (`playbox up --egress FILE`) and grants names in [`crate::Limits::allowed_egress`]. For each task the host binds a private
//! loopback HTTP/1.1 proxy on `127.0.0.1:0` with a random per-task capability token in the base URL path
//! (`http://127.0.0.1:PORT/t/<token>/<upstream>`), injects the secret header on the host side from [`SecretStore`], refuses
//! redirects, strips unapproved guest headers, checks the path against `allow_paths`, streams response chunks (including SSE),
//! and writes a structured JSONL log containing only metadata (never headers, queries, secrets, or bodies).

use crate::netpolicy;
use crate::secrets::{self, SecretStore, SecretUse};
use anyhow::{anyhow, bail, Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Write as _;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Semaphore};

pub const MAX_HEADER_BYTES: usize = 16 * 1024;
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_MEMORY_LOG_ENTRIES: usize = 4096;
pub const DEFAULT_MAX_LOG_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CONCURRENT: usize = 16;
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(15);
const TOTAL_REQ_TIMEOUT: Duration = Duration::from_secs(120);

const FORBIDDEN_GUEST_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "cookie",
    "set-cookie",
    "host",
    "connection",
    "keep-alive",
    "transfer-encoding",
    "upgrade",
    "expect",
    "te",
    "trailer",
    "forwarded",
    "x-forwarded-for",
    "x-real-ip",
];

/// Returns true for headers a guest must never control (auth, hop-by-hop, forwarding, and host-owned `x-playbox-*` correlation headers).
#[must_use]
pub fn is_forbidden_guest_header(lower_name: &str) -> bool {
    FORBIDDEN_GUEST_HEADERS.contains(&lower_name) || lower_name.starts_with("x-playbox-")
}

fn default_pass_headers() -> Vec<String> {
    vec!["content-type".into(), "accept".into(), "user-agent".into()]
}

/// One named upstream API the host owner offers to sandboxed tasks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressUpstream {
    pub name: String,
    pub base: String,
    pub auth_header: String,
    #[serde(default)]
    pub auth_prefix: String,
    pub secret: String,
    pub allow_paths: Vec<String>,
    #[serde(default = "default_pass_headers")]
    pub pass_headers: Vec<String>,
    /// Host-injected headers added after stripping guest headers (e.g. `X-Playbox-Room-Id`, `X-Playbox-Cache-Mode`).
    #[serde(default)]
    pub extra_headers: BTreeMap<String, String>,
    /// Environment variable passed to the guest (defaults to `PLAYBOX_EGRESS_<NAME>_URL`).
    #[serde(default)]
    pub env_var: Option<String>,
    /// Allow plain `http://` upstreams (for local tests or loopback `playbox-aiproxy`; `https://` required otherwise).
    #[serde(default)]
    pub insecure_http: bool,
    /// Allow upstream hostnames resolving to RFC1918/private addresses when `https://` is used.
    #[serde(default)]
    pub allow_private: bool,
}

impl EgressUpstream {
    /// Fail-closed BOX-7 upstream preset targeting a host-side `playbox-aiproxy` gateway.
    ///
    /// Allows only model inference, embedding, and rerank endpoints (never `/api/*`, `/mcp/*`, or `/swagger/*`).
    /// Plain `http://` is permitted only when `base` targets loopback (`127.0.0.1`, `[::1]`, or `localhost`).
    #[must_use]
    pub fn aiproxy_preset(
        name: impl Into<String>,
        base: impl Into<String>,
        secret: impl Into<String>,
    ) -> Self {
        let base_s = base.into();
        let is_loopback_http = url::Url::parse(&base_s).ok().is_some_and(|u| {
            u.scheme() == "http"
                && matches!(
                    u.host_str(),
                    Some("127.0.0.1" | "localhost" | "::1" | "[::1]")
                )
        });
        Self {
            name: name.into(),
            base: base_s,
            auth_header: "authorization".into(),
            auth_prefix: "Bearer ".into(),
            secret: secret.into(),
            allow_paths: vec![
                "/v1/messages".into(),
                "/v1/chat/completions".into(),
                "/v1/responses".into(),
                "/v1/embeddings".into(),
                "/v1/rerank".into(),
                "/v1beta/models/*".into(),
            ],
            pass_headers: vec![
                "content-type".into(),
                "accept".into(),
                "user-agent".into(),
                "anthropic-version".into(),
                "anthropic-beta".into(),
                "openai-beta".into(),
            ],
            extra_headers: BTreeMap::new(),
            env_var: Some("PLAYBOX_AIPROXY_URL".into()),
            insecure_http: is_loopback_http,
            allow_private: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EgressConfig {
    pub upstreams: Vec<EgressUpstream>,
    #[serde(default)]
    pub log_file: Option<PathBuf>,
    /// Maximum size in bytes of `log_file` before rotating to `<log_file>.1`. Defaults to 16 MiB.
    #[serde(default)]
    pub max_log_bytes: Option<u64>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawEgressConfig {
    List(Vec<EgressUpstream>),
    Full(EgressConfig),
}

impl EgressConfig {
    /// Parse an owner egress config file (either a JSON array of upstreams or `{ "upstreams": [...], "log_file": ... }`)
    /// and validate static invariants (fail-closed `allow_paths`, valid names, schemes, and headers).
    ///
    /// # Errors
    ///
    /// Fails if the JSON is malformed or any upstream violates fail-closed rules.
    pub fn from_json_bytes(raw: &[u8]) -> Result<Self> {
        let cfg = match serde_json::from_slice::<RawEgressConfig>(raw)
            .context("egress config must be a JSON array of upstreams or {upstreams, log_file}")?
        {
            RawEgressConfig::List(upstreams) => Self {
                upstreams,
                log_file: None,
                max_log_bytes: None,
            },
            RawEgressConfig::Full(c) => c,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    /// Load and validate from `path`.
    ///
    /// # Errors
    ///
    /// Fails if `path` cannot be read or fails [`Self::from_json_bytes`].
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_json_bytes(&raw)
    }

    /// Validate static configuration invariants.
    ///
    /// # Errors
    ///
    /// Fails on duplicate/invalid upstream names, empty `allow_paths`, `http://` without `insecure_http`, or invalid header/env names.
    pub fn validate(&self) -> Result<()> {
        if self.max_log_bytes == Some(0) {
            bail!("max_log_bytes must be positive");
        }
        let mut seen = BTreeSet::new();
        for u in &self.upstreams {
            if !secrets::valid_name(&u.name) {
                bail!("bad egress upstream name {:?}", u.name);
            }
            if !seen.insert(&u.name) {
                bail!("duplicate egress upstream {:?}", u.name);
            }
            if !secrets::valid_name(&u.secret) {
                bail!(
                    "bad secret name {:?} in egress upstream {:?}",
                    u.secret,
                    u.name
                );
            }
            let parsed = url::Url::parse(&u.base)
                .with_context(|| format!("bad base URL in egress upstream {:?}", u.name))?;
            match parsed.scheme() {
                "https" => {}
                "http" if u.insecure_http => {}
                "http" => bail!(
                    "egress upstream {:?} uses http:// without insecure_http=true",
                    u.name
                ),
                other => bail!(
                    "egress upstream {:?} uses unsupported scheme {other:?}",
                    u.name
                ),
            }
            if parsed.host_str().is_none()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                bail!(
                    "egress upstream {:?} base URL must have a host and no query or fragment",
                    u.name
                );
            }
            HeaderName::from_bytes(u.auth_header.trim().as_bytes()).with_context(|| {
                format!(
                    "bad auth_header {:?} in egress upstream {:?}",
                    u.auth_header, u.name
                )
            })?;
            if u.allow_paths.is_empty() {
                bail!(
                    "egress upstream {:?} must have a non-empty allow_paths list (fail closed)",
                    u.name
                );
            }
            for pat in &u.allow_paths {
                validate_allow_pattern(pat).with_context(|| {
                    format!(
                        "bad allow_paths entry {pat:?} in egress upstream {:?}",
                        u.name
                    )
                })?;
            }
            for h in &u.pass_headers {
                let lower = h.trim().to_ascii_lowercase();
                HeaderName::from_bytes(lower.as_bytes()).with_context(|| {
                    format!(
                        "bad pass_headers entry {h:?} in egress upstream {:?}",
                        u.name
                    )
                })?;
                if is_forbidden_guest_header(&lower) {
                    bail!(
                        "header {h:?} cannot be forwarded from the guest in egress upstream {:?}",
                        u.name
                    );
                }
            }
            let auth_lower = u.auth_header.trim().to_ascii_lowercase();
            for (k, v) in &u.extra_headers {
                let lower = k.trim().to_ascii_lowercase();
                if lower == auth_lower {
                    bail!(
                        "extra_headers in egress upstream {:?} cannot override auth_header {:?}",
                        u.name,
                        u.auth_header
                    );
                }
                HeaderName::from_bytes(lower.as_bytes()).with_context(|| {
                    format!(
                        "bad extra_headers key {k:?} in egress upstream {:?}",
                        u.name
                    )
                })?;
                HeaderValue::from_str(v).with_context(|| {
                    format!(
                        "bad extra_headers value for {k:?} in egress upstream {:?}",
                        u.name
                    )
                })?;
            }
            if let Some(ev) = &u.env_var {
                if !valid_env_name(ev) {
                    bail!("bad env_var {ev:?} in egress upstream {:?}", u.name);
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.upstreams.iter().any(|u| u.name == name)
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&EgressUpstream> {
        self.upstreams.iter().find(|u| u.name == name)
    }
}

fn valid_env_name(s: &str) -> bool {
    let mut bytes = s.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && s.len() <= 64
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

#[must_use]
pub fn default_env_var(upstream_name: &str) -> String {
    let norm: String = upstream_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("PLAYBOX_EGRESS_{norm}_URL")
}

fn validate_allow_pattern(pat: &str) -> Result<()> {
    if !pat.starts_with('/') || pat.contains("..") || pat.contains("//") || pat.contains('\\') {
        bail!("pattern must start with '/', without '..', '//' or '\\'");
    }
    if pat[..pat.len().saturating_sub(1)].contains('*') {
        bail!("'*' wildcard is only allowed at the very end of an allow_paths entry");
    }
    Ok(())
}

/// Validate and normalize an untrusted request path (without query string).
/// Rejects `..`, percent-encoded dots/slashes (`%2e`, `%2f`, `%5c`), `//`, `\`, and control characters.
pub fn normalize_request_path(raw_path: &str) -> Result<String> {
    if !raw_path.starts_with('/') {
        bail!("not_origin_form");
    }
    if raw_path
        .bytes()
        .any(|b| b <= 0x20 || b == 0x7f || b == b'\\')
    {
        bail!("bad_path_chars");
    }
    let lower = raw_path.to_ascii_lowercase();
    if lower.contains("%2e")
        || lower.contains("%2f")
        || lower.contains("%5c")
        || lower.contains("%00")
    {
        bail!("encoded_traversal");
    }
    if raw_path.contains("//") {
        bail!("double_slash");
    }
    for seg in raw_path.split('/') {
        if seg == "." || seg == ".." {
            bail!("dot_segment");
        }
    }
    Ok(raw_path.to_string())
}

#[must_use]
pub fn path_matches(allow_paths: &[String], clean_path: &str) -> bool {
    allow_paths.iter().any(|pat| {
        if let Some(prefix) = pat.strip_suffix('*') {
            clean_path.starts_with(prefix)
        } else {
            clean_path == pat
        }
    })
}

/// One structured JSONL record written per proxy request (never contains headers, queries, tokens, secrets, or bodies).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EgressLogEntry {
    pub ts_ms: u64,
    pub upstream: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub req_bytes: usize,
    pub resp_bytes: u64,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denied: Option<String>,
}

struct PreparedUpstream {
    base_url: url::Url,
    base_prefix: String,
    client: reqwest::Client,
    auth_header: HeaderName,
    auth_value: HeaderValue,
    extra_headers: Vec<(HeaderName, HeaderValue)>,
    allow_paths: Vec<String>,
    pass_headers: BTreeSet<String>,
}

struct SharedState {
    token_hash: blake3::Hash,
    upstreams: BTreeMap<String, PreparedUpstream>,
    log_file: Option<PathBuf>,
    max_log_bytes: u64,
    file_mu: Mutex<()>,
    entries: Mutex<VecDeque<EgressLogEntry>>,
    dropped_entries: AtomicU64,
    sem: Semaphore,
    arc_maverick: Option<Arc<crate::maverick::ArcMaverickHook>>,
}

fn rotated_log_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".1");
    PathBuf::from(os)
}

impl SharedState {
    fn record(&self, entry: EgressLogEntry) {
        if let Some(path) = &self.log_file {
            if let Ok(_fguard) = self.file_mu.lock() {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Ok(line) = serde_json::to_string(&entry) {
                    let needed = u64::try_from(line.len() + 1).unwrap_or(u64::MAX);
                    let cur = std::fs::metadata(path).map_or(0, |m| m.len());
                    if cur > 0 && cur.saturating_add(needed) > self.max_log_bytes {
                        let _ = std::fs::rename(path, rotated_log_path(path));
                    }
                    #[cfg(unix)]
                    let open = {
                        use std::os::unix::fs::OpenOptionsExt;
                        std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .mode(0o600)
                            .open(path)
                    };
                    #[cfg(not(unix))]
                    let open = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path);
                    if let Ok(mut f) = open {
                        let _ = writeln!(f, "{line}");
                    }
                }
            }
        }
        if let Ok(mut guard) = self.entries.lock() {
            if guard.len() >= MAX_MEMORY_LOG_ENTRIES {
                guard.pop_front();
                self.dropped_entries.fetch_add(1, Ordering::Relaxed);
            }
            guard.push_back(entry);
        }
    }
}

/// Running per-task loopback proxy. Dropping it stops accepting connections and aborts the listener loop.
pub struct Proxy {
    port: u16,
    token: String,
    envs: Vec<(String, String)>,
    used: Vec<SecretUse>,
    state: Arc<SharedState>,
    stop: Option<oneshot::Sender<()>>,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
    }
}

fn random_hex_16() -> Result<String> {
    let mut b = [0u8; 16];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

impl Proxy {
    /// Start a per-task proxy on `127.0.0.1:0` serving `names` from `cfg`, loading secret values from `secrets`.
    ///
    /// # Errors
    ///
    /// Fails if any requested upstream is unknown, duplicated, missing its secret, resolves to a forbidden address, or cannot bind.
    pub async fn start(
        cfg: &EgressConfig,
        secrets: &SecretStore,
        names: &[String],
    ) -> Result<Self> {
        Self::start_with_host(cfg, secrets, names, Ipv4Addr::LOCALHOST, "127.0.0.1").await
    }

    /// Like [`Proxy::start`], binding on `bind_ip:0` and advertising `guest_host` in the generated capability URLs
    /// (e.g. `host.docker.internal` for an `OpenShell` sandbox reaching the host proxy).
    ///
    /// # Errors
    ///
    /// Fails if any requested upstream is unknown, duplicated, missing its secret, resolves to a forbidden address, or cannot bind.
    pub async fn start_with_host(
        cfg: &EgressConfig,
        secrets: &SecretStore,
        names: &[String],
        bind_ip: Ipv4Addr,
        guest_host: &str,
    ) -> Result<Self> {
        Self::start_with_host_and_headers(
            cfg,
            secrets,
            names,
            bind_ip,
            guest_host,
            &BTreeMap::new(),
        )
        .await
    }

    /// Like [`Proxy::start_with_host`], injecting additional host-trusted correlation headers
    /// (e.g. `X-Playbox-Room-Id`, `X-Playbox-Member-Id`, `X-Playbox-Task-Id`, `X-Playbox-Run-Id`)
    /// after stripping guest headers and before injecting the upstream credential.
    ///
    /// # Errors
    ///
    /// Fails if any requested upstream is unknown, duplicated, missing its secret, resolves to a forbidden address,
    /// contains invalid correlation headers, or cannot bind.
    pub async fn start_with_host_and_headers(
        cfg: &EgressConfig,
        secrets: &SecretStore,
        names: &[String],
        bind_ip: Ipv4Addr,
        guest_host: &str,
        correlation_headers: &BTreeMap<String, String>,
    ) -> Result<Self> {
        Self::start_with_arc_maverick(
            cfg,
            secrets,
            names,
            bind_ip,
            guest_host,
            correlation_headers,
            None,
        )
        .await
    }

    /// Like [`Proxy::start_with_host_and_headers`], optionally attaching a native Rust
    /// [`crate::maverick::ArcMaverickHook`] for CPU-ARC local skill bypass, Goldilocks Maverick
    /// Verification-Only, and dual-signed (`Ed25519 + ML-DSA-65`) `RoomLog` receipts.
    ///
    /// # Errors
    ///
    /// Fails if any requested upstream is unknown, duplicated, missing its secret, resolves to a forbidden address,
    /// contains invalid correlation headers, or cannot bind.
    pub async fn start_with_arc_maverick(
        cfg: &EgressConfig,
        secrets: &SecretStore,
        names: &[String],
        bind_ip: Ipv4Addr,
        guest_host: &str,
        correlation_headers: &BTreeMap<String, String>,
        arc_maverick: Option<Arc<crate::maverick::ArcMaverickHook>>,
    ) -> Result<Self> {
        cfg.validate()?;
        if guest_host.is_empty()
            || guest_host.contains(['/', '?', '#', '@', ':', '\r', '\n'])
            || guest_host.chars().any(char::is_whitespace)
        {
            bail!("invalid egress guest_host {guest_host:?}");
        }
        if names.is_empty() {
            bail!("cannot start egress proxy with an empty upstream list");
        }
        if names.len() > secrets::MAX_PER_TASK {
            bail!(
                "a task may request at most {} egress upstreams",
                secrets::MAX_PER_TASK
            );
        }
        let mut seen = BTreeSet::new();
        let mut prepared = BTreeMap::new();
        let mut used = Vec::new();
        let mut raw_envs = Vec::new();

        for name in names {
            if !seen.insert(name) {
                bail!("egress upstream {name:?} listed twice");
            }
            let u = cfg.get(name).ok_or_else(|| {
                anyhow!("egress upstream {name:?} is not configured on this host")
            })?;
            let (secret_val, secret_use) = secrets.read_utf8(&u.secret)?;
            used.push(secret_use);

            let base_url = url::Url::parse(&u.base)?;
            let host = base_url
                .host_str()
                .ok_or_else(|| anyhow!("missing host in {:?}", u.base))?
                .to_string();
            let port = base_url
                .port_or_known_default()
                .ok_or_else(|| anyhow!("missing port in {:?}", u.base))?;

            let mut builder = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(TOTAL_REQ_TIMEOUT)
                .connect_timeout(Duration::from_secs(10));

            if !u.insecure_http {
                let resolved: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
                    .await
                    .with_context(|| format!("resolving egress upstream {host}:{port}"))?
                    .collect();
                if resolved.is_empty() {
                    bail!("egress upstream {host} did not resolve");
                }
                for addr in &resolved {
                    if netpolicy::always_rejected(addr.ip())
                        || (!u.allow_private && netpolicy::private(addr.ip()))
                    {
                        bail!(
                            "egress upstream {:?} ({host}) resolved to forbidden address {}",
                            u.name,
                            addr.ip()
                        );
                    }
                }
                if host.parse::<std::net::IpAddr>().is_err() {
                    builder = builder.resolve_to_addrs(&host, &resolved);
                }
            }

            let client = builder.build().context("building egress HTTP client")?;
            let auth_header = HeaderName::from_bytes(u.auth_header.trim().as_bytes())?;
            let auth_value = HeaderValue::from_str(&format!("{}{}", u.auth_prefix, secret_val))
                .map_err(|_| anyhow!("secret {:?} contains invalid HTTP header bytes", u.secret))?;
            let pass_headers = u
                .pass_headers
                .iter()
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !is_forbidden_guest_header(h))
                .collect();
            let mut merged_extra: BTreeMap<String, String> = u
                .extra_headers
                .iter()
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.clone()))
                .collect();
            for (k, v) in correlation_headers {
                merged_extra.insert(k.trim().to_ascii_lowercase(), v.clone());
            }
            let auth_lower = u.auth_header.trim().to_ascii_lowercase();
            let mut extra_headers = Vec::with_capacity(merged_extra.len());
            for (k, v) in merged_extra {
                if k == auth_lower {
                    bail!(
                        "correlation header {k:?} cannot override auth_header {:?}",
                        u.auth_header
                    );
                }
                let hn = HeaderName::from_bytes(k.as_bytes())
                    .with_context(|| format!("bad extra/correlation header name {k:?}"))?;
                let hv = HeaderValue::from_str(&v)
                    .with_context(|| format!("bad extra/correlation header value for {k:?}"))?;
                extra_headers.push((hn, hv));
            }
            let base_prefix = base_url.path().trim_end_matches('/').to_string();
            let env_key = u
                .env_var
                .clone()
                .unwrap_or_else(|| default_env_var(&u.name));
            raw_envs.push((env_key, u.name.clone()));

            prepared.insert(
                u.name.clone(),
                PreparedUpstream {
                    base_url,
                    base_prefix,
                    client,
                    auth_header,
                    auth_value,
                    extra_headers,
                    allow_paths: u.allow_paths.clone(),
                    pass_headers,
                },
            );
        }

        let listener = TcpListener::bind((bind_ip, 0))
            .await
            .context("binding egress proxy")?;
        let port = listener.local_addr()?.port();
        let token = random_hex_16()?;
        let envs = raw_envs
            .into_iter()
            .map(|(k, name)| (k, format!("http://{guest_host}:{port}/t/{token}/{name}")))
            .collect();

        let state = Arc::new(SharedState {
            token_hash: blake3::hash(token.as_bytes()),
            upstreams: prepared,
            log_file: cfg.log_file.clone(),
            max_log_bytes: cfg.max_log_bytes.unwrap_or(DEFAULT_MAX_LOG_BYTES),
            file_mu: Mutex::new(()),
            entries: Mutex::new(VecDeque::new()),
            dropped_entries: AtomicU64::new(0),
            sem: Semaphore::new(MAX_CONCURRENT),
            arc_maverick,
        });
        let (stop_tx, mut stop_rx) = oneshot::channel();
        let runner_state = Arc::clone(&state);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    acc = listener.accept() => {
                        let Ok((stream, _)) = acc else { break };
                        let st = Arc::clone(&runner_state);
                        tokio::spawn(async move {
                            let Ok(_permit) = st.sem.acquire().await else { return };
                            let _ = tokio::time::timeout(TOTAL_REQ_TIMEOUT, serve_one(stream, &st)).await;
                        });
                    }
                }
            }
        });

        Ok(Self {
            port,
            token,
            envs,
            used,
            state,
            stop: Some(stop_tx),
        })
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    #[must_use]
    pub fn base_url(&self, upstream: &str) -> String {
        format!("http://127.0.0.1:{}/t/{}/{upstream}", self.port, self.token)
    }

    /// `(ENV_KEY, base_url)` pairs to inject into the guest environment.
    #[must_use]
    pub fn env_vars(&self) -> &[(String, String)] {
        &self.envs
    }

    /// Names and blake3 digests of the secrets loaded for this task's proxy (for receipts and the audit log).
    #[must_use]
    pub fn secrets_used(&self) -> &[SecretUse] {
        &self.used
    }

    #[must_use]
    pub fn entries(&self) -> Vec<EgressLogEntry> {
        self.state
            .entries
            .lock()
            .map_or_else(|_| Vec::new(), |g| g.iter().cloned().collect())
    }

    #[must_use]
    pub fn dropped_entries(&self) -> u64 {
        self.state.dropped_entries.load(Ordering::Relaxed)
    }
}

struct ParsedReq {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

async fn read_http_request(
    stream: &mut TcpStream,
) -> std::result::Result<ParsedReq, (u16, &'static str)> {
    let mut buf = Vec::with_capacity(4096);
    let header_end = tokio::time::timeout(HEADER_READ_TIMEOUT, async {
        loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                return Ok(pos);
            }
            if buf.len() >= MAX_HEADER_BYTES {
                return Err((431u16, "headers_too_large"));
            }
            let mut chunk = [0u8; 2048];
            let n = stream
                .read(&mut chunk)
                .await
                .map_err(|_| (400u16, "read_error"))?;
            if n == 0 {
                return Err((400u16, "unexpected_eof"));
            }
            buf.extend_from_slice(&chunk[..n]);
        }
    })
    .await
    .map_err(|_| (408u16, "header_timeout"))??;

    let head =
        std::str::from_utf8(&buf[..header_end]).map_err(|_| (400u16, "invalid_utf8_headers"))?;
    let mut lines = head.split("\r\n");
    let req_line = lines.next().ok_or((400u16, "empty_request_line"))?;
    let mut parts = req_line.split_whitespace();
    let method = parts
        .next()
        .ok_or((400u16, "bad_request_line"))?
        .to_string();
    let target = parts
        .next()
        .ok_or((400u16, "bad_request_line"))?
        .to_string();
    let version = parts.next().ok_or((400u16, "bad_request_line"))?;
    if parts.next().is_some() || !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
        return Err((400u16, "bad_http_version"));
    }
    if matches!(method.as_str(), "CONNECT" | "TRACE") {
        return Err((405u16, "method_not_allowed"));
    }

    let mut headers = Vec::new();
    let mut content_len: Option<usize> = None;
    for line in lines {
        let Some((name, val)) = line.split_once(':') else {
            return Err((400u16, "malformed_header"));
        };
        let lower = name.trim().to_ascii_lowercase();
        let v = val.trim().to_string();
        if matches!(lower.as_str(), "transfer-encoding" | "upgrade" | "expect") {
            return Err((400u16, "unsupported_framing_header"));
        }
        if lower == "content-length" {
            let n: usize = v.parse().map_err(|_| (400u16, "bad_content_length"))?;
            if n > MAX_BODY_BYTES {
                return Err((413u16, "body_too_large"));
            }
            if content_len.replace(n).is_some() {
                return Err((400u16, "duplicate_content_length"));
            }
        }
        headers.push((lower, v));
    }

    let want = content_len.unwrap_or(0);
    let already = buf.len() - (header_end + 4);
    if already > want {
        return Err((400u16, "trailing_bytes_past_content_length"));
    }
    let mut body = Vec::with_capacity(want);
    body.extend_from_slice(&buf[header_end + 4..]);
    while body.len() < want {
        let mut chunk = vec![0u8; (want - body.len()).min(16 * 1024)];
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|_| (400u16, "body_read_error"))?;
        if n == 0 {
            return Err((400u16, "truncated_body"));
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Ok(ParsedReq {
        method,
        target,
        headers,
        body,
    })
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        _ => "Response",
    }
}

async fn write_error(stream: &mut TcpStream, status: u16, msg: &str) -> u64 {
    let body = format!("{{\"error\":{msg:?}}}\n");
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reason_phrase(status),
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
    let _ = stream.shutdown().await;
    body.len() as u64
}

fn token_matches(expected: &blake3::Hash, candidate: &str) -> bool {
    let got = blake3::hash(candidate.as_bytes());
    expected
        .as_bytes()
        .iter()
        .zip(got.as_bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// Split `/t/<token>/<upstream>/<rest>[?query]` into `(token, upstream, path_only, query)`.
fn split_proxy_target(
    target: &str,
) -> std::result::Result<(&str, &str, &str, Option<&str>), &'static str> {
    if !target.starts_with('/') || target.starts_with("//") {
        return Err("not_origin_form");
    }
    let (raw_path, query) = match target.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (target, None),
    };
    let rest = raw_path
        .strip_prefix("/t/")
        .ok_or("missing_capability_token")?;
    let (tok, after_tok) = rest.split_once('/').ok_or("missing_upstream")?;
    let (upstream, sub_path) = match after_tok.find('/') {
        Some(idx) => (&after_tok[..idx], &after_tok[idx..]),
        None => (after_tok, "/"),
    };
    if tok.is_empty() || upstream.is_empty() {
        return Err("missing_upstream");
    }
    Ok((tok, upstream, sub_path, query))
}

async fn serve_one(mut stream: TcpStream, state: &SharedState) {
    let t0 = Instant::now();
    let ts_ms = now_ms();
    let req = match read_http_request(&mut stream).await {
        Ok(r) => r,
        Err((status, why)) => {
            let resp_bytes = write_error(&mut stream, status, why).await;
            state.record(EgressLogEntry {
                ts_ms,
                upstream: "-".into(),
                method: "-".into(),
                path: "-".into(),
                status,
                req_bytes: 0,
                resp_bytes,
                duration_ms: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
                denied: Some(why.into()),
            });
            return;
        }
    };

    let req_bytes = req.body.len();
    let (tok, upstream_name, raw_sub_path, query) = match split_proxy_target(&req.target) {
        Ok(parts) => parts,
        Err(why) => {
            let status = if why == "missing_capability_token" {
                403
            } else {
                400
            };
            let resp_bytes = write_error(&mut stream, status, why).await;
            state.record(EgressLogEntry {
                ts_ms,
                upstream: "-".into(),
                method: req.method,
                path: "-".into(),
                status,
                req_bytes,
                resp_bytes,
                duration_ms: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
                denied: Some(why.into()),
            });
            return;
        }
    };

    if !token_matches(&state.token_hash, tok) {
        let resp_bytes = write_error(&mut stream, 403, "bad_capability_token").await;
        state.record(EgressLogEntry {
            ts_ms,
            upstream: upstream_name.to_string(),
            method: req.method,
            path: "-".into(),
            status: 403,
            req_bytes,
            resp_bytes,
            duration_ms: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
            denied: Some("bad_capability_token".into()),
        });
        return;
    }

    let clean_path = match normalize_request_path(raw_sub_path) {
        Ok(p) => p,
        Err(e) => {
            let why = e.to_string();
            let resp_bytes = write_error(&mut stream, 400, &why).await;
            state.record(EgressLogEntry {
                ts_ms,
                upstream: upstream_name.to_string(),
                method: req.method,
                path: "-".into(),
                status: 400,
                req_bytes,
                resp_bytes,
                duration_ms: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
                denied: Some(why),
            });
            return;
        }
    };

    let Some(up) = state.upstreams.get(upstream_name) else {
        let resp_bytes = write_error(&mut stream, 403, "upstream_not_granted").await;
        state.record(EgressLogEntry {
            ts_ms,
            upstream: upstream_name.to_string(),
            method: req.method,
            path: clean_path,
            status: 403,
            req_bytes,
            resp_bytes,
            duration_ms: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
            denied: Some("upstream_not_granted".into()),
        });
        return;
    };

    if !path_matches(&up.allow_paths, &clean_path) {
        let resp_bytes = write_error(&mut stream, 403, "path_not_allowed").await;
        state.record(EgressLogEntry {
            ts_ms,
            upstream: upstream_name.to_string(),
            method: req.method,
            path: clean_path,
            status: 403,
            req_bytes,
            resp_bytes,
            duration_ms: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
            denied: Some("path_not_allowed".into()),
        });
        return;
    }

    if let Some(hook) = &state.arc_maverick {
        if let Some(hit) = hook.intercept_request(&req.method, &clean_path, &req.body) {
            let mut head = format!(
                "HTTP/1.1 {} {}\r\nContent-Length: {}\r\n",
                hit.status,
                reason_phrase(hit.status),
                hit.body.len()
            );
            for (k, v) in &hit.headers {
                if !k.contains(['\r', '\n']) && !v.contains(['\r', '\n']) {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
            }
            head.push_str("Connection: close\r\n\r\n");
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(&hit.body).await;
            let _ = stream.shutdown().await;
            state.record(EgressLogEntry {
                ts_ms,
                upstream: upstream_name.to_string(),
                method: req.method,
                path: clean_path,
                status: hit.status,
                req_bytes,
                resp_bytes: hit.body.len() as u64,
                duration_ms: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
                denied: hit.denied,
            });
            return;
        }
    }

    let Ok(method) = reqwest::Method::from_bytes(req.method.as_bytes()) else {
        let resp_bytes = write_error(&mut stream, 400, "invalid_method").await;
        state.record(EgressLogEntry {
            ts_ms,
            upstream: upstream_name.to_string(),
            method: req.method,
            path: clean_path,
            status: 400,
            req_bytes,
            resp_bytes,
            duration_ms: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
            denied: Some("invalid_method".into()),
        });
        return;
    };

    let mut dst_url = up.base_url.clone();
    dst_url.set_path(&format!("{}{}", up.base_prefix, clean_path));
    dst_url.set_query(query);

    let mut out_headers = HeaderMap::new();
    for (k, v) in &req.headers {
        if up.pass_headers.contains(k) && !is_forbidden_guest_header(k) {
            if let (Ok(hn), Ok(hv)) = (
                HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(v),
            ) {
                out_headers.append(hn, hv);
            }
        }
    }
    // Host-trusted correlation/policy headers injected after stripping guest headers.
    for (hn, hv) in &up.extra_headers {
        out_headers.insert(hn.clone(), hv.clone());
    }
    // Host-held credential injected last: guest headers can never override or coexist with it.
    out_headers.insert(up.auth_header.clone(), up.auth_value.clone());

    let mut resp = match up
        .client
        .request(method, dst_url)
        .headers(out_headers)
        .body(req.body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => {
            let resp_bytes = write_error(&mut stream, 502, "upstream_unreachable").await;
            state.record(EgressLogEntry {
                ts_ms,
                upstream: upstream_name.to_string(),
                method: req.method,
                path: clean_path,
                status: 502,
                req_bytes,
                resp_bytes,
                duration_ms: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
                denied: Some("upstream_unreachable".into()),
            });
            return;
        }
    };

    let status = resp.status().as_u16();
    // Redirects are never followed by reqwest (Policy::none), and we strip Location so the guest never sees upstream redirects.
    let mut head = format!("HTTP/1.1 {status} {}\r\n", reason_phrase(status));
    for (hn, hv) in resp.headers() {
        let n = hn.as_str();
        if matches!(
            n,
            "content-type"
                | "cache-control"
                | "x-request-id"
                | "request-id"
                | "retry-after"
                | "x-playbox-stop-reason"
                | "x-aiproxy-request-id"
        ) {
            if let Ok(s) = hv.to_str() {
                if !s.contains(['\r', '\n']) {
                    head.push_str(&format!("{hn}: {s}\r\n"));
                }
            }
        }
    }
    head.push_str("Connection: close\r\n\r\n");
    if stream.write_all(head.as_bytes()).await.is_err() {
        return;
    }

    let mut resp_bytes = 0u64;
    while let Ok(Some(chunk)) = resp.chunk().await {
        resp_bytes = resp_bytes.saturating_add(chunk.len() as u64);
        if stream.write_all(&chunk).await.is_err() {
            break;
        }
        let _ = stream.flush().await;
    }
    let _ = stream.shutdown().await;

    state.record(EgressLogEntry {
        ts_ms,
        upstream: upstream_name.to_string(),
        method: req.method,
        path: clean_path,
        status,
        req_bytes,
        resp_bytes,
        duration_ms: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
        denied: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_secret_store(secret_name: &str, secret_val: &str) -> (PathBuf, SecretStore) {
        let dir = std::env::temp_dir().join(format!(
            "pb-egress-sec-{}-{}",
            std::process::id(),
            random_hex_16().unwrap()
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
            std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(dir.join(secret_name))
                .unwrap();
            f.write_all(secret_val.as_bytes()).unwrap();
        }
        (dir.clone(), SecretStore::new(dir))
    }

    // REQ: BOX-7
    #[test]
    fn path_normalization_and_allow_rules_fail_closed() {
        assert_eq!(
            normalize_request_path("/v1/messages").unwrap(),
            "/v1/messages"
        );
        for bad in [
            "http://evil.example/v1/messages",
            "/v1/../admin",
            "/v1/%2e%2e/admin",
            "/v1/%2E%2E/admin",
            "/v1/%2fadmin",
            "/v1//messages",
            "/v1/\\evil",
            "/v1/a b",
        ] {
            assert!(normalize_request_path(bad).is_err(), "accepted {bad:?}");
        }

        let allow = vec!["/v1/messages".to_string(), "/v1/models/*".to_string()];
        assert!(path_matches(&allow, "/v1/messages"));
        assert!(path_matches(&allow, "/v1/models/claude"));
        assert!(!path_matches(&allow, "/v1/complete"));

        // Empty allow_paths, plain http without insecure_http, and forbidden pass_headers all fail closed.
        let empty = br#"[{"name":"a","base":"https://api.example.com","auth_header":"x-api-key","secret":"k","allow_paths":[]}]"#;
        assert!(EgressConfig::from_json_bytes(empty).is_err());

        let http_no_opt = br#"[{"name":"a","base":"http://127.0.0.1:9","auth_header":"x-api-key","secret":"k","allow_paths":["/v1"]}]"#;
        assert!(EgressConfig::from_json_bytes(http_no_opt).is_err());

        let bad_hdr = br#"[{"name":"a","base":"https://api.example.com","auth_header":"x-api-key","secret":"k","allow_paths":["/v1"],"pass_headers":["authorization"]}]"#;
        assert!(EgressConfig::from_json_bytes(bad_hdr).is_err());
    }

    // REQ: BOX-7
    #[tokio::test]
    async fn proxy_injects_secret_strips_guest_auth_refuses_redirects_streams_sse_and_logs_without_secrets(
    ) {
        let (sec_dir, secrets) = temp_secret_store("anthropic_key", "pbtest-host-credential-val\n");
        let log_path = sec_dir.join("egress.jsonl");

        // Local fake upstream API: verifies injected secret, checks guest headers were stripped, serves SSE and a 307 redirect.
        let upstream_listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                let _ = std::fs::remove_dir_all(&sec_dir);
                return;
            }
            Err(e) => panic!("bind fake upstream: {e}"),
        };
        let up_port = upstream_listener.local_addr().unwrap().port();
        let seen_headers: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = Arc::clone(&seen_headers);
        tokio::spawn(async move {
            while let Ok((mut s, _)) = upstream_listener.accept().await {
                let seen = Arc::clone(&seen_clone);
                tokio::spawn(async move {
                    let Ok(req) = read_http_request(&mut s).await else {
                        return;
                    };
                    seen.lock()
                        .unwrap()
                        .extend(req.headers.iter().map(|(k, v)| format!("{k}:{v}")));
                    if req.target.starts_with("/v1/redirect") {
                        let _ = s
                            .write_all(
                                b"HTTP/1.1 307 Temporary Redirect\r\nLocation: http://169.254.169.254/latest/meta-data\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .await;
                    } else {
                        let _ = s
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                            )
                            .await;
                        let _ = s.write_all(b"data: chunk-1\n\n").await;
                        let _ = s.flush().await;
                        let _ = s.write_all(b"data: chunk-2\n\n").await;
                    }
                    let _ = s.shutdown().await;
                });
            }
        });

        let cfg = EgressConfig {
            upstreams: vec![EgressUpstream {
                name: "anthropic".into(),
                base: format!("http://127.0.0.1:{up_port}"),
                auth_header: "x-api-key".into(),
                auth_prefix: String::new(),
                secret: "anthropic_key".into(),
                allow_paths: vec!["/v1/messages".into(), "/v1/redirect".into()],
                pass_headers: vec!["content-type".into(), "anthropic-version".into()],
                extra_headers: BTreeMap::new(),
                env_var: Some("ANTHROPIC_BASE_URL".into()),
                insecure_http: true,
                allow_private: false,
            }],
            log_file: Some(log_path.clone()),
            max_log_bytes: None,
        };

        let proxy = Proxy::start(&cfg, &secrets, &["anthropic".to_string()])
            .await
            .unwrap();
        assert_eq!(proxy.secrets_used().len(), 1);
        assert_eq!(proxy.secrets_used()[0].name, "anthropic_key");

        // 1. Valid request with hostile guest headers + secret query param: guest auth stripped, host key injected, SSE returned.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let resp = client
            .post(format!(
                "{}/v1/messages?secret_query=MUST_NOT_LOG",
                proxy.base_url("anthropic")
            ))
            .header("content-type", "application/json")
            .header("anthropic-version", "2023-06-01")
            .header("x-api-key", "pbtest-FAKE-GUEST-KEY")
            .header("authorization", "Bearer guest-spoof")
            .header("cookie", "s=1")
            .body(r#"{"prompt":"hi"}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body = resp.text().await.unwrap();
        assert_eq!(body, "data: chunk-1\n\ndata: chunk-2\n\n");

        let upstream_got = seen_headers.lock().unwrap().clone();
        assert!(upstream_got.contains(&"x-api-key:pbtest-host-credential-val".to_string()));
        assert!(!upstream_got.iter().any(|h| h.contains("FAKE-GUEST-KEY")));
        assert!(!upstream_got.iter().any(|h| h.starts_with("authorization:")));
        assert!(!upstream_got.iter().any(|h| h.starts_with("cookie:")));

        // 2. Upstream 307 redirect is NOT followed, and Location is stripped from guest response.
        let redir = client
            .post(format!("{}/v1/redirect", proxy.base_url("anthropic")))
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(redir.status().as_u16(), 307);
        assert!(redir.headers().get("location").is_none());

        // 3. Disallowed path & missing capability token are refused and logged.
        let denied_path = client
            .get(format!("{}/v1/admin", proxy.base_url("anthropic")))
            .send()
            .await
            .unwrap();
        assert_eq!(denied_path.status().as_u16(), 403);

        let bad_tok = client
            .get(format!(
                "http://127.0.0.1:{}/t/wrongtoken/anthropic/v1/messages",
                proxy.port()
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(bad_tok.status().as_u16(), 403);

        // 4. JSONL egress log has every request's path WITHOUT query, and never contains the secret, token, or body.
        let raw_log = std::fs::read_to_string(&log_path).unwrap();
        assert!(!raw_log.contains("pbtest-host-credential-val"));
        assert!(!raw_log.contains("MUST_NOT_LOG"));
        assert!(!raw_log.contains("FAKE-GUEST-KEY"));
        assert!(!raw_log.contains(&proxy.token));
        let entries = proxy.entries();
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].path, "/v1/messages");
        assert_eq!(entries[0].status, 200);
        assert_eq!(entries[2].denied.as_deref(), Some("path_not_allowed"));
        assert_eq!(entries[3].denied.as_deref(), Some("bad_capability_token"));

        let _ = std::fs::remove_dir_all(&sec_dir);
    }

    // REQ: BOX-7
    #[test]
    fn egress_logs_are_bounded_in_memory_and_rotate_on_disk() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("pb-egress-cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("egress.jsonl");

        let state = SharedState {
            token_hash: blake3::hash(b"tok"),
            upstreams: BTreeMap::new(),
            log_file: Some(log_path.clone()),
            max_log_bytes: 280,
            file_mu: Mutex::new(()),
            entries: Mutex::new(VecDeque::new()),
            dropped_entries: AtomicU64::new(0),
            sem: Semaphore::new(1),
            arc_maverick: None,
        };
        let total = MAX_MEMORY_LOG_ENTRIES + 25;
        for i in 0..total {
            state.record(EgressLogEntry {
                ts_ms: i as u64,
                upstream: "anthropic".into(),
                method: "POST".into(),
                path: "/v1/messages".into(),
                status: 200,
                req_bytes: 10,
                resp_bytes: 20,
                duration_ms: 1,
                denied: None,
            });
        }

        assert_eq!(state.entries.lock().unwrap().len(), MAX_MEMORY_LOG_ENTRIES);
        assert_eq!(state.dropped_entries.load(Ordering::Relaxed), 25);
        assert_eq!(state.entries.lock().unwrap().front().unwrap().ts_ms, 25);

        let cur_len = std::fs::metadata(&log_path).unwrap().len();
        let rot_path = rotated_log_path(&log_path);
        let rot_len = std::fs::metadata(&rot_path).unwrap().len();
        assert!(cur_len <= 280, "active log exceeded cap: {cur_len}");
        assert!(rot_len <= 280, "rotated log exceeded cap: {rot_len}");
        assert_eq!(
            std::fs::metadata(&log_path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // REQ: BOX-7, PROXY-4
    #[tokio::test]
    async fn aiproxy_preset_strips_guest_correlation_headers_and_injects_host_headers() {
        let (sec_dir, secrets) = temp_secret_store("aiproxy_tok", "pbx-worker-tok-123\n");
        let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                let _ = std::fs::remove_dir_all(&sec_dir);
                return;
            }
            Err(e) => panic!("bind fake aiproxy: {e}"),
        };
        let up_port = listener.local_addr().unwrap().port();
        let seen_headers: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = Arc::clone(&seen_headers);
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let seen = Arc::clone(&seen_clone);
                tokio::spawn(async move {
                    let Ok(req) = read_http_request(&mut s).await else {
                        return;
                    };
                    seen.lock()
                        .unwrap()
                        .extend(req.headers.iter().map(|(k, v)| format!("{k}:{v}")));
                    let _ = s
                        .write_all(
                            b"HTTP/1.1 402 Payment Required\r\nContent-Type: application/json\r\nX-Playbox-Stop-Reason: budget_exhausted\r\nContent-Length: 27\r\nConnection: close\r\n\r\n{\"error\":\"budget_exhausted\"}",
                        )
                        .await;
                    let _ = s.shutdown().await;
                });
            }
        });

        // Fail closed: pass_headers may never forward guest `x-playbox-*` headers.
        let spoof_cfg = br#"[{"name":"llm","base":"https://api.example.com","auth_header":"authorization","secret":"k","allow_paths":["/v1/messages"],"pass_headers":["x-playbox-run-id"]}]"#;
        assert!(EgressConfig::from_json_bytes(spoof_cfg).is_err());

        let preset = EgressUpstream::aiproxy_preset(
            "llm",
            format!("http://127.0.0.1:{up_port}"),
            "aiproxy_tok",
        );
        assert!(preset.insecure_http);
        assert!(preset.allow_private);

        let cfg = EgressConfig {
            upstreams: vec![preset],
            log_file: None,
            max_log_bytes: None,
        };
        let mut corr = BTreeMap::new();
        corr.insert("X-Playbox-Room-Id".into(), "room-alpha".into());
        corr.insert("X-Playbox-Run-Id".into(), "run-42".into());
        corr.insert("X-Playbox-Cache-Mode".into(), "off".into());

        let proxy = Proxy::start_with_host_and_headers(
            &cfg,
            &secrets,
            &["llm".to_string()],
            Ipv4Addr::LOCALHOST,
            "127.0.0.1",
            &corr,
        )
        .await
        .unwrap();

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();

        // Disallowed admin/MCP paths are blocked at BOX-7 before reaching aiproxy.
        for blocked in [
            "/api/playbox/ledger",
            "/mcp/public/1",
            "/swagger/index.html",
        ] {
            let r = client
                .get(format!("{}{blocked}", proxy.base_url("llm")))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status().as_u16(), 403, "expected 403 for {blocked}");
        }

        // Allowed inference path strips guest-forged X-Playbox-* headers and injects host correlation + Bearer token.
        let r = client
            .post(format!("{}/v1/messages", proxy.base_url("llm")))
            .header("content-type", "application/json")
            .header("x-playbox-run-id", "SPOOFED-RUN")
            .header("x-playbox-cache-mode", "replay")
            .body(r#"{"model":"claude-sonnet-4-6"}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), 402);
        assert_eq!(
            r.headers()
                .get("x-playbox-stop-reason")
                .and_then(|v| v.to_str().ok()),
            Some("budget_exhausted")
        );

        let got = seen_headers.lock().unwrap().clone();
        assert!(got.contains(&"authorization:Bearer pbx-worker-tok-123".to_string()));
        assert!(got.contains(&"x-playbox-room-id:room-alpha".to_string()));
        assert!(got.contains(&"x-playbox-run-id:run-42".to_string()));
        assert!(got.contains(&"x-playbox-cache-mode:off".to_string()));
        assert!(!got.iter().any(|h| h.contains("SPOOFED-RUN")));
        assert!(!got.iter().any(|h| h.contains("replay")));

        let _ = std::fs::remove_dir_all(&sec_dir);
    }
}
