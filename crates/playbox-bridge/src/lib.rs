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
        methods::SEND_MESSAGE | methods::GET_TASK | methods::LIST_TASKS | methods::CANCEL_TASK | methods::GET_EXTENDED_AGENT_CARD
    )
}

fn eq_const_time(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() != b.len()) as u8;
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

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Bearer")], "missing or wrong bearer token\n").into_response()
}

async fn card(State(b): State<Arc<Bridge>>) -> Response {
    // The card is public like any A2A card: it reveals what the room offers, not who is in it.
    let mut card = match b.client.agent_card().await {
        Ok(c) => c,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("room unreachable: {e:#}\n")).into_response(),
    };
    card.supported_interfaces = vec![AgentInterface::new(format!("{}/jsonrpc", b.base_url), "JSONRPC")];
    card.description = format!("{} (HTTP bridge)", card.description);
    let mut v = serde_json::to_value(&card).unwrap_or(Value::Null);
    if b.token.is_some() {
        v["securitySchemes"] = json!({"bridge": {"httpAuthSecurityScheme": {"scheme": "Bearer", "description": "Token set when the bridge was started."}}});
        v["securityRequirements"] = json!([{"schemes": {"bridge": {"list": []}}}]);
    }
    Json(v).into_response()
}

async fn rpc(State(b): State<Arc<Bridge>>, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    if !authorized(&b, &headers) {
        return unauthorized();
    }
    let req: JsonRpcRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return Json(JsonRpcResponse::error(JsonRpcId::Null, A2AError::parse_error(e.to_string()).to_jsonrpc_error())).into_response(),
    };
    let id = req.id.clone();
    if !forwarded(&req.method) {
        let e = if methods::is_valid(&req.method) {
            A2AError::unsupported_operation(format!("{} is not supported by this room", req.method))
        } else {
            A2AError::new(a2a::error_code::METHOD_NOT_FOUND, format!("method not found: {}", req.method))
        };
        return Json(JsonRpcResponse::error(id, e.to_jsonrpc_error())).into_response();
    }
    match b.client.call_raw(&req.method, req.params.unwrap_or(Value::Null)).await {
        Ok(mut resp) => {
            resp.id = id;
            Json(resp).into_response()
        }
        Err(e) => Json(JsonRpcResponse::error(id, A2AError::internal(format!("room unreachable: {e:#}")).to_jsonrpc_error())).into_response(),
    }
}

/// Serve the bridge on `listener` until the task is dropped. `public_url` is how callers reach it
/// (it goes into the agent card), e.g. `http://127.0.0.1:8787`.
pub async fn serve(client: Client, listener: TcpListener, token: Option<String>, public_url: String) -> Result<()> {
    let addr = listener.local_addr()?;
    if !addr.ip().is_loopback() && token.is_none() {
        bail!("refusing to listen on {addr} without a bearer token: anyone who can reach it would act as the bridge's room member");
    }
    let state = Arc::new(Bridge { client, token, base_url: public_url.trim_end_matches('/').to_string() });
    let app = Router::new()
        .route("/.well-known/agent-card.json", get(card))
        .route("/jsonrpc", post(rpc))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state);
    axum::serve(listener, app).await?;
    Ok(())
}

pub async fn bind(addr: &str) -> Result<(TcpListener, SocketAddr)> {
    let l = TcpListener::bind(addr).await?;
    let a = l.local_addr()?;
    Ok((l, a))
}
