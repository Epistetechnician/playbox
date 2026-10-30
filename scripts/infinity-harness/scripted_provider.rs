//! A model provider process (like `infinity-provider-bedrock`) whose "model" follows a fixed script:
//! create a Playbox room, subscribe to it, say hello, then report the room event that comes back.
//! Used to run the stock Infinity daemon end to end when no real model is available.
//! `REPO` names the git repository to share.

use std::io::Write;
use std::sync::Arc;

use async_trait::async_trait;
use infinity_provider_protocol::completion::{
    CompletionError, CompletionRequest, FinalResponse, ModelStream, StreamChunk, Usage,
};
use infinity_provider_protocol::message::{Message, ToolCall, ToolResultContent, UserContent};
use infinity_provider_protocol::remote::serve_provider;
use infinity_provider_protocol::{ModelEntry, SingleModelProvider};

#[derive(Clone)]
struct Script {
    repo: String,
}

fn tool_results(req: &CompletionRequest) -> Vec<String> {
    let mut out = vec![];
    for m in &req.chat_history {
        if let Message::User { content } = m {
            for c in content.iter() {
                if let UserContent::ToolResult(r) = c {
                    let text = r
                        .content
                        .iter()
                        .filter_map(|t| match t {
                            ToolResultContent::Text(t) => Some(t.text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    out.push(text);
                }
            }
        }
    }
    out
}

#[async_trait]
impl infinity_provider_protocol::CompletionModel for Script {
    async fn stream(&self, req: CompletionRequest) -> Result<ModelStream, CompletionError> {
        let results = tool_results(&req);
        let call = |name: &str, args: serde_json::Value| {
            StreamChunk::ToolCall(ToolCall::new(uuid::Uuid::new_v4().to_string(), name, args))
        };
        let mut chunks = vec![];
        match results.len() {
            0 => {
                chunks.push(StreamChunk::Text("Creating a Playbox room.".into()));
                chunks.push(call("playbox_create", serde_json::json!({"repo": self.repo, "local": true})));
            }
            1 => chunks.push(call("playbox_subscribe", serde_json::json!({"interval_ms": 500}))),
            2 => chunks.push(call("playbox_say", serde_json::json!({"text": "hello from Infinity"}))),
            3 => chunks.push(StreamChunk::Text("Said hello; waiting for the room's event.".into())),
            _ => {
                let last = results.last().cloned().unwrap_or_default();
                chunks.push(StreamChunk::Text(format!("GOT EVENT: {last}")));
            }
        }
        chunks.push(StreamChunk::Final(FinalResponse { usage: Some(Usage::default()) }));
        Ok(Box::pin(futures_util::stream::iter(chunks.into_iter().map(Ok))))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let repo = std::env::var("REPO")?;
    let entry = ModelEntry {
        model_id: "scripted".into(),
        display_name: "scripted".into(),
        context_window: 0,
        max_output_tokens: None,
        supports_image_input: false,
    };
    let provider = Arc::new(SingleModelProvider::new(entry, Script { repo }));
    let (socket_path, server) = serve_provider(provider)?;
    println!("{}", socket_path.display());
    std::io::stdout().flush()?;
    server.await;
    Ok(())
}
