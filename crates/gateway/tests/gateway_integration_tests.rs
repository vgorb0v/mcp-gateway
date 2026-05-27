use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use mcp_gateway::capabilities::{CachedServerCapabilities, CapabilityStore};
use mcp_gateway::config::{
    Config, GroupConfig, LimitsConfig, RestartPolicy, ServerConfig, TransportKind,
};
use mcp_gateway::http;
use mcp_gateway::jsonrpc::{JsonRpcId, JsonRpcNotification, JsonRpcRequest};
use mcp_gateway::registry::BackendRegistry;
use mcp_gateway::sessions::{SessionManager, SessionTarget};
use serde_json::json;
use tower::ServiceExt;

fn fake_command() -> String {
    if let Some(path) = option_env!("CARGO_BIN_EXE_fake-mcp-server") {
        return path.to_string();
    }
    let mut target_dir = std::env::current_exe().expect("current test executable");
    target_dir.pop();
    if target_dir.file_name().and_then(|name| name.to_str()) == Some("deps") {
        target_dir.pop();
    }
    let mut example = target_dir.join("examples/fake-mcp-server");
    if cfg!(windows) {
        example.set_extension("exe");
    }
    example.to_string_lossy().to_string()
}

fn test_config() -> Config {
    let mut servers = BTreeMap::new();
    servers.insert(
        "github".to_string(),
        ServerConfig {
            transport: TransportKind::Stdio,
            command: fake_command(),
            args: vec![],
            env: BTreeMap::from([("FAKE_MCP_NAME".to_string(), "github".to_string())]),
            lazy: Some(true),
            singleton: Some(true),
            dangerous: Some(false),
            path_allowlist: vec![],
            idle_timeout_seconds: Some(1),
            startup_timeout_seconds: Some(5),
            request_timeout_seconds: Some(5),
            restart_policy: Some(RestartPolicy::OnFailure),
        },
    );
    servers.insert(
        "filesystem".to_string(),
        ServerConfig {
            transport: TransportKind::Stdio,
            command: fake_command(),
            args: vec![],
            env: BTreeMap::from([("FAKE_MCP_NAME".to_string(), "filesystem".to_string())]),
            lazy: Some(true),
            singleton: Some(true),
            dangerous: Some(false),
            path_allowlist: vec!["/tmp/mcp-gateway-fixture".into()],
            idle_timeout_seconds: Some(1),
            startup_timeout_seconds: Some(5),
            request_timeout_seconds: Some(5),
            restart_policy: Some(RestartPolicy::OnFailure),
        },
    );
    let groups = BTreeMap::from([(
        "coding".to_string(),
        GroupConfig {
            servers: vec!["github".to_string(), "filesystem".to_string()],
            expose_tools: true,
            expose_prompts: true,
            expose_resources: true,
        },
    )]);
    Config {
        listen: "127.0.0.1:7777".to_string(),
        defaults: Default::default(),
        servers,
        groups,
        clients: Default::default(),
        limits: Default::default(),
        metrics: Default::default(),
    }
}

fn test_config_with_limits(limits: LimitsConfig) -> Config {
    let mut cfg = test_config();
    cfg.limits = limits;
    cfg
}

fn seed_capability_cache(
    path: &std::path::Path,
    cfg: &Config,
    servers: &[&str],
) -> CapabilityStore {
    let store = CapabilityStore::new(path.to_path_buf()).expect("capability store");
    for server in servers {
        let server_config = cfg.servers.get(*server).expect("server config");
        store
            .upsert_server(
                server,
                CachedServerCapabilities::from_parts(
                    server,
                    server_config,
                    vec![
                        json!({ "name": "echo", "description": "Echo arguments", "inputSchema": { "type": "object" }, "execution": { "taskSupport": "forbidden" } }),
                        json!({ "name": "search_repositories", "description": "Search repos", "inputSchema": { "type": "object" } }),
                    ],
                    vec![json!({ "name": "explain" })],
                    vec![json!({ "uri": format!("fake://{server}/one"), "name": "one" })],
                ),
            )
            .expect("seed cache");
    }
    store
}

#[tokio::test]
async fn cached_direct_tools_list_returns_unprefixed_tools_without_starting_backend() {
    let cfg = test_config();
    let temp = tempfile::tempdir().expect("temp dir");
    let cache_file = temp.path().join("mcp_manifest_cache.json");
    seed_capability_cache(&cache_file, &cfg, &["github"]);
    let registry =
        BackendRegistry::new_with_capability_cache(cfg, cache_file).expect("cached registry");

    let response = registry
        .handle_server_request(
            "github",
            JsonRpcRequest::new(JsonRpcId::Number(1), "tools/list", None),
        )
        .await
        .expect("cached tools/list");

    let result = response.result.unwrap();
    let tools = result["tools"].as_array().unwrap();
    let names: Vec<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"echo".to_string()));
    assert!(names.iter().all(|name| !name.contains('.')));
    assert!(registry
        .server_status()
        .await
        .iter()
        .all(|server| server.state == "stopped"));
}

#[tokio::test]
async fn cached_group_tools_list_prefixes_tools_without_starting_backends() {
    let cfg = test_config();
    let temp = tempfile::tempdir().expect("temp dir");
    let cache_file = temp.path().join("mcp_manifest_cache.json");
    seed_capability_cache(&cache_file, &cfg, &["github", "filesystem"]);
    let registry =
        BackendRegistry::new_with_capability_cache(cfg, cache_file).expect("cached registry");

    let response = registry
        .handle_group_request(
            "coding",
            JsonRpcRequest::new(JsonRpcId::Number(1), "tools/list", None),
        )
        .await
        .expect("cached group tools/list");

    let result = response.result.unwrap();
    let tools = result["tools"].as_array().unwrap();
    let names: Vec<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"github.echo".to_string()));
    assert!(names.contains(&"filesystem.search_repositories".to_string()));
    assert!(tools.iter().all(|tool| tool.get("execution").is_none()));
    assert!(registry
        .server_status()
        .await
        .iter()
        .all(|server| server.state == "stopped"));
}

#[tokio::test]
async fn missing_capability_cache_returns_mcp_error_without_starting_backend() {
    let cfg = test_config();
    let temp = tempfile::tempdir().expect("temp dir");
    let registry =
        BackendRegistry::new_with_capability_cache(cfg, temp.path().join("missing.json"))
            .expect("cached registry");

    let response = registry
        .handle_server_request(
            "github",
            JsonRpcRequest::new(JsonRpcId::Number(7), "tools/list", None),
        )
        .await
        .expect("missing cache should be JSON-RPC error");

    let error = response.error.expect("error");
    assert_eq!(error.code, -32000);
    assert!(error.message.contains("mcpgateway refresh-capabilities"));
    assert!(registry
        .server_status()
        .await
        .iter()
        .all(|server| server.state == "stopped"));
}

#[tokio::test]
async fn refresh_capabilities_discovers_cache_and_stops_temporary_backend() {
    let cfg = test_config();
    let temp = tempfile::tempdir().expect("temp dir");
    let cache_file = temp.path().join("mcp_manifest_cache.json");
    let registry =
        BackendRegistry::new_with_capability_cache(cfg, cache_file.clone()).expect("registry");

    registry
        .refresh_server_capabilities("github")
        .await
        .expect("refresh");

    assert!(registry
        .server_status()
        .await
        .iter()
        .all(|server| server.state == "stopped"));
    let reloaded = CapabilityStore::new(cache_file).expect("reload");
    let github = reloaded.server("github").expect("github cache");
    let names: Vec<_> = github
        .tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"echo".to_string()));
    assert!(names.contains(&"search_repositories".to_string()));
}

#[tokio::test]
async fn direct_server_initialize_reports_individual_server_name() {
    let registry = BackendRegistry::new(test_config()).expect("registry");

    let response = registry
        .handle_server_request(
            "github",
            JsonRpcRequest::new(JsonRpcId::Number(1), "initialize", Some(json!({}))),
        )
        .await
        .expect("initialize");

    assert_eq!(response.result.unwrap()["serverInfo"]["name"], "github");
    assert!(registry
        .server_status()
        .await
        .iter()
        .all(|server| server.state == "stopped"));
}

#[tokio::test]
async fn grouped_tools_list_aggregates_with_prefixes_and_starts_lazily() {
    let registry = BackendRegistry::new(test_config()).expect("registry");
    assert_eq!(registry.server_status().await[0].state, "stopped");

    let response = registry
        .handle_group_request(
            "coding",
            JsonRpcRequest::new(JsonRpcId::Number(1), "tools/list", None),
        )
        .await
        .expect("group response");

    let result = response.result.unwrap();
    let tools = result["tools"].as_array().unwrap();
    let names: Vec<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();

    assert!(names.contains(&"github.echo".to_string()));
    assert!(names.contains(&"filesystem.search_repositories".to_string()));
    assert!(tools.iter().all(|tool| tool.get("execution").is_none()));
    assert!(registry
        .server_status()
        .await
        .iter()
        .all(|server| server.state == "running"));
    registry.shutdown_all().await;
}

#[tokio::test]
async fn idle_backend_shuts_down_after_timeout() {
    let registry = BackendRegistry::new(test_config()).expect("registry");
    registry
        .handle_group_request(
            "coding",
            JsonRpcRequest::new(JsonRpcId::Number(1), "tools/list", None),
        )
        .await
        .expect("tools");

    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert!(registry
        .server_status()
        .await
        .iter()
        .all(|server| server.state == "stopped"));
}

#[tokio::test]
async fn stderr_logs_redact_secret_patterns() {
    let registry = BackendRegistry::new(test_config()).expect("registry");
    registry
        .handle_group_request(
            "coding",
            JsonRpcRequest::new(JsonRpcId::Number(1), "tools/list", None),
        )
        .await
        .expect("tools");

    tokio::time::sleep(Duration::from_millis(100)).await;
    let logs = registry.logs("github");
    let joined = logs
        .iter()
        .map(|entry| entry.line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!joined.contains("FAKE_TEST_TOKEN_DO_NOT_USE"));
    assert!(joined.contains("[REDACTED]"));
    registry.shutdown_all().await;
}

#[tokio::test]
async fn prefixed_tools_call_routes_to_exactly_one_backend_and_preserves_id() {
    let registry = BackendRegistry::new(test_config()).expect("registry");
    let response = registry
        .handle_group_request(
            "coding",
            JsonRpcRequest::new(
                JsonRpcId::String("abc".to_string()),
                "tools/call",
                Some(json!({ "name": "github.search_repositories", "arguments": { "q": "rust" } })),
            ),
        )
        .await
        .expect("call response");

    assert_eq!(response.id, JsonRpcId::String("abc".to_string()));
    let text = response.result.unwrap()["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("github"));
    assert!(!text.contains("filesystem"));
    registry.shutdown_all().await;
}

#[tokio::test]
async fn concurrent_requests_with_same_client_id_are_isolated() {
    let mut cfg = test_config();
    cfg.servers
        .get_mut("github")
        .expect("github")
        .env
        .insert("FAKE_MCP_DELAY_MS".to_string(), "200".to_string());
    let registry = BackendRegistry::new(cfg).expect("registry");

    registry
        .handle_server_request(
            "github",
            JsonRpcRequest::new(JsonRpcId::Number(1), "tools/list", None),
        )
        .await
        .expect("warm backend");

    let request = JsonRpcRequest::new(
        JsonRpcId::Number(42),
        "tools/call",
        Some(json!({ "name": "echo", "arguments": { "n": 1 } })),
    );

    let (first, second) = tokio::join!(
        registry.handle_server_request("github", request.clone()),
        registry.handle_server_request("github", request),
    );

    for response in [first, second] {
        let response = response.expect("same client id request should not lose response");
        assert_eq!(response.id, JsonRpcId::Number(42));
        assert!(
            response.error.is_none(),
            "backend response should not become an error: {response:?}"
        );
        assert!(response.result.is_some());
    }
    registry.shutdown_all().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_cold_start_spawns_one_backend_process() {
    let browser_url = start_slow_browser_version_endpoint(Duration::from_millis(200)).await;
    let temp = tempfile::tempdir().expect("temp dir");
    let marker = temp.path().join("starts.txt");
    let mut cfg = test_config();
    let server = cfg.servers.get_mut("github").expect("github");
    server.args.push(format!("--browser-url={browser_url}"));
    server.env.insert(
        "FAKE_MCP_START_MARKER".to_string(),
        marker.to_string_lossy().to_string(),
    );
    let registry = BackendRegistry::new(cfg).expect("registry");

    let mut tasks = Vec::new();
    for id in 1..=12 {
        let registry = registry.clone();
        tasks.push(tokio::spawn(async move {
            registry
                .handle_server_request(
                    "github",
                    JsonRpcRequest::new(JsonRpcId::Number(id), "tools/list", None),
                )
                .await
        }));
    }

    for task in tasks {
        let response = task.await.expect("join").expect("request");
        assert!(response.error.is_none(), "request failed: {response:?}");
    }

    let starts = fs::read_to_string(&marker).unwrap_or_default();
    assert_eq!(
        starts.lines().count(),
        1,
        "cold start must spawn one backend process, starts:\n{starts}"
    );
    registry.shutdown_all().await;
}

#[tokio::test]
async fn backend_notification_reaches_matching_bridge_session() {
    let cfg = test_config();
    let registry = BackendRegistry::new(cfg.clone()).expect("registry");
    let sessions = SessionManager::new(cfg);
    sessions.start_notification_forwarder(registry.notification_stream());
    let session = sessions.create("coding").expect("session");

    registry
        .handle_group_request(
            "coding",
            JsonRpcRequest::new(JsonRpcId::Number(9), "fake/emit_notification", None),
        )
        .await
        .expect("emit");

    let events = sessions
        .poll(&session.id, Duration::from_secs(2))
        .await
        .expect("events");
    assert!(events
        .iter()
        .any(|event| event.method == "notifications/tools/list_changed"));
    registry.shutdown_all().await;
}

#[tokio::test]
async fn backend_notification_reaches_matching_direct_server_session() {
    let cfg = test_config();
    let registry = BackendRegistry::new(cfg.clone()).expect("registry");
    let sessions = SessionManager::new(cfg);
    sessions.start_notification_forwarder(registry.notification_stream());
    let session = sessions
        .create(SessionTarget::Server("github".to_string()))
        .expect("session");

    registry
        .handle_server_request(
            "github",
            JsonRpcRequest::new(JsonRpcId::Number(9), "fake/emit_notification", None),
        )
        .await
        .expect("emit");

    let events = sessions
        .poll(&session.id, Duration::from_secs(2))
        .await
        .expect("events");
    assert!(events
        .iter()
        .any(|event| event.method == "notifications/tools/list_changed"));
    registry.shutdown_all().await;
}

#[tokio::test]
async fn browser_server_with_unreachable_debug_url_fails_before_spawning_backend() {
    let mut cfg = test_config();
    cfg.servers.clear();
    cfg.servers.insert(
        "browser-tools".to_string(),
        ServerConfig {
            transport: TransportKind::Stdio,
            command: fake_command(),
            args: vec!["--browser-url=http://127.0.0.1:9".to_string()],
            env: BTreeMap::new(),
            lazy: Some(true),
            singleton: Some(true),
            dangerous: Some(true),
            path_allowlist: vec![],
            idle_timeout_seconds: Some(1),
            startup_timeout_seconds: Some(1),
            request_timeout_seconds: Some(1),
            restart_policy: Some(RestartPolicy::OnFailure),
        },
    );
    cfg.groups.clear();
    cfg.groups.insert(
        "coding".to_string(),
        GroupConfig {
            servers: vec!["browser-tools".to_string()],
            expose_tools: true,
            expose_prompts: true,
            expose_resources: true,
        },
    );
    let registry = BackendRegistry::new(cfg).expect("registry");

    let err = registry
        .handle_server_request(
            "browser-tools",
            JsonRpcRequest::new(JsonRpcId::Number(1), "tools/list", None),
        )
        .await
        .expect_err("unreachable browser attach URL should fail");

    assert!(err
        .to_string()
        .contains("Browser remote debugging is not reachable"));
    assert_eq!(registry.server_status().await[0].state, "stopped");
}

#[tokio::test]
async fn browser_server_without_attach_url_lists_tools_without_debug_port() {
    let mut cfg = test_config();
    cfg.servers.clear();
    cfg.servers.insert(
        "browser-tools".to_string(),
        ServerConfig {
            transport: TransportKind::Stdio,
            command: fake_command(),
            args: vec![
                "--headless=true".to_string(),
                "--isolated=true".to_string(),
                "--viewport=1280x720".to_string(),
                "--experimentalPageIdRouting".to_string(),
                "--no-usage-statistics".to_string(),
                "--no-performance-crux".to_string(),
            ],
            env: BTreeMap::from([("FAKE_MCP_NAME".to_string(), "browser-tools".to_string())]),
            lazy: Some(true),
            singleton: Some(true),
            dangerous: Some(true),
            path_allowlist: vec![],
            idle_timeout_seconds: Some(1),
            startup_timeout_seconds: Some(5),
            request_timeout_seconds: Some(5),
            restart_policy: Some(RestartPolicy::OnFailure),
        },
    );
    cfg.groups.clear();
    cfg.groups.insert(
        "coding".to_string(),
        GroupConfig {
            servers: vec!["browser-tools".to_string()],
            expose_tools: true,
            expose_prompts: true,
            expose_resources: true,
        },
    );
    let registry = BackendRegistry::new(cfg).expect("registry");

    let response = registry
        .handle_server_request(
            "browser-tools",
            JsonRpcRequest::new(JsonRpcId::Number(1), "tools/list", None),
        )
        .await
        .expect("tools/list should not require a debug Chrome port");

    let names: Vec<_> = response.result.unwrap()["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"echo".to_string()));
    assert!(names.iter().all(|name| !name.contains('.')));
    assert_eq!(registry.server_status().await[0].state, "running");
    registry.shutdown_all().await;
}

#[tokio::test]
#[cfg(unix)]
async fn stopping_backend_terminates_child_process_group() {
    let mut cfg = test_config();
    let server = cfg.servers.get_mut("github").expect("github");
    server
        .env
        .insert("FAKE_MCP_CHILD_SLEEP_SECS".to_string(), "30".to_string());
    let registry = BackendRegistry::new(cfg).expect("registry");

    let response = registry
        .handle_server_request(
            "github",
            JsonRpcRequest::new(JsonRpcId::Number(1), "fake/child_pid", None),
        )
        .await
        .expect("child pid");
    let child_pid = response.result.unwrap()["pid"].as_u64().expect("pid") as u32;
    assert!(process_exists(child_pid));

    registry.shutdown_all().await;

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while process_exists(child_pid) && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        !process_exists(child_pid),
        "child process {child_pid} survived backend shutdown"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn server_stop_route_terminates_backend_child_process_group() {
    let mut cfg = test_config();
    let server = cfg.servers.get_mut("github").expect("github");
    server
        .env
        .insert("FAKE_MCP_CHILD_SLEEP_SECS".to_string(), "30".to_string());
    let registry = BackendRegistry::new(cfg.clone()).expect("registry");
    let sessions = SessionManager::new(cfg.clone());
    let app = http::app(cfg, registry.clone(), sessions);

    let response = registry
        .handle_server_request(
            "github",
            JsonRpcRequest::new(JsonRpcId::Number(1), "fake/child_pid", None),
        )
        .await
        .expect("child pid");
    let child_pid = response.result.unwrap()["pid"].as_u64().expect("pid") as u32;
    assert!(process_exists(child_pid));

    let stop_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/servers/github/stop")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("stop response");

    assert_eq!(stop_response.status(), StatusCode::OK);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while process_exists(child_pid) && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        !process_exists(child_pid),
        "child process {child_pid} survived /servers/github/stop"
    );
}

#[tokio::test]
async fn capability_refresh_route_discovers_cache_and_stops_backend() {
    let mut cfg = test_config();
    cfg.clients.managed_clients.clear();
    let temp = tempfile::tempdir().expect("temp dir");
    let cache_file = temp.path().join("mcp_manifest_cache.json");
    let registry = BackendRegistry::new_with_capability_cache(cfg.clone(), cache_file.clone())
        .expect("registry");
    let sessions = SessionManager::new(cfg.clone());
    let app = http::app(cfg, registry.clone(), sessions);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/servers/github/capabilities/refresh")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("refresh response");

    assert_eq!(response.status(), StatusCode::OK);
    assert!(registry
        .server_status()
        .await
        .iter()
        .all(|server| server.state == "stopped"));
    let cache = CapabilityStore::new(cache_file).expect("cache");
    assert!(cache.server("github").expect("github").tools.len() >= 2);
}

#[tokio::test]
async fn failed_group_capability_refresh_stops_temporary_backends() {
    let mut cfg = test_config();
    let filesystem = cfg.servers.get_mut("filesystem").expect("filesystem");
    filesystem
        .env
        .insert("FAKE_MCP_CRASH_AFTER_INIT".to_string(), "1".to_string());
    filesystem.startup_timeout_seconds = Some(1);
    filesystem.request_timeout_seconds = Some(1);
    let temp = tempfile::tempdir().expect("temp dir");
    let registry = BackendRegistry::new_with_capability_cache(cfg, temp.path().join("cache.json"))
        .expect("registry");

    let err = registry
        .refresh_group_capabilities("coding")
        .await
        .expect_err("refresh should fail when one backend crashes");
    assert!(err.to_string().contains("filesystem"));

    let status = registry.server_status().await;
    let github = status
        .iter()
        .find(|server| server.name == "github")
        .expect("github status");
    assert_eq!(
        github.state, "stopped",
        "backend started only for refresh should be stopped after failure"
    );
    registry.shutdown_all().await;
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    let pid = pid.to_string();
    std::process::Command::new("ps")
        .args(["-p", pid.as_str()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

async fn start_slow_browser_version_endpoint(delay: Duration) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind browser version endpoint");
    let addr = listener.local_addr().expect("local addr");
    let app = Router::new().route(
        "/json/version",
        get(move || async move {
            tokio::time::sleep(delay).await;
            Json(json!({ "Browser": "fake" }))
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn invalid_prefixed_tool_call_returns_jsonrpc_error_with_original_id() {
    let registry = BackendRegistry::new(test_config()).expect("registry");
    let response = registry
        .handle_group_request(
            "coding",
            JsonRpcRequest::new(
                JsonRpcId::Number(55),
                "tools/call",
                Some(json!({ "name": "missing.tool" })),
            ),
        )
        .await
        .expect("error response");

    assert_eq!(response.id, JsonRpcId::Number(55));
    assert_eq!(response.error.unwrap().code, -32602);
    registry.shutdown_all().await;
}

#[tokio::test]
async fn client_initialized_notification_is_recorded_per_session() {
    let cfg = test_config();
    let sessions = SessionManager::new(cfg);
    let session = sessions.create("coding").expect("session");
    sessions
        .record_client_notification(&session.id, JsonRpcNotification::new("initialized", None))
        .expect("record");
    assert!(sessions.is_initialized(&session.id));
}

#[tokio::test]
async fn sessions_reject_clients_over_limit_and_delete_releases_slot() {
    let cfg = test_config_with_limits(LimitsConfig {
        max_clients: 1,
        ..Default::default()
    });
    let sessions = SessionManager::new(cfg);
    let first = sessions.create("coding").expect("first session");

    let err = sessions
        .create("coding")
        .expect_err("second session should exceed max_clients");
    assert!(err.to_string().contains("max clients"));

    sessions.delete(&first.id).expect("delete session");
    sessions
        .create("coding")
        .expect("slot should be available after delete");
}

#[tokio::test]
async fn session_event_queue_retains_only_configured_number_of_events() {
    let cfg = test_config_with_limits(LimitsConfig {
        max_session_events: 2,
        ..Default::default()
    });
    let registry = BackendRegistry::new(cfg.clone()).expect("registry");
    let sessions = SessionManager::new(cfg);
    sessions.start_notification_forwarder(registry.notification_stream());
    let session = sessions.create("coding").expect("session");

    for id in 1..=3 {
        registry
            .handle_group_request(
                "coding",
                JsonRpcRequest::new(JsonRpcId::Number(id), "fake/emit_notification", None),
            )
            .await
            .expect("emit notification");
    }

    let events = sessions
        .poll(&session.id, Duration::from_secs(2))
        .await
        .expect("events");
    assert_eq!(events.len(), 2);
    registry.shutdown_all().await;
}

#[tokio::test]
async fn logs_retain_configured_entries_and_truncate_long_lines() {
    let mut cfg = test_config_with_limits(LimitsConfig {
        max_log_entries_per_server: 1,
        max_log_line_bytes: 12,
        ..Default::default()
    });
    cfg.servers.get_mut("github").expect("github").env.insert(
        "FAKE_MCP_STDERR_LINE".to_string(),
        "abcdefghijklmnopqrstuvwxyz".to_string(),
    );
    let registry = BackendRegistry::new(cfg).expect("registry");

    registry
        .handle_server_request(
            "github",
            JsonRpcRequest::new(JsonRpcId::Number(1), "tools/list", None),
        )
        .await
        .expect("tools");

    tokio::time::sleep(Duration::from_millis(100)).await;
    let logs = registry.logs("github");
    assert_eq!(logs.len(), 1);
    assert!(logs[0].line.len() <= 12);
    registry.shutdown_all().await;
}

#[tokio::test]
async fn backend_pending_request_limit_rejects_excess_concurrency() {
    let mut cfg = test_config_with_limits(LimitsConfig {
        max_pending_requests_per_backend: 1,
        ..Default::default()
    });
    cfg.servers
        .get_mut("github")
        .expect("github")
        .env
        .insert("FAKE_MCP_DELAY_MS".to_string(), "300".to_string());
    let registry = BackendRegistry::new(cfg).expect("registry");

    registry
        .handle_server_request(
            "github",
            JsonRpcRequest::new(JsonRpcId::Number(1), "tools/list", None),
        )
        .await
        .expect("initialize");

    let first = registry.handle_server_request(
        "github",
        JsonRpcRequest::new(
            JsonRpcId::Number(2),
            "tools/call",
            Some(json!({ "name": "echo", "arguments": {} })),
        ),
    );
    let second = registry.handle_server_request(
        "github",
        JsonRpcRequest::new(
            JsonRpcId::Number(3),
            "tools/call",
            Some(json!({ "name": "echo", "arguments": {} })),
        ),
    );
    let (first, second) = tokio::join!(first, second);

    let rejected = [first, second]
        .into_iter()
        .filter(|result| {
            result
                .as_ref()
                .err()
                .map(|err| err.to_string().contains("pending request limit"))
                .unwrap_or(false)
        })
        .count();
    assert_eq!(rejected, 1);
    registry.shutdown_all().await;
}

#[tokio::test]
async fn restart_attempts_stop_after_configured_limit() {
    let mut cfg = test_config_with_limits(LimitsConfig {
        max_restart_attempts: 1,
        restart_window_seconds: 60,
        ..Default::default()
    });
    let server = cfg.servers.get_mut("github").expect("github");
    server
        .env
        .insert("FAKE_MCP_CRASH_AFTER_INIT".to_string(), "1".to_string());
    server.startup_timeout_seconds = Some(1);
    server.request_timeout_seconds = Some(1);
    let registry = BackendRegistry::new(cfg).expect("registry");

    for id in 1..=2 {
        let _ = registry
            .handle_server_request(
                "github",
                JsonRpcRequest::new(JsonRpcId::Number(id), "tools/list", None),
            )
            .await;
    }

    let err = registry
        .handle_server_request(
            "github",
            JsonRpcRequest::new(JsonRpcId::Number(3), "tools/list", None),
        )
        .await
        .expect_err("restart limit should prevent another spawn");
    assert!(err.to_string().contains("restart limit"));
}

#[tokio::test]
async fn session_delete_route_removes_bridge_session() {
    let cfg = test_config();
    let registry = BackendRegistry::new(cfg.clone()).expect("registry");
    let sessions = SessionManager::new(cfg.clone());
    let session = sessions.create("coding").expect("session");
    let app = http::app(cfg, registry, sessions.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/sessions/{}", session.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("delete response");

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(sessions.group_for_session(&session.id).is_err());
}

#[tokio::test]
async fn sessions_route_rejects_jsonrpc_payloads_with_clear_client_error() {
    let cfg = test_config();
    let registry = BackendRegistry::new(cfg.clone()).expect("registry");
    let sessions = SessionManager::new(cfg.clone());
    let app = http::app(cfg, registry, sessions);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
                ))
                .unwrap(),
        )
        .await
        .expect("session response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn oversized_http_jsonrpc_body_returns_payload_too_large() {
    let cfg = test_config_with_limits(LimitsConfig {
        max_message_bytes: 32,
        ..Default::default()
    });
    let registry = BackendRegistry::new(cfg.clone()).expect("registry");
    let sessions = SessionManager::new(cfg.clone());
    let app = http::app(cfg, registry, sessions);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/groups/coding/mcp")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"padding":"too large"}}"#,
                ))
                .unwrap(),
        )
        .await
        .expect("oversized response");

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
