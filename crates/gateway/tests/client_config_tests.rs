use std::fs;
use std::process::Command;

use mcp_gateway::client_config::{apply_configs, ApplyConfigOptions, ClientKind};

#[test]
fn writes_individual_client_configs_idempotently_and_removes_gateway_entry() {
    let home = tempfile::tempdir().expect("temp home");
    let codex_dir = home.path().join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    fs::write(
        codex_dir.join("config.toml"),
        r#"
model = "gpt-5"

[mcp_servers.mcp-gateway]
command = "old-wrapper"
"#,
    )
    .unwrap();

    let claude = home.path().join(".claude.json");
    fs::write(
        &claude,
        r#"{"mcpServers":{"mcp-gateway":{"command":"old-wrapper"}}, "other": true}"#,
    )
    .unwrap();

    let opts = ApplyConfigOptions {
        clients: vec![
            ClientKind::Codex,
            ClientKind::ClaudeCode,
            ClientKind::Antigravity,
            ClientKind::Vscode,
        ],
        servers: vec![
            "alpha-tools".to_string(),
            "beta-tools".to_string(),
            "local-shell".to_string(),
        ],
        bridge_bin: home.path().join(".mcp-gateway/bin/mcp-gateway-bridge"),
        mcp_dir: home.path().join(".mcp-gateway/mcps"),
        state_file: home.path().join(".mcp-gateway/run/state.json"),
        dedupe: vec!["mcp-gateway".to_string()],
        dry_run: false,
        home: home.path().to_path_buf(),
    };

    let changed_once = apply_configs(opts.clone()).expect("first apply");
    let changed_twice = apply_configs(opts).expect("second apply");

    assert!(!changed_once.is_empty());
    assert!(
        changed_twice.is_empty(),
        "second apply should be idempotent"
    );

    let codex_text = fs::read_to_string(codex_dir.join("config.toml")).unwrap();
    assert!(codex_text.contains("[mcp_servers.alpha-tools]"));
    assert!(codex_text.contains("[mcp_servers.beta-tools]"));
    assert!(codex_text.contains("[mcp_servers.local-shell]"));
    assert!(codex_text.contains("model = \"gpt-5\""));
    assert!(!codex_text.contains("[mcp_servers.mcp-gateway]"));

    let claude_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(claude).unwrap()).unwrap();
    assert!(claude_json["other"].as_bool().unwrap());
    assert!(claude_json["mcpServers"]["beta-tools"].is_object());
    assert!(claude_json["mcpServers"]["local-shell"].is_object());
    assert!(claude_json["mcpServers"]["mcp-gateway"].is_null());
    assert!(
        !fs::read_to_string(home.path().join(".gemini/antigravity/mcp_config.json"))
            .unwrap()
            .contains("serverUrl")
    );
    assert!(
        !fs::read_to_string(home.path().join(".gemini/antigravity/mcp_config.json"))
            .unwrap()
            .contains("/sessions")
    );

    let vscode_mcp: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            home.path()
                .join("Library/Application Support/Code/User/mcp.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(vscode_mcp["servers"]["beta-tools"].is_object());
    assert_eq!(vscode_mcp["servers"]["beta-tools"]["type"], "stdio");
    assert!(
        vscode_mcp["servers"]["mcp-gateway"].is_null(),
        "gateway parent entry should be removed"
    );

    let vscode_settings: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            home.path()
                .join("Library/Application Support/Code/User/settings.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(vscode_settings["chat.mcp.autostart"], true);

    for server in ["alpha-tools", "beta-tools", "local-shell"] {
        let shim = home.path().join(format!(".mcp-gateway/mcps/{server}"));
        let shim_text = fs::read_to_string(shim).unwrap();
        assert!(shim_text.contains("--server"));
        assert!(shim_text.contains(server));
        assert!(shim_text.contains("--state-file"));
        for forbidden in [
            "docker",
            "open -na",
            "remote-debugging",
            "CHROME_REMOTE_DEBUGGING_URL",
            "9222",
            "--browser-url",
            "CONTAINER",
        ] {
            assert!(
                !shim_text.contains(forbidden),
                "unexpected {forbidden} in {server} shim:\n{shim_text}"
            );
        }
    }
}

#[test]
fn generated_configs_pass_client_identity_via_shim_args() {
    let home = tempfile::tempdir().expect("temp home");

    apply_configs(ApplyConfigOptions {
        clients: vec![
            ClientKind::Codex,
            ClientKind::ClaudeCode,
            ClientKind::Antigravity,
            ClientKind::Vscode,
        ],
        servers: vec!["example-tools".to_string()],
        bridge_bin: home.path().join(".mcp-gateway/bin/mcp-gateway-bridge"),
        mcp_dir: home.path().join(".mcp-gateway/mcps"),
        state_file: home.path().join(".mcp-gateway/run/state.json"),
        dedupe: vec!["mcp-gateway".to_string()],
        dry_run: false,
        home: home.path().to_path_buf(),
    })
    .expect("apply configs");

    let claude: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(home.path().join(".claude.json")).unwrap())
            .unwrap();
    let claude_args = &claude["mcpServers"]["example-tools"]["args"];
    assert_eq!(claude_args[0], "--client");
    assert_eq!(claude_args[1], "claude-code");

    let antigravity: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(home.path().join(".gemini/antigravity/mcp_config.json")).unwrap(),
    )
    .unwrap();
    let antigravity_args = &antigravity["mcpServers"]["example-tools"]["args"];
    assert_eq!(antigravity_args[0], "--client");
    assert_eq!(antigravity_args[1], "antigravity");

    let vscode: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            home.path()
                .join("Library/Application Support/Code/User/mcp.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let vscode_args = &vscode["servers"]["example-tools"]["args"];
    assert_eq!(vscode_args[0], "--client");
    assert_eq!(vscode_args[1], "vscode");

    let codex_text =
        fs::read_to_string(home.path().join(".codex/config.toml")).expect("codex config");
    assert!(
        codex_text.contains("args = [\"--client\", \"codex\"]"),
        "expected codex args line in:\n{codex_text}"
    );
}

#[test]
fn antigravity_gateway_cache_is_removed_when_managed_configs_are_applied() {
    let home = tempfile::tempdir().expect("temp home");
    let stale_cache = home.path().join(".gemini/antigravity/mcp/mcp-gateway");
    fs::create_dir_all(&stale_cache).unwrap();
    fs::write(stale_cache.join("example.echo.json"), "{}").unwrap();

    apply_configs(ApplyConfigOptions {
        clients: vec![ClientKind::Antigravity],
        servers: vec!["example-tools".to_string()],
        bridge_bin: home.path().join(".mcp-gateway/bin/mcp-gateway-bridge"),
        mcp_dir: home.path().join(".mcp-gateway/mcps"),
        state_file: home.path().join(".mcp-gateway/run/state.json"),
        dedupe: vec!["mcp-gateway".to_string()],
        dry_run: false,
        home: home.path().to_path_buf(),
    })
    .expect("apply configs");

    assert!(!stale_cache.exists());
}

#[test]
#[cfg(unix)]
fn generated_server_shim_runs_without_extra_arguments_under_nounset() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempfile::tempdir().expect("temp home");
    let bridge = home.path().join(".mcp-gateway/bin/mcp-gateway-bridge");
    fs::create_dir_all(bridge.parent().unwrap()).unwrap();
    fs::write(
        &bridge,
        r#"#!/usr/bin/env bash
printf '%s\n' "$@" > "$BRIDGE_ARGS_FILE"
"#,
    )
    .unwrap();
    let mut bridge_perms = fs::metadata(&bridge).unwrap().permissions();
    bridge_perms.set_mode(0o755);
    fs::set_permissions(&bridge, bridge_perms).unwrap();

    apply_configs(ApplyConfigOptions {
        clients: vec![],
        servers: vec!["example-tools".to_string()],
        bridge_bin: bridge.clone(),
        mcp_dir: home.path().join(".mcp-gateway/mcps"),
        state_file: home.path().join(".mcp-gateway/run/state.json"),
        dedupe: vec![],
        dry_run: false,
        home: home.path().to_path_buf(),
    })
    .expect("write shim");

    let args_file = home.path().join("bridge-args.txt");
    let shim = home.path().join(".mcp-gateway/mcps/example-tools");
    let output = Command::new(&shim)
        .env("BRIDGE_ARGS_FILE", &args_file)
        .output()
        .expect("run shim");

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let bridge_args = fs::read_to_string(args_file).unwrap();
    assert!(bridge_args.contains("--server\nexample-tools"));
    assert!(bridge_args.contains("--state-file"));
    assert!(!bridge_args.contains("--group"));
}

#[test]
#[cfg(unix)]
fn generated_server_shim_falls_back_to_homebrew_bridge_on_path() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempfile::tempdir().expect("temp home");
    let brew_bin = home.path().join("homebrew/bin");
    let bridge = brew_bin.join("mcp-gateway-bridge");
    fs::create_dir_all(&brew_bin).unwrap();
    fs::write(
        &bridge,
        r#"#!/usr/bin/env bash
printf '%s\n' "$@" > "$BRIDGE_ARGS_FILE"
"#,
    )
    .unwrap();
    let mut bridge_perms = fs::metadata(&bridge).unwrap().permissions();
    bridge_perms.set_mode(0o755);
    fs::set_permissions(&bridge, bridge_perms).unwrap();

    apply_configs(ApplyConfigOptions {
        clients: vec![],
        servers: vec!["example-tools".to_string()],
        bridge_bin: home.path().join(".mcp-gateway/bin/mcp-gateway-bridge"),
        mcp_dir: home.path().join(".mcp-gateway/mcps"),
        state_file: home.path().join(".mcp-gateway/run/state.json"),
        dedupe: vec![],
        dry_run: false,
        home: home.path().to_path_buf(),
    })
    .expect("write shim");

    let args_file = home.path().join("bridge-args.txt");
    let shim = home.path().join(".mcp-gateway/mcps/example-tools");
    let output = Command::new(&shim)
        .env("BRIDGE_ARGS_FILE", &args_file)
        .env("MCP_GATEWAY_BRIDGE", &bridge)
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", brew_bin.to_string_lossy()),
        )
        .output()
        .expect("run shim");

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let bridge_args = fs::read_to_string(args_file).unwrap();
    assert!(bridge_args.contains("--server\nexample-tools"));
    assert!(bridge_args.contains("--state-file"));
}

#[test]
fn empty_client_and_server_selection_writes_no_client_configs_or_shims() {
    let home = tempfile::tempdir().expect("temp home");

    let changed = apply_configs(ApplyConfigOptions {
        clients: vec![],
        servers: vec![],
        bridge_bin: home.path().join(".mcp-gateway/bin/mcp-gateway-bridge"),
        mcp_dir: home.path().join(".mcp-gateway/mcps"),
        state_file: home.path().join(".mcp-gateway/run/state.json"),
        dedupe: vec!["mcp-gateway".to_string()],
        dry_run: false,
        home: home.path().to_path_buf(),
    })
    .expect("apply empty configs");

    assert!(changed.is_empty());
    assert!(!home.path().join(".codex/config.toml").exists());
    assert!(!home.path().join(".claude.json").exists());
    assert!(!home.path().join(".mcp-gateway/mcps").exists());
}
