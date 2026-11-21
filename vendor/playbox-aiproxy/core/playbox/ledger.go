package playbox

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"math"
	"net/http"
	"slices"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/labring/aiproxy/core/model"
)

const LedgerSchema = "playbox.llm_ledger.v1"

// LedgerEntry is one host-observed LLM request inside `playbox.llm_ledger.v1`.
type LedgerEntry struct {
	ReqID             string  `json:"req_id"`
	TsMs              int64   `json:"ts_ms"`
	TaskID            string  `json:"task_id"`
	MemberID          string  `json:"member_id"`
	RequestedModel    string  `json:"requested_model"`
	UpstreamModel     string  `json:"upstream_model"`
	ChannelID         int     `json:"channel_id"`
	FailoverFrom      string  `json:"failover_from"`
	Status            int     `json:"status"`
	RetryCount        int64   `json:"retry_count"`
	BudgetCutoff      string  `json:"budget_cutoff"`
	InputTokens       int64   `json:"input_tokens"`
	CachedReadTokens  int64   `json:"cached_read_tokens"`
	CachedWriteTokens int64   `json:"cached_write_tokens"`
	OutputTokens      int64   `json:"output_tokens"`
	ReasoningTokens   int64   `json:"reasoning_tokens"`
	TTFTMs            int64   `json:"ttft_ms"`
	LatencyMs         int64   `json:"latency_ms"`
	CostUSD           float64 `json:"cost_usd"`
	PromptSHA256      string  `json:"prompt_sha256"`
	ResponseSHA256    string  `json:"response_sha256"`
}

type LedgerTotals struct {
	Requests          int      `json:"requests"`
	Retries           int64    `json:"retries"`
	Failovers         int      `json:"failovers"`
	BudgetCutoffs     int      `json:"budget_cutoffs"`
	InputTokens       int64    `json:"input_tokens"`
	CachedReadTokens  int64    `json:"cached_read_tokens"`
	CachedWriteTokens int64    `json:"cached_write_tokens"`
	OutputTokens      int64    `json:"output_tokens"`
	ReasoningTokens   int64    `json:"reasoning_tokens"`
	CostUSD           float64  `json:"cost_usd"`
	MedianTTFTMs      int64    `json:"median_ttft_ms"`
	P95LatencyMs      int64    `json:"p95_latency_ms"`
	ModelsUsed        []string `json:"models_used"`
}

type Ledger struct {
	Schema         string        `json:"schema"`
	GatewayDigest  string        `json:"gateway_digest"`
	RoomID         string        `json:"room_id"`
	RunID          string        `json:"run_id"`
	CacheMode      string        `json:"cache_mode"`
	FailoverPolicy string        `json:"failover_policy"`
	BodyRetention  bool          `json:"body_retention"`
	Totals         LedgerTotals  `json:"totals"`
	Entries        []LedgerEntry `json:"entries"`
	LedgerSHA256   string        `json:"ledger_sha256,omitempty"`
}

func round6(v float64) float64 {
	return math.Round(v*1e6) / 1e6
}

func sha256Hex(s string) string {
	if s == "" {
		return ""
	}
	sum := sha256.Sum256([]byte(s))
	return hex.EncodeToString(sum[:])
}

func percentile(sorted []int64, p float64) int64 {
	if len(sorted) == 0 {
		return 0
	}
	idx := int(math.Ceil(p*float64(len(sorted)))) - 1
	if idx < 0 {
		idx = 0
	}
	if idx >= len(sorted) {
		idx = len(sorted) - 1
	}
	return sorted[idx]
}

// CanonicalLedgerBytes emits deterministic UTF-8 sorted-key JSON matching Python `canonical_ledger_bytes`.
func CanonicalLedgerBytes(l Ledger) ([]byte, error) {
	l.LedgerSHA256 = ""
	raw, err := json.Marshal(l)
	if err != nil {
		return nil, err
	}
	var generic map[string]any
	if err := json.Unmarshal(raw, &generic); err != nil {
		return nil, err
	}
	delete(generic, "ledger_sha256")
	return json.Marshal(generic)
}

func ComputeLedgerSHA256(l Ledger) (string, error) {
	b, err := CanonicalLedgerBytes(l)
	if err != nil {
		return "", err
	}
	sum := sha256.Sum256(b)
	return hex.EncodeToString(sum[:]), nil
}

// BuildLedger aggregates a slice of `model.Log` rows into a signed-ready `playbox.llm_ledger.v1`.
func BuildLedger(logs []*model.Log, roomID, runID string) (Ledger, error) {
	entries := make([]LedgerEntry, 0, len(logs))
	modelSet := make(map[string]struct{})
	var ttfts []int64
	var lats []int64
	var totals LedgerTotals

	cacheMode := "off"
	failoverPolicy := FailoverSameModelOnly

	// Sort deterministically by (RequestAt, ID) ascending so ledger_sha256 is order-stable.
	ordered := make([]*model.Log, len(logs))
	copy(ordered, logs)
	sort.Slice(ordered, func(i, j int) bool {
		if !ordered[i].RequestAt.Equal(ordered[j].RequestAt) {
			return ordered[i].RequestAt.Before(ordered[j].RequestAt)
		}
		return ordered[i].ID < ordered[j].ID
	})

	for _, lg := range ordered {
		if lg == nil {
			continue
		}
		md := lg.Metadata
		if roomID == "" && md != nil {
			roomID = md["pbx_room_id"]
		}
		if runID == "" && md != nil {
			runID = md["pbx_run_id"]
		}
		if md != nil {
			if cm := md["pbx_cache_mode"]; cm != "" {
				cacheMode = cm
			}
			if fp := md["pbx_failover_policy"]; fp != "" {
				failoverPolicy = fp
			}
		}
		latMs := lg.CreatedAt.Sub(lg.RequestAt).Milliseconds()
		if latMs < 0 {
			latMs = 0
		}
		ttftMs := int64(lg.TTFBMilliseconds)
		if ttftMs > 0 {
			ttfts = append(ttfts, ttftMs)
		}
		if latMs > 0 {
			lats = append(lats, latMs)
		}

		failoverFrom := ""
		budgetCutoff := ""
		taskID := ""
		memberID := ""
		if md != nil {
			failoverFrom = md["pbx_failover_from"]
			budgetCutoff = md["pbx_budget_cutoff"]
			taskID = md["pbx_task_id"]
			memberID = md["pbx_member_id"]
		}

		promptHash := ""
		respHash := ""
		if lg.RequestDetail != nil {
			promptHash = sha256Hex(lg.RequestDetail.RequestBody)
			respHash = sha256Hex(lg.RequestDetail.ResponseBody)
		}

		cost := round6(lg.Amount.UsedAmount)
		reqID := string(lg.RequestID)
		if reqID == "" {
			reqID = strconv.Itoa(lg.ID)
		}

		entry := LedgerEntry{
			ReqID:             reqID,
			TsMs:              lg.RequestAt.UnixMilli(),
			TaskID:            taskID,
			MemberID:          memberID,
			RequestedModel:    lg.Model,
			UpstreamModel:     lg.Model,
			ChannelID:         lg.ChannelID,
			FailoverFrom:      failoverFrom,
			Status:            lg.Code,
			RetryCount:        int64(lg.RetryTimes),
			BudgetCutoff:      budgetCutoff,
			InputTokens:       int64(lg.Usage.InputTokens),
			CachedReadTokens:  int64(lg.Usage.CachedTokens),
			CachedWriteTokens: int64(lg.Usage.CacheCreationTokens),
			OutputTokens:      int64(lg.Usage.OutputTokens),
			ReasoningTokens:   int64(lg.Usage.ReasoningTokens),
			TTFTMs:            ttftMs,
			LatencyMs:         latMs,
			CostUSD:           cost,
			PromptSHA256:      promptHash,
			ResponseSHA256:    respHash,
		}
		entries = append(entries, entry)

		totals.Requests++
		totals.Retries += entry.RetryCount
		if failoverFrom != "" {
			totals.Failovers++
		}
		if budgetCutoff != "" {
			totals.BudgetCutoffs++
		}
		totals.InputTokens += entry.InputTokens
		totals.CachedReadTokens += entry.CachedReadTokens
		totals.CachedWriteTokens += entry.CachedWriteTokens
		totals.OutputTokens += entry.OutputTokens
		totals.ReasoningTokens += entry.ReasoningTokens
		totals.CostUSD = round6(totals.CostUSD + cost)
		if lg.Model != "" {
			modelSet[lg.Model] = struct{}{}
		}
	}

	slices.Sort(ttfts)
	slices.Sort(lats)
	totals.MedianTTFTMs = percentile(ttfts, 0.50)
	totals.P95LatencyMs = percentile(lats, 0.95)
	models := make([]string, 0, len(modelSet))
	for m := range modelSet {
		models = append(models, m)
	}
	slices.Sort(models)
	totals.ModelsUsed = models

	ledger := Ledger{
		Schema:         LedgerSchema,
		GatewayDigest:  GatewayDigest(),
		RoomID:         roomID,
		RunID:          runID,
		CacheMode:      cacheMode,
		FailoverPolicy: failoverPolicy,
		BodyRetention:  RetainBodiesDays() > 0,
		Totals:         totals,
		Entries:        entries,
	}
	digest, err := ComputeLedgerSHA256(ledger)
	if err != nil {
		return Ledger{}, err
	}
	ledger.LedgerSHA256 = digest
	return ledger, nil
}

func queryFilteredLogs(c *gin.Context, includeDetail bool, maxRows int) ([]*model.Log, error) {
	if model.LogDB == nil {
		return nil, nil
	}
	q := model.LogDB.Model(&model.Log{})
	if includeDetail {
		q = q.Preload("RequestDetail")
	}
	if group := strings.TrimSpace(c.Query("group")); group != "" {
		q = q.Where("group_id = ?", group)
	}
	if tokID, err := strconv.Atoi(strings.TrimSpace(c.Query("token_id"))); err == nil && tokID > 0 {
		q = q.Where("token_id = ?", tokID)
	}
	if sinceMs, err := strconv.ParseInt(strings.TrimSpace(c.Query("since_ms")), 10, 64); err == nil && sinceMs > 0 {
		q = q.Where("request_at >= ?", time.UnixMilli(sinceMs))
	}
	if untilMs, err := strconv.ParseInt(strings.TrimSpace(c.Query("until_ms")), 10, 64); err == nil && untilMs > 0 {
		q = q.Where("request_at <= ?", time.UnixMilli(untilMs))
	}
	if maxRows <= 0 || maxRows > 5000 {
		maxRows = 2000
	}
	var rows []*model.Log
	if err := q.Order("request_at DESC, id DESC").Limit(maxRows).Find(&rows).Error; err != nil {
		return nil, err
	}

	runID := strings.TrimSpace(c.Query("run_id"))
	taskID := strings.TrimSpace(c.Query("task_id"))
	roomID := strings.TrimSpace(c.Query("room_id"))
	if runID == "" && taskID == "" && roomID == "" {
		return rows, nil
	}
	out := make([]*model.Log, 0, len(rows))
	for _, lg := range rows {
		md := lg.Metadata
		if runID != "" && (md == nil || md["pbx_run_id"] != runID) {
			continue
		}
		if taskID != "" && (md == nil || md["pbx_task_id"] != taskID) {
			continue
		}
		if roomID != "" && lg.GroupID != "room:"+roomID && (md == nil || md["pbx_room_id"] != roomID) {
			continue
		}
		out = append(out, lg)
	}
	return out, nil
}

// RegisterRoutes mounts Playbox control-plane & Hub telemetry endpoints under `/api/playbox/*`
// (caller must protect the parent RouterGroup with `middleware.AdminAuth`).
func RegisterRoutes(apiRouter *gin.RouterGroup) {
	pbx := apiRouter.Group("/playbox")
	{
		pbx.GET("/posture", HandlePosture)
		pbx.GET("/ledger", HandleLedger)
		pbx.GET("/stats", HandleStats)
		pbx.GET("/activity", HandleActivity)
		pbx.GET("/logs", HandleMetadataLogs)
		pbx.GET("/logs/:id/body", HandleLogBody)
		pbx.POST("/token_meta", HandleSetTokenMeta)
	}
}

func HandlePosture(c *gin.Context) {
	c.JSON(http.StatusOK, gin.H{
		"success": true,
		"data": gin.H{
			"playbox_hardened":    Hardened,
			"mcp_disabled":        Hardened,
			"web_search_disabled": Hardened,
			"swagger_disabled":    Hardened,
			"loopback_only":       Hardened && !AllowNonLoopback(),
			"body_retention_days": RetainBodiesDays(),
			"gateway_digest":      GatewayDigest(),
		},
	})
}

func HandleLedger(c *gin.Context) {
	rows, err := queryFilteredLogs(c, true, 5000)
	if err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"success": false, "message": err.Error()})
		return
	}
	ledger, err := BuildLedger(rows, strings.TrimSpace(c.Query("room_id")), strings.TrimSpace(c.Query("run_id")))
	if err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"success": false, "message": err.Error()})
		return
	}
	c.JSON(http.StatusOK, gin.H{"success": true, "data": ledger})
}

func HandleStats(c *gin.Context) {
	rows, err := queryFilteredLogs(c, false, 2000)
	if err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"success": false, "message": err.Error()})
		return
	}
	ledger, err := BuildLedger(rows, strings.TrimSpace(c.Query("room_id")), strings.TrimSpace(c.Query("run_id")))
	if err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"success": false, "message": err.Error()})
		return
	}

	byModel := make(map[string]map[string]any)
	byMember := make(map[string]map[string]any)
	buckets := make(map[int64]int)
	for _, e := range ledger.Entries {
		b := (e.TsMs / (5 * 60 * 1000)) * (5 * 60 * 1000)
		buckets[b]++
		if _, ok := byModel[e.UpstreamModel]; !ok {
			byModel[e.UpstreamModel] = map[string]any{"requests": 0, "tokens": int64(0), "cost_usd": 0.0}
		}
		bm := byModel[e.UpstreamModel]
		bm["requests"] = bm["requests"].(int) + 1
		bm["tokens"] = bm["tokens"].(int64) + e.InputTokens + e.OutputTokens
		bm["cost_usd"] = round6(bm["cost_usd"].(float64) + e.CostUSD)

		mem := e.MemberID
		if mem == "" {
			mem = "host"
		}
		if _, ok := byMember[mem]; !ok {
			byMember[mem] = map[string]any{"requests": 0, "tokens": int64(0), "cost_usd": 0.0}
		}
		bw := byMember[mem]
		bw["requests"] = bw["requests"].(int) + 1
		bw["tokens"] = bw["tokens"].(int64) + e.InputTokens + e.OutputTokens
		bw["cost_usd"] = round6(bw["cost_usd"].(float64) + e.CostUSD)
	}

	c.JSON(http.StatusOK, gin.H{
		"success": true,
		"data": gin.H{
			"provenance": "host_aiproxy",
			"totals":     ledger.Totals,
			"by_model":   byModel,
			"by_member":  byMember,
			"buckets":    buckets,
		},
	})
}

func HandleActivity(c *gin.Context) {
	rows, err := queryFilteredLogs(c, false, 500)
	if err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"success": false, "message": err.Error()})
		return
	}
	ledger, err := BuildLedger(rows, strings.TrimSpace(c.Query("room_id")), strings.TrimSpace(c.Query("run_id")))
	if err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"success": false, "message": err.Error()})
		return
	}
	spans := make([]gin.H, 0, len(ledger.Entries))
	for _, e := range ledger.Entries {
		spans = append(spans, gin.H{
			"req_id":        e.ReqID,
			"member_id":     e.MemberID,
			"task_id":       e.TaskID,
			"model":         e.UpstreamModel,
			"channel_id":    e.ChannelID,
			"start_ms":      e.TsMs,
			"end_ms":        e.TsMs + e.LatencyMs,
			"ttft_ms":       e.TTFTMs,
			"status":        e.Status,
			"retry_count":   e.RetryCount,
			"failover_from": e.FailoverFrom,
			"budget_cutoff": e.BudgetCutoff,
			"cost_usd":      e.CostUSD,
		})
	}
	c.JSON(http.StatusOK, gin.H{
		"success": true,
		"data": gin.H{
			"provenance": "host_aiproxy",
			"spans":      spans,
		},
	})
}

func HandleMetadataLogs(c *gin.Context) {
	limit, _ := strconv.Atoi(strings.TrimSpace(c.Query("limit")))
	if limit <= 0 || limit > 200 {
		limit = 50
	}
	rows, err := queryFilteredLogs(c, false, limit)
	if err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"success": false, "message": err.Error()})
		return
	}
	ledger, err := BuildLedger(rows, strings.TrimSpace(c.Query("room_id")), strings.TrimSpace(c.Query("run_id")))
	if err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"success": false, "message": err.Error()})
		return
	}
	// Newest first for Hub Trail display (never returns prompt/response bodies here).
	slices.Reverse(ledger.Entries)
	c.JSON(http.StatusOK, gin.H{
		"success": true,
		"data": gin.H{
			"provenance":     "host_aiproxy",
			"body_retention": RetainBodiesDays() > 0,
			"entries":        ledger.Entries,
		},
	})
}

func HandleLogBody(c *gin.Context) {
	if RetainBodiesDays() <= 0 {
		c.JSON(http.StatusForbidden, gin.H{
			"success": false,
			"message": "body retention is disabled (PLAYBOX_RETAIN_BODIES_DAYS=0)",
		})
		return
	}
	if model.LogDB == nil {
		c.JSON(http.StatusNotFound, gin.H{"success": false, "message": "log db unavailable"})
		return
	}
	idParam := strings.TrimSpace(c.Param("id"))
	var lg model.Log
	q := model.LogDB.Preload("RequestDetail")
	if numID, err := strconv.Atoi(idParam); err == nil {
		q = q.Where("id = ?", numID)
	} else {
		q = q.Where("request_id = ?", idParam)
	}
	if group := strings.TrimSpace(c.Query("group")); group != "" {
		q = q.Where("group_id = ?", group)
	}
	if err := q.First(&lg).Error; err != nil {
		c.JSON(http.StatusNotFound, gin.H{"success": false, "message": "log not found"})
		return
	}
	if lg.RequestDetail == nil {
		c.JSON(http.StatusNotFound, gin.H{"success": false, "message": "no body stored for request"})
		return
	}
	c.JSON(http.StatusOK, gin.H{
		"success": true,
		"data": gin.H{
			"req_id":        string(lg.RequestID),
			"request_body":  ScrubSecrets(lg.RequestDetail.RequestBody),
			"response_body": ScrubSecrets(lg.RequestDetail.ResponseBody),
			"scrubbed":      true,
		},
	})
}
