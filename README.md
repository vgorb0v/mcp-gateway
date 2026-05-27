# MCP Gateway

MCP Gateway is a lightweight, host-native macOS supervisor for MCP servers. It runs as a user service, keeps a private loopback-only control plane, exposes each configured backend through a per-server stdio shim in `~/.mcp-gateway/mcps/<server>`, serves discovery from a cache, and lazy-starts backends only for real tool calls.

Fresh installs are empty-core: no default MCP servers, no default managed AI clients, no API keys, and no browser sessions.

## Install

### Homebrew

Homebrew is the primary install path:

```bash
brew tap vgorb0v/tap
brew install mcp-gateway
brew services start mcp-gateway
mcpgateway doctor
```

The formula installs `mcp-gateway`, `mcp-gateway-bridge`, and `mcpgateway` into Homebrew's prefix. `brew services start mcp-gateway` provisions local state under `~/.mcp-gateway/` before starting the daemon. Homebrew owns service management, so use `brew services start mcp-gateway`, `brew services stop mcp-gateway`, and `brew services restart mcp-gateway`.

### GitHub Release

Download the archive for your Mac from [GitHub Releases](https://github.com/vgorb0v/mcp-gateway/releases), then install the binaries:

```bash
tar -xzf mcp-gateway-vX.Y.Z-aarch64-apple-darwin.tar.gz
cd mcp-gateway-vX.Y.Z-aarch64-apple-darwin
./mcpgateway install
```

Use the `x86_64-apple-darwin` archive on Intel Macs. The standalone installer writes binaries and shims under `~/.mcp-gateway/` and manages the user LaunchAgent directly.

`install-native` remains as a compatibility alias for older scripts.

### Build From Source

```bash
cargo build --release --workspace --bins
target/release/mcpgateway install
```

## 90-Second Demo

Run a self-contained proof path without adding real MCP packages or touching launchctl:

```bash
mcpgateway demo
```

From a source checkout:

```bash
make demo
```

The demo uses a temporary home, installs the local binaries with `--skip-launchctl`, writes a temporary `demo-tools` overlay, refreshes cached capabilities, starts the daemon directly, and exercises `initialize`, `tools/list`, and `tools/call` through the generated shim.

Use `mcpgateway demo --keep` to keep the temporary files for inspection.

## First Real Server

Install a package you trust, then refresh capabilities:

```bash
mcpgateway add @modelcontextprotocol/server-filesystem --name filesystem --arg="$HOME/Documents"
mcpgateway refresh all
mcpgateway ps
```

`mcpgateway add` writes a single-server overlay under `~/.mcp-gateway/config/servers.d/`. `mcpgateway refresh all` rebuilds the capability cache and reloads a running gateway service so clients see the new server.

## Client Integration

Client configs point to:

```text
~/.mcp-gateway/mcps/<server>
```

The shim calls `mcp-gateway-bridge`, which reads `~/.mcp-gateway/run/state.json`; client configs do not hard-code daemon ports and do not add a visible aggregate `mcp-gateway` MCP entry.

```bash
mcpgateway import-clients --write
mcpgateway apply-configs --clients codex,claude-code
mcpgateway doctor
```

Supported adapters are optional conveniences: Codex, Claude Code, Claude Desktop, VS Code, and Antigravity.

## Troubleshooting

```bash
mcpgateway doctor
mcpgateway status
mcpgateway stop <server>
mcpgateway stop all
mcpgateway refresh all
```

- Gateway not running: inspect `mcpgateway doctor`, then run `brew services restart mcp-gateway` for Homebrew installs or rerun `mcpgateway install` for standalone installs.
- Capability cache missing or stale: run `mcpgateway refresh all`.
- Client cannot find a shim: rerun `mcpgateway apply-configs --clients <client>` and verify the shim exists.
- Browser MCP server using the wrong browser: stop it immediately and reprovision Chrome for Testing explicitly with `mcpgateway install --provision chrome-for-testing`.

More detail is in [docs/troubleshooting.md](docs/troubleshooting.md).

## Architecture

```mermaid
flowchart LR
  Client["AI client"] --> Shim["~/.mcp-gateway/mcps/server-a"]
  Shim --> Bridge["mcp-gateway-bridge"]
  Bridge --> State["run/state.json"]
  State --> Daemon["mcp-gateway daemon"]
  Daemon --> Cache["cache/mcp_manifest_cache.json"]
  Daemon --> Backend["server-a stdio backend"]
  Service["Homebrew or launchd user service"] --> Daemon
```

The binaries are:

- `mcp-gateway`: local supervisor daemon.
- `mcp-gateway-bridge`: per-server stdio bridge executed by shims.
- `mcpgateway`: installer, config helper, capability refresher, doctor, status, and demo CLI.

Configuration lives at `~/.mcp-gateway/config/gateway.yaml`; user-installed server overlays live in `~/.mcp-gateway/config/servers.d/*.yaml`.

See [docs/architecture.md](docs/architecture.md), [docs/configuration.md](docs/configuration.md), and [docs/security.md](docs/security.md).

## Development

```bash
make check
make smoke
make demo
make ci
```

Useful one-offs:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --workspace --bins
```

The project is GitHub-release-only for now. Crates are marked `publish = false` until the crates.io packaging strategy is intentionally revisited.

## Limitations

- macOS is the supported host-native install target.
- Backend support is stdio-focused today.
- Release binaries are not signed or notarized yet.
- No default MCP servers or managed AI clients are installed.
