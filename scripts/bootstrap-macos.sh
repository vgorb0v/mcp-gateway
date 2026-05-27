#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "MCP Gateway native install is supported on macOS." >&2
  exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "Rust/Cargo is required. Install Rust with rustup, then rerun this script." >&2
  exit 1
fi

echo "Rust: $(rustc --version)"
echo "Cargo: $(cargo --version)"

if command -v pnpm >/dev/null 2>&1; then
  echo "pnpm: $(pnpm --version)"
else
  echo "pnpm: not installed; only needed for Node-backed MCP server packages"
fi

cargo fetch
echo "Bootstrap complete. Run: make check"
