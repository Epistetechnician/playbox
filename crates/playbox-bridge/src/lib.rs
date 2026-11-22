//! HTTP bridge: lets stock A2A clients (HTTP + JSON-RPC) talk to a Playbox room.
//!
//! The bridge joins the room once, as a single member, and forwards each HTTP call over iroh with
//! that member's grant. Everything HTTP callers do is therefore done *as that member*, within its
//! grant; callers share its tasks. Protect the bridge accordingly:
//!
//! - it binds loopback by default and refuses a non-loopback address without a bearer token;
//! - only standard A2A methods are forwarded; `playbox.Join` is never reachable over HTTP;
//! - the bridge's own grant expires like any other (default 1h); restart it with a fresh link then.

use a2a::{methods, A2AError, AgentInterface, JsonRpcId, JsonRpcRequest, JsonRpcResponse};
use anyhow::{bail, Result};
use axum::{
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use playbox_net::Client;
use serde_json::{json, Value};
use std::{net::SocketAddr, sync::Arc};
use tokio::{net::TcpListener, sync::RwLock};

const BODY_LIMIT: usize = 64 * 1024 * 1024;

struct Bridge {
    client: RwLock<Arc<Client>>,
    token: Option<String>,
    base_url: String,
}

impl Bridge {
    /// The room connection, reopened first if the host closed it (it does after 15 idle minutes).
    async fn client(&self) -> Result<Arc<Client>> {
        let c = self.client.read().await.clone();
        if !c.is_closed() {
            return Ok(c);
        }
        let mut w = self.client.write().await;
        // Another request may have reconnected while this one waited for the lock.
        if w.is_closed() {
            *w = Arc::new(w.reconnect().await?);
        }
        Ok(w.clone())
    }
}

/// Methods HTTP callers may use.
fn forwarded(method: &str) -> bool {
    matches!(
        method,
        methods::SEND_MESSAGE
            | methods::GET_TASK
            | methods::LIST_TASKS
            | methods::CANCEL_TASK
            | methods::GET_EXTENDED_AGENT_CARD
    )
}

fn eq_const_time(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = u8::from(a.len() != b.len());
    for i in 0..a.len().max(b.len()) {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    diff == 0
}

fn authorized(b: &Bridge, h: &HeaderMap) -> bool {
    let Some(want) = &b.token else { return true };
    h.get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|got| eq_const_time(got, want))
}

/// Whether `Host` names this machine's loopback. Browsers always send it; plain clients may not.
/// Fails closed on non-UTF-8, duplicate `Host` headers, or invalid ports.
fn host_is_loopback(h: &HeaderMap) -> bool {
    let mut vals = h.get_all(header::HOST).iter();
    let Some(first) = vals.next() else {
        return true;
    };
    if vals.next().is_some() {
        return false;
    }
    let Ok(v) = first.to_str() else {
        return false;
    };
    let name = if let Some(rest) = v.strip_prefix('[') {
        let Some((addr, after)) = rest.split_once(']') else {
            return false;
        };
        if !after.is_empty() {
            let Some(port) = after.strip_prefix(':') else {
                return false;
            };
            if port.parse::<u16>().is_err() {
                return false;
            }
        }
        format!("[{addr}]")
    } else if let Some((n, port)) = v.rsplit_once(':') {
        if port.parse::<u16>().is_err() {
            return false;
        }
        n.to_string()
    } else {
        v.to_string()
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "[::1]"
    )
}

/// Reject browser Fetch Metadata indicating a cross-site or same-site (non-same-origin) request.
fn sec_fetch_site_allowed(h: &HeaderMap) -> bool {
    let mut vals = h.get_all("sec-fetch-site").iter();
    let Some(first) = vals.next() else {
        return true;
    };
    if vals.next().is_some() {
        return false;
    }
    let Ok(v) = first.to_str() else {
        return false;
    };
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "same-origin" | "none"
    )
}

/// Protect a bridge that has no token from the web pages in the user's own browser.
///
/// - **Cross-site requests:** a page can POST to `http://127.0.0.1:port` without any permission, as a
///   "simple" request. Browsers always attach `Origin` (and `Sec-Fetch-Site`) to those, and ordinary
///   clients (curl, SDKs) never do, so any request with an `Origin` or cross-site `Sec-Fetch-Site` is refused.
/// - **DNS rebinding:** a page can make its own hostname resolve to 127.0.0.1 and then talk to us as
///   "same origin". The request still carries the attacker's hostname in `Host`, so only loopback names pass.
///
/// With a token neither matters: a page would have to know the token.
fn browser_guard(b: &Bridge, h: &HeaderMap) -> Option<Response> {
    if b.token.is_some() {
        return None;
    }
    if h.contains_key(header::ORIGIN) || !sec_fetch_site_allowed(h) {
        return Some(
            (
                StatusCode::FORBIDDEN,
                "browser requests need a bridge token (--token)\n",
            )
                .into_response(),
        );
    }
    if !host_is_loopback(h) {
        return Some(
            (
                StatusCode::FORBIDDEN,
                "unexpected Host header for a loopback bridge\n",
            )
                .into_response(),
        );
    }
    None
}

fn json_content_type(h: &HeaderMap) -> bool {
    let ct = h
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let essence = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(
        essence.as_str(),
        "application/json" | "application/a2a+json"
    )
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        "missing or wrong bearer token\n",
    )
        .into_response()
}

async fn card(State(b): State<Arc<Bridge>>, headers: HeaderMap) -> Response {
    if let Some(refusal) = browser_guard(&b, &headers) {
        return refusal;
    }
    // The card is public like any A2A card: it reveals what the room offers, not who is in it.
    let card = match b.client().await {
        Ok(c) => c.agent_card().await,
        Err(e) => Err(e),
    };
    let mut card = match card {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("room unreachable: {e:#}\n"),
            )
                .into_response()
        }
    };
    card.supported_interfaces = vec![AgentInterface::new(
        format!("{}/jsonrpc", b.base_url),
        "JSONRPC",
    )];
    card.description = format!("{} (HTTP bridge)", card.description);
    let mut v = serde_json::to_value(&card).unwrap_or(Value::Null);
    if b.token.is_some() {
        v["securitySchemes"] = json!({"bridge": {"httpAuthSecurityScheme": {"scheme": "Bearer", "description": "Token set when the bridge was started."}}});
        v["securityRequirements"] = json!([{"schemes": {"bridge": {"list": []}}}]);
    }
    Json(v).into_response()
}

async fn rpc(
    State(b): State<Arc<Bridge>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(refusal) = browser_guard(&b, &headers) {
        return refusal;
    }
    if !authorized(&b, &headers) {
        return unauthorized();
    }
    if !json_content_type(&headers) {
        // Also what makes a browser send a CORS preflight, which this server never approves.
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "send Content-Type: application/json\n",
        )
            .into_response();
    }
    let req: JsonRpcRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return Json(JsonRpcResponse::error(
                JsonRpcId::Null,
                A2AError::parse_error(e.to_string()).to_jsonrpc_error(),
            ))
            .into_response()
        }
    };
    let id = req.id.clone();
    if !forwarded(&req.method) {
        let e = if methods::is_valid(&req.method) {
            A2AError::unsupported_operation(format!("{} is not supported by this room", req.method))
        } else {
            A2AError::new(
                a2a::error_code::METHOD_NOT_FOUND,
                format!("method not found: {}", req.method),
            )
        };
        return Json(JsonRpcResponse::error(id, e.to_jsonrpc_error())).into_response();
    }
    let sent = match b.client().await {
        Ok(c) => {
            c.call_raw(&req.method, req.params.unwrap_or(Value::Null))
                .await
        }
        Err(e) => Err(e),
    };
    match sent {
        Ok(mut resp) => {
            resp.id = id;
            Json(resp).into_response()
        }
        Err(e) => Json(JsonRpcResponse::error(
            id,
            A2AError::internal(format!("room unreachable: {e:#}")).to_jsonrpc_error(),
        ))
        .into_response(),
    }
}

/// Serve the bridge on `listener` until the task is dropped. `public_url` is how callers reach it
/// (it goes into the agent card), e.g. `http://127.0.0.1:8787`.
///
/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
pub async fn serve(
    client: Client,
    listener: TcpListener,
    token: Option<String>,
    public_url: String,
) -> Result<()> {
    if token.as_deref().is_some_and(|t| t.trim().is_empty()) {
        bail!("bearer token must not be empty or whitespace");
    }
    let addr = listener.local_addr()?;
    if !addr.ip().is_loopback() && token.is_none() {
        bail!("refusing to listen on {addr} without a bearer token: anyone who can reach it would act as the bridge's room member");
    }
    let state = Arc::new(Bridge {
        client: RwLock::new(Arc::new(client)),
        token,
        base_url: public_url.trim_end_matches('/').to_string(),
    });
    let app = Router::new()
        .route("/.well-known/agent-card.json", get(card))
        .route("/jsonrpc", post(rpc))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state);
    axum::serve(listener, app).await?;
    Ok(())
}

/// # Errors
///
/// Returns an error if the operation fails.
pub async fn bind(addr: &str) -> Result<(TcpListener, SocketAddr)> {
    let l = TcpListener::bind(addr).await?;
    let a = l.local_addr()?;
    Ok((l, a))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn host_guard_fails_closed_on_non_utf8_duplicate_and_malformed_hosts() {
        // Non-UTF-8 Host header must not fail open.
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_bytes(b"\xff\xfe").unwrap());
        assert!(!host_is_loopback(&h));

        // Duplicate Host headers (even if first is loopback) must be rejected.
        let mut dup = HeaderMap::new();
        dup.append(header::HOST, HeaderValue::from_static("127.0.0.1:8080"));
        dup.append(header::HOST, HeaderValue::from_static("evil.example"));
        assert!(!host_is_loopback(&dup));

        // Malformed / overflowing ports must be rejected.
        for bad in [
            "localhost:80@evil.com",
            "localhost:99999",
            "[::1]:bad",
            "[::1]extra",
            "evil.example:8080",
        ] {
            let mut m = HeaderMap::new();
            m.insert(header::HOST, HeaderValue::from_str(bad).unwrap());
            assert!(!host_is_loopback(&m), "must reject Host: {bad}");
        }

        // Valid loopback hosts still pass.
        for ok in [
            "localhost",
            "localhost:8080",
            "127.0.0.1",
            "127.0.0.1:3000",
            "[::1]",
            "[::1]:3000",
        ] {
            let mut m = HeaderMap::new();
            m.insert(header::HOST, HeaderValue::from_str(ok).unwrap());
            assert!(host_is_loopback(&m), "must accept Host: {ok}");
        }
    }

    #[test]
    fn sec_fetch_site_blocks_cross_site_and_non_utf8() {
        assert!(sec_fetch_site_allowed(&HeaderMap::new()));

        for ok in ["same-origin", "none", "SAME-ORIGIN"] {
            let mut h = HeaderMap::new();
            h.insert("sec-fetch-site", HeaderValue::from_str(ok).unwrap());
            assert!(sec_fetch_site_allowed(&h), "must accept {ok}");
        }

        for bad in ["cross-site", "same-site", ""] {
            let mut h = HeaderMap::new();
            h.insert("sec-fetch-site", HeaderValue::from_str(bad).unwrap());
            assert!(!sec_fetch_site_allowed(&h), "must reject {bad}");
        }

        let mut raw = HeaderMap::new();
        raw.insert("sec-fetch-site", HeaderValue::from_bytes(b"\xff").unwrap());
        assert!(!sec_fetch_site_allowed(&raw));
    }
}
