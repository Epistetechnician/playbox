package playbox

import (
	"fmt"
	"net"
	"os"
	"regexp"
	"strconv"
	"strings"
)

// GatewayDigest can be overridden at build time via:
// `-ldflags "-X github.com/labring/aiproxy/core/playbox.BuildGatewayDigest=sha256:..."`
var BuildGatewayDigest = ""

func AllowNonLoopback() bool {
	v := strings.ToLower(strings.TrimSpace(os.Getenv("PLAYBOX_ALLOW_NON_LOOPBACK")))
	return v == "1" || v == "true" || v == "yes"
}

func RetainBodiesDays() int {
	raw := strings.TrimSpace(os.Getenv("PLAYBOX_RETAIN_BODIES_DAYS"))
	if raw == "" {
		return 0
	}
	n, err := strconv.Atoi(raw)
	if err != nil || n < 0 {
		return 0
	}
	return n
}

func GatewayDigest() string {
	if env := strings.TrimSpace(os.Getenv("PLAYBOX_GATEWAY_DIGEST")); env != "" {
		return env
	}
	return BuildGatewayDigest
}

// ResolveListenAddr enforces loopback/UDS defaults in hardened builds unless explicitly overridden.
func ResolveListenAddr(raw string) (string, error) {
	addr := strings.TrimSpace(raw)
	if Hardened && (addr == "" || addr == "0.0.0.0:3000" || addr == ":3000") && !AllowNonLoopback() {
		return "127.0.0.1:3000", nil
	}
	if strings.HasPrefix(addr, "unix://") {
		sock := strings.TrimPrefix(addr, "unix://")
		if sock == "" {
			return "", fmt.Errorf("empty unix socket path in listen addr %q", raw)
		}
		return addr, nil
	}
	if Hardened && !AllowNonLoopback() {
		host, _, err := net.SplitHostPort(addr)
		if err != nil {
			return "", fmt.Errorf("invalid listen addr %q: %w", addr, err)
		}
		if !IsLoopbackHost(host) {
			return "", fmt.Errorf(
				"playbox_hardened refuses non-loopback listen addr %q (set PLAYBOX_ALLOW_NON_LOOPBACK=1 to override)",
				addr,
			)
		}
	}
	return addr, nil
}

func IsLoopbackHost(host string) bool {
	h := strings.ToLower(strings.TrimSpace(host))
	if h == "localhost" || h == "127.0.0.1" || h == "::1" {
		return true
	}
	ip := net.ParseIP(h)
	return ip != nil && ip.IsLoopback()
}

var secretPatterns = []*regexp.Regexp{
	regexp.MustCompile(`sk-[A-Za-z0-9_\-]{16,}`),
	regexp.MustCompile(`AIza[0-9A-Za-z_\-]{20,}`),
	regexp.MustCompile(`gh[pousr]_[A-Za-z0-9]{20,}`),
	regexp.MustCompile(`AKIA[0-9A-Z]{16}`),
	regexp.MustCompile(`(?i)(authorization\s*:\s*bearer\s+)[^\s"']+`),
	regexp.MustCompile(`(?i)(x-api-key\s*:\s*)[^\s"']+`),
}

// ScrubSecrets redacts common API keys and bearer tokens before opted-in body persistence or Hub display.
func ScrubSecrets(s string) string {
	out := s
	for _, re := range secretPatterns {
		if re.NumSubexp() >= 1 {
			out = re.ReplaceAllString(out, "${1}[REDACTED]")
		} else {
			out = re.ReplaceAllString(out, "[REDACTED]")
		}
	}
	return out
}
