//! The Hindsight HTTP backend: a minimal HTTP/1.1 client and the retain, recall and delete calls.

use super::backend::{Backend, Item, RawHit, RecallRequest};
use super::MAX_RESPONSE_BYTES;
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// An unmodified Hindsight server (`ghcr.io/vectorize-io/hindsight`), reached over plain HTTP: run it beside the host.
#[derive(Debug, Clone)]
pub struct Hindsight {
    host: String,
    port: u16,
    base: String,
    pub timeout: Duration,
    /// Wait for the server to finish extracting and indexing before `remember` answers.
    pub wait: bool,
}

impl Hindsight {
    /// `url`: `http://127.0.0.1:8888/v1/default` (the part after the port is the tenant prefix).
    ///
    /// # Errors
    ///
    /// Returns an error if the URL is not a plain `http://host[:port][/prefix]` address.
    pub fn new(url: &str) -> Result<Self> {
        let rest = url.strip_prefix("http://").ok_or_else(|| {
            anyhow!("the memory server URL must start with http:// (it runs beside the host)")
        })?;
        let (hostport, path) = rest.split_once('/').map_or((rest, ""), |(a, b)| (a, b));
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (h, p.parse::<u16>().context("memory server port")?),
            None => (hostport, 80),
        };
        if host.is_empty() || host.contains(|c: char| c.is_whitespace() || c == '/') {
            bail!("bad memory server host");
        }
        Ok(Self {
            host: host.to_string(),
            port,
            base: if path.is_empty() {
                String::new()
            } else {
                format!("/{}", path.trim_end_matches('/'))
            },
            timeout: Duration::from_secs(120),
            wait: false,
        })
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Vec<u8>)> {
        let payload = body
            .map(serde_json::to_vec)
            .transpose()?
            .unwrap_or_default();
        let head = format!(
            "{method} {}{path} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            self.base,
            self.host,
            self.port,
            payload.len()
        );
        let run = async {
            let mut s = tokio::net::TcpStream::connect((self.host.as_str(), self.port))
                .await
                .context("connect to the memory server")?;
            let mut out = head.into_bytes();
            out.extend_from_slice(&payload);
            s.write_all(&out).await?;
            let mut buf = Vec::new();
            (&mut s)
                .take(MAX_RESPONSE_BYTES as u64 + 1)
                .read_to_end(&mut buf)
                .await?;
            if buf.len() > MAX_RESPONSE_BYTES {
                bail!("the memory server's answer is too large");
            }
            parse_response(&buf)
        };
        tokio::time::timeout(self.timeout, run)
            .await
            .map_err(|_| anyhow!("the memory server did not answer in time"))?
    }
}

pub(crate) fn parse_response(buf: &[u8]) -> Result<(u16, Vec<u8>)> {
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("malformed answer from the memory server"))?;
    let head = std::str::from_utf8(&buf[..split]).context("answer header")?;
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .unwrap_or("")
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("malformed status line"))?;
    let chunked = lines.any(|l| {
        l.to_ascii_lowercase()
            .replace(' ', "")
            .starts_with("transfer-encoding:chunked")
    });
    let body = &buf[split + 4..];
    if !chunked {
        return Ok((status, body.to_vec()));
    }
    let (mut out, mut rest) = (Vec::new(), body);
    loop {
        let eol = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| anyhow!("malformed chunked answer"))?;
        let size_text = std::str::from_utf8(&rest[..eol])?
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        let size = usize::from_str_radix(size_text, 16).context("chunk size")?;
        rest = &rest[eol + 2..];
        if size == 0 {
            return Ok((status, out));
        }
        if rest.len() < size + 2 {
            bail!("truncated chunked answer");
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

#[async_trait]
impl Backend for Hindsight {
    async fn retain(&self, bank: &str, item: &Item) -> Result<()> {
        let body = json!({
            "items": [{
                "content": item.text,
                "document_id": item.doc,
                "metadata": item.metadata,
                "tags": item.tags,
                "observation_scopes": "per_tag",
                "context": "shared room memory",
            }],
            "async": !self.wait,
        });
        let (code, out) = self
            .request("POST", &format!("/banks/{bank}/memories"), Some(&body))
            .await?;
        if code != 200 {
            bail!(
                "the memory server refused the entry ({code}): {}",
                String::from_utf8_lossy(&out)
                    .chars()
                    .take(200)
                    .collect::<String>()
            );
        }
        Ok(())
    }

    async fn recall(&self, bank: &str, req: &RecallRequest) -> Result<Vec<RawHit>> {
        let body = json!({
            "query": req.query,
            "max_tokens": req.max_tokens,
            "tags": req.tags,
            "tags_match": "all_strict",
            "types": ["world", "experience"],
        });
        let (code, out) = self
            .request(
                "POST",
                &format!("/banks/{bank}/memories/recall"),
                Some(&body),
            )
            .await?;
        if code == 404 {
            return Ok(vec![]); // no entries yet, so no bank yet
        }
        if code != 200 {
            bail!("the memory server refused the query ({code})");
        }
        let v: Value = serde_json::from_slice(&out).context("recall answer")?;
        Ok(v.get("results")
            .and_then(Value::as_array)
            .map(|rs| {
                rs.iter()
                    .map(|r| RawHit {
                        doc: r
                            .get("document_id")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        text: r
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        tags: r
                            .get("tags")
                            .and_then(Value::as_array)
                            .map(|t| {
                                t.iter()
                                    .filter_map(|x| x.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        kind: r.get("type").and_then(Value::as_str).map(str::to_string),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn delete(&self, bank: &str, doc: &str) -> Result<()> {
        let (code, out) = self
            .request("DELETE", &format!("/banks/{bank}/documents/{doc}"), None)
            .await?;
        if code == 200 || code == 204 || code == 404 {
            return Ok(());
        }
        bail!(
            "the memory server would not delete the entry ({code}): {}",
            String::from_utf8_lossy(&out)
                .chars()
                .take(200)
                .collect::<String>()
        )
    }
}

// ---- an in-process index, for tests and for rooms with no memory server ----
