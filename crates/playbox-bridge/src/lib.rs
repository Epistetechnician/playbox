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
use tokio::net::TcpListener;

const BODY_LIMIT: usize = 64 * 1024 * 1024;

struct Bridge {
    client: Client,
    token: Option<String>,
    base_url: String,
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
fn host_is_loopback(h: &HeaderMap) -> bool {
    let Some(v) = h.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let name = v
        .rsplit_once(':')
        .filter(|(_, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(v, |(n, _)| n);
    matches!(
        name.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "[::1]"
    )
}

/// Protect a bridge that has no token from the web pages in the user's own browser.
///
/// - **Cross-site requests:** a page can POST to `http://127.0.0.1:port` without any permission, as a
///   "simple" request. Browsers always attach `Origin` to those, and ordinary clients (curl, SDKs) never
///   do, so any request with an `Origin` is refused.
/// - **DNS rebinding:** a page can make its own hostname resolve to 127.0.0.1 and then talk to us as
///   "same origin". The request still carries the attacker's hostname in `Host`, so only loopback names pass.
///
/// With a token neither matters: a page would have to know the token.
fn browser_guard(b: &Bridge, h: &HeaderMap) -> Option<Response> {
    if b.token.is_some() {
        return None;
    }
    if h.contains_key(header::ORIGIN) {
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
    let mut card = match b.client.agent_card().await {
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
    match b
        .client
        .call_raw(&req.method, req.params.unwrap_or(Value::Null))
        .await
    {
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
    let addr = listener.local_addr()?;
    if !addr.ip().is_loopback() && token.is_none() {
        bail!("refusing to listen on {addr} without a bearer token: anyone who can reach it would act as the bridge's room member");
    }
    let state = Arc::new(Bridge {
        client,
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
