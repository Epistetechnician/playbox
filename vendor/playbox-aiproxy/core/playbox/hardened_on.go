//go:build playbox_hardened

package playbox

// Hardened is true when built with `-tags playbox_hardened`.
// In hardened mode: MCP servers, Web Search plugin, Swagger, and public web UI are compiled out;
// the server binds to loopback (or Unix socket) only unless explicitly overridden; and body retention
// defaults to metadata-only (0 days).
const Hardened = true
