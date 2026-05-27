# MCP Gateway Agent Notes

- MCP Gateway is a host-native macOS user service. Do not assume Docker, containers, `/workspace`, or `host.docker.internal`.
- The project should stay MCP-server agnostic and AI-client agnostic. Known client adapters are optional conveniences; do not add default MCP servers or default managed clients.
- Configured MCP entries point to shims in `~/.mcp-gateway/mcps/<server>`. The shims call the hidden local Rust supervisor through `~/.mcp-gateway/run/state.json`; client configs should not hard-code gateway ports.
- App launch and MCP discovery must not start real backends. `initialize` and list methods are served from `~/.mcp-gateway/cache/mcp_manifest_cache.json`; only `tools/call` should lazy-start a backend.
- Use `mcpgateway refresh-capabilities all` after backend package/config changes to rebuild cached tool/prompt/resource metadata.
- Browser-oriented MCP servers must not use `/Applications/Google Chrome.app`, the user's regular profile, or a generated fixed debug port by default. Chrome for Testing provisioning is opt-in via `mcpgateway install-native --provision chrome-for-testing`.
- Use `mcpgateway stop <server>` or `mcpgateway stop all` to reap AI browser/backend processes immediately.
- Attaching a browser MCP server to an existing logged-in browser session is opt-in only via that server's explicit attach flags.
- Prefer `mcpgateway install-native` to refresh binaries, backend packages, shims, client configs, and the macOS LaunchAgent.
