.PHONY: help fmt fmt-check clippy test build build-debug install install-debug install-path deploy refresh ps clean

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

build:
	$(CARGO) build --release --workspace --bins

build-debug:
	$(CARGO) build --workspace --bins

install: build
	$(RELEASE_CLI) install-native

install-debug: build-debug
	$(DEBUG_CLI) install-native

install-path: build
	$(RELEASE_CLI) install-native --add-to-path

deploy: fmt-check clippy test install

refresh:
	mcpgateway refresh-capabilities all

ps:
	mcpgateway ps

clean:
	$(CARGO) clean
