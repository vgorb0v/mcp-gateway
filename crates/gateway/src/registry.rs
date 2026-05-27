use std::collections::BTreeMap;
use std::sync::Arc;

use futures::future::join_all;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::backend::{BackendHealth, BackendNotification, BackendTransport, StdioBackend};
use crate::capabilities::{CachedServerCapabilities, CapabilityCache, CapabilityStore};
use crate::client_config::{ApplyConfigOptions, ClientKind};
use crate::config::{Config, TransportKind};
use crate::error::{GatewayError, Result};
use crate::jsonrpc::{
    JsonRpcError, JsonRpcId, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    METHOD_INITIALIZE, METHOD_INITIALIZED, METHOD_PROMPTS_LIST, METHOD_RESOURCES_LIST,
    METHOD_TOOLS_CALL, METHOD_TOOLS_LIST, NOTIFICATION_PROMPTS_LIST_CHANGED,
    NOTIFICATION_RESOURCES_LIST_CHANGED, NOTIFICATION_TOOLS_LIST_CHANGED,
};
use crate::logs::LogStore;
use crate::metrics::RpcMetricsSnapshot;
use crate::native::NativeInstallPaths;
use crate::security::{is_sensitive_key, Redactor};

#[derive(Debug, Clone)]
pub struct BackendMetricsSnapshot {
    pub name: String,
    pub state: String,
    pub pid: Option<u32>,
    pub uptime_seconds: Option<u64>,
    pub initialized: bool,
    pub last_error: Option<String>,
    /// RSS observed via the legacy `ps`-based sampler. Higher-fidelity metrics
    /// from the `sysinfo` sampler take precedence in published events, but
    /// this is a useful fallback when sysinfo can't see the process tree.
    pub rss_kb_fallback: Option<u64>,
    pub rpc: RpcMetricsSnapshot,
}

#[derive(Clone)]
pub struct BackendRegistry {
    inner: Arc<Inner>,
}

struct Inner {
    config: Arc<Config>,
    backends: BTreeMap<String, Arc<dyn BackendTransport>>,
    capabilities: CapabilityStore,
    logs: LogStore,
    notifications: broadcast::Sender<BackendNotification>,
}

impl BackendRegistry {
    pub fn new(config: impl Into<Arc<Config>>) -> Result<Self> {
        Self::new_inner(config, CapabilityStore::disabled())
    }

    pub fn new_with_capability_cache(
        config: impl Into<Arc<Config>>,
        cache_file: impl Into<std::path::PathBuf>,
    ) -> Result<Self> {
        Self::new_inner(config, CapabilityStore::new(cache_file.into())?)
    }

    fn new_inner(config: impl Into<Arc<Config>>, capabilities: CapabilityStore) -> Result<Self> {
        let config = config.into();
        config.validate()?;
        let secret_values = config
            .servers
            .values()
            .flat_map(|server| {
                server
                    .env
                    .iter()
                    .filter(|(key, _)| is_sensitive_key(key))
                    .map(|(_, value)| value.clone())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let logs = LogStore::with_limits(Redactor::new(secret_values), &config.limits);
        let (notifications, _) = broadcast::channel(config.limits.gateway_notification_broadcast);
        let mut backends: BTreeMap<String, Arc<dyn BackendTransport>> = BTreeMap::new();

        for (name, server) in &config.servers {
            match server.transport {
                TransportKind::Stdio => {
                    let backend: Arc<dyn BackendTransport> = Arc::new(StdioBackend::new(
                        name.clone(),
                        server.clone(),
                        config.defaults.clone(),
                        config.limits.clone(),
                        logs.clone(),
                    ));
                    let mut backend_rx = backend.notifications();
                    let gateway_tx = notifications.clone();
                    let capability_store = capabilities.clone();
                    let server_name = name.clone();
                    let server_config = server.clone();
                    let backend_for_refresh = backend.clone();
                    tokio::spawn(async move {
                        loop {
                            match backend_rx.recv().await {
                                Ok(notification) => {
                                    if is_list_changed_notification(&notification.notification)
                                        && capability_store.is_enabled()
                                    {
                                        let _ = capability_store
                                            .refresh_from_backend(
                                                &server_name,
                                                &server_config,
                                                backend_for_refresh.clone(),
                                            )
                                            .await;
                                    }
                                    let _ = gateway_tx.send(notification);
                                }
                                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                                Err(broadcast::error::RecvError::Closed) => break,
                            }
                        }
                    });
                    backends.insert(name.clone(), backend);
                }
            }
        }

        Ok(Self {
            inner: Arc::new(Inner {
                config,
                backends,
                capabilities,
                logs,
                notifications,
            }),
        })
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn notification_stream(&self) -> broadcast::Receiver<BackendNotification> {
        self.inner.notifications.subscribe()
    }

    pub fn capability_cache(&self) -> CapabilityCache {
        self.inner.capabilities.snapshot()
    }

    pub fn logs(&self, server: &str) -> Vec<crate::logs::LogEntry> {
        self.inner.logs.get(server)
    }

    pub async fn server_status(&self) -> Vec<BackendHealth> {
        let futures = self
            .inner
            .backends
            .values()
            .map(|backend| backend.health())
            .collect::<Vec<_>>();
        join_all(futures).await
    }

    /// Per-backend snapshots used by the metrics sampler. Each entry pairs the
    /// existing `BackendHealth` view with the current RPC counter snapshot,
    /// so the sampler can emit a single normalized event per server.
    pub async fn metrics_snapshots(&self) -> Vec<BackendMetricsSnapshot> {
        let futures = self.inner.backends.iter().map(|(name, backend)| {
            let backend = backend.clone();
            let name = name.clone();
            async move {
                let health = backend.health().await;
                let rpc = backend.rpc_metrics();
                BackendMetricsSnapshot {
                    name,
                    state: health.state,
                    pid: health.pid,
                    uptime_seconds: health.uptime_seconds,
                    initialized: health.initialized,
                    last_error: health.last_error,
                    rss_kb_fallback: health.rss_kb,
                    rpc,
                }
            }
        });
        join_all(futures).await
    }

    /// Single-server metrics snapshot used by the detail endpoint.
    pub async fn metrics_snapshot(&self, name: &str) -> Result<BackendMetricsSnapshot> {
        let backend = self.backend(name)?;
        let health = backend.health().await;
        let rpc = backend.rpc_metrics();
        Ok(BackendMetricsSnapshot {
            name: name.to_string(),
            state: health.state,
            pid: health.pid,
            uptime_seconds: health.uptime_seconds,
            initialized: health.initialized,
            last_error: health.last_error,
            rss_kb_fallback: health.rss_kb,
            rpc,
        })
    }

    /// Stores the freshly-sampled process metrics back onto the backend so the
    /// next `health()` call returns RSS/CPU without spawning a subprocess. A
    /// no-op for unknown server names — the sampler is best-effort.
    pub fn update_resource_sample(
        &self,
        name: &str,
        sample: Option<crate::metrics::ResourceSample>,
    ) {
        if let Some(backend) = self.inner.backends.get(name) {
            backend.update_resource_sample(sample);
        }
    }

    pub async fn server_health(&self, name: &str) -> Result<BackendHealth> {
        Ok(self.backend(name)?.health().await)
    }

    pub async fn restart(&self, name: &str) -> Result<BackendHealth> {
        let backend = self.backend(name)?;
        backend.restart().await?;
        Ok(backend.health().await)
    }

    pub async fn stop(&self, name: &str) -> Result<BackendHealth> {
        let backend = self.backend(name)?;
        backend.stop().await?;
        Ok(backend.health().await)
    }

    pub async fn shutdown_all(&self) {
        let futures = self
            .inner
            .backends
            .values()
            .map(|backend| backend.stop())
            .collect::<Vec<_>>();
        let _ = join_all(futures).await;
    }

    pub async fn refresh_server_capabilities(
        &self,
        name: &str,
    ) -> Result<CachedServerCapabilities> {
        let backend = self.backend(name)?;
        let server = self
            .inner
            .config
            .servers
            .get(name)
            .ok_or_else(|| GatewayError::NotFound(format!("server '{name}'")))?
            .clone();
        let was_running = backend.health().await.pid.is_some();
        let result = self
            .inner
            .capabilities
            .refresh_from_backend(name, &server, backend.clone())
            .await;
        if !was_running {
            let _ = backend.stop().await;
        }
        result
    }

    pub async fn refresh_group_capabilities(
        &self,
        group_name: &str,
    ) -> Result<Vec<CachedServerCapabilities>> {
        let servers = {
            let group = self.group(group_name)?;
            group.servers.clone()
        };
        // First pass: hit every backend, collect capability snapshots in
        // memory. We don't persist or notify yet because that would write the
        // cache file N times for an N-server group.
        let mut fetched: Vec<(String, CachedServerCapabilities)> =
            Vec::with_capacity(servers.len());
        let mut shutdown: Vec<String> = Vec::new();
        let result = async {
            for server_name in &servers {
                let backend = self.backend(server_name)?;
                let server_config = self
                    .inner
                    .config
                    .servers
                    .get(server_name)
                    .ok_or_else(|| GatewayError::NotFound(format!("server '{server_name}'")))?
                    .clone();
                let was_running = backend.health().await.pid.is_some();
                if !was_running {
                    shutdown.push(server_name.clone());
                }
                let capabilities = self
                    .inner
                    .capabilities
                    .fetch_from_backend(server_name, &server_config, backend.clone())
                    .await?;
                fetched.push((server_name.clone(), capabilities));
            }
            // Second pass: a single batched persist for the whole group.
            self.inner
                .capabilities
                .upsert_servers(fetched.clone())
                .await?;
            Ok::<_, GatewayError>(())
        }
        .await;
        // Stop any backends we lazily started purely to query capabilities.
        for name in shutdown {
            if let Ok(backend) = self.backend(&name) {
                let _ = backend.stop().await;
            }
        }
        result?;
        Ok(fetched.into_iter().map(|(_, cap)| cap).collect())
    }

    pub fn sync_managed_client_configs(&self) -> Result<Vec<std::path::PathBuf>> {
        let clients = self
            .inner
            .config
            .clients
            .managed_clients
            .iter()
            .filter_map(|client| ClientKind::from_config_name(client))
            .collect::<Vec<_>>();
        if clients.is_empty() {
            return Ok(Vec::new());
        }
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| {
                GatewayError::Config("HOME is required to sync client configs".into())
            })?;
        let paths = NativeInstallPaths::for_home(&home);
        let group = self
            .inner
            .config
            .groups
            .get(&self.inner.config.clients.default_group)
            .ok_or_else(|| {
                GatewayError::Config(format!(
                    "clients.default_group '{}' does not exist",
                    self.inner.config.clients.default_group
                ))
            })?;
        crate::client_config::apply_configs(ApplyConfigOptions {
            clients,
            servers: group.servers.clone(),
            bridge_bin: paths.bin_dir.join("mcp-gateway-bridge"),
            mcp_dir: paths.mcp_dir.clone(),
            state_file: paths.state_file.clone(),
            dedupe: vec!["mcp-gateway".to_string()],
            dry_run: false,
            home,
        })
    }

    pub async fn handle_server_request(
        &self,
        server: &str,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse> {
        match request.method.as_str() {
            METHOD_INITIALIZE => Ok(gateway_initialize_response(request.id, server)),
            METHOD_TOOLS_LIST if self.inner.capabilities.is_enabled() => {
                Ok(self.cached_server_list(server, request.id, METHOD_TOOLS_LIST, "tools"))
            }
            METHOD_PROMPTS_LIST if self.inner.capabilities.is_enabled() => {
                Ok(self.cached_server_list(server, request.id, METHOD_PROMPTS_LIST, "prompts"))
            }
            METHOD_RESOURCES_LIST if self.inner.capabilities.is_enabled() => {
                Ok(self.cached_server_list(server, request.id, METHOD_RESOURCES_LIST, "resources"))
            }
            _ => self.backend(server)?.send_request(request).await,
        }
    }

    pub async fn handle_server_notification(
        &self,
        server: &str,
        notification: JsonRpcNotification,
    ) -> Result<()> {
        if notification.method == METHOD_INITIALIZED {
            return Ok(());
        }
        self.backend(server)?.send_notification(notification).await
    }

    pub async fn handle_group_request(
        &self,
        group_name: &str,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse> {
        let group = self.group(group_name)?;
        match request.method.as_str() {
            METHOD_INITIALIZE => Ok(gateway_initialize_response(
                request.id,
                &self.inner.config.clients.server_name,
            )),
            METHOD_TOOLS_LIST if group.expose_tools && self.inner.capabilities.is_enabled() => {
                Ok(self.cached_group_list(
                    group_name,
                    request.id,
                    METHOD_TOOLS_LIST,
                    "tools",
                    PrefixMode::ToolName,
                ))
            }
            METHOD_TOOLS_LIST if group.expose_tools => {
                self.aggregate_list(
                    group_name,
                    request.id,
                    METHOD_TOOLS_LIST,
                    "tools",
                    PrefixMode::ToolName,
                )
                .await
            }
            METHOD_PROMPTS_LIST if group.expose_prompts && self.inner.capabilities.is_enabled() => {
                Ok(self.cached_group_list(
                    group_name,
                    request.id,
                    METHOD_PROMPTS_LIST,
                    "prompts",
                    PrefixMode::PromptName,
                ))
            }
            METHOD_PROMPTS_LIST if group.expose_prompts => {
                self.aggregate_list(
                    group_name,
                    request.id,
                    METHOD_PROMPTS_LIST,
                    "prompts",
                    PrefixMode::PromptName,
                )
                .await
            }
            METHOD_RESOURCES_LIST
                if group.expose_resources && self.inner.capabilities.is_enabled() =>
            {
                Ok(self.cached_group_list(
                    group_name,
                    request.id,
                    METHOD_RESOURCES_LIST,
                    "resources",
                    PrefixMode::ResourceMeta,
                ))
            }
            METHOD_RESOURCES_LIST if group.expose_resources => {
                self.aggregate_list(
                    group_name,
                    request.id,
                    METHOD_RESOURCES_LIST,
                    "resources",
                    PrefixMode::ResourceMeta,
                )
                .await
            }
            METHOD_TOOLS_CALL => self.route_tool_call(group_name, request).await,
            _ => self.fanout_first_success(group_name, request).await,
        }
    }

    pub async fn handle_group_notification(
        &self,
        group_name: &str,
        notification: JsonRpcNotification,
    ) -> Result<()> {
        if notification.method == METHOD_INITIALIZED {
            return Ok(());
        }
        let group = self.group(group_name)?;
        let futures = group.servers.iter().map(|server| async {
            self.backend(server)?
                .send_notification(notification.clone())
                .await
        });
        for result in join_all(futures).await {
            result?;
        }
        Ok(())
    }

    async fn route_tool_call(
        &self,
        group_name: &str,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse> {
        let tool_name = request
            .params
            .as_ref()
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                GatewayError::JsonRpc("tools/call params.name is required".to_string())
            })?;
        let Some((server, backend_tool)) = tool_name.split_once('.') else {
            return Ok(JsonRpcResponse::error(
                request.id,
                JsonRpcError::invalid_params("tool name must be prefixed as server.tool"),
            ));
        };
        let server = server.to_string();
        let backend_tool = backend_tool.to_string();
        let group = self.group(group_name)?;
        if !group.servers.iter().any(|name| name == &server) {
            return Ok(JsonRpcResponse::error(
                request.id,
                JsonRpcError::invalid_params(format!(
                    "server '{server}' is not part of group '{group_name}'"
                )),
            ));
        }
        let mut params = request.params.unwrap_or_else(|| json!({}));
        if let Some(object) = params.as_object_mut() {
            object.insert("name".to_string(), Value::String(backend_tool));
        }
        let backend_request =
            JsonRpcRequest::new(request.id.clone(), METHOD_TOOLS_CALL, Some(params));
        match self.backend(&server)?.send_request(backend_request).await {
            Ok(mut response) => {
                response.id = request.id;
                Ok(response)
            }
            Err(err) => Ok(JsonRpcResponse::error(
                request.id,
                JsonRpcError::gateway_error(err.to_string(), Some(json!({ "server": server }))),
            )),
        }
    }

    fn cached_server_list(
        &self,
        server: &str,
        id: JsonRpcId,
        method: &str,
        result_key: &str,
    ) -> JsonRpcResponse {
        let Some(capabilities) = self.inner.capabilities.server(server) else {
            return missing_cache_response(id, server, method);
        };
        let items = match result_key {
            "tools" => capabilities.tools,
            "prompts" => capabilities.prompts,
            "resources" => capabilities.resources,
            _ => Vec::new(),
        };
        JsonRpcResponse::success(id, json!({ result_key: items }))
    }

    fn cached_group_list(
        &self,
        group_name: &str,
        id: JsonRpcId,
        method: &str,
        result_key: &str,
        prefix_mode: PrefixMode,
    ) -> JsonRpcResponse {
        let Ok(group) = self.group(group_name) else {
            return JsonRpcResponse::error(
                id,
                JsonRpcError::gateway_error(format!("group '{group_name}' not found"), None),
            );
        };
        let mut items = Vec::new();
        let mut failures = Vec::new();
        for server in &group.servers {
            let Some(capabilities) = self.inner.capabilities.server(server) else {
                failures.push(json!({
                    "server": server,
                    "error": missing_cache_message(server, method)
                }));
                continue;
            };
            let server_items = match result_key {
                "tools" => capabilities.tools,
                "prompts" => capabilities.prompts,
                "resources" => capabilities.resources,
                _ => Vec::new(),
            };
            for item in server_items {
                let mut item = item;
                apply_prefix(server, &mut item, prefix_mode);
                items.push(item);
            }
        }
        if items.is_empty() && !failures.is_empty() {
            return JsonRpcResponse::error(
                id,
                JsonRpcError::gateway_error(
                    format!("all cached capabilities missing for {method}"),
                    Some(json!({ "partialFailures": failures })),
                ),
            );
        }
        let mut result = json!({ result_key: items });
        if !failures.is_empty() {
            result["_meta"] = json!({
                "gateway": { "partialFailures": failures }
            });
        }
        JsonRpcResponse::success(id, result)
    }

    async fn aggregate_list(
        &self,
        group_name: &str,
        id: JsonRpcId,
        method: &str,
        result_key: &str,
        prefix_mode: PrefixMode,
    ) -> Result<JsonRpcResponse> {
        let group = self.group(group_name)?;
        let futures = group.servers.iter().map(|server| {
            let backend = self.backend(server);
            async move {
                let backend = backend?;
                let req = JsonRpcRequest::new(
                    JsonRpcId::String(format!("gateway-{}-{}", server, Uuid::new_v4())),
                    method,
                    None,
                );
                let response = backend.send_request(req).await?;
                Ok::<_, GatewayError>((server.clone(), response))
            }
        });

        let mut items = Vec::new();
        let mut failures = Vec::new();
        for result in join_all(futures).await {
            match result {
                Ok((server, response)) => {
                    if let Some(error) = response.error {
                        failures.push(json!({ "server": server, "error": error.message }));
                        continue;
                    }
                    let Some(result) = response.result else {
                        failures.push(json!({ "server": server, "error": "missing result" }));
                        continue;
                    };
                    let Some(array) = result.get(result_key).and_then(Value::as_array) else {
                        failures.push(json!({ "server": server, "error": format!("missing result.{result_key}") }));
                        continue;
                    };
                    for item in array {
                        let mut item = item.clone();
                        apply_prefix(&server, &mut item, prefix_mode);
                        items.push(item);
                    }
                }
                Err(err) => failures.push(json!({ "error": err.to_string() })),
            }
        }

        if items.is_empty() && !failures.is_empty() {
            return Ok(JsonRpcResponse::error(
                id,
                JsonRpcError::gateway_error(
                    format!("all backends failed for {method}"),
                    Some(json!({ "partialFailures": failures })),
                ),
            ));
        }

        let mut result = json!({ result_key: items });
        if !failures.is_empty() {
            result["_meta"] = json!({
                "gateway": {
                    "partialFailures": failures
                }
            });
        }
        Ok(JsonRpcResponse::success(id, result))
    }

    async fn fanout_first_success(
        &self,
        group_name: &str,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse> {
        let group = self.group(group_name)?;
        let mut failures = Vec::new();
        for server in &group.servers {
            let mut backend_request = request.clone();
            backend_request.id = request.id.clone();
            match self.backend(server)?.send_request(backend_request).await {
                Ok(mut response) if response.error.is_none() => {
                    response.id = request.id;
                    return Ok(response);
                }
                Ok(response) => failures.push(json!({
                    "server": server,
                    "error": response.error.map(|e| e.message).unwrap_or_else(|| "unknown error".to_string())
                })),
                Err(err) => failures.push(json!({ "server": server, "error": err.to_string() })),
            }
        }
        Ok(JsonRpcResponse::error(
            request.id,
            JsonRpcError::method_not_found(format!(
                "method '{}' was not handled by any backend",
                request.method
            ))
            .with_data(json!({ "failures": failures })),
        ))
    }

    fn backend(&self, name: &str) -> Result<Arc<dyn BackendTransport>> {
        self.inner
            .backends
            .get(name)
            .cloned()
            .ok_or_else(|| GatewayError::NotFound(format!("server '{name}'")))
    }

    fn group(&self, name: &str) -> Result<&crate::config::GroupConfig> {
        self.inner
            .config
            .groups
            .get(name)
            .ok_or_else(|| GatewayError::NotFound(format!("group '{name}'")))
    }
}

#[derive(Clone, Copy)]
enum PrefixMode {
    ToolName,
    PromptName,
    ResourceMeta,
}

fn apply_prefix(server: &str, item: &mut Value, mode: PrefixMode) {
    match mode {
        PrefixMode::ToolName | PrefixMode::PromptName => {
            if matches!(mode, PrefixMode::ToolName) {
                strip_backend_tool_metadata(item);
            }
            if let Some(name) = item.get("name").and_then(Value::as_str) {
                let prefixed = format!("{server}.{name}");
                item["name"] = Value::String(prefixed);
            }
        }
        PrefixMode::ResourceMeta => {
            let meta = item
                .as_object_mut()
                .map(|object| object.entry("_meta").or_insert_with(|| json!({})));
            if let Some(meta) = meta {
                meta["gateway"] = json!({ "server": server });
            }
        }
    }
}

fn strip_backend_tool_metadata(item: &mut Value) {
    if let Some(object) = item.as_object_mut() {
        object.remove("execution");
    }
}

fn missing_cache_response(id: JsonRpcId, server: &str, method: &str) -> JsonRpcResponse {
    JsonRpcResponse::error(
        id,
        JsonRpcError::gateway_error(missing_cache_message(server, method), None),
    )
}

fn missing_cache_message(server: &str, method: &str) -> String {
    format!(
        "cached capabilities for server '{server}' are missing while handling {method}; run `mcpgateway refresh-capabilities {server}` or `mcpgateway refresh-capabilities all`"
    )
}

fn is_list_changed_notification(notification: &JsonRpcNotification) -> bool {
    matches!(
        notification.method.as_str(),
        NOTIFICATION_TOOLS_LIST_CHANGED
            | NOTIFICATION_PROMPTS_LIST_CHANGED
            | NOTIFICATION_RESOURCES_LIST_CHANGED
    )
}

fn gateway_initialize_response(id: JsonRpcId, server_name: &str) -> JsonRpcResponse {
    JsonRpcResponse::success(
        id,
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": { "listChanged": true },
                "prompts": { "listChanged": true },
                "resources": { "listChanged": true }
            },
            "serverInfo": {
                "name": server_name,
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
    )
}

trait WithData {
    fn with_data(self, data: Value) -> Self;
}

impl WithData for JsonRpcError {
    fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}
