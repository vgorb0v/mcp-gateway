use std::collections::BTreeMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mcp_gateway::config::{Config, GroupConfig, ServerConfig, TransportKind};
use mcp_gateway::http;
use mcp_gateway::registry::BackendRegistry;
use mcp_gateway::sessions::SessionManager;
use tower::ServiceExt;

fn test_config() -> Config {
    let mut servers = BTreeMap::new();
    servers.insert(
        "alpha-tools".to_string(),
        ServerConfig {
            transport: TransportKind::Stdio,
            command: "/bin/true".to_string(),
            args: vec![],
            env: BTreeMap::new(),
            lazy: Some(true),
            singleton: Some(true),
            dangerous: Some(false),
            path_allowlist: vec![],
            idle_timeout_seconds: Some(1),
            startup_timeout_seconds: Some(5),
            request_timeout_seconds: Some(5),
            restart_policy: None,
        },
    );
    let groups = BTreeMap::from([(
        "coding".to_string(),
        GroupConfig {
            servers: vec!["alpha-tools".to_string()],
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

fn build_app() -> axum::Router {
    let cfg = test_config();
    let registry = BackendRegistry::new(cfg.clone()).expect("registry");
    let sessions = SessionManager::new(cfg.clone());
    sessions.start_notification_forwarder(registry.notification_stream());
    http::app(cfg, registry, sessions)
}

#[tokio::test]
async fn health_endpoint_remains_available() {
    let response = build_app()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("health response");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn removed_api_v1_bootstrap_is_not_mounted() {
    let removed_path = ["/api", "/v1/bootstrap"].concat();
    let response = build_app()
        .oneshot(
            Request::builder()
                .uri(removed_path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("bootstrap response");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn request_without_browser_origin_is_allowed_for_cli_and_bridge() {
    let response = build_app()
        .oneshot(
            Request::builder()
                .uri("/health")
                .header("host", "127.0.0.1:7777")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("health response");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn non_local_browser_origin_is_rejected() {
    let response = build_app()
        .oneshot(
            Request::builder()
                .uri("/health")
                .header("host", "127.0.0.1:7777")
                .header("origin", "https://example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("health response");

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn local_browser_origin_is_allowed() {
    let response = build_app()
        .oneshot(
            Request::builder()
                .uri("/health")
                .header("host", "localhost:7777")
                .header("origin", "http://localhost:3000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("health response");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn non_local_host_header_is_rejected() {
    let response = build_app()
        .oneshot(
            Request::builder()
                .uri("/health")
                .header("host", "attacker.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("health response");

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
