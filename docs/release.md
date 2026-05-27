# Release Process

MCP Gateway currently publishes GitHub release artifacts only. Crates are marked `publish = false` until a crates.io strategy is explicitly chosen.

The repository pins the stable Rust channel in `rust-toolchain.toml`. A numeric MSRV is not declared yet; add one only after verifying that exact toolchain in CI.

## Pre-release Checklist

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --workspace --bins
scripts/smoke-test.sh
```

Also run the publication blocker scan from the PR checklist and investigate any non-migration matches.

## Version Bump

Update versions in:

- `crates/gateway/Cargo.toml`
- `crates/bridge/Cargo.toml`
- `crates/gatewayctl/Cargo.toml`
- `CHANGELOG.md`

## Tagging

```bash
git tag vX.Y.Z
git push origin vX.Y.Z
```

The release workflow builds:

- `aarch64-apple-darwin`
- `x86_64-apple-darwin`

Each archive contains `mcp-gateway`, `mcp-gateway-bridge`, `mcpgateway`, `README.md`, and `LICENSE`. `SHA256SUMS` is generated for the archives.

## Signing and Notarization

Release binaries are not signed or notarized yet. Do not claim otherwise in release notes.

Future work may add signed artifacts, notarization, and a Homebrew tap.

## Rollback

If a release is bad, publish a new patch release. Existing users can rerun `mcpgateway install-native` with the replacement binaries; config and backend overlays are preserved.
