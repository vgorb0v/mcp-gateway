# Troubleshooting

Start with:

```bash
mcpgateway doctor
mcpgateway ps
```

## Gateway Not Running

```bash
launchctl print gui/$UID/io.github.mcpgateway.daemon
tail -n 100 ~/.mcp-gateway/logs/gateway.err.log
target/release/mcpgateway install-native
```

If the old private label exists, reinstalling unloads and removes its plist best-effort.

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
target/release/mcpgateway install-native
```

Do not delete `~/.mcp-gateway/config/` unless you intentionally want to remove server and client configuration.

## Safe Diagnostics

Before sharing diagnostics, redact:

- API keys, cookies, tokens, auth headers.
- Private local paths.
- Browser profile paths.
- Backend package configs that contain credentials.
