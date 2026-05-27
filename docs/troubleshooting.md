# Troubleshooting

Start with:

```bash
mcpgateway doctor
mcpgateway ps
```

## Gateway Not Running

For Homebrew installs:

```bash
brew services info mcp-gateway
brew services restart mcp-gateway
tail -n 100 ~/.mcp-gateway/logs/gateway.err.log
```

For standalone release or source installs:

```bash
launchctl print gui/$UID/io.github.mcpgateway.daemon
tail -n 100 ~/.mcp-gateway/logs/gateway.err.log
target/release/mcpgateway install
```

If the old private label exists, reinstalling unloads and removes its plist best-effort. Homebrew installs remove the manual MCP Gateway plist during provisioning and rely on `brew services start/stop mcp-gateway`.

## Capability Cache Missing

```bash
mcpgateway refresh-capabilities all
```

List methods do not start real backends as a fallback. Missing cache entries return an MCP error asking for a refresh.

## Client Cannot Find Shim

```bash
ls -l ~/.mcp-gateway/mcps
mcpgateway apply-configs --clients codex,claude-code
```

Verify the client entry points at a per-server shim, not a parent `mcp-gateway` command.

## pnpm Missing

`pnpm` is needed only for installing Node-backed MCP packages. Existing Rust binaries and stdio shims do not require it.

## Reset Runtime State

To reset runtime state without deleting config:

```bash
mcpgateway stop all
rm -f ~/.mcp-gateway/run/state.json
rm -f ~/.mcp-gateway/cache/mcp_manifest_cache.json
mcpgateway refresh-capabilities all
brew services restart mcp-gateway
```

For standalone installs, replace the final line with `target/release/mcpgateway install`.

Do not delete `~/.mcp-gateway/config/` unless you intentionally want to remove server and client configuration.

## Safe Diagnostics

Before sharing diagnostics, redact:

- API keys, cookies, tokens, auth headers.
- Private local paths.
- Browser profile paths.
- Backend package configs that contain credentials.
