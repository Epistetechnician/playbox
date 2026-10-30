//! Runs the real Infinity local runtime against a running `playbox-rap`. Only the model is scripted
//! (no credentials were provided); the runtime, RAP tool client and callback bridge are Infinity's.
//! The HTTP client adds a bearer token, which Infinity's stock `SimpleHttpClient` cannot do.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use infinity_agent_core::stores::{InMemoryConversationStore, InMemoryStateStore};
use infinity_agent_core::system::{AgentEvent, AgentSystemBuilder, StaticModel};
use infinity_agent_core::tools::rap_tool::RapTool;
use infinity_provider_protocol::completion::{
    CompletionError, CompletionRequest, FinalResponse, ModelStream, StreamChunk, Usage,
};
use infinity_provider_protocol::message::{Message, ToolCall, UserContent};
use infinity_provider_protocol::{ModelEntry, SingleModelProvider};
use infinity_rap_bridge::RapCallbackBridge;
use rap_client::http::{HttpClient, InMemoryToolsetCache};
use rap_client::toolset_loader::ToolsetLoader;

#[derive(Debug)]
struct HttpError(String);
impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for HttpError {}

#[derive(Clone)]
struct BearerClient {
    client: reqwest::Client,
    token: String,
}

#[async_trait]
impl HttpClient for BearerClient {
    type Error = HttpError;

    async fn post(&self, url: &str, body: &str) -> Result<u16, HttpError> {
        let r = self
            .client
            .post(url)
            .bearer_auth(&self.token)
            .header("content-type", "application/json")
            .body(body.to_owned())
            .send()
            .await
            .map_err(|e| HttpError(e.to_string()))?;
        Ok(r.status().as_u16())
    }

    async fn get(&self, url: &str) -> Result<(u16, Vec<u8>), HttpError> {
        let r = self
            .client
            .get(url)
            .bearer_auth(&self.token)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| HttpError(e.to_string()))?;
        let status = r.status().as_u16();
        Ok((status, r.bytes().await.map_err(|e| HttpError(e.to_string()))?.to_vec()))
    }
}

/// Create the room, subscribe to it, say something, then wait for the event to come back.
#[derive(Clone)]
struct Script {
    repo: String,
}

fn tool_results(req: &CompletionRequest) -> usize {
    req.chat_history
        .iter()
        .map(|m| match m {
            Message::User { content } => content.iter().filter(|c| matches!(c, UserContent::ToolResult(_))).count(),
            _ => 0,
        })
        .sum()
}

#[async_trait]
impl infinity_provider_protocol::CompletionModel for Script {
    async fn stream(&self, req: CompletionRequest) -> Result<ModelStream, CompletionError> {
        let step = tool_results(&req);
        let call = |name: &str, args: serde_json::Value| {
            StreamChunk::ToolCall(ToolCall::new(uuid::Uuid::new_v4().to_string(), name, args))
        };
        let mut chunks = vec![];
        match step {
            0 => {
                chunks.push(StreamChunk::Text("Creating a Playbox room.".into()));
                chunks.push(call("playbox_create", serde_json::json!({"repo": self.repo, "local": true})));
            }
            1 => chunks.push(call("playbox_subscribe", serde_json::json!({"interval_ms": 500}))),
            2 => chunks.push(call("playbox_say", serde_json::json!({"text": "hello from an Infinity agent"}))),
            _ => chunks.push(StreamChunk::Text("Done; waiting for room events.".into())),
        }
        chunks.push(StreamChunk::Final(FinalResponse { usage: Some(Usage::default()) }));
        Ok(Box::pin(futures_util::stream::iter(chunks.into_iter().map(Ok))))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let base = std::env::var("RAP_URL").expect("RAP_URL");
    let token = std::env::var("RAP_TOKEN").expect("RAP_TOKEN");
    let repo = std::env::var("REPO").expect("REPO");
    let http = BearerClient { client: reqwest::Client::new(), token };

    tokio::task::LocalSet::new()
        .run_until(async move {
            let bridge = RapCallbackBridge::bind().await.expect("bind callback");
            let callback_url = bridge.callback_url().to_owned();
            let session: rap_protocol::ThreadId = "playbox-session".into();
            let loaded = ToolsetLoader::new(http.clone(), InMemoryToolsetCache::new())
                .load_toolsets(&[base.clone()], &session)
                .await
                .expect("discover toolset");
            let mut tools: Vec<Box<dyn infinity_agent_core::tools::Tool<_>>> = Vec::new();
            for set in loaded {
                eprintln!("discovered toolset {:?} with {} tools", set.manifest.name, set.manifest.tools.len());
                let endpoint = set.manifest.endpoint;
                for def in set.manifest.tools {
                    tools.push(Box::new(RapTool {
                        descriptor: def.into(),
                        endpoint: endpoint.clone(),
                        http_client: http.clone(),
                        callback_url: Some(callback_url.clone()),
                    }));
                }
            }

            let entry = ModelEntry {
                model_id: "scripted".into(),
                display_name: "scripted".into(),
                context_window: 0,
                max_output_tokens: None,
                supports_image_input: false,
            };
            let provider = Arc::new(SingleModelProvider::new(entry.clone(), Script { repo }));
            let system = AgentSystemBuilder::new_local(
                InMemoryConversationStore::new(),
                InMemoryStateStore::new(),
                StaticModel::from_entry(provider, &entry),
            )
            .tools(tools)
            .start();
            let (_views, _task) = bridge.serve_into(system.sender());

            let mut thread = system.thread_builder().launch().await;
            thread.send_user_text("Start a Playbox room and tell me what happens in it.").await.expect("send");
            let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
            let mut saw_event = false;
            while let Ok(Some(event)) = tokio::time::timeout_at(deadline, thread.recv()).await {
                match &event {
                    AgentEvent::ToolCall { name, .. } => println!("model called: {name}"),
                    AgentEvent::ToolResult { segments } => println!("tool result: {segments:?}"),
                    AgentEvent::SubscriptionEvent { name, text } => {
                        println!("subscription event [{name}]: {text}");
                        saw_event |= text.contains("hello from an Infinity agent");
                    }
                    AgentEvent::TextChunk { text } => println!("model: {text}"),
                    other => println!("event: {other:?}"),
                }
                if saw_event {
                    break;
                }
            }
            println!("{}", if saw_event { "RESULT: ok" } else { "RESULT: no room event arrived" });
        })
        .await;
}
