package playbox

import (
	"bytes"
	"fmt"
	"io"
	"net/http"
	"strings"
	"sync"

	"github.com/gin-gonic/gin"
	"github.com/labring/aiproxy/core/model"
	"github.com/labring/aiproxy/core/relay/adaptor"
	"github.com/labring/aiproxy/core/relay/meta"
	relaymodel "github.com/labring/aiproxy/core/relay/model"
	"github.com/labring/aiproxy/core/relay/plugin"
	"github.com/labring/aiproxy/core/relay/plugin/noop"
)

var _ plugin.Plugin = (*BudgetGuardPlugin)(nil)

// BudgetGuardPlugin enforces hard pre-flight and mid-stream USD caps per Playbox worker token
// at the host network boundary (`PROXY-2`), preventing runaway loops from overshooting `BudgetCap`.
type BudgetGuardPlugin struct {
	noop.Noop
	mu        sync.Mutex
	inFlight  map[int]float64
	cooldowns *CooldownTracker
}

var defaultBudgetGuard = NewBudgetGuardPlugin()

func DefaultBudgetGuard() *BudgetGuardPlugin {
	return defaultBudgetGuard
}

func NewBudgetGuardPlugin() *BudgetGuardPlugin {
	return &BudgetGuardPlugin{
		inFlight:  make(map[int]float64),
		cooldowns: DefaultCooldowns,
	}
}

func estimateInputUSD(m *meta.Meta) float64 {
	if m == nil {
		return 0
	}
	price := m.ModelConfig.Price
	inUnit := float64(price.GetInputPriceUnit())
	if inUnit <= 0 {
		inUnit = float64(model.PriceUnit)
	}
	inTokens := float64(m.RequestUsage.InputTokens)
	if inTokens <= 0 {
		inTokens = float64(m.RequestUsage.TotalTokens)
	}
	return (inTokens / inUnit) * float64(price.InputPrice)
}

func outputUSDPerToken(m *meta.Meta) float64 {
	if m == nil {
		return 0
	}
	price := m.ModelConfig.Price
	outUnit := float64(price.GetOutputPriceUnit())
	if outUnit <= 0 {
		outUnit = float64(model.PriceUnit)
	}
	if price.OutputPrice > 0 {
		return float64(price.OutputPrice) / outUnit
	}
	if price.InputPrice > 0 {
		return float64(price.InputPrice) / outUnit
	}
	return 0
}

func (p *BudgetGuardPlugin) reserve(tokenID int, quota, used, est float64) (float64, bool) {
	if quota <= 0 || tokenID <= 0 {
		return 0, true
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	active := p.inFlight[tokenID]
	remaining := quota - used - active
	if remaining <= 0 || (est > 0 && remaining < est) {
		return remaining, false
	}
	reserveAmt := est
	if reserveAmt <= 0 {
		reserveAmt = 1e-6
	}
	p.inFlight[tokenID] = active + reserveAmt
	return remaining - reserveAmt, true
}

func (p *BudgetGuardPlugin) release(tokenID int, reserved float64) {
	if tokenID <= 0 || reserved <= 0 {
		return
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	p.inFlight[tokenID] -= reserved
	if p.inFlight[tokenID] <= 1e-9 {
		delete(p.inFlight, tokenID)
	}
}

func (p *BudgetGuardPlugin) DoRequest(
	m *meta.Meta,
	store adaptor.Store,
	c *gin.Context,
	req *http.Request,
	do adaptor.DoRequest,
) (*http.Response, error) {
	if m != nil && m.Token.Quota > 0 {
		est := estimateInputUSD(m)
		if _, ok := p.reserve(m.Token.ID, m.Token.Quota, m.Token.UsedAmount, est); !ok {
			if c != nil {
				c.Header(HeaderStopReason, "budget_exhausted")
				SetRequestMetaField(c, "pbx_budget_cutoff", "preflight")
			}
			return nil, &budgetExhaustedError{
				tokenID: m.Token.ID,
				quota:   m.Token.Quota,
				used:    m.Token.UsedAmount,
			}
		}
		if est <= 0 {
			est = 1e-6
		}
		if c != nil {
			c.Set("pbx_budget_reserved", est)
		}
	}

	resp, err := do.DoRequest(m, store, c, req)
	if err != nil {
		if m != nil && c != nil {
			if r, ok := c.Get("pbx_budget_reserved"); ok {
				if f, ok := r.(float64); ok {
					p.release(m.Token.ID, f)
					c.Set("pbx_budget_reserved", 0.0)
				}
			}
		}
		return nil, err
	}

	// Track upstream 429/529 immediately in Playbox's sub-second cooldown tracker.
	if resp != nil && (resp.StatusCode == http.StatusTooManyRequests || resp.StatusCode == 529) && m != nil && p.cooldowns != nil {
		chanKey := fmt.Sprintf("%d", m.Channel.ID)
		p.cooldowns.MarkRateLimited(chanKey, resp.Header.Get("Retry-After"))
		if c != nil {
			SetRequestMetaField(c, "pbx_failover_from", fmt.Sprintf("ch:%d:%d", m.Channel.ID, resp.StatusCode))
		}
	}
	return resp, nil
}

func (p *BudgetGuardPlugin) DoResponse(
	m *meta.Meta,
	store adaptor.Store,
	c *gin.Context,
	resp *http.Response,
	do adaptor.DoResponse,
) (adaptor.DoResponseResult, adaptor.Error) {
	if m != nil && c != nil {
		if r, ok := c.Get("pbx_budget_reserved"); ok {
			if f, ok := r.(float64); ok && f > 0 {
				defer p.release(m.Token.ID, f)
			}
		}
	}

	if m == nil || m.Token.Quota <= 0 || c == nil || resp == nil || resp.Body == nil {
		return do.DoResponse(m, store, c, resp)
	}

	remainingUSD := m.Token.Quota - m.Token.UsedAmount - estimateInputUSD(m)
	perTok := outputUSDPerToken(m)
	if remainingUSD <= 0 {
		c.Header(HeaderStopReason, "budget_exhausted")
		SetRequestMetaField(c, "pbx_budget_cutoff", "preflight")
		_ = resp.Body.Close()
		return adaptor.DoResponseResult{}, relaymodel.WrapperOpenAIErrorWithMessage(
			"playbox worker hard USD cap reached",
			"budget_exhausted",
			http.StatusPaymentRequired,
		)
	}

	if perTok > 0 {
		// Wrap upstream response body so streaming tokens exceeding remainingUSD are cut off mid-turn.
		// ~4 UTF-8 bytes of generated delta content per output token.
		maxOutTokens := int64(remainingUSD / perTok)
		maxBytes := maxOutTokens * 4
		if maxBytes < 256 {
			maxBytes = 256
		}
		isSSE := strings.Contains(resp.Header.Get("Content-Type"), "event-stream")
		meter := &budgetLimitReader{
			rc:       resp.Body,
			maxBytes: maxBytes,
			sseMode:  isSSE,
			onCutoff: func() {
				c.Header(HeaderStopReason, "budget_exhausted")
				SetRequestMetaField(c, "pbx_budget_cutoff", "midstream")
			},
		}
		resp.Body = meter
	}

	return do.DoResponse(m, store, c, resp)
}

type budgetExhaustedError struct {
	tokenID int
	quota   float64
	used    float64
}

func (e *budgetExhaustedError) Error() string {
	return fmt.Sprintf("playbox budget_exhausted: token=%d used=%.6f quota=%.6f", e.tokenID, e.used, e.quota)
}

func (e *budgetExhaustedError) StatusCode() int {
	return http.StatusPaymentRequired
}

type budgetLimitReader struct {
	rc       io.ReadCloser
	read     int64
	maxBytes int64
	sseMode  bool
	lineBuf  bytes.Buffer
	cut      bool
	onCutoff func()
}

// countSSEDeltaBytes counts generated output bytes inside SSE delta events while ignoring
// Responses/Anthropic lifecycle events (`response.created`, `response.completed`, echoed tool schemas).
func countSSEDeltaBytes(line []byte) int64 {
	trimmed := bytes.TrimSpace(line)
	if !bytes.HasPrefix(trimmed, []byte("data:")) {
		return 0
	}
	payload := bytes.TrimSpace(bytes.TrimPrefix(trimmed, []byte("data:")))
	if len(payload) == 0 || bytes.Equal(payload, []byte("[DONE]")) {
		return 0
	}
	// Skip non-delta Responses/Claude lifecycle frames that echo request instructions & tool schemas.
	if bytes.Contains(payload, []byte(`"response.created"`)) ||
		bytes.Contains(payload, []byte(`"response.in_progress"`)) ||
		bytes.Contains(payload, []byte(`"response.completed"`)) ||
		bytes.Contains(payload, []byte(`"response.output_item.`)) ||
		bytes.Contains(payload, []byte(`"response.content_part.`)) ||
		bytes.Contains(payload, []byte(`"message_start"`)) {
		return 0
	}
	if idx := bytes.Index(payload, []byte(`"delta":`)); idx >= 0 {
		rest := payload[idx+len(`"delta":`):]
		// Estimate generated content length without full JSON parse overhead on every chunk.
		if cIdx := bytes.Index(rest, []byte(`"content":"`)); cIdx >= 0 {
			sub := rest[cIdx+len(`"content":"`):]
			if end := bytes.IndexByte(sub, '"'); end >= 0 {
				return int64(end)
			}
		}
		if len(rest) > 0 && rest[0] == '"' {
			sub := rest[1:]
			if end := bytes.IndexByte(sub, '"'); end >= 0 {
				return int64(end)
			}
		}
		if tIdx := bytes.Index(rest, []byte(`"text":"`)); tIdx >= 0 {
			sub := rest[tIdx+len(`"text":"`):]
			if end := bytes.IndexByte(sub, '"'); end >= 0 {
				return int64(end)
			}
		}
		if aIdx := bytes.Index(rest, []byte(`"arguments":"`)); aIdx >= 0 {
			sub := rest[aIdx+len(`"arguments":"`):]
			if end := bytes.IndexByte(sub, '"'); end >= 0 {
				return int64(end)
			}
		}
		return int64(len(rest)) / 2
	}
	return 0
}

func (b *budgetLimitReader) Read(p []byte) (int, error) {
	if b.cut {
		return 0, io.EOF
	}
	n, err := b.rc.Read(p)
	if n > 0 {
		var charged int64
		if b.sseMode {
			b.lineBuf.Write(p[:n])
			for {
				data := b.lineBuf.Bytes()
				nl := bytes.IndexByte(data, '\n')
				if nl < 0 {
					break
				}
				line := data[:nl+1]
				charged += countSSEDeltaBytes(line)
				b.lineBuf.Next(nl + 1)
			}
		} else {
			charged = int64(n)
		}
		b.read += charged
		if b.read > b.maxBytes {
			b.cut = true
			if b.onCutoff != nil {
				b.onCutoff()
			}
			_ = b.rc.Close()
			excess := b.read - b.maxBytes
			keep := int64(n) - excess
			if keep < 0 {
				keep = 0
			}
			return int(keep), io.EOF
		}
	}
	return n, err
}

func (b *budgetLimitReader) Close() error {
	return b.rc.Close()
}
