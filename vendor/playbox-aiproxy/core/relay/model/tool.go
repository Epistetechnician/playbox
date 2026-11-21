package model

type Tool struct {
	Type         string   `json:"type"`
	Function     Function `json:"function"`
	Name         string   `json:"name,omitempty"`
	Execution    string   `json:"execution,omitempty"`
	Description  string   `json:"description,omitempty"`
	Parameters   any      `json:"parameters,omitempty"`
	Strict       *bool    `json:"strict,omitempty"`
	DeferLoading *bool    `json:"defer_loading,omitempty"`
}

type Function struct {
	Parameters  any    `json:"parameters,omitempty"`
	Arguments   string `json:"arguments,omitempty"`
	Description string `json:"description,omitempty"`
	Name        string `json:"name,omitempty"`
	Strict      *bool  `json:"strict,omitempty"`
}

type GoogleExtraContent struct {
	ThoughtSignature string `json:"thought_signature,omitempty"`
}

type ExtraContent struct {
	Google *GoogleExtraContent `json:"google,omitempty"`
}

type ToolCall struct {
	Index        int           `json:"index"`
	ID           string        `json:"id,omitempty"`
	Type         string        `json:"type,omitempty"`
	Function     Function      `json:"function"`
	ExtraContent *ExtraContent `json:"extra_content,omitempty"`
}
