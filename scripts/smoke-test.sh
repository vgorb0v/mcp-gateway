#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp_home="$(mktemp -d)"
trap 'rm -rf "$tmp_home"' EXIT

cd "$repo_root"
cargo build --release --workspace --bins

target/release/mcpgateway install \
  --home "$tmp_home" \
  --skip-launchctl \
  --skip-pnpm-install \
  --no-path-prompt

test -x "$tmp_home/.mcp-gateway/bin/mcp-gateway"
test -x "$tmp_home/.mcp-gateway/bin/mcp-gateway-bridge"
test -x "$tmp_home/.mcp-gateway/bin/mcpgateway"
test -f "$tmp_home/.mcp-gateway/config/gateway.yaml"
test -d "$tmp_home/.mcp-gateway/mcps"

if grep -R "^  mcp-gateway:" "$tmp_home/.mcp-gateway/config/gateway.yaml" >/dev/null 2>&1; then
  echo "unexpected default aggregate server in generated config" >&2
  exit 1
fi

target/release/mcpgateway doctor \
  --home "$tmp_home" \
  --skip-launchctl

echo "smoke test completed in $tmp_home"
