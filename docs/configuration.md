# Configuration

The primary config file is `~/.mcp-gateway/config/gateway.yaml`. Managed server overlays live in `~/.mcp-gateway/config/servers.d/*.yaml`.

## gateway.yaml

```yaml
listen: "127.0.0.1:0"

defaults:
  idle_timeout_seconds: 1200
  startup_timeout_seconds: 30
  request_timeout_seconds: 60
  restart_policy: "on_failure"

servers: {}

groups:
  coding:
    servers: []
    expose_tools: true
    expose_prompts: true
    expose_resources: true

clients:
  default_group: "coding"
  server_name: "mcp-gateway"
  managed_clients: []
```

`listen` must be loopback-only. `0.0.0.0` and other non-loopback addresses are rejected.

## Servers

Only stdio backends are supported today.

```yaml
servers:
  docs:
    transport: stdio
    command: "docs-mcp-server"
    args: ["--stdio"]
    env:
      DOCS_TOKEN: "${DOCS_TOKEN}"
    lazy: true
    singleton: false
    dangerous: false
    path_allowlist: []
    idle_timeout_seconds: 1200
    startup_timeout_seconds: 30
    request_timeout_seconds: 60
    restart_policy: "on_failure"
```

- `lazy`: when true, the backend starts only when needed.
- `singleton`: when true, the backend should run as one shared process.
- `dangerous`: documents that a backend can inspect or mutate sensitive local state.
- `path_allowlist`: metadata for operator review; it is not a filesystem sandbox.
- `restart_policy`: `never`, `on_failure`, or `always`.

## servers.d Overlays

`mcpgateway add` and `mcpgateway import-clients --write` write one file per server:

```yaml
id: docs
display_name: "docs"
enabled: true
group_memberships:
  - coding
runtime:
  transport: stdio
  command: "docs-mcp-server"
  args: ["--stdio"]
  lazy: true
  idle_timeout_seconds: 1200
  startup_timeout_seconds: 30
  request_timeout_seconds: 60
env:
  DOCS_MODE: "local"
secrets:
  - name: DOCS_TOKEN
    required: true
```

Disabled overlays are parsed but not merged. Overlays cannot shadow servers already defined in `gateway.yaml`. The canonical timeout fields are `idle_timeout_seconds`, `startup_timeout_seconds`, and `request_timeout_seconds`; older `*_secs` overlay fields still load for compatibility.

## Groups and Clients

Groups define the server set exposed to a managed client. Client configs are only changed when requested via `--clients` or `clients.managed_clients`.

Supported client adapters:

- `codex`
- `claude-code`
- `claude-desktop`
- `vscode`
- `antigravity`

## Environment

`${NAME}` placeholders expand from the process environment after `~/.mcp-gateway/env` is loaded. The env file is owner-only because it can contain secrets.

## Limits

The `limits` section controls max clients, message bytes, batch items, pending backend requests, session events, log retention, and restart windows. Keep limits conservative for local developer machines.
