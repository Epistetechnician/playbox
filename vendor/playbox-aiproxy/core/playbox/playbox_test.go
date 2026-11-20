package playbox

import (
	"bytes"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/labring/aiproxy/core/model"
	"github.com/labring/aiproxy/core/relay/adaptor"
	"github.com/labring/aiproxy/core/relay/meta"
)

func init() {
	gin.SetMode(gin.TestMode)
}

func TestCooldownTrackerAndAliasPolicy_PROXY1(t *testing.T) {
	ct := NewCooldownTracker(2*time.Second, 30*time.Second)
	now := time.Unix(1_700_000_000, 0)
	ct.nowFn = func() time.Time { return now }

	// Huge upstream Retry-After (20 min) is clamped to maxCooldown (30s) so 429s never stall runs.
	d := ct.MarkRateLimited("ch:1", "1200")
	if d > 30*time.Second || d < 2*time.Second {
		t.Fatalf("expected clamped cooldown in [2s,30s], got %v", d)
	}
	if !ct.IsCoolingDown("ch:1") {
		t.Fatalf("expected ch:1 to be cooling down")
	}
	if ct.IsCoolingDown("ch:2") {
		t.Fatalf("ch:2 should remain immediately selectable")
	}

	now = now.Add(31 * time.Second)
	if ct.IsCoolingDown("ch:1") {
		t.Fatalf("expected ch:1 cooldown to expire")
	}

	resolver := NewAliasResolver(map[string][]string{
		"pbx-frontier": {"claude-opus-5-5-high", "gemini-4-neon-high", "gpt-5.6-luna"},
	})
	// Scientific benchmark mode: strictly refuses cross-family substitution.
	benchModels := resolver.Resolve("pbx-frontier", FailoverSameModelOnly)
	if len(benchModels) != 1 || benchModels[0] != "pbx-frontier" {
		t.Fatalf("same_model_only must not substitute aliases: got %v", benchModels)
	}
	// Production alias_cascade mode: returns full cross-provider failover chain.
	prodModels := resolver.Resolve("pbx-frontier", FailoverAliasCascade)
	if len(prodModels) != 3 || prodModels[1] != "gemini-4-neon-high" || prodModels[2] != "gpt-5.6-luna" {
		t.Fatalf("unexpected alias_cascade chain: %v", prodModels)
	}
}

type fakeDoRequest struct {
	called bool
	resp   *http.Response
}

func (f *fakeDoRequest) DoRequest(_ *meta.Meta, _ adaptor.Store, _ *gin.Context, _ *http.Request) (*http.Response, error) {
	f.called = true
	return f.resp, nil
}

type fakeDoResponse struct {
	readBytes int
}

func (f *fakeDoResponse) DoResponse(_ *meta.Meta, _ adaptor.Store, _ *gin.Context, resp *http.Response) (adaptor.DoResponseResult, adaptor.Error) {
	b, _ := io.ReadAll(resp.Body)
	f.readBytes = len(b)
	return adaptor.DoResponseResult{}, nil
}

func TestBudgetGuardPreFlightAndMidStreamCutoff_PROXY2(t *testing.T) {
	p := NewBudgetGuardPlugin()

	// 1. Pre-flight rejects before hitting upstream when token already spent its hard USD cap.
	w := httptest.NewRecorder()
	c, _ := gin.CreateTestContext(w)
	m := &meta.Meta{
		Token: model.TokenCache{ID: 7, Quota: 0.05, UsedAmount: 0.05},
	}
	doReq := &fakeDoRequest{}
	_, err := p.DoRequest(m, nil, c, httptest.NewRequest(http.MethodPost, "/v1/messages", nil), doReq)
	if err == nil {
		t.Fatalf("expected budget_exhausted pre-flight error")
	}
	if doReq.called {
		t.Fatalf("upstream must not be called when token quota is exhausted")
	}
	if w.Header().Get(HeaderStopReason) != "budget_exhausted" {
		t.Fatalf("expected X-Playbox-Stop-Reason=budget_exhausted, got %q", w.Header().Get(HeaderStopReason))
	}

	// 2. Mid-stream cutoff aborts oversized streaming output mid-turn when remaining budget is tiny.
	w2 := httptest.NewRecorder()
	c2, _ := gin.CreateTestContext(w2)
	m2 := &meta.Meta{
		Token: model.TokenCache{ID: 8, Quota: 0.02, UsedAmount: 0.019}, // $0.001 left
		ModelConfig: model.ModelConfig{
			Price: model.Price{
				OutputPrice:     10.0, // $10 per 1k tokens -> $0.001 buys 0.1k = 100 tokens (~400 bytes)
				OutputPriceUnit: 1000,
			},
		},
	}
	bigStream := bytes.Repeat([]byte("data: token\n"), 200) // 2400 bytes
	resp := &http.Response{
		StatusCode: http.StatusOK,
		Body:       io.NopCloser(bytes.NewReader(bigStream)),
		Header:     http.Header{},
	}
	doResp := &fakeDoResponse{}
	_, aerr := p.DoResponse(m2, nil, c2, resp, doResp)
	if aerr != nil {
		t.Fatalf("unexpected adaptor error: %v", aerr)
	}
	if doResp.readBytes >= len(bigStream) {
		t.Fatalf("expected mid-stream truncation below %d bytes, read %d", len(bigStream), doResp.readBytes)
	}
	if w2.Header().Get(HeaderStopReason) != "budget_exhausted" {
		t.Fatalf("expected mid-stream X-Playbox-Stop-Reason=budget_exhausted, got %q", w2.Header().Get(HeaderStopReason))
	}
	if got := GetCorrelationMetadata(c2)["pbx_budget_cutoff"]; got != "midstream" {
		t.Fatalf("expected pbx_budget_cutoff=midstream, got %q", got)
	}
}

func TestCanonicalLedgerAndScrubbing_PROXY3_PROXY5(t *testing.T) {
	t0 := time.UnixMilli(1795130000000)
	logs := []*model.Log{
		{
			ID:               2,
			RequestID:        "req-002",
			RequestAt:        t0.Add(2 * time.Second),
			CreatedAt:        t0.Add(4 * time.Second),
			TTFBMilliseconds: 350,
			GroupID:          "room:alpha",
			Model:            "claude-sonnet-4-6",
			ChannelID:        2,
			Code:             200,
			RetryTimes:       1,
			Usage: model.Usage{
				InputTokens:         5000,
				CachedTokens:        3000,
				CacheCreationTokens: 500,
				OutputTokens:        250,
				ReasoningTokens:     80,
			},
			Amount: model.Amount{UsedAmount: 0.012345},
			Metadata: map[string]string{
				"pbx_room_id":         "alpha",
				"pbx_run_id":          "run-99",
				"pbx_member_id":       "w1",
				"pbx_task_id":         "t-1",
				"pbx_failover_from":   "ch:1:429",
				"pbx_cache_mode":      "off",
				"pbx_failover_policy": FailoverSameModelOnly,
			},
		},
	}

	ledger, err := BuildLedger(logs, "", "")
	if err != nil {
		t.Fatalf("BuildLedger failed: %v", err)
	}
	if ledger.Schema != LedgerSchema || len(ledger.LedgerSHA256) != 64 {
		t.Fatalf("unexpected ledger schema/digest: %+v", ledger)
	}
	if ledger.Totals.Failovers != 1 || ledger.Totals.CostUSD != 0.012345 || ledger.Totals.ReasoningTokens != 80 {
		t.Fatalf("unexpected totals: %+v", ledger.Totals)
	}
	recomputed, err := ComputeLedgerSHA256(ledger)
	if err != nil || recomputed != ledger.LedgerSHA256 {
		t.Fatalf("ledger sha mismatch: %s vs %s (%v)", recomputed, ledger.LedgerSHA256, err)
	}
	// Tampering with cost breaks ledger_sha256.
	tampered := ledger
	tampered.Totals.CostUSD = 0.000001
	tamperedSha, _ := ComputeLedgerSHA256(tampered)
	if tamperedSha == ledger.LedgerSHA256 {
		t.Fatalf("tampered ledger must not match original sha256")
	}

	// Secret scrubbing (`PROXY-5`).
	raw := "Authorization: Bearer " + "sk-" + "ant-api03-SECRETTOKEN1234567890 and AIzaSyD12345678901234567890"
	scrubbed := ScrubSecrets(raw)
	if strings.Contains(scrubbed, "SECRETTOKEN") || strings.Contains(scrubbed, "AIzaSyD") {
		t.Fatalf("secret leaked after ScrubSecrets: %s", scrubbed)
	}
}

func TestHardenedListenAndPosture_PROXY7(t *testing.T) {
	t.Setenv("PLAYBOX_ALLOW_NON_LOOPBACK", "")
	addr, err := ResolveListenAddr("0.0.0.0:3000")
	if err != nil {
		t.Fatalf("ResolveListenAddr default failed: %v", err)
	}
	if Hardened && addr != "127.0.0.1:3000" {
		t.Fatalf("hardened mode must clamp default 0.0.0.0:3000 to 127.0.0.1:3000, got %q", addr)
	}
	if Hardened {
		if _, err := ResolveListenAddr("192.168.1.5:3000"); err == nil {
			t.Fatalf("hardened mode must refuse non-loopback bind without override")
		}
	}
	if u, err := ResolveListenAddr("unix:///tmp/pbx-aiproxy.sock"); err != nil || u != "unix:///tmp/pbx-aiproxy.sock" {
		t.Fatalf("expected unix socket addr to be accepted, got %q (%v)", u, err)
	}
}
