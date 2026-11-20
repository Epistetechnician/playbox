package common

import (
	"context"
	"errors"
	"net"
	"strings"
	"syscall"

	"github.com/jackc/pgx/v5/pgconn"
	"github.com/redis/go-redis/v9"
)

// connectionErrorPatterns contains string patterns that indicate connection errors
// Used as fallback for database drivers that may wrap errors in unexpected ways
var connectionErrorPatterns = []string{
	"dial error",
	"connection refused",
	"connection reset",
	"operation not permitted",
	"failed to connect",
	"dial tcp",
	"no such host",
	"i/o timeout",
	"network is unreachable",
	"host is unreachable",
	"connection timed out",
}

// IsDBConnectionError checks if the error is a database connection error
// using type-based error checking with errors.As, with string pattern fallback
func IsDBConnectionError(err error) bool {
	if err == nil {
		return false
	}

	// Check for context errors (timeout, cancelled)
	if errors.Is(err, context.DeadlineExceeded) || errors.Is(err, context.Canceled) {
		return true
	}

	// Check for pgx/pgconn connection errors
	if _, ok := errors.AsType[*pgconn.ConnectError](err); ok {
		return true
	}

	// Check for pgx/pgconn timeout errors
	if pgconn.Timeout(err) {
		return true
	}

	// Check for redis connection errors
	if errors.Is(err, redis.ErrClosed) ||
		errors.Is(err, redis.ErrPoolExhausted) ||
		errors.Is(err, redis.ErrPoolTimeout) {
		return true
	}

	// Check for network operation errors
	if _, ok := errors.AsType[*net.OpError](err); ok {
		return true
	}

	// Check for DNS errors
	if _, ok := errors.AsType[*net.DNSError](err); ok {
		return true
	}

	// Check for syscall errors (connection refused, reset, etc.)
	if syscallErr, ok := errors.AsType[syscall.Errno](err); ok {
		switch syscallErr {
		case syscall.ECONNREFUSED, // connection refused
			syscall.ECONNRESET,   // connection reset by peer
			syscall.ECONNABORTED, // connection aborted
			syscall.ETIMEDOUT,    // connection timed out
			syscall.ENETUNREACH,  // network is unreachable
			syscall.EHOSTUNREACH, // host is unreachable
			syscall.EPERM,        // operation not permitted
			syscall.ENOENT:       // no such file or directory (for unix sockets)
			return true
		}
	}

	// Check for net.AddrError
	if _, ok := errors.AsType[*net.AddrError](err); ok {
		return true
	}

	// Fallback: string pattern matching for other drivers
	errStr := strings.ToLower(err.Error())
	for _, pattern := range connectionErrorPatterns {
		if strings.Contains(errStr, pattern) {
			return true
		}
	}

	return false
}
