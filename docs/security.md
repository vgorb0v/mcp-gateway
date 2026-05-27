# Security

MCP Gateway is local-only by design. It is not a remote MCP proxy and does not expose a public API.

## Local Control Plane

The daemon refuses non-loopback `listen` addresses by default. Keep `listen` as `127.0.0.1:0` unless you are developing locally and understand the risk.

The HTTP layer validates Host and Origin:

- No `Origin` header is allowed for CLI and bridge requests.
- Local origins such as `http://127.0.0.1:<port>`, `http://localhost:<port>`, and `http://[::1]:<port>` are allowed.
- Non-local browser origins are rejected.
- Non-local Host headers are rejected.

This reduces browser-origin and DNS rebinding risk for the localhost control plane.

## Files

- `~/.mcp-gateway/run/state.json`: owner-only.
- `~/.mcp-gateway/env`: owner-only.
- `~/.mcp-gateway/cache/mcp_manifest_cache.json`: public capability metadata, no env keys or values.
- `~/.mcp-gateway/logs/`: launchd stdout/stderr logs. Review before sharing.

## Secrets and Logs

The gateway redacts:

- Sensitive key names such as token, secret, key, password, auth, and cookie.
- Configured env values for sensitive keys.
- Common URL token query parameters.
- Backend stderr/stdout lines before they are served by `/logs/{server}`.

Redaction is defense-in-depth. Do not rely on it as the only secret-handling boundary.

## MCP Backends

Backends are local processes. They can read files, start subprocesses, open browsers, or make network calls depending on the package. Install only packages you trust.

`dangerous: true` is a warning label for humans and client adapters. It is not a sandbox.

## Browser MCP Policy

Browser-oriented MCP servers are opt-in. They must not default to `/Applications/Google Chrome.app`, the user's regular profile, or a generated fixed debug port. Use `mcpgateway install --provision chrome-for-testing` when a server needs an isolated browser binary.
