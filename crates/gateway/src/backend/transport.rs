use async_trait::async_trait;
use serde::Serialize;
use tokio::sync::broadcast;

use crate::jsonrpc::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use crate::metrics::{ResourceSample, RpcMetricsSnapshot};
use crate::Result;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BackendHealth {
    pub name: String,
    pub state: String,
    pub pid: Option<u32>,
    pub uptime_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rss_kb: Option<u64>,
    pub initialized: bool,
    pub last_error: Option<String>,
    /// Seconds since the backend last serviced a request. `None` while the
    /// backend is stopped. Surfaced in `mcpgateway ps` so operators can
    /// see who's about to idle-shutdown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_seconds: Option<u64>,
    /// In-flight requests currently being serviced by this backend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_requests: Option<u64>,
    /// Requests sent to the backend that haven't received a response yet
    /// (the `pending` map in `StdioBackend`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_requests: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct BackendNotification {
    pub server: String,
    pub notification: JsonRpcNotification,
}

#[async_trait]
pub trait BackendTransport: Send + Sync {
    fn name(&self) -> &str;
    async fn start(&self) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    async fn restart(&self) -> Result<()>;
    async fn send_request(&self, request: JsonRpcRequest) -> Result<JsonRpcResponse>;
    async fn send_notification(&self, notification: JsonRpcNotification) -> Result<()>;
    async fn health(&self) -> BackendHealth;
    fn notifications(&self) -> broadcast::Receiver<BackendNotification>;
    /// Snapshot of JSON-RPC counters and latency percentiles. Returned by-value
    /// because callers (HTTP handlers, the periodic sampler) want a stable
    /// point-in-time view they can serialize without holding the lock.
    fn rpc_metrics(&self) -> RpcMetricsSnapshot;
    /// Pushes the latest process-tree resource sample into the backend's
    /// cache. Called by the periodic metrics sampler so subsequent `health()`
    /// calls return RSS/CPU without spawning `ps` or stalling on `sysinfo`.
    /// Default impl is a no-op for transports that don't track process state.
    fn update_resource_sample(&self, _sample: Option<ResourceSample>) {}
    /// The last sample written via `update_resource_sample`, or `None` if the
    /// metrics sampler hasn't run yet (or the backend isn't process-backed).
    fn last_resource_sample(&self) -> Option<ResourceSample> {
        None
    }
}
