package openai

import (
	"bytes"
	"net/http"
	"testing"

	"github.com/bytedance/sonic"
	"github.com/labring/aiproxy/core/relay/meta"
	relaymodel "github.com/labring/aiproxy/core/relay/model"
)

func TestGeminiThoughtSignatureRoundTripAndMaxTokensClamp(t *testing.T) {
	sig := recordToolCallThoughtSig("toolu_vertex_1", &relaymodel.ExtraContent{
		Google: &relaymodel.GoogleExtraContent{ThoughtSignature: "sig_gemini3_abc"},
	})
	if sig != "sig_gemini3_abc" {
		t.Fatalf("unexpected recorded signature: %q", sig)
	}

	// Simulate an Anthropic SDK client stripping `signature` on `tool_use` in turn 2.
	claudeBody := `{
		"model": "google/gemini-3-flash-preview",
		"max_tokens": 128000,
		"messages": [
			{"role": "user", "content": "run ls"},
			{"role": "assistant", "content": [
				{"type": "tool_use", "id": "toolu_vertex_1", "name": "bash", "input": {"cmd": "ls"}}
			]},
			{"role": "user", "content": [
				{"type": "tool_result", "tool_use_id": "toolu_vertex_1", "content": "ok"}
			]}
		]
	}`
	req, err := http.NewRequest(http.MethodPost, "/v1/messages", bytes.NewBufferString(claudeBody))
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Content-Type", "application/json")
	m := &meta.Meta{ActualModel: "google/gemini-3-flash-preview", OriginModel: "google/gemini-3-flash-preview"}

	oaiReq, err := ConvertClaudeRequestModel(m, req)
	if err != nil {
		t.Fatalf("ConvertClaudeRequestModel failed: %v", err)
	}
	if oaiReq.MaxTokens != 65536 {
		t.Fatalf("expected MaxTokens clamped to 65536, got %d", oaiReq.MaxTokens)
	}
	var assistantMsg *relaymodel.Message
	for i := range oaiReq.Messages {
		if oaiReq.Messages[i].Role == relaymodel.RoleAssistant {
			assistantMsg = &oaiReq.Messages[i]
			break
		}
	}
	if assistantMsg == nil || len(assistantMsg.ToolCalls) != 1 {
		t.Fatalf("expected 1 assistant tool call, got %+v", assistantMsg)
	}
	gotSig := ""
	if assistantMsg.ToolCalls[0].ExtraContent != nil && assistantMsg.ToolCalls[0].ExtraContent.Google != nil {
		gotSig = assistantMsg.ToolCalls[0].ExtraContent.Google.ThoughtSignature
	}
	if gotSig != "sig_gemini3_abc" {
		t.Fatalf("expected restored thought_signature sig_gemini3_abc, got %q", gotSig)
	}

	// Also verify OpenAI /v1/chat/completions path restores cached signature and clamps max_tokens.
	oaiBody := `{
		"model": "google/gemini-3-flash-preview",
		"max_tokens": 100000,
		"messages": [
			{"role": "user", "content": "hi"},
			{"role": "assistant", "tool_calls": [{"id": "toolu_vertex_1", "type": "function", "function": {"name": "bash", "arguments": "{}"}}]},
			{"role": "tool", "tool_call_id": "toolu_vertex_1", "content": "ok"}
		]
	}`
	oaiHTTPReq, err := http.NewRequest(http.MethodPost, "/v1/chat/completions", bytes.NewBufferString(oaiBody))
	if err != nil {
		t.Fatal(err)
	}
	oaiHTTPReq.Header.Set("Content-Type", "application/json")
	conv, err := ConvertChatCompletionsRequest(m, oaiHTTPReq, false)
	if err != nil {
		t.Fatalf("ConvertChatCompletionsRequest failed: %v", err)
	}
	buf := new(bytes.Buffer)
	_, _ = buf.ReadFrom(conv.Body)
	var parsed relaymodel.GeneralOpenAIRequest
	if err := sonic.Unmarshal(buf.Bytes(), &parsed); err != nil {
		t.Fatalf("unmarshal converted OpenAI request: %v", err)
	}
	if parsed.MaxTokens != 65536 {
		t.Fatalf("expected OpenAI MaxTokens clamped to 65536, got %d", parsed.MaxTokens)
	}
	if parsed.Messages[1].ToolCalls[0].ExtraContent == nil ||
		parsed.Messages[1].ToolCalls[0].ExtraContent.Google == nil ||
		parsed.Messages[1].ToolCalls[0].ExtraContent.Google.ThoughtSignature != "sig_gemini3_abc" {
		t.Fatalf("expected OpenAI ExtraContent restored, got %+v", parsed.Messages[1].ToolCalls[0].ExtraContent)
	}
}
