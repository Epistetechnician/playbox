package qianfan

import "github.com/labring/aiproxy/core/relay/meta"

type Config struct {
	AppID          string   `json:"appid"`
	ResponseModels []string `json:"response_models"`
}

func (a *Adaptor) loadConfig(meta *meta.Meta) (Config, error) {
	cfg := Config{}
	return a.configCache.Load(meta, cfg)
}

func configSchema() map[string]any {
	return map[string]any{
		"type": "object",
		"properties": map[string]any{
			"responses_first_event_timeout": map[string]any{
				"type":        "integer",
				"title":       "Responses first event timeout",
				"description": "Maximum seconds to buffer initial Responses API lifecycle events while waiting for the first output or error event. Increase this value to allow late upstream errors to trigger channel retries.",
				"default":     2,
				"minimum":     0,
			},
			"map_reasoning_to_reasoning_content": map[string]any{
				"type":        "boolean",
				"title":       "Map reasoning To reasoning_content",
				"description": "Rewrite upstream chat completion `reasoning` fields to `reasoning_content` in both streaming and non-streaming responses.",
			},
			"appid": map[string]any{
				"type":        "string",
				"title":       "AppID Header",
				"description": "Optional Qianfan appid header used to distinguish usage and billing by application.",
			},
			"response_models": map[string]any{
				"type":        "array",
				"title":       "Additional Responses API Models",
				"description": "Additional Qianfan model names that support the Responses API beyond the built-in documented list.",
				"items": map[string]any{
					"type": "string",
				},
			},
		},
	}
}
