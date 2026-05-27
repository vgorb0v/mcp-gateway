class McpGateway < Formula
  desc "Host-native macOS supervisor for lazy-started MCP servers"
  homepage "https://github.com/vgorb0v/mcp-gateway"
  url "https://github.com/vgorb0v/mcp-gateway.git", branch: "main"
  version "0.1.0"
  license "MIT"
  head "https://github.com/vgorb0v/mcp-gateway.git", branch: "main"

  depends_on "rust" => :build
  depends_on :macos

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/gateway")
    system "cargo", "install", *std_cargo_args(path: "crates/bridge")
    system "cargo", "install", *std_cargo_args(path: "crates/gatewayctl")

    (libexec/"mcp-gateway-service").write <<~EOS
      #!/usr/bin/env bash
      set -euo pipefail

      home="${HOME:-}"
      if [[ -z "$home" || "$home" == /private/tmp/mcp-gateway-postinstall-* ]]; then
        home="$(/usr/bin/dscl . -read "/Users/$(/usr/bin/id -un)" NFSHomeDirectory | /usr/bin/awk '{print $2}')"
      fi

      "#{opt_bin}/mcpgateway" install --homebrew --skip-pnpm-install --no-path-prompt --home "$home"
      exec "#{opt_bin}/mcp-gateway" serve \\
        --config "$home/.mcp-gateway/config/gateway.yaml" \\
        --env-file "$home/.mcp-gateway/env" \\
        --state-file "$home/.mcp-gateway/run/state.json" \\
        --capability-cache-file "$home/.mcp-gateway/cache/mcp_manifest_cache.json"
    EOS
    chmod 0755, libexec/"mcp-gateway-service"
  end

  def post_install
    system bin/"mcpgateway", "install", "--homebrew", "--skip-pnpm-install", "--no-path-prompt"
  end

  service do
    run opt_libexec/"mcp-gateway-service"
    run_type :immediate
    keep_alive true
    working_dir "#{Dir.home}/.mcp-gateway"
    log_path "#{Dir.home}/.mcp-gateway/logs/gateway.out.log"
    error_log_path "#{Dir.home}/.mcp-gateway/logs/gateway.err.log"
    environment_variables HOME: Dir.home,
                          PATH: "#{Dir.home}/.mcp-gateway/mcp-backends/node_modules/.bin:#{std_service_path_env}"
  end

  def caveats
    <<~EOS
      Start the user service with:
        brew services start mcp-gateway

      Local gateway state lives under:
        ~/.mcp-gateway
    EOS
  end

  test do
    system bin/"mcp-gateway", "--version"
    system bin/"mcp-gateway-bridge", "--version"
    system bin/"mcpgateway", "--version"
  end
end
