# Client Integrations

MCP Gateway writes per-server stdio entries for supported clients. It does not add a visible aggregate `mcp-gateway` MCP server.

## Managed Entries

Managed entries point to:

```text
~/.mcp-gateway/mcps/<server>
```

The shim calls `mcp-gateway-bridge`, which reads `~/.mcp-gateway/run/state.json`. Client configs do not hard-code the daemon port.

## Unmanaged Entries

The reconciler preserves entries the user added manually. It also writes backups under `~/.mcp-gateway/backups/` before overwriting files and tracks managed ownership in `~/.mcp-gateway/state/client-manifest.json`.

Use:

```bash
mcpgateway import-clients --write
mcpgateway apply-configs --clients codex,claude-code
mcpgateway doctor
```

## Codex

Codex config is read from `~/.codex/config.toml`. Entries are written under `[mcp_servers.<server>]`.

## Claude Code

Claude Code config is read from `~/.claude.json`. Entries are written under `mcpServers`.

## Claude Desktop

Claude Desktop config is read from `~/Library/Application Support/Claude/claude_desktop_config.json`.

## VS Code

VS Code MCP config is read from `~/Library/Application Support/Code/User/mcp.json`. MCP Gateway also sets `chat.mcp.autostart` in VS Code settings.

## Antigravity

Antigravity config is read from `~/.gemini/antigravity/mcp_config.json`. Stale aggregate gateway cache entries are removed when managed configs are applied.

## Drift

If a previously managed entry is hand-edited, `mcpgateway doctor` reports drift. Review the config before applying again because apply will restore the managed entry shape.
