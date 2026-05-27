//! Regression tests for the performance audit findings:
//!   #1 `health()` must source RSS from the cached `ResourceSample`, never
//!      from a `ps` shell-out, so the request hot path is alloc/spawn-free.
//!   #2 Notifying one session must NOT wake unrelated sessions — each session
//!      gets its own `Notify`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use mcp_gateway::backend::{BackendTransport, StdioBackend};
use mcp_gateway::config::{
    Config, DefaultsConfig, GroupConfig, LimitsConfig, ServerConfig, TransportKind,
};
use mcp_gateway::jsonrpc::{JsonRpcNotification, NOTIFICATION_TOOLS_LIST_CHANGED};
use mcp_gateway::logs::LogStore;
use mcp_gateway::metrics::ResourceSample;
use mcp_gateway::security::Redactor;
use mcp_gateway::sessions::{SessionManager, SessionTarget};

fn test_server_config() -> ServerConfig {
    ServerConfig {
        transport: TransportKind::Stdio,
        command: "/bin/true".to_string(),
        args: vec![],
        env: BTreeMap::new(),
        lazy: Some(true),
        singleton: Some(false),
        dangerous: Some(false),
        path_allowlist: vec![],
        idle_timeout_seconds: Some(1),
        startup_timeout_seconds: Some(5),
        request_timeout_seconds: Some(5),
        restart_policy: None,
    }
}

fn test_config() -> Config {
    let mut servers = BTreeMap::new();
    servers.insert("alpha".to_string(), test_server_config());
    servers.insert("beta".to_string(), test_server_config());
    let groups = BTreeMap::from([
        (
            "alpha-only".to_string(),
            GroupConfig {
                servers: vec!["alpha".to_string()],
                expose_tools: true,
                expose_prompts: true,
                expose_resources: true,
            },
        ),
        (
            "beta-only".to_string(),
            GroupConfig {
                servers: vec!["beta".to_string()],
                expose_tools: true,
                expose_prompts: true,
                expose_resources: true,
            },
        ),
    ]);
    Config {
        listen: "127.0.0.1:0".to_string(),
        defaults: Default::default(),
        servers,
        groups,
        clients: Default::default(),
        limits: Default::default(),
        metrics: Default::default(),
    }
}

#[tokio::test]
async fn health_returns_rss_from_cached_sample_without_spawning_ps() {
    // Construct a StdioBackend directly so we can poke its sample cache and
    // verify the read path uses it. `health()` would normally shell out to
    // `ps` (which works against PIDs the backend never spawned for a `/bin/true`
    // command) — after the fix it must instead reflect whatever the metrics
    // sampler pushed in.
    let logs = LogStore::with_limits(Redactor::new(Vec::new()), &LimitsConfig::default());
    let backend = StdioBackend::new(
        "alpha".to_string(),
        test_server_config(),
        DefaultsConfig::default(),
        LimitsConfig::default(),
        logs,
    );

    // Before any sample is pushed, health()'s `rss_kb` is None.
    let first = backend.health().await;
    assert_eq!(first.rss_kb, None);

    backend.update_resource_sample(Some(ResourceSample {
        rss_kb: Some(123_456),
        cpu_percent: Some(4.2),
    }));

    let after = backend.health().await;
    assert_eq!(after.rss_kb, Some(123_456));
    assert_eq!(
        backend.last_resource_sample().and_then(|s| s.rss_kb),
        Some(123_456)
    );
}

#[tokio::test]
async fn notifying_one_backend_does_not_wake_sessions_on_unrelated_backends() {
    let cfg = test_config();
    let sessions = SessionManager::new(cfg.clone());

    let alpha_session = sessions
        .create_with_client(SessionTarget::Server("alpha".to_string()), Some("a".into()))
        .expect("alpha session");
    let beta_session = sessions
        .create_with_client(SessionTarget::Server("beta".to_string()), Some("b".into()))
        .expect("beta session");

    // Wire up the backend-notification forwarder before spawning beta's poll
    // so the wakeup path is fully ready.
    use mcp_gateway::backend::BackendNotification;
    let receiver_tx: tokio::sync::broadcast::Sender<BackendNotification> =
        tokio::sync::broadcast::channel(16).0;
    sessions.start_notification_forwarder(receiver_tx.subscribe());

    // Spawn a poll on beta with a short timeout; if it gets woken spuriously
    // by a backend notification on alpha, the poll will return well before
    // the deadline. Capture beta's actual start instant so we can measure its
    // full sleep duration regardless of where the rest of the test ran.
    let sessions_clone = sessions.clone();
    let beta_id = beta_session.id.clone();
    let beta_started = std::time::Instant::now();
    let beta_poll_timeout = Duration::from_millis(400);
    let beta_poll =
        tokio::spawn(async move { sessions_clone.poll(&beta_id, beta_poll_timeout).await });

    // Let beta's poll loop reach the `notify.notified()` await before we fire.
    tokio::time::sleep(Duration::from_millis(50)).await;

    receiver_tx
        .send(BackendNotification {
            server: "alpha".to_string(),
            notification: JsonRpcNotification::new(NOTIFICATION_TOOLS_LIST_CHANGED, None),
        })
        .expect("send alpha notification");

    // The forwarder is async; give it a moment to enqueue + fire the per-session notify.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Alpha's queue should now have an event waiting; beta's should be empty.
    let alpha_events = Arc::new(sessions.clone());
    let alpha_result = alpha_events
        .poll(&alpha_session.id, Duration::from_millis(10))
        .await
        .expect("alpha poll");
    assert_eq!(alpha_result.len(), 1, "alpha should receive its event");

    // Beta's poll should still be sleeping until its deadline — we expect it
    // to time out with zero events. If a shared `Notify` woke it spuriously,
    // it would return well before the deadline. Allow generous slack for CI.
    let beta_result = beta_poll.await.expect("beta join").expect("beta poll");
    let beta_elapsed = beta_started.elapsed();
    assert!(
        beta_result.is_empty(),
        "beta must not receive alpha's events"
    );
    assert!(
        beta_elapsed >= Duration::from_millis(350),
        "beta poll exited too early ({beta_elapsed:?}) — looks like shared-Notify wakeup; \
         expected to sleep ~{beta_poll_timeout:?}"
    );
}
