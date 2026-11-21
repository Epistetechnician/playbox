package controller

import (
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/gin-gonic/gin"
	"github.com/labring/aiproxy/core/model"
	"github.com/labring/aiproxy/core/playbox"
)

func TestFilterChannelsSkipsCoolingDown429ChannelAndFallsBackWhenAllCooling(t *testing.T) {
	ch1 := newGlobalScopedChannel(&model.Channel{ID: 9101, Status: model.ChannelStatusEnabled})
	ch2 := newGlobalScopedChannel(&model.Channel{ID: 9102, Status: model.ChannelStatusEnabled})
	defer playbox.DefaultCooldowns.Clear("9101")
	defer playbox.DefaultCooldowns.Clear("9102")

	// Mark channel 9101 as 429 rate-limited: selector must immediately prefer healthy 9102.
	playbox.DefaultCooldowns.MarkRateLimited("9101", "1200")
	got := filterChannels([]*scopedChannel{ch1, ch2}, nil, 0)
	if len(got) != 1 || got[0].channel.ID != 9102 {
		t.Fatalf("expected only healthy channel 9102, got %+v", got)
	}

	// If every candidate channel is cooling down, fall back to full filtered list instead of false 503.
	playbox.DefaultCooldowns.MarkRateLimited("9102", "1200")
	gotAll := filterChannels([]*scopedChannel{ch1, ch2}, nil, 0)
	if len(gotAll) != 2 {
		t.Fatalf("expected fallback to both channels when all cooling down, got %d", len(gotAll))
	}
}

func TestResolvePlayboxModelCandidatesHonorsFailoverPolicyHeader(t *testing.T) {
	gin.SetMode(gin.TestMode)
	prev := playboxAliases
	playboxAliases = playbox.NewAliasResolver(map[string][]string{
		"pbx-frontier": {"claude-opus-5-5-high", "gemini-4-neon-high", "gpt-5.6-luna"},
	})
	defer func() { playboxAliases = prev }()

	// Default / same_model_only: strictly refuses cross-family substitution.
	w1 := httptest.NewRecorder()
	c1, _ := gin.CreateTestContext(w1)
	c1.Request = httptest.NewRequest(http.MethodPost, "/v1/messages", nil)
	c1.Request.Header.Set(playbox.HeaderFailoverPolicy, playbox.FailoverSameModelOnly)
	cands1 := resolvePlayboxModelCandidates(c1, "pbx-frontier")
	if len(cands1) != 1 || cands1[0] != "pbx-frontier" {
		t.Fatalf("same_model_only must not expand alias, got %v", cands1)
	}

	// alias_cascade: expands ordered cross-provider fallback chain.
	w2 := httptest.NewRecorder()
	c2, _ := gin.CreateTestContext(w2)
	c2.Request = httptest.NewRequest(http.MethodPost, "/v1/messages", nil)
	c2.Request.Header.Set(playbox.HeaderFailoverPolicy, playbox.FailoverAliasCascade)
	cands2 := resolvePlayboxModelCandidates(c2, "pbx-frontier")
	if len(cands2) != 3 || cands2[0] != "claude-opus-5-5-high" || cands2[2] != "gpt-5.6-luna" {
		t.Fatalf("expected 3-model alias_cascade chain, got %v", cands2)
	}
}
