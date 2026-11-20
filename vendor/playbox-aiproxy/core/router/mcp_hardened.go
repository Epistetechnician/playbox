//go:build playbox_hardened

package router

import "github.com/gin-gonic/gin"

// SetMCPRouter is compiled out in playbox_hardened builds so no un-sandboxed
// MCP server can execute tools outside playbox-mcp, Biscuit grants, and witness.py.
func SetMCPRouter(_ *gin.Engine) {}
