# Support

MCP Gateway is an open-source macOS developer tool. Community support happens through GitHub issues and discussions once the repository is public.

## Supported

- Source builds on macOS with stable Rust.
- User LaunchAgent installation through `mcpgateway install-native`.
- Stdio MCP backends.
- Per-server shim generation for supported clients.
- Lazy backend startup and capability-cache based discovery.

## Best Effort

- Linux `cargo check` and non-macOS development.
- Third-party MCP server package behavior.
- Client config formats that change outside MCP Gateway.
- Browser MCP workflows beyond the isolation policy documented here.

## Diagnostics

Run:

```bash
mcpgateway doctor
mcpgateway ps
mcpgateway refresh-capabilities all
```

Before sharing diagnostics, remove secrets, local private paths, cookies, auth headers, and browser profile details.
