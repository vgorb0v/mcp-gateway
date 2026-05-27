.PHONY: help fmt fmt-check clippy test build build-debug install install-debug install-path deploy check ci smoke demo nextest refresh ps clean

CARGO ?= cargo
RELEASE_CLI := target/release/mcpgateway
DEBUG_CLI := target/debug/mcpgateway

help:
	@printf '%s\n' \
		'Targets:' \
		'  make build         Build release binaries' \
		'  make install       Build release binaries and install locally' \
		'  make deploy        Format-check, test, build release, install locally' \
		'  make build-debug   Build debug binaries for fast iteration' \
		'  make install-debug Build debug binaries and install locally' \
		'  make install-path  Install locally and add ~/.mcp-gateway/bin to PATH' \
		'  make check         Run formatting, clippy, and tests' \
		'  make ci            Run the local CI gate' \
		'  make smoke         Run the temp-home installer smoke test' \
		'  make demo          Run the 90-second local demo proof path' \
		'  make nextest       Run tests with cargo-nextest when installed' \
		'  make refresh       Refresh cached MCP capabilities' \
		'  make ps            Show gateway backend status' \
		'  make test          Run workspace tests' \
		'  make fmt           Format Rust code' \
		'  make fmt-check     Check Rust formatting' \
		'  make clippy        Run clippy with warnings denied' \
		'  make clean         Remove Cargo build artifacts'

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

test:
	$(CARGO) test --workspace

check: fmt-check clippy test

ci: check build smoke

smoke:
	scripts/smoke-test.sh

demo: build
	$(RELEASE_CLI) demo

nextest:
	$(CARGO) nextest run --workspace

build:
	$(CARGO) build --release --workspace --bins

build-debug:
	$(CARGO) build --workspace --bins

install: build
	$(RELEASE_CLI) install

install-debug: build-debug
	$(DEBUG_CLI) install

install-path: build
	$(RELEASE_CLI) install --add-to-path

deploy: fmt-check clippy test install

refresh:
	mcpgateway refresh-capabilities all

ps:
	mcpgateway ps

clean:
	$(CARGO) clean
