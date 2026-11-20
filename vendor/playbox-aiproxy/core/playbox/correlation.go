package playbox

import (
	"maps"
	"net/http"
	"strings"
	"sync"

	"github.com/gin-gonic/gin"
)

var (
	tokenMetaMu sync.RWMutex
	tokenMeta   = make(map[int]map[string]string)
)

// SetTokenMeta pins host-side Playbox correlation metadata to a minted worker token ID so
// OpenAI/Anthropic/Gemini SDK calls that do not send custom X-Playbox-* headers are still
// deterministically attributed to `run_id`, `room_id`, `member_id`, `cache_mode`, and `failover_policy`.
func SetTokenMeta(tokenID int, meta map[string]string) {
	if tokenID <= 0 {
		return
	}
	clean := make(map[string]string, len(meta))
	for k, v := range meta {
		v = strings.TrimSpace(v)
		if v == "" {
			continue
		}
		if len(v) > 128 {
			v = v[:128]
		}
		clean[k] = v
	}
	tokenMetaMu.Lock()
	if len(clean) == 0 {
		delete(tokenMeta, tokenID)
	} else {
		tokenMeta[tokenID] = clean
	}
	tokenMetaMu.Unlock()
}

// ApplyTokenMeta merges host-pinned token metadata onto the Gin request context.
// Host-pinned values take precedence over client-supplied headers so workers cannot spoof run_id or policy.
func ApplyTokenMeta(c *gin.Context, tokenID int) {
	if c == nil || tokenID <= 0 {
		return
	}
	tokenMetaMu.RLock()
	pinned, ok := tokenMeta[tokenID]
	var copyPinned map[string]string
	if ok && len(pinned) > 0 {
		copyPinned = make(map[string]string, len(pinned))
		maps.Copy(copyPinned, pinned)
	}
	tokenMetaMu.RUnlock()
	if len(copyPinned) == 0 {
		return
	}
	for k, v := range copyPinned {
		SetRequestMetaField(c, k, v)
	}
}

type setTokenMetaRequest struct {
	TokenID        int    `json:"token_id"`
	RoomID         string `json:"room_id"`
	MemberID       string `json:"member_id"`
	TaskID         string `json:"task_id"`
	RunID          string `json:"run_id"`
	CacheMode      string `json:"cache_mode"`
	FailoverPolicy string `json:"failover_policy"`
}

func HandleSetTokenMeta(c *gin.Context) {
	var req setTokenMetaRequest
	if err := c.ShouldBindJSON(&req); err != nil || req.TokenID <= 0 {
		c.JSON(http.StatusBadRequest, gin.H{"success": false, "message": "valid token_id required"})
		return
	}
	SetTokenMeta(req.TokenID, map[string]string{
		"pbx_room_id":         req.RoomID,
		"pbx_member_id":       req.MemberID,
		"pbx_task_id":         req.TaskID,
		"pbx_run_id":          req.RunID,
		"pbx_cache_mode":      req.CacheMode,
		"pbx_failover_policy": req.FailoverPolicy,
	})
	c.JSON(http.StatusOK, gin.H{"success": true})
}

const (
	HeaderRoomID         = "X-Playbox-Room-Id"
	HeaderMemberID       = "X-Playbox-Member-Id"
	HeaderTaskID         = "X-Playbox-Task-Id"
	HeaderRunID          = "X-Playbox-Run-Id"
	HeaderCacheMode      = "X-Playbox-Cache-Mode"
	HeaderFailoverPolicy = "X-Playbox-Failover-Policy"
	HeaderStopReason     = "X-Playbox-Stop-Reason"

	ctxCorrelationKey = "playbox_correlation_meta"
)

// CorrelationMiddleware extracts host-injected X-Playbox-* correlation headers
// (guests can never forge these when behind BOX-7, which strips x-playbox-* before injecting).
func CorrelationMiddleware() gin.HandlerFunc {
	return func(c *gin.Context) {
		meta := make(map[string]string, 6)
		pairs := [][2]string{
			{HeaderRoomID, "pbx_room_id"},
			{HeaderMemberID, "pbx_member_id"},
			{HeaderTaskID, "pbx_task_id"},
			{HeaderRunID, "pbx_run_id"},
			{HeaderCacheMode, "pbx_cache_mode"},
			{HeaderFailoverPolicy, "pbx_failover_policy"},
		}
		for _, kv := range pairs {
			v := strings.TrimSpace(c.GetHeader(kv[0]))
			if v != "" {
				if len(v) > 128 {
					v = v[:128]
				}
				meta[kv[1]] = v
			}
		}
		if len(meta) > 0 {
			c.Set(ctxCorrelationKey, meta)
		}
		c.Next()
	}
}

// GetCorrelationMetadata returns a copy of Playbox correlation metadata attached to the Gin request.
func GetCorrelationMetadata(c *gin.Context) map[string]string {
	if c == nil {
		return nil
	}
	raw, ok := c.Get(ctxCorrelationKey)
	if !ok {
		return nil
	}
	m, ok := raw.(map[string]string)
	if !ok || len(m) == 0 {
		return nil
	}
	out := make(map[string]string, len(m))
	maps.Copy(out, m)
	return out
}

// MergeCorrelationMetadata merges Playbox correlation metadata into the request's metadata map.
func MergeCorrelationMetadata(c *gin.Context, base map[string]string) map[string]string {
	corr := GetCorrelationMetadata(c)
	if len(corr) == 0 {
		return base
	}
	out := make(map[string]string, len(base)+len(corr))
	maps.Copy(out, base)
	maps.Copy(out, corr)
	return out
}

// SetRequestMetaField attaches a host-observed metadata field (e.g. budget_cutoff, failover_from).
func SetRequestMetaField(c *gin.Context, key, value string) {
	if c == nil || key == "" {
		return
	}
	var m map[string]string
	if raw, ok := c.Get(ctxCorrelationKey); ok {
		if existing, ok := raw.(map[string]string); ok {
			m = existing
		}
	}
	if m == nil {
		m = make(map[string]string, 4)
		c.Set(ctxCorrelationKey, m)
	}
	m[key] = value
}
