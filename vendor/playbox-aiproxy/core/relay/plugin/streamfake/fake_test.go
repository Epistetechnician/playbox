//nolint:testpackage
package streamfake

import (
	"bytes"
	"io"
	"net/http"
	"testing"

	"github.com/bytedance/sonic"
	coremodel "github.com/labring/aiproxy/core/model"
	"github.com/labring/aiproxy/core/relay/adaptor"
	"github.com/labring/aiproxy/core/relay/meta"
	"github.com/labring/aiproxy/core/relay/mode"
	"github.com/labring/aiproxy/core/relay/plugin/patch"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

type streamFakeConvertRequestStub struct {
	body []byte
}

func (s *streamFakeConvertRequestStub) ConvertRequest(
	_ *meta.Meta,
	_ adaptor.Store,
	req *http.Request,
) (adaptor.ConvertResult, error) {
	body, err := io.ReadAll(req.Body)
	if err != nil {
		return adaptor.ConvertResult{}, err
	}

	s.body = body

	return adaptor.ConvertResult{Body: bytes.NewReader(body)}, nil
}

func newStreamFakeTestMeta() *meta.Meta {
	return meta.NewMeta(nil, mode.ChatCompletions, "gpt-4.1", coremodel.ModelConfig{
		Model: "gpt-4.1",
		Plugin: map[string]map[string]any{
			"stream-fake": {"enable": true},
		},
	})
}

func TestConvertRequestEnablesFakeStreamForMultipleChoices(t *testing.T) {
	m := newStreamFakeTestMeta()
	req := httptestNewJSONRequest(t, `{"model":"gpt-4.1","messages":[],"n":2}`)
	stub := &streamFakeConvertRequestStub{}

	_, err := (&StreamFake{}).ConvertRequest(m, nil, req, stub)
	require.NoError(t, err)

	value, ok := m.Get(fakeStreamKey)
	require.True(t, ok)
	assert.Equal(t, true, value)
	assert.Len(t, patch.GetLazyPatches(m), 1)
}

func TestConvertRequestEnablesFakeStreamForSingleChoice(t *testing.T) {
	m := newStreamFakeTestMeta()
	req := httptestNewJSONRequest(t, `{"model":"gpt-4.1","messages":[]}`)
	stub := &streamFakeConvertRequestStub{}

	_, err := (&StreamFake{}).ConvertRequest(m, nil, req, stub)
	require.NoError(t, err)

	value, ok := m.Get(fakeStreamKey)
	require.True(t, ok)
	assert.Equal(t, true, value)
	assert.Len(t, patch.GetLazyPatches(m), 1)
}

func httptestNewJSONRequest(t *testing.T, body string) *http.Request {
	t.Helper()

	req, err := http.NewRequestWithContext(
		t.Context(),
		http.MethodPost,
		"/v1/chat/completions",
		bytes.NewReader([]byte(body)),
	)
	require.NoError(t, err)
	req.Header.Set("Content-Type", "application/json")

	return req
}

func TestConvertToNonStreamPreservesMultipleChoices(t *testing.T) {
	rw := &fakeStreamResponseWriter{}

	chunks := []string{
		`{"choices":[{"delta":{"role":"assistant","content":"A"},"finish_reason":null,"index":0},{"delta":{"role":"assistant","content":"B"},"finish_reason":null,"index":1}],"created":1767597874,"id":"chatcmpl-test","model":"gpt-4.1","object":"chat.completion.chunk"}`,
		`{"choices":[{"delta":{"content":" one"},"finish_reason":null,"index":0},{"delta":{"content":" two"},"finish_reason":null,"index":1}],"created":1767597874,"id":"chatcmpl-test","model":"gpt-4.1","object":"chat.completion.chunk"}`,
		`{"choices":[{"delta":{},"finish_reason":"stop","index":0},{"delta":{},"finish_reason":"length","index":1}],"created":1767597874,"id":"chatcmpl-test","model":"gpt-4.1","object":"chat.completion.chunk","usage":{"completion_tokens":4,"prompt_tokens":10,"total_tokens":14}}`,
	}

	for _, chunk := range chunks {
		err := rw.parseStreamingData([]byte(chunk))
		require.NoError(t, err)
	}

	result, err := rw.convertToNonStream()
	require.NoError(t, err)

	var response map[string]any

	err = sonic.Unmarshal(result, &response)
	require.NoError(t, err)

	choices, ok := response["choices"].([]any)
	require.True(t, ok)
	require.Len(t, choices, 2)

	choice0, ok := choices[0].(map[string]any)
	require.True(t, ok)
	assert.Equal(t, float64(0), choice0["index"])
	assert.Equal(t, "stop", choice0["finish_reason"])
	message0, ok := choice0["message"].(map[string]any)
	require.True(t, ok)
	assert.Equal(t, "A one", message0["content"])

	choice1, ok := choices[1].(map[string]any)
	require.True(t, ok)
	assert.Equal(t, float64(1), choice1["index"])
	assert.Equal(t, "length", choice1["finish_reason"])
	message1, ok := choice1["message"].(map[string]any)
	require.True(t, ok)
	assert.Equal(t, "B two", message1["content"])
}

func TestParseStreamingDataWithContentFilterFields(t *testing.T) {
	tests := []struct {
		name                      string
		chunks                    []string
		expectedContent           string
		hasPromptFilterResults    bool
		hasContentFilterResults   bool
		hasContentFilterResult    bool
		expectedFinishReason      string
		checkObfuscationPreserved bool
	}{
		{
			name: "Azure OpenAI stream with content filter fields",
			chunks: []string{
				`{"choices":[],"created":0,"id":"","model":"gpt-4.1-mini","object":"","prompt_filter_results":[{"prompt_index":0,"content_filter_results":{}}]}`,
				`{"choices":[{"content_filter_result":{"error":{"code":"content_filter_error","message":"The contents are not filtered"}},"content_filter_results":{},"delta":{"content":"","refusal":null,"role":"assistant"},"finish_reason":null,"index":0,"logprobs":null}],"created":1767597874,"id":"chatcmpl-test","model":"gpt-4.1-mini","obfuscation":"x","object":"chat.completion.chunk","system_fingerprint":"fp_test","usage":null}`,
				`{"choices":[{"content_filter_result":{"error":{"code":"content_filter_error","message":"The contents are not filtered"}},"delta":{"content":"Hello"},"finish_reason":null,"index":0,"logprobs":null}],"created":1767597874,"id":"chatcmpl-test","model":"gpt-4.1-mini","obfuscation":"abc123","object":"chat.completion.chunk","system_fingerprint":"fp_test","usage":null}`,
				`{"choices":[{"content_filter_result":{"error":{"code":"content_filter_error","message":"The contents are not filtered"}},"delta":{"content":" World"},"finish_reason":null,"index":0,"logprobs":null}],"created":1767597874,"id":"chatcmpl-test","model":"gpt-4.1-mini","obfuscation":"def456","object":"chat.completion.chunk","system_fingerprint":"fp_test","usage":null}`,
				`{"choices":[{"content_filter_result":{"error":{"code":"content_filter_error","message":"The contents are not filtered"}},"delta":{},"finish_reason":"stop","index":0,"logprobs":null}],"created":1767597874,"id":"chatcmpl-test","model":"gpt-4.1-mini","obfuscation":"ghi789","object":"chat.completion.chunk","system_fingerprint":"fp_test","usage":null}`,
				`{"choices":[],"created":1767597874,"id":"chatcmpl-test","model":"gpt-4.1-mini","obfuscation":"G","object":"chat.completion.chunk","system_fingerprint":"fp_test","usage":{"completion_tokens":2,"completion_tokens_details":{"accepted_prediction_tokens":0,"audio_tokens":0,"reasoning_tokens":0,"rejected_prediction_tokens":0},"prompt_tokens":886,"prompt_tokens_details":{"audio_tokens":0,"cached_tokens":0},"total_tokens":888}}`,
			},
			expectedContent:           "Hello World",
			hasPromptFilterResults:    true,
			hasContentFilterResults:   true,
			hasContentFilterResult:    true,
			expectedFinishReason:      "stop",
			checkObfuscationPreserved: true,
		},
		{
			name: "simple stream without filter fields",
			chunks: []string{
				`{"choices":[{"delta":{"content":"Hi"},"finish_reason":null,"index":0}],"created":1767597874,"id":"chatcmpl-test","model":"gpt-4","object":"chat.completion.chunk"}`,
				`{"choices":[{"delta":{},"finish_reason":"stop","index":0}],"created":1767597874,"id":"chatcmpl-test","model":"gpt-4","object":"chat.completion.chunk","usage":{"completion_tokens":1,"prompt_tokens":10,"total_tokens":11}}`,
			},
			expectedContent:         "Hi",
			hasPromptFilterResults:  false,
			hasContentFilterResults: false,
			hasContentFilterResult:  false,
			expectedFinishReason:    "stop",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			rw := &fakeStreamResponseWriter{}

			// Parse all chunks
			for _, chunk := range tt.chunks {
				err := rw.parseStreamingData([]byte(chunk))
				require.NoError(t, err)
			}

			state := rw.choices[0]
			require.NotNil(t, state)

			// Check content
			assert.Equal(t, tt.expectedContent, state.contentBuilder.String())

			// Check finish reason
			assert.Equal(t, tt.expectedFinishReason, state.finishReason)

			// Check prompt_filter_results
			if tt.hasPromptFilterResults {
				assert.NotNil(t, rw.promptFilterResults)
			} else {
				assert.Nil(t, rw.promptFilterResults)
			}

			// Check content_filter_results
			if tt.hasContentFilterResults {
				assert.NotNil(t, state.contentFilterResults)
			} else {
				assert.Nil(t, state.contentFilterResults)
			}

			// Check content_filter_result
			if tt.hasContentFilterResult {
				assert.NotNil(t, state.contentFilterResult)
			} else {
				assert.Nil(t, state.contentFilterResult)
			}

			// Convert to non-stream and verify
			result, err := rw.convertToNonStream()
			require.NoError(t, err)

			var response map[string]any

			err = sonic.Unmarshal(result, &response)
			require.NoError(t, err)

			// Check object type is changed to non-stream
			assert.Equal(t, "chat.completion", response["object"])

			// Check prompt_filter_results is preserved
			if tt.hasPromptFilterResults {
				assert.NotNil(t, response["prompt_filter_results"])
				promptFilters, ok := response["prompt_filter_results"].([]any)
				require.True(t, ok)
				assert.Len(t, promptFilters, 1)
			}

			// Check choices structure
			choices, ok := response["choices"].([]any)
			require.True(t, ok)
			require.Len(t, choices, 1)
			choice, ok := choices[0].(map[string]any)
			require.True(t, ok)

			// Check message content
			message, ok := choice["message"].(map[string]any)
			require.True(t, ok)
			assert.Equal(t, "assistant", message["role"])
			assert.Equal(t, tt.expectedContent, message["content"])

			// Check content_filter_results in choice
			if tt.hasContentFilterResults {
				assert.NotNil(t, choice["content_filter_results"])
			}

			// Check content_filter_result in choice
			if tt.hasContentFilterResult {
				assert.NotNil(t, choice["content_filter_result"])
				cfr, ok := choice["content_filter_result"].(map[string]any)
				require.True(t, ok)
				assert.NotNil(t, cfr["error"])
			}

			// Check obfuscation field is preserved (in lastChunk)
			if tt.checkObfuscationPreserved {
				assert.NotNil(t, response["obfuscation"])
			}

			// Check usage is preserved
			assert.NotNil(t, response["usage"])
		})
	}
}

func TestConvertToNonStreamWithAllFields(t *testing.T) {
	rw := &fakeStreamResponseWriter{}

	// Simulate parsing Azure OpenAI stream with all fields
	chunks := []string{
		`{"choices":[],"created":0,"id":"chatcmpl-test","model":"gpt-4.1-mini","object":"chat.completion.chunk","prompt_filter_results":[{"prompt_index":0,"content_filter_results":{"hate":{"filtered":false,"severity":"safe"},"self_harm":{"filtered":false,"severity":"safe"}}}]}`,
		`{"choices":[{"content_filter_result":{"error":{"code":"content_filter_error","message":"The contents are not filtered"}},"content_filter_results":{"hate":{"filtered":false,"severity":"safe"}},"delta":{"content":"Test","role":"assistant"},"finish_reason":null,"index":0}],"created":1767597874,"id":"chatcmpl-test","model":"gpt-4.1-mini","obfuscation":"xyz","object":"chat.completion.chunk","system_fingerprint":"fp_test"}`,
		`{"choices":[{"content_filter_result":{"error":{"code":"content_filter_error","message":"The contents are not filtered"}},"content_filter_results":{"hate":{"filtered":false,"severity":"safe"}},"delta":{},"finish_reason":"stop","index":0}],"created":1767597874,"id":"chatcmpl-test","model":"gpt-4.1-mini","obfuscation":"final","object":"chat.completion.chunk","system_fingerprint":"fp_test","usage":{"completion_tokens":1,"prompt_tokens":100,"total_tokens":101}}`,
	}

	for _, chunk := range chunks {
		err := rw.parseStreamingData([]byte(chunk))
		require.NoError(t, err)
	}

	result, err := rw.convertToNonStream()
	require.NoError(t, err)

	var response map[string]any

	err = sonic.Unmarshal(result, &response)
	require.NoError(t, err)

	// Verify response structure
	assert.Equal(t, "chat.completion", response["object"])
	assert.Equal(t, "chatcmpl-test", response["id"])
	assert.Equal(t, "gpt-4.1-mini", response["model"])
	assert.Equal(t, "final", response["obfuscation"]) // last obfuscation value preserved
	assert.Equal(t, "fp_test", response["system_fingerprint"])

	// Verify prompt_filter_results
	promptFilters, ok := response["prompt_filter_results"].([]any)
	require.True(t, ok)
	require.Len(t, promptFilters, 1)
	promptFilter, ok := promptFilters[0].(map[string]any)
	require.True(t, ok)
	assert.Equal(t, float64(0), promptFilter["prompt_index"])
	assert.NotNil(t, promptFilter["content_filter_results"])

	// Verify choices
	choices, ok := response["choices"].([]any)
	require.True(t, ok)
	require.Len(t, choices, 1)
	choice, ok := choices[0].(map[string]any)
	require.True(t, ok)

	// Verify content_filter_results in choice
	cfResults, ok := choice["content_filter_results"].(map[string]any)
	require.True(t, ok)
	assert.NotNil(t, cfResults["hate"])

	// Verify content_filter_result in choice
	cfResult, ok := choice["content_filter_result"].(map[string]any)
	require.True(t, ok)
	assert.NotNil(t, cfResult["error"])

	// Verify message
	message, ok := choice["message"].(map[string]any)
	require.True(t, ok)
	assert.Equal(t, "assistant", message["role"])
	assert.Equal(t, "Test", message["content"])

	// Verify finish_reason
	assert.Equal(t, "stop", choice["finish_reason"])

	// Verify usage
	usage, ok := response["usage"].(map[string]any)
	require.True(t, ok)
	assert.Equal(t, float64(1), usage["completion_tokens"])
	assert.Equal(t, float64(100), usage["prompt_tokens"])
	assert.Equal(t, float64(101), usage["total_tokens"])
}

func TestParseStreamingDataWithAudioDelta(t *testing.T) {
	rw := &fakeStreamResponseWriter{}

	chunks := []string{
		`{"choices":[{"delta":{"role":"assistant","audio":{"id":"audio-","data":"AAAA","transcript":"hel","expires_at":100}},"finish_reason":null,"index":0}],"created":1767597874,"id":"chatcmpl-test","model":"gpt-4o-audio-preview","object":"chat.completion.chunk"}`,
		`{"choices":[{"delta":{"audio":{"id":"test","data":"BBBB","transcript":"lo","expires_at":200}},"finish_reason":null,"index":0}],"created":1767597874,"id":"chatcmpl-test","model":"gpt-4o-audio-preview","object":"chat.completion.chunk"}`,
		`{"choices":[{"delta":{},"finish_reason":"stop","index":0}],"created":1767597874,"id":"chatcmpl-test","model":"gpt-4o-audio-preview","object":"chat.completion.chunk","usage":{"completion_tokens":10,"completion_tokens_details":{"audio_tokens":8},"prompt_tokens":2,"total_tokens":12}}`,
	}

	for _, chunk := range chunks {
		err := rw.parseStreamingData([]byte(chunk))
		require.NoError(t, err)
	}

	result, err := rw.convertToNonStream()
	require.NoError(t, err)

	var response map[string]any

	err = sonic.Unmarshal(result, &response)
	require.NoError(t, err)

	choices, ok := response["choices"].([]any)
	require.True(t, ok)
	require.Len(t, choices, 1)

	choice, ok := choices[0].(map[string]any)
	require.True(t, ok)

	message, ok := choice["message"].(map[string]any)
	require.True(t, ok)

	audio, ok := message["audio"].(map[string]any)
	require.True(t, ok)

	assert.Equal(t, "test", audio["id"])
	assert.Equal(t, "AAAABBBB", audio["data"])
	assert.Equal(t, "hello", audio["transcript"])
	assert.Equal(t, float64(200), audio["expires_at"])
	assert.Equal(t, "stop", choice["finish_reason"])
}

func TestParseStreamingDataIgnoresSSEData(t *testing.T) {
	rw := &fakeStreamResponseWriter{}

	// SSE data prefix should be ignored (IsValidSSEData checks for "data: " prefix)
	err := rw.parseStreamingData([]byte("data: "))
	assert.NoError(t, err)
	assert.Nil(t, rw.lastChunk)

	// SSE data with actual content should also be ignored
	err = rw.parseStreamingData([]byte("data: {\"test\":1}"))
	assert.NoError(t, err)
	assert.Nil(t, rw.lastChunk)
}

func TestParseStreamingDataWithEmptyChoices(t *testing.T) {
	rw := &fakeStreamResponseWriter{}

	// First chunk with empty choices (prompt_filter_results only)
	chunk := `{"choices":[],"created":0,"id":"","model":"gpt-4.1-mini","object":"","prompt_filter_results":[{"prompt_index":0,"content_filter_results":{}}]}`
	err := rw.parseStreamingData([]byte(chunk))
	require.NoError(t, err)

	assert.NotNil(t, rw.promptFilterResults)
	assert.Empty(t, rw.choices) // No choice content yet
}

func TestContentFilterResultPreservesErrorDetails(t *testing.T) {
	rw := &fakeStreamResponseWriter{}

	chunk := `{"choices":[{"content_filter_result":{"error":{"code":"content_filter_error","message":"The contents are not filtered"}},"delta":{"content":"test"},"finish_reason":"stop","index":0}],"created":1767597874,"id":"chatcmpl-test","model":"gpt-4","object":"chat.completion.chunk","usage":{"completion_tokens":1,"prompt_tokens":10,"total_tokens":11}}`
	err := rw.parseStreamingData([]byte(chunk))
	require.NoError(t, err)

	result, err := rw.convertToNonStream()
	require.NoError(t, err)

	var response map[string]any

	err = sonic.Unmarshal(result, &response)
	require.NoError(t, err)

	choices, ok := response["choices"].([]any)
	require.True(t, ok)
	choice, ok := choices[0].(map[string]any)
	require.True(t, ok)
	cfResult, ok := choice["content_filter_result"].(map[string]any)
	require.True(t, ok)
	cfError, ok := cfResult["error"].(map[string]any)
	require.True(t, ok)

	assert.Equal(t, "content_filter_error", cfError["code"])
	assert.Equal(t, "The contents are not filtered", cfError["message"])
}
