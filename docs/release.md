# Release Process

MCP Gateway publishes GitHub release artifacts and updates the custom Homebrew tap. Crates are marked `publish = false` until a crates.io strategy is explicitly chosen.

The repository pins Rust `1.95.0` in `rust-toolchain.toml`, workspace package metadata, and CI. Treat that as the current MSRV until intentionally changed.

## Pre-release Checklist

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --workspace --bins
scripts/smoke-test.sh
target/release/mcpgateway demo
```

Also run the publication blocker scan from the PR checklist and investigate any non-migration matches.

## Version Bump

Update the workspace version in `Cargo.toml` and document the release in `CHANGELOG.md`.

## Tagging

```bash
git tag vX.Y.Z
git push origin vX.Y.Z
```

The release workflow builds:

- `aarch64-apple-darwin`
- `x86_64-apple-darwin`

Each archive contains `mcp-gateway`, `mcp-gateway-bridge`, `mcpgateway`, `README.md`, and `LICENSE`. `SHA256SUMS` is generated for the archives.

The tap workflow updates `vgorb0v/homebrew-tap` on published releases. It expects `HOMEBREW_TAP_TOKEN` to have write access to that repository.

## Signing and Notarization

Release binaries are not signed or notarized yet. Do not claim otherwise in release notes.

Future work may add signed artifacts and notarization.

## Rollback

If a release is bad, publish a new patch release. Homebrew users can run `brew upgrade mcp-gateway` and standalone users can rerun `mcpgateway install` with the replacement binaries; config and backend overlays are preserved.
