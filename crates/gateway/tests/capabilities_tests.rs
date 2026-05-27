use std::collections::BTreeMap;
use std::fs;

use mcp_gateway::capabilities::{CachedServerCapabilities, CapabilityStore};
use mcp_gateway::config::{RestartPolicy, ServerConfig, TransportKind};
use serde_json::json;

#[test]
fn capability_cache_writes_atomically_and_excludes_server_env_values() {
    let temp = tempfile::tempdir().expect("temp dir");
    let cache_file = temp.path().join("mcp_manifest_cache.json");
    let store = CapabilityStore::new(cache_file.clone()).expect("store");
    let server = ServerConfig {
        transport: TransportKind::Stdio,
        command: "example-mcp-server".to_string(),
        args: vec!["--flag".to_string()],
        env: BTreeMap::from([(
            "SERVICE_API_TOKEN".to_string(),
            "FAKE_TEST_TOKEN_DO_NOT_USE".to_string(),
        )]),
        lazy: Some(true),
        singleton: Some(false),
        dangerous: Some(false),
        path_allowlist: vec![],
        idle_timeout_seconds: Some(1200),
        startup_timeout_seconds: Some(30),
        request_timeout_seconds: Some(60),
        restart_policy: Some(RestartPolicy::OnFailure),
    };

    store
        .upsert_server(
            "code-host",
            CachedServerCapabilities::from_parts(
                "code-host",
                &server,
                vec![json!({"name": "search_repositories", "inputSchema": {"type": "object"}})],
                vec![json!({"name": "explain"})],
                vec![json!({"uri": "fake://code-host/one", "name": "one"})],
            ),
        )
        .expect("write cache");

    let text = fs::read_to_string(&cache_file).expect("cache file");
    assert!(text.contains("search_repositories"));
    assert!(text.contains("config_fingerprint"));
    assert!(!text.contains("FAKE_TEST_TOKEN_DO_NOT_USE"));
    assert!(!text.contains("SERVICE_API_TOKEN"));

    let reloaded = CapabilityStore::new(cache_file).expect("reload");
    let code_host = reloaded
        .server("code-host")
        .expect("code-host should reload from cache");
    assert_eq!(code_host.tools[0]["name"], "search_repositories");
    assert_eq!(code_host.prompts[0]["name"], "explain");
    assert_eq!(code_host.resources[0]["uri"], "fake://code-host/one");
}
