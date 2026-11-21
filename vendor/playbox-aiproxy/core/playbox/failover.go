package playbox

import (
	"encoding/json"
	"math/rand/v2"
	"net/http"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"
)

const (
	FailoverSameModelOnly = "same_model_only"
	FailoverAliasCascade  = "alias_cascade"
)

// CooldownTracker implements sub-second per-channel 429/5xx circuit-breaking so a single
// rate-limited key/OAuth pool fails over to healthy channels in milliseconds instead of stalling.
type CooldownTracker struct {
	mu          sync.RWMutex
	until       map[string]time.Time
	minCooldown time.Duration
	maxCooldown time.Duration
	nowFn       func() time.Time
}

var DefaultCooldowns = NewCooldownTracker(2*time.Second, 30*time.Second)

func NewCooldownTracker(minCooldown, maxCooldown time.Duration) *CooldownTracker {
	if minCooldown <= 0 {
		minCooldown = 2 * time.Second
	}
	if maxCooldown < minCooldown {
		maxCooldown = minCooldown
	}
	return &CooldownTracker{
		until:       make(map[string]time.Time),
		minCooldown: minCooldown,
		maxCooldown: maxCooldown,
		nowFn:       time.Now,
	}
}

// MarkRateLimited puts channelKey into short jittered cooldown (bounded by maxCooldown).
func (t *CooldownTracker) MarkRateLimited(channelKey string, retryAfterHeader string) time.Duration {
	if channelKey == "" {
		return 0
	}
	d := t.minCooldown
	if retryAfterHeader != "" {
		if secs, err := strconv.Atoi(strings.TrimSpace(retryAfterHeader)); err == nil && secs > 0 {
			d = time.Duration(secs) * time.Second
		} else if parsed, err := http.ParseTime(strings.TrimSpace(retryAfterHeader)); err == nil {
			if delta := parsed.Sub(t.nowFn()); delta > 0 {
				d = delta
			}
		}
	}
	if d < t.minCooldown {
		d = t.minCooldown
	}
	if d > t.maxCooldown {
		d = t.maxCooldown
	}
	// Add up to 15% jitter to prevent thundering herd across parallel workers.
	jitter := time.Duration(rand.Int64N(int64(d)/6 + 1))
	d += jitter
	if d > t.maxCooldown {
		d = t.maxCooldown
	}
	t.mu.Lock()
	t.until[channelKey] = t.nowFn().Add(d)
	t.mu.Unlock()
	return d
}

// IsCoolingDown returns true while channelKey is inside its 429 cooldown window.
func (t *CooldownTracker) IsCoolingDown(channelKey string) bool {
	t.mu.RLock()
	exp, ok := t.until[channelKey]
	t.mu.RUnlock()
	if !ok {
		return false
	}
	if t.nowFn().After(exp) {
		t.mu.Lock()
		delete(t.until, channelKey)
		t.mu.Unlock()
		return false
	}
	return true
}

// Clear removes cooldown state for channelKey (e.g. after a successful probe).
func (t *CooldownTracker) Clear(channelKey string) {
	t.mu.Lock()
	delete(t.until, channelKey)
	t.mu.Unlock()
}

// AliasResolver maps logical Playbox model aliases (e.g. `pbx-frontier`) to ordered
// cross-provider fallback models while strictly honoring `FailoverSameModelOnly` on benchmarks.
type AliasResolver struct {
	aliases map[string][]string
}

func LoadAliasesFromEnv() *AliasResolver {
	raw := strings.TrimSpace(os.Getenv("PLAYBOX_MODEL_ALIASES"))
	if raw == "" {
		return NewAliasResolver(nil)
	}
	var m map[string][]string
	if err := json.Unmarshal([]byte(raw), &m); err != nil {
		return NewAliasResolver(nil)
	}
	return NewAliasResolver(m)
}

func NewAliasResolver(aliases map[string][]string) *AliasResolver {
	clean := make(map[string][]string, len(aliases))
	for k, list := range aliases {
		key := strings.TrimSpace(k)
		if key == "" {
			continue
		}
		var out []string
		for _, item := range list {
			s := strings.TrimSpace(item)
			if s != "" {
				out = append(out, s)
			}
		}
		if len(out) > 0 {
			clean[key] = out
		}
	}
	return &AliasResolver{aliases: clean}
}

// Resolve returns the ordered candidate upstream models permitted under policy.
// Under `same_model_only` (the benchmark default), cross-family substitution is refused:
// only the exact requested model snapshot is returned.
func (r *AliasResolver) Resolve(requestedModel, policy string) []string {
	req := strings.TrimSpace(requestedModel)
	if req == "" {
		return nil
	}
	pol := strings.ToLower(strings.TrimSpace(policy))
	if pol == "" || pol == FailoverSameModelOnly {
		return []string{req}
	}
	if pol == FailoverAliasCascade && r != nil {
		if chain, ok := r.aliases[req]; ok && len(chain) > 0 {
			out := make([]string, len(chain))
			copy(out, chain)
			return out
		}
	}
	return []string{req}
}
