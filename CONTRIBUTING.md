# Contributing

Thanks for helping make MCP Gateway better. This project is intentionally macOS host-native and MCP/client agnostic, so changes should preserve the existing daemon, bridge, shim, cache, and LaunchAgent architecture.

## Local Setup

```bash
cargo build --workspace --bins
cargo test --workspace
```

For a native install from your checkout:

```bash
cargo build --release --workspace --bins
target/release/mcpgateway install-native
```

Use `--skip-launchctl --no-path-prompt --home <temp-dir>` when testing installer behavior without touching your real home directory.

## Quality Gates

Run these before opening a PR:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --workspace --bins
```

## Testing launchd Safely

- Prefer temp homes and `--skip-launchctl` for installer tests.
- For real launchd testing, inspect `launchctl print gui/$UID/io.github.mcpgateway.daemon`.
- Do not write LaunchDaemons or system services; MCP Gateway is a user LaunchAgent.
- Do not remove user config while testing migration from the old private label.

## Client Adapters

Client adapters are conveniences, not product defaults. New adapters should:

- Generate per-server shim entries in `~/.mcp-gateway/mcps/<server>`.
- Preserve unmanaged entries.
- Create backups before overwriting client config.
- Detect drift for entries previously managed by MCP Gateway.
- Avoid adding a visible aggregate `mcp-gateway` server entry.

## Backend Behavior

Backend changes must preserve lazy start and cached discovery:

- `initialize`, `tools/list`, `prompts/list`, and `resources/list` must be served from cached capabilities when the cache is enabled.
- Only real tool execution should start a backend during normal agent startup.
- `mcpgateway refresh-capabilities all` may start backends briefly, then stop those it started.
- Do not add default MCP servers, default clients, sample secrets, or browser sessions.

## Pull Requests

Please keep PRs cohesive, include tests for behavior changes, document security-sensitive changes, and call out any launchd, file permission, or client-config migration impact.
