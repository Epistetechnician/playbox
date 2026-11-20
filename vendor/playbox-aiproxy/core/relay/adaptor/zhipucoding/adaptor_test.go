//nolint:testpackage
package zhipucoding

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/gin-gonic/gin"
	coremodel "github.com/labring/aiproxy/core/model"
	"github.com/labring/aiproxy/core/relay/meta"
	"github.com/labring/aiproxy/core/relay/mode"
)

func TestAdaptorSupportMode(t *testing.T) {
	adaptor := &Adaptor{}

	supportedModes := []mode.Mode{
		mode.ChatCompletions,
		mode.Completions,
		mode.Anthropic,
		mode.Gemini,
		mode.Responses,
	}
	for _, m := range supportedModes {
		if !adaptor.SupportMode(&meta.Meta{Mode: m}) {
			t.Fatalf("expected mode %s to be supported", m)
		}
	}

	unsupportedModes := []mode.Mode{
		mode.ResponsesGet,
		mode.ResponsesDelete,
		mode.ResponsesCancel,
		mode.ResponsesInputItems,
		mode.ResponsesCompact,
		mode.Embeddings,
		mode.AudioSpeech,
		mode.Rerank,
	}
	for _, m := range unsupportedModes {
		if adaptor.SupportMode(&meta.Meta{Mode: m}) {
			t.Fatalf("expected mode %s to be unsupported", m)
		}
	}
}

func TestAdaptorGetRequestURL(t *testing.T) {
	adaptor := &Adaptor{}
	channel := &coremodel.Channel{
		BaseURL: "https://open.bigmodel.cn",
	}

	tests := []struct {
		name string
		mode mode.Mode
		want string
	}{
		{
			name: "anthropic uses native anthropic endpoint",
			mode: mode.Anthropic,
			want: "https://open.bigmodel.cn/api/anthropic/v1/messages",
		},
		{
			name: "gemini uses coding chat completions",
			mode: mode.Gemini,
			want: "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
		},
		{
			name: "chat uses coding chat completions",
			mode: mode.ChatCompletions,
			want: "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
		},
		{
			name: "completions uses coding completions",
			mode: mode.Completions,
			want: "https://open.bigmodel.cn/api/coding/paas/v4/completions",
		},
		{
			name: "responses uses coding responses endpoint",
			mode: mode.Responses,
			want: "https://open.bigmodel.cn/api/v1/responses",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			m := meta.NewMeta(channel, tt.mode, "glm-5.1", coremodel.ModelConfig{})

			got, err := adaptor.GetRequestURL(m, nil, nil)
			if err != nil {
				t.Fatalf("GetRequestURL returned error: %v", err)
			}

			if got.Method != http.MethodPost {
				t.Fatalf("expected method %s, got %s", http.MethodPost, got.Method)
			}

			if got.URL != tt.want {
				t.Fatalf("expected URL %s, got %s", tt.want, got.URL)
			}

			if m.Mode != tt.mode {
				t.Fatalf("expected mode to remain %s, got %s", tt.mode, m.Mode)
			}

			if m.Channel.BaseURL != channel.BaseURL {
				t.Fatalf(
					"expected base URL to remain %s, got %s",
					channel.BaseURL,
					m.Channel.BaseURL,
				)
			}
		})
	}
}

func TestAdaptorGetRequestURLUnsupportedResponsesSubAPI(t *testing.T) {
	adaptor := &Adaptor{}
	m := meta.NewMeta(
		&coremodel.Channel{BaseURL: "https://open.bigmodel.cn"},
		mode.ResponsesGet,
		"glm-5.1",
		coremodel.ModelConfig{},
	)

	if _, err := adaptor.GetRequestURL(m, nil, nil); err == nil {
		t.Fatal("expected ResponsesGet mode to be unsupported")
	}
}

func TestAdaptorConvertRequestResponses(t *testing.T) {
	adaptor := &Adaptor{}
	m := meta.NewMeta(nil, mode.Responses, "glm-5.3", coremodel.ModelConfig{})

	req, err := http.NewRequestWithContext(
		context.Background(),
		http.MethodPost,
		"/v1/responses",
		strings.NewReader(`{
			"model":"model-alias",
			"input":[{"role":"system","content":"be concise"}],
			"stream":true
		}`),
	)
	if err != nil {
		t.Fatalf("failed to create request: %v", err)
	}

	result, err := adaptor.ConvertRequest(m, nil, req)
	if err != nil {
		t.Fatalf("ConvertRequest returned error: %v", err)
	}

	body, err := io.ReadAll(result.Body)
	if err != nil {
		t.Fatalf("failed to read converted body: %v", err)
	}

	var payload struct {
		Model  string `json:"model"`
		Stream bool   `json:"stream"`
		Input  []struct {
			Role string `json:"role"`
		} `json:"input"`
	}
	if err := json.Unmarshal(body, &payload); err != nil {
		t.Fatalf("failed to unmarshal converted body: %v", err)
	}

	if payload.Model != "glm-5.3" {
		t.Fatalf("expected mapped model glm-5.3, got %s", payload.Model)
	}

	if !payload.Stream {
		t.Fatal("expected stream to remain enabled")
	}

	if len(payload.Input) != 1 || payload.Input[0].Role != "developer" {
		t.Fatalf("expected system input role to normalize to developer, got %#v", payload.Input)
	}
}

func TestAdaptorDoResponseResponses(t *testing.T) {
	gin.SetMode(gin.TestMode)

	adaptor := &Adaptor{}
	m := meta.NewMeta(nil, mode.Responses, "glm-5.3", coremodel.ModelConfig{})
	recorder := httptest.NewRecorder()
	ctx, _ := gin.CreateTestContext(recorder)
	resp := &http.Response{
		StatusCode: http.StatusOK,
		Header: http.Header{
			"Content-Type": {"application/json"},
		},
		Body: io.NopCloser(strings.NewReader(`{
			"id":"resp_123",
			"object":"response",
			"status":"completed",
			"model":"glm-5.3",
			"output":[],
			"usage":{"input_tokens":2,"output_tokens":3,"total_tokens":5}
		}`)),
	}

	result, relayErr := adaptor.DoResponse(m, nil, ctx, resp)
	if relayErr != nil {
		t.Fatalf("DoResponse returned error: %v", relayErr)
	}

	if result.UpstreamID != "resp_123" {
		t.Fatalf("expected upstream response ID resp_123, got %s", result.UpstreamID)
	}

	if result.Usage.TotalTokens != 5 {
		t.Fatalf("expected total token usage 5, got %d", result.Usage.TotalTokens)
	}

	if recorder.Code != http.StatusOK {
		t.Fatalf("expected status %d, got %d", http.StatusOK, recorder.Code)
	}
}

func TestAdaptorDoResponseResponsesStream(t *testing.T) {
	gin.SetMode(gin.TestMode)

	adaptor := &Adaptor{}
	m := meta.NewMeta(nil, mode.Responses, "glm-5.3", coremodel.ModelConfig{})
	recorder := httptest.NewRecorder()
	ctx, _ := gin.CreateTestContext(recorder)
	ctx.Request = httptest.NewRequestWithContext(
		context.Background(),
		http.MethodPost,
		"/v1/responses",
		nil,
	)
	body := strings.Join([]string{
		"event: response.created",
		`data: {"type":"response.created","response":{"id":"resp_stream","object":"response","status":"in_progress","model":"glm-5.3","output":[],"store":false}}`,
		"",
		"event: response.completed",
		`data: {"type":"response.completed","response":{"id":"resp_stream","object":"response","status":"completed","model":"glm-5.3","output":[],"store":false,"usage":{"input_tokens":2,"output_tokens":3,"total_tokens":5}}}`,
		"",
	}, "\n")
	resp := &http.Response{
		StatusCode: http.StatusOK,
		Header: http.Header{
			"Content-Type": {"text/event-stream"},
		},
		Body: io.NopCloser(strings.NewReader(body)),
	}

	result, relayErr := adaptor.DoResponse(m, nil, ctx, resp)
	if relayErr != nil {
		t.Fatalf("DoResponse returned error: %v", relayErr)
	}

	if result.UpstreamID != "resp_stream" {
		t.Fatalf("expected upstream response ID resp_stream, got %s", result.UpstreamID)
	}

	if result.Usage.TotalTokens != 5 {
		t.Fatalf("expected total token usage 5, got %d", result.Usage.TotalTokens)
	}

	if !strings.Contains(recorder.Body.String(), `"type":"response.completed"`) {
		t.Fatalf("expected completed event, got %q", recorder.Body.String())
	}
}

func TestAdaptorDoResponseUsesZhipuErrorHandler(t *testing.T) {
	adaptor := &Adaptor{}
	m := meta.NewMeta(
		&coremodel.Channel{BaseURL: "https://open.bigmodel.cn"},
		mode.ChatCompletions,
		"glm-5.1",
		coremodel.ModelConfig{},
	)
	resp := &http.Response{
		StatusCode: http.StatusTooManyRequests,
		Body: io.NopCloser(
			//nolint:lll
			strings.NewReader(`{"error":{"code":"1113","message":"余额不足或无可用资源包,请充值。"}}`),
		),
	}

	_, err := adaptor.DoResponse(m, nil, nil, resp)
	if err == nil {
		t.Fatal("expected zhipu error")
	}

	if err.StatusCode() != http.StatusPaymentRequired {
		t.Fatalf("expected status %d, got %d", http.StatusPaymentRequired, err.StatusCode())
	}
}
