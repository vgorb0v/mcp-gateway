use std::fs;

use mcp_gateway::config::Config;

#[test]
fn loads_yaml_expands_env_and_preserves_explicit_server_flags() {
    std::env::set_var("SERVICE_TOKEN", "service_test_token");

    let cfg = Config::load_str(
        r#"
listen: "127.0.0.1:7777"
defaults:
  idle_timeout_seconds: 300
  startup_timeout_seconds: 30
  request_timeout_seconds: 60
  restart_policy: "on_failure"
servers:
  browser-tools:
    transport: stdio
    command: "npx"
    args:
      - "-y"
      - "chrome-devtools-mcp@latest"
      - "--headless=true"
      - "--isolated=true"
      - "--viewport=1280x720"
      - "--experimentalPageIdRouting"
      - "--no-usage-statistics"
      - "--no-performance-crux"
    lazy: true
    singleton: true
    dangerous: true
    path_allowlist: ["/tmp/browser-data"]
  code-host:
    transport: stdio
    command: "npx"
    args: ["-y", "some-code-host-mcp"]
    env:
      SERVICE_API_TOKEN: "${SERVICE_TOKEN}"
    lazy: true
groups:
  coding:
    servers: ["browser-tools", "code-host"]
clients:
  default_group: "coding"
  server_name: "mcp-gateway"
"#,
    )
    .expect("valid config");

    assert_eq!(cfg.listen, "127.0.0.1:7777");
    let chrome_args = &cfg.servers["browser-tools"].args;
    assert!(chrome_args.contains(&"--headless=true".to_string()));
    assert!(chrome_args.contains(&"--isolated=true".to_string()));
    assert!(chrome_args.contains(&"--viewport=1280x720".to_string()));
    assert!(chrome_args.contains(&"--experimentalPageIdRouting".to_string()));
    assert!(chrome_args.contains(&"--no-usage-statistics".to_string()));
    assert!(chrome_args.contains(&"--no-performance-crux".to_string()));
    assert!(chrome_args.iter().all(|arg| !arg.contains("9222")));
    assert!(chrome_args.iter().all(|arg| !arg.contains("browser-url")));
    assert_eq!(
        cfg.servers["code-host"].env["SERVICE_API_TOKEN"],
        "service_test_token"
    );
    assert!(cfg.servers["browser-tools"].singleton());
    assert!(cfg.servers["browser-tools"].dangerous());
    assert_eq!(
        cfg.servers["browser-tools"].path_allowlist,
        vec![std::path::PathBuf::from("/tmp/browser-data")]
    );
}

#[test]
fn non_loopback_listen_addresses_are_rejected() {
    let err = Config::load_str(
        r#"
listen: "0.0.0.0:7777"
servers:
  alpha:
    transport: stdio
    command: "pnpm"
groups:
  coding:
    servers: ["alpha"]
"#,
    )
    .expect_err("non-loopback listener should fail");

    assert!(err.to_string().contains("loopback"));
}

#[test]
fn invalid_group_reference_fails_validation() {
    let err = Config::load_str(
        r#"
servers:
  alpha:
    transport: stdio
    command: "npx"
groups:
  coding:
    servers: ["alpha", "missing"]
"#,
    )
    .expect_err("missing server should fail");

    assert!(err.to_string().contains("missing"));
}

#[test]
fn server_ids_must_be_path_safe() {
    let err = Config::load_str(
        r#"
servers:
  "../escaped":
    transport: stdio
    command: "npx"
groups:
  coding:
    servers: ["../escaped"]
"#,
    )
    .expect_err("path traversal server id should fail");

    assert!(err.to_string().contains("path-safe"));
}

#[test]
fn server_names_and_packages_do_not_trigger_implicit_safety_rules() {
    let cfg = Config::load_str(
        r#"
servers:
  chrome-devtools:
    transport: stdio
    command: "npx"
    args: ["-y", "chrome-devtools-mcp@latest"]
  filesystem:
    transport: stdio
    command: "mcp-server-filesystem"
    args: ["/tmp"]
groups:
  coding:
    servers: ["chrome-devtools", "filesystem"]
"#,
    )
    .expect("server names and packages are not special");

    assert!(!cfg.servers["chrome-devtools"].singleton());
    assert!(!cfg.servers["chrome-devtools"].dangerous());
    assert!(cfg.servers["filesystem"].path_allowlist.is_empty());
}

#[test]
fn limits_parse_with_defaults_and_explicit_values() {
    let defaults = Config::load_str(
        r#"
servers:
  alpha:
    transport: stdio
    command: "pnpm"
groups:
  coding:
    servers: ["alpha"]
"#,
    )
    .expect("default limits");

    assert_eq!(defaults.limits.max_clients, 16);
    assert_eq!(defaults.listen, "127.0.0.1:0");
    assert_eq!(defaults.defaults.idle_timeout_seconds, 1200);
    assert_eq!(defaults.limits.max_message_bytes, 8_388_608);
    assert_eq!(defaults.limits.max_batch_items, 32);
    assert_eq!(defaults.limits.max_pending_requests_per_backend, 128);
    assert_eq!(defaults.limits.backend_stdin_queue, 64);
    assert_eq!(defaults.limits.max_session_events, 128);
    assert_eq!(defaults.limits.session_ttl_seconds, 900);
    assert_eq!(defaults.limits.max_log_entries_per_server, 256);
    assert_eq!(defaults.limits.max_log_line_bytes, 8192);
    assert_eq!(defaults.limits.backend_notification_broadcast, 128);
    assert_eq!(defaults.limits.gateway_notification_broadcast, 256);
    assert_eq!(defaults.limits.max_restart_attempts, 3);
    assert_eq!(defaults.limits.restart_window_seconds, 60);
    assert!(defaults.clients.managed_clients.is_empty());

    let explicit = Config::load_str(
        r#"
limits:
  max_clients: 2
  max_message_bytes: 4096
  max_batch_items: 4
  max_pending_requests_per_backend: 3
  backend_stdin_queue: 2
  max_session_events: 5
  session_ttl_seconds: 7
  max_log_entries_per_server: 11
  max_log_line_bytes: 13
  backend_notification_broadcast: 17
  gateway_notification_broadcast: 19
  max_restart_attempts: 1
  restart_window_seconds: 23
servers:
  beta:
    transport: stdio
    command: "pnpm"
groups:
  coding:
    servers: ["beta"]
"#,
    )
    .expect("explicit limits");

    assert_eq!(explicit.limits.max_clients, 2);
    assert_eq!(explicit.limits.max_message_bytes, 4096);
    assert_eq!(explicit.limits.max_batch_items, 4);
    assert_eq!(explicit.limits.max_pending_requests_per_backend, 3);
    assert_eq!(explicit.limits.backend_stdin_queue, 2);
    assert_eq!(explicit.limits.max_session_events, 5);
    assert_eq!(explicit.limits.session_ttl_seconds, 7);
    assert_eq!(explicit.limits.max_log_entries_per_server, 11);
    assert_eq!(explicit.limits.max_log_line_bytes, 13);
    assert_eq!(explicit.limits.backend_notification_broadcast, 17);
    assert_eq!(explicit.limits.gateway_notification_broadcast, 19);
    assert_eq!(explicit.limits.max_restart_attempts, 1);
    assert_eq!(explicit.limits.restart_window_seconds, 23);
}

#[test]
fn native_empty_core_config_renders_from_typed_defaults() {
    let cfg = Config::native_empty_core();
    assert!(cfg.servers.is_empty());
    assert_eq!(cfg.groups["coding"].servers, Vec::<String>::new());
    assert!(cfg.clients.managed_clients.is_empty());
    assert_eq!(cfg.limits.max_clients, 128);
    assert_eq!(cfg.limits.session_ttl_seconds, 300);

    let rendered = mcp_gateway::config::render_native_empty_core_yaml().unwrap();
    assert!(rendered.contains("servers: {}"), "{rendered}");
    assert!(rendered.contains("managed_clients: []"), "{rendered}");
    assert!(rendered.contains("max_clients: 128"), "{rendered}");
    let round_trip = Config::load_str(&rendered).expect("native config round trips");
    assert_eq!(round_trip, cfg);
}

#[test]
fn zero_limits_are_rejected() {
    let err = Config::load_str(
        r#"
limits:
  max_clients: 0
servers:
  alpha:
    transport: stdio
    command: "pnpm"
groups:
  coding:
    servers: ["alpha"]
"#,
    )
    .expect_err("zero max_clients should fail");

    assert!(err.to_string().contains("limits.max_clients"));
}

#[test]
fn server_env_preserves_node_options_and_node_env() {
    let yaml = r#"
listen: "127.0.0.1:7777"
servers:
  example:
    transport: stdio
    command: "example-mcp"
    args: []
    env:
      NODE_ENV: "production"
      NODE_OPTIONS: "--max-old-space-size=128 --max-semi-space-size=8"
    lazy: true
"#;
    let config = mcp_gateway::config::Config::load_str(yaml).expect("config loads");
    let server = config.servers.get("example").expect("server present");
    assert_eq!(
        server.env.get("NODE_ENV").map(String::as_str),
        Some("production")
    );
    assert_eq!(
        server.env.get("NODE_OPTIONS").map(String::as_str),
        Some("--max-old-space-size=128 --max-semi-space-size=8"),
    );
}

#[test]
fn servers_d_overlay_merges_managed_server_and_joins_group() {
    let tmp = tempfile::tempdir().expect("config dir");
    let gateway_yaml = tmp.path().join("gateway.yaml");
    fs::write(
        &gateway_yaml,
        r#"
listen: "127.0.0.1:0"
servers:
  builtin:
    transport: stdio
    command: "/bin/true"
    args: []
    lazy: true
groups:
  coding:
    servers: ["builtin"]
clients:
  default_group: "coding"
"#,
    )
    .unwrap();

    let overlay_dir = tmp.path().join("servers.d");
    fs::create_dir_all(&overlay_dir).unwrap();
    fs::write(
        overlay_dir.join("docs-helper.yaml"),
        r#"
id: docs-helper
enabled: true
group_memberships:
  - coding
runtime:
  command: pnpm
  args:
    - exec
    - docs-helper-mcp
  lazy: true
  idle_timeout_secs: 300
env:
  DOCS_HELPER_MODE: default
"#,
    )
    .unwrap();

    let cfg = Config::load_file(&gateway_yaml).expect("config loads");
    let docs_helper = cfg.servers.get("docs-helper").expect("overlay merged");
    assert_eq!(docs_helper.command, "pnpm");
    assert_eq!(docs_helper.args, vec!["exec", "docs-helper-mcp"]);
    assert_eq!(docs_helper.idle_timeout_seconds, Some(300));
    assert_eq!(
        docs_helper.env.get("DOCS_HELPER_MODE").map(String::as_str),
        Some("default")
    );
    let coding = cfg.groups.get("coding").expect("coding group");
    assert!(
        coding.servers.iter().any(|s| s == "docs-helper"),
        "docs-helper should be added to coding: {:?}",
        coding.servers
    );
}

#[test]
fn servers_d_overlay_accepts_canonical_timeout_seconds_and_legacy_secs_aliases() {
    let canonical = Config::load_str(
        r#"
servers:
  builtin:
    transport: stdio
    command: "/bin/true"
groups:
  coding:
    servers: ["builtin"]
"#,
    )
    .unwrap();

    let text = r#"
id: canonical
runtime:
  command: pnpm
  idle_timeout_seconds: 11
  startup_timeout_seconds: 12
  request_timeout_seconds: 13
"#;
    let overlay: mcp_gateway::config::ManagedServerOverlay =
        serde_yaml::from_str(text).expect("canonical seconds parse");
    let server = overlay.to_server_config().expect("server");
    assert_eq!(server.idle_timeout_seconds, Some(11));
    assert_eq!(server.startup_timeout_seconds, Some(12));
    assert_eq!(server.request_timeout_seconds, Some(13));

    let text = r#"
id: legacy
runtime:
  command: pnpm
  idle_timeout_secs: 21
  startup_timeout_secs: 22
  request_timeout_secs: 23
"#;
    let overlay: mcp_gateway::config::ManagedServerOverlay =
        serde_yaml::from_str(text).expect("legacy secs parse");
    let server = overlay.to_server_config().expect("server");
    assert_eq!(server.idle_timeout_seconds, Some(21));
    assert_eq!(server.startup_timeout_seconds, Some(22));
    assert_eq!(server.request_timeout_seconds, Some(23));
    assert!(canonical.validate().is_ok());
}

#[test]
fn servers_d_overlay_skips_disabled_entries() {
    let tmp = tempfile::tempdir().expect("config dir");
    let gateway_yaml = tmp.path().join("gateway.yaml");
    fs::write(
        &gateway_yaml,
        r#"
listen: "127.0.0.1:0"
servers:
  builtin:
    transport: stdio
    command: "/bin/true"
    args: []
    lazy: true
groups:
  coding:
    servers: ["builtin"]
clients:
  default_group: "coding"
"#,
    )
    .unwrap();
    let overlay_dir = tmp.path().join("servers.d");
    fs::create_dir_all(&overlay_dir).unwrap();
    fs::write(
        overlay_dir.join("dormant.yaml"),
        r#"
id: dormant
enabled: false
runtime:
  command: pnpm
  args: ["exec", "dormant-mcp"]
"#,
    )
    .unwrap();

    let cfg = Config::load_file(&gateway_yaml).expect("config loads");
    assert!(!cfg.servers.contains_key("dormant"));
}

#[test]
fn servers_d_overlay_refuses_to_shadow_a_builtin_server() {
    let tmp = tempfile::tempdir().expect("config dir");
    let gateway_yaml = tmp.path().join("gateway.yaml");
    fs::write(
        &gateway_yaml,
        r#"
listen: "127.0.0.1:0"
servers:
  existing:
    transport: stdio
    command: "/bin/true"
    args: []
    lazy: true
groups:
  coding:
    servers: ["existing"]
clients:
  default_group: "coding"
"#,
    )
    .unwrap();
    let overlay_dir = tmp.path().join("servers.d");
    fs::create_dir_all(&overlay_dir).unwrap();
    fs::write(
        overlay_dir.join("existing.yaml"),
        r#"
id: existing
enabled: true
runtime:
  command: /tmp/imposter
"#,
    )
    .unwrap();

    let err = Config::load_file(&gateway_yaml).expect_err("must refuse shadow");
    let message = format!("{err}");
    assert!(
        message.contains("existing") && message.contains("already configured"),
        "expected shadow conflict message, got: {message}"
    );
}
