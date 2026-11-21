package streamfake

import (
	"bytes"
	"errors"
	"fmt"
	"maps"
	"net/http"
	"slices"
	"strconv"

	"github.com/bytedance/sonic"
	"github.com/bytedance/sonic/ast"
	"github.com/gin-gonic/gin"
	"github.com/labring/aiproxy/core/common"
	"github.com/labring/aiproxy/core/common/conv"
	"github.com/labring/aiproxy/core/relay/adaptor"
	"github.com/labring/aiproxy/core/relay/meta"
	"github.com/labring/aiproxy/core/relay/mode"
	relaymodel "github.com/labring/aiproxy/core/relay/model"
	"github.com/labring/aiproxy/core/relay/plugin"
	"github.com/labring/aiproxy/core/relay/plugin/noop"
	"github.com/labring/aiproxy/core/relay/plugin/patch"
	"github.com/labring/aiproxy/core/relay/render"
	"github.com/labring/aiproxy/core/relay/utils"
)

var _ plugin.Plugin = (*StreamFake)(nil)

// StreamFake implements the stream fake functionality
type StreamFake struct {
	noop.Noop
	configCache utils.PluginConfigCache[Config]
}

// NewStreamFakePlugin creates a new stream fake plugin instance
func NewStreamFakePlugin() plugin.Plugin {
	return &StreamFake{}
}

// Constants for metadata keys
const (
	fakeStreamKey = "fake_stream"
)

// getConfig retrieves the plugin configuration
func (p *StreamFake) getConfig(meta *meta.Meta) (*Config, error) {
	pluginConfig, err := p.configCache.Load(meta, "stream-fake", Config{})
	if err != nil {
		return nil, err
	}

	return &pluginConfig, nil
}

// ConvertRequest modifies the request to enable streaming if it's originally non-streaming
func (p *StreamFake) ConvertRequest(
	meta *meta.Meta,
	store adaptor.Store,
	req *http.Request,
	do adaptor.ConvertRequest,
) (adaptor.ConvertResult, error) {
	// Only process chat completions
	if meta.Mode != mode.ChatCompletions {
		return do.ConvertRequest(meta, store, req)
	}

	// Check if stream fake is enabled
	pluginConfig, err := p.getConfig(meta)
	if err != nil || !pluginConfig.Enable {
		return do.ConvertRequest(meta, store, req)
	}

	body, err := common.GetRequestBodyReusable(req)
	if err != nil {
		return adaptor.ConvertResult{}, fmt.Errorf("failed to read request body: %w", err)
	}

	node, err := common.GetJSONNodeNoCopy(body)
	if err != nil {
		return do.ConvertRequest(meta, store, req)
	}

	stream, _ := node.Get("stream").Bool()
	if stream {
		// Already streaming, no need to fake
		return do.ConvertRequest(meta, store, req)
	}

	patch.AddLazyPatch(meta, patch.PatchOperation{
		Op: patch.OpFunction,
		Function: func(root *ast.Node) (bool, error) {
			_, err := root.Set("stream", ast.NewBool(true))
			if err != nil {
				return false, err
			}

			return true, nil
		},
	})
	meta.Set(fakeStreamKey, true)

	return do.ConvertRequest(meta, store, req)
}

// DoResponse handles the response processing to collect streaming data and convert back to non-streaming
func (p *StreamFake) DoResponse(
	meta *meta.Meta,
	store adaptor.Store,
	c *gin.Context,
	resp *http.Response,
	do adaptor.DoResponse,
) (adaptor.DoResponseResult, adaptor.Error) {
	// Only process chat completions
	if meta.Mode != mode.ChatCompletions {
		return do.DoResponse(meta, store, c, resp)
	}

	// Check if this is a fake stream request
	isFakeStream, ok := meta.Get(fakeStreamKey)
	if !ok {
		return do.DoResponse(meta, store, c, resp)
	}

	isFakeStreamBool, ok := isFakeStream.(bool)
	if !ok || !isFakeStreamBool {
		return do.DoResponse(meta, store, c, resp)
	}

	return p.handleFakeStreamResponse(meta, store, c, resp, do)
}

// handleFakeStreamResponse processes the streaming response and converts it back to non-streaming
func (p *StreamFake) handleFakeStreamResponse(
	meta *meta.Meta,
	store adaptor.Store,
	c *gin.Context,
	resp *http.Response,
	do adaptor.DoResponse,
) (adaptor.DoResponseResult, adaptor.Error) {
	log := common.GetLogger(c)
	// Create a custom response writer to collect streaming data
	rw := &fakeStreamResponseWriter{
		ResponseWriter: c.Writer,
	}

	c.Writer = rw
	defer func() {
		c.Writer = rw.ResponseWriter
	}()

	// Process the streaming response
	result, relayErr := do.DoResponse(meta, store, c, resp)
	if relayErr != nil {
		return result, relayErr
	}

	// Convert collected streaming chunks to non-streaming response
	respBody, err := rw.convertToNonStream()
	if err != nil {
		log.Errorf("failed to convert to non-streaming response: %v", err)
		return result, relayErr
	}

	// Set appropriate headers for non-streaming response
	c.Header("Content-Type", "application/json")
	c.Header("Content-Length", strconv.Itoa(len(respBody)))

	// Remove streaming-specific headers
	c.Header("Cache-Control", "")
	c.Header("Connection", "")
	c.Header("Transfer-Encoding", "")
	c.Header("X-Accel-Buffering", "")

	// Write the non-streaming response
	_, _ = rw.ResponseWriter.Write(respBody)

	return result, nil
}

// fakeStreamResponseWriter captures streaming response data
type fakeStreamResponseWriter struct {
	gin.ResponseWriter

	lastChunk *ast.Node
	usageNode *ast.Node
	choices   map[int]*fakeStreamChoiceState

	// Azure OpenAI prompt-level content filtering fields
	promptFilterResults *ast.Node
}

type fakeStreamChoiceState struct {
	contentBuilder   bytes.Buffer
	reasoningContent bytes.Buffer
	finishReason     relaymodel.FinishReason
	logprobsContent  []ast.Node
	toolCalls        []*relaymodel.ToolCall
	contentParts     []relaymodel.MessageContent // for image/multimodal content
	signature        string                      // for thought signature
	audio            map[string]*bytes.Buffer
	audioFields      map[string]any

	// Azure OpenAI choice-level content filtering fields
	contentFilterResults *ast.Node // choice-level filter results
	contentFilterResult  *ast.Node // choice-level filter result (alternative field name)
}

func (rw *fakeStreamResponseWriter) choiceState(index int) *fakeStreamChoiceState {
	if rw.choices == nil {
		rw.choices = make(map[int]*fakeStreamChoiceState)
	}

	state := rw.choices[index]
	if state == nil {
		state = &fakeStreamChoiceState{}
		rw.choices[index] = state
	}

	return state
}

// ignore flush
func (rw *fakeStreamResponseWriter) Flush() {}

// ignore WriteHeaderNow
func (rw *fakeStreamResponseWriter) WriteHeaderNow() {}

func (rw *fakeStreamResponseWriter) Write(b []byte) (int, error) {
	// Parse streaming data
	_ = rw.parseStreamingData(b)

	return len(b), nil
}

func (rw *fakeStreamResponseWriter) WriteString(s string) (int, error) {
	return rw.Write(conv.StringToBytes(s))
}

// parseStreamingData extracts individual chunks from streaming response
func (rw *fakeStreamResponseWriter) parseStreamingData(data []byte) error {
	if render.IsValidSSEData(data) {
		data = render.ExtractSSEData(data)
		if len(data) == 0 || render.IsSSEDone(data) {
			return nil
		}
	}

	node, err := common.GetJSONNodeNoCopy(data)
	if err != nil || !node.Valid() {
		return nil
	}

	choicesNode := node.Get("choices")
	if err := choicesNode.Check(); err != nil {
		return nil
	}

	rw.lastChunk = &node

	usageNode := node.Get("usage")
	if err := usageNode.Check(); err != nil {
		if !errors.Is(err, ast.ErrNotExist) {
			return err
		}
	} else {
		rw.usageNode = usageNode
	}

	// Extract prompt_filter_results from first chunk (only save once)
	if rw.promptFilterResults == nil {
		promptFilterResultsNode := node.Get("prompt_filter_results")
		if err := promptFilterResultsNode.Check(); err == nil {
			rw.promptFilterResults = promptFilterResultsNode
		}
	}

	return choicesNode.ForEach(func(_ ast.Sequence, choiceNode *ast.Node) bool {
		choiceIndex, err := choiceNode.Get("index").Int64()
		if err != nil {
			choiceIndex = 0
		}

		state := rw.choiceState(int(choiceIndex))

		// Extract content_filter_results from choice (keep last non-empty value)
		contentFilterResultsNode := choiceNode.Get("content_filter_results")
		if err := contentFilterResultsNode.Check(); err == nil {
			state.contentFilterResults = contentFilterResultsNode
		}

		// Extract content_filter_result from choice (alternative field name, keep last non-empty value)
		contentFilterResultNode := choiceNode.Get("content_filter_result")
		if err := contentFilterResultNode.Check(); err == nil {
			state.contentFilterResult = contentFilterResultNode
		}

		deltaNode := choiceNode.Get("delta")
		if err := deltaNode.Check(); err != nil {
			return true
		}

		contentNode := deltaNode.Get("content")
		if err := contentNode.Check(); err == nil {
			// Try as string first (common case)
			if content, err := contentNode.String(); err == nil {
				state.contentBuilder.WriteString(content)
			} else {
				// Try as array (for image/multimodal content)
				_ = contentNode.ForEach(func(_ ast.Sequence, partNode *ast.Node) bool {
					partRaw, err := partNode.Raw()
					if err != nil {
						return true
					}

					var part relaymodel.MessageContent
					if err := sonic.UnmarshalString(partRaw, &part); err != nil {
						return true
					}

					// Keep all parts in contentParts for multimodal content
					state.contentParts = append(state.contentParts, part)

					return true
				})
			}
		}

		reasoningContent, err := deltaNode.Get("reasoning_content").String()
		if err == nil {
			state.reasoningContent.WriteString(reasoningContent)
		}

		// Handle signature for thought
		if signature, err := deltaNode.Get("signature").String(); err == nil && signature != "" {
			state.signature = signature
		}

		state.processAudioDelta(deltaNode.Get("audio"))

		_ = deltaNode.Get("tool_calls").
			ForEach(func(_ ast.Sequence, toolCallNode *ast.Node) bool {
				if toolCallNode == nil || toolCallNode.TypeSafe() == ast.V_NULL {
					return true
				}

				toolCallRaw, err := toolCallNode.Raw()
				if err != nil {
					return true
				}

				var toolCall relaymodel.ToolCall
				if err := sonic.UnmarshalString(toolCallRaw, &toolCall); err != nil {
					return true
				}

				state.toolCalls = mergeToolCalls(state.toolCalls, &toolCall)

				return true
			})

		finishReason, err := choiceNode.Get("finish_reason").String()
		if err == nil && finishReason != "" {
			state.finishReason = finishReason
		}

		logprobsContentNode := choiceNode.GetByPath("logprobs", "content")
		if err := logprobsContentNode.Check(); err == nil {
			l, err := logprobsContentNode.Len()
			if err != nil {
				return true
			}

			state.logprobsContent = slices.Grow(state.logprobsContent, l)
			_ = logprobsContentNode.ForEach(
				func(_ ast.Sequence, logprobsContentNode *ast.Node) bool {
					state.logprobsContent = append(
						state.logprobsContent,
						*logprobsContentNode,
					)

					return true
				},
			)
		}

		return true
	})
}

func (rw *fakeStreamResponseWriter) convertToNonStream() ([]byte, error) {
	lastChunk := rw.lastChunk
	if lastChunk == nil {
		return nil, errors.New("last chunk is nil")
	}

	_, err := lastChunk.Set("object", ast.NewString(relaymodel.ChatCompletionObject))
	if err != nil {
		return nil, err
	}

	if rw.usageNode != nil {
		_, err = lastChunk.Set("usage", *rw.usageNode)
		if err != nil {
			return nil, err
		}
	}

	indexes := make([]int, 0, len(rw.choices))
	for index := range rw.choices {
		indexes = append(indexes, index)
	}

	slices.Sort(indexes)

	choices := make([]any, 0, len(indexes))
	for _, index := range indexes {
		choices = append(choices, rw.choices[index].buildChoice(index))
	}

	_, err = lastChunk.SetAny("choices", choices)
	if err != nil {
		return nil, err
	}

	// Add prompt_filter_results to response if present
	if rw.promptFilterResults != nil {
		_, err = lastChunk.Set("prompt_filter_results", *rw.promptFilterResults)
		if err != nil {
			return nil, err
		}
	}

	return lastChunk.MarshalJSON()
}

func (state *fakeStreamChoiceState) buildChoice(index int) map[string]any {
	message := map[string]any{
		"role": "assistant",
	}

	// Use contentParts if available (for image/multimodal content), otherwise use string content
	if len(state.contentParts) > 0 {
		message["content"] = state.contentParts
	} else {
		message["content"] = state.contentBuilder.String()
	}

	reasoningContent := state.reasoningContent.String()
	if reasoningContent != "" {
		message["reasoning_content"] = reasoningContent
	}

	if state.signature != "" {
		message["signature"] = state.signature
	}

	if audio := state.buildAudio(); len(audio) > 0 {
		message["audio"] = audio
	}

	if len(state.toolCalls) > 0 {
		message["tool_calls"] = state.buildToolCalls()
	}

	if len(state.logprobsContent) > 0 {
		message["logprobs"] = map[string]any{
			"content": state.logprobsContent,
		}
	}

	// Build choice with content filter fields
	choice := map[string]any{
		"index":         index,
		"message":       message,
		"finish_reason": state.finishReason,
	}

	// Add content_filter_results to choice if present
	if state.contentFilterResults != nil {
		contentFilterResultsRaw, err := state.contentFilterResults.Interface()
		if err == nil {
			choice["content_filter_results"] = contentFilterResultsRaw
		}
	}

	// Add content_filter_result to choice if present (alternative field name)
	if state.contentFilterResult != nil {
		contentFilterResultRaw, err := state.contentFilterResult.Interface()
		if err == nil {
			choice["content_filter_result"] = contentFilterResultRaw
		}
	}

	return choice
}

func (state *fakeStreamChoiceState) processAudioDelta(audioNode *ast.Node) {
	if audioNode == nil || audioNode.TypeSafe() != ast.V_OBJECT {
		return
	}

	if err := audioNode.Check(); err != nil {
		return
	}

	_ = audioNode.ForEach(func(seq ast.Sequence, fieldNode *ast.Node) bool {
		if seq.Key == nil || fieldNode == nil {
			return true
		}

		key := *seq.Key

		if shouldAppendAudioField(key) && fieldNode.TypeSafe() == ast.V_STRING {
			value, err := fieldNode.String()
			if err != nil {
				return true
			}

			if state.audio == nil {
				state.audio = make(map[string]*bytes.Buffer)
			}

			builder := state.audio[key]
			if builder == nil {
				builder = &bytes.Buffer{}
				state.audio[key] = builder
			}

			builder.WriteString(value)

			return true
		}

		value, err := fieldNode.Interface()
		if err != nil {
			return true
		}

		if state.audioFields == nil {
			state.audioFields = make(map[string]any)
		}

		state.audioFields[key] = value

		return true
	})
}

func shouldAppendAudioField(key string) bool {
	return key == "data" || key == "transcript"
}

func (state *fakeStreamChoiceState) buildAudio() map[string]any {
	audio := make(map[string]any, len(state.audio)+len(state.audioFields))

	maps.Copy(audio, state.audioFields)

	for key, builder := range state.audio {
		audio[key] = builder.String()
	}

	return audio
}

func (state *fakeStreamChoiceState) buildToolCalls() []*relaymodel.ToolCall {
	if len(state.toolCalls) == 0 {
		return nil
	}

	slices.SortFunc(state.toolCalls, func(a, b *relaymodel.ToolCall) int {
		return a.Index - b.Index
	})

	if state.toolCalls[0].Index == 0 {
		return state.toolCalls
	}
	// fix tool call index start with 0
	for i, v := range state.toolCalls {
		v.Index = i
	}

	return state.toolCalls
}

func mergeToolCalls(
	oldToolCalls []*relaymodel.ToolCall,
	newToolCall *relaymodel.ToolCall,
) []*relaymodel.ToolCall {
	findedToolCallIndex := slices.IndexFunc(oldToolCalls, func(t *relaymodel.ToolCall) bool {
		return t.Index == newToolCall.Index
	})
	if findedToolCallIndex != -1 {
		oldToolCall := oldToolCalls[findedToolCallIndex]
		oldToolCalls[findedToolCallIndex] = mergeToolCall(oldToolCall, newToolCall)
	} else {
		oldToolCalls = append(oldToolCalls, newToolCall)
	}

	return oldToolCalls
}

func mergeToolCall(oldToolCall, newToolCall *relaymodel.ToolCall) *relaymodel.ToolCall {
	if oldToolCall == nil {
		return newToolCall
	}

	if newToolCall == nil {
		return oldToolCall
	}

	merged := &relaymodel.ToolCall{
		Index:    oldToolCall.Index,
		ID:       oldToolCall.ID,
		Type:     oldToolCall.Type,
		Function: oldToolCall.Function,
	}

	// Update ID if new one is provided and old one is empty
	if newToolCall.ID != "" && oldToolCall.ID == "" {
		merged.ID = newToolCall.ID
	}

	// Update function name if new one is provided and old one is empty
	if newToolCall.Function.Name != "" && oldToolCall.Function.Name == "" {
		merged.Function.Name = newToolCall.Function.Name
	}

	// Only append arguments if they're not empty
	if newToolCall.Function.Arguments != "" {
		merged.Function.Arguments += newToolCall.Function.Arguments
	}

	return merged
}
