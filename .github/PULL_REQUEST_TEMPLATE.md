## Summary

-

## Checks

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cargo build --release --workspace --bins`

## Safety

- [ ] Preserves macOS host-native install behavior.
- [ ] Preserves per-server shims and hidden daemon port.
- [ ] Preserves empty-core fresh installs.
- [ ] Does not add default MCP servers, clients, secrets, or browser sessions.
- [ ] Does not start real backends for cached discovery methods.
