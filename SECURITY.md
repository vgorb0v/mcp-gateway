# Security Policy

MCP Gateway is a local macOS user service. It is not a remote MCP proxy and should not expose a public network API.

## Reporting a Vulnerability

Please report suspected vulnerabilities through GitHub Security Advisories for this repository. If advisories are unavailable, open a minimal GitHub issue asking for a private disclosure channel and do not include exploit details.

Include:

- Affected version or commit.
- Steps to reproduce.
- Impact and any required local permissions.
- Relevant logs with secrets, cookies, tokens, and private paths removed.

## Supported Versions

Security fixes target the latest released version and `main`. Release binaries are distributed through GitHub Releases.

## Security Model

The local control plane binds to loopback, shims read the hidden daemon URL from `~/.mcp-gateway/run/state.json`, and browser automation backends are opt-in. See [docs/security.md](docs/security.md) for the full model.
