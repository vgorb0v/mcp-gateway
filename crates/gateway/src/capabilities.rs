use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::backend::BackendTransport;
use crate::config::ServerConfig;
use crate::error::{GatewayError, Result};
use crate::jsonrpc::{
    JsonRpcId, JsonRpcRequest, METHOD_PROMPTS_LIST, METHOD_RESOURCES_LIST, METHOD_TOOLS_LIST,
};

const CACHE_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct CapabilityCache {
    pub version: u32,
    pub servers: BTreeMap<String, CachedServerCapabilities>,
}

impl Default for CapabilityCache {
    fn default() -> Self {
        Self {
            version: CACHE_VERSION,
            servers: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct CachedServerCapabilities {
    #[serde(rename = "serverInfo")]
    pub server_info: Value,
    pub tools: Vec<Value>,
    pub prompts: Vec<Value>,
    pub resources: Vec<Value>,
    #[serde(rename = "generatedAt")]
    pub generated_at: String,
    pub config_fingerprint: String,
}

impl CachedServerCapabilities {
    pub fn from_parts(
        server_name: &str,
        server: &ServerConfig,
        tools: Vec<Value>,
        prompts: Vec<Value>,
        resources: Vec<Value>,
    ) -> Self {
        Self {
            server_info: json!({
                "name": server_name,
                "version": env!("CARGO_PKG_VERSION")
            }),
            tools,
            prompts,
            resources,
            generated_at: now_timestamp(),
            config_fingerprint: config_fingerprint(server),
        }
    }
}

#[derive(Clone)]
pub struct CapabilityStore {
    path: Option<PathBuf>,
    cache: Arc<Mutex<CapabilityCache>>,
}

impl CapabilityStore {
    pub fn disabled() -> Self {
        Self {
            path: None,
            cache: Arc::new(Mutex::new(CapabilityCache::default())),
        }
    }

    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let cache = match fs::read_to_string(&path) {
            Ok(text) if !text.trim().is_empty() => serde_json::from_str(&text)?,
            Ok(_) => CapabilityCache::default(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => CapabilityCache::default(),
            Err(err) => return Err(err.into()),
        };
        Ok(Self {
            path: Some(path),
            cache: Arc::new(Mutex::new(cache)),
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.path.is_some()
    }

    pub fn snapshot(&self) -> CapabilityCache {
        self.cache.lock().clone()
    }

    pub fn server(&self, name: &str) -> Option<CachedServerCapabilities> {
        self.cache.lock().servers.get(name).cloned()
    }

    pub fn upsert_server(&self, name: &str, capabilities: CachedServerCapabilities) -> Result<()> {
        self.insert_into_cache(name, capabilities);
        self.persist_blocking()
    }

    /// Async variant of [`upsert_server`] that offloads the disk write to
    /// `tokio::task::spawn_blocking` so it never stalls the reactor. Used by
    /// `refresh_from_backend` on the hot async path; sync callers (tests,
    /// non-async installers) keep `upsert_server`.
    pub async fn upsert_server_async(
        &self,
        name: &str,
        capabilities: CachedServerCapabilities,
    ) -> Result<()> {
        self.insert_into_cache(name, capabilities);
        self.persist_async().await
    }

    /// Apply a batch of server capability updates and persist them in a
    /// single disk write. Used by `refresh_group_capabilities` so a 5-server
    /// refresh writes the cache file once instead of five times.
    pub async fn upsert_servers(
        &self,
        servers: Vec<(String, CachedServerCapabilities)>,
    ) -> Result<()> {
        if servers.is_empty() {
            return Ok(());
        }
        {
            let mut cache = self.cache.lock();
            cache.version = CACHE_VERSION;
            for (name, capabilities) in servers {
                cache.servers.insert(name, capabilities);
            }
        }
        self.persist_async().await
    }

    /// Fetch capabilities from a backend without touching the cache or disk.
    /// Callers that need batched persistence collect the results from many
    /// servers and pass them to [`upsert_servers`] in one shot.
    pub async fn fetch_from_backend(
        &self,
        name: &str,
        server: &ServerConfig,
        backend: Arc<dyn BackendTransport>,
    ) -> Result<CachedServerCapabilities> {
        let tools = list_items(&backend, METHOD_TOOLS_LIST, "tools", true).await?;
        let prompts = list_items(&backend, METHOD_PROMPTS_LIST, "prompts", false).await?;
        let resources = list_items(&backend, METHOD_RESOURCES_LIST, "resources", false).await?;
        Ok(CachedServerCapabilities::from_parts(
            name, server, tools, prompts, resources,
        ))
    }

    pub async fn refresh_from_backend(
        &self,
        name: &str,
        server: &ServerConfig,
        backend: Arc<dyn BackendTransport>,
    ) -> Result<CachedServerCapabilities> {
        let capabilities = self.fetch_from_backend(name, server, backend).await?;
        self.upsert_server_async(name, capabilities.clone()).await?;
        Ok(capabilities)
    }

    fn insert_into_cache(&self, name: &str, capabilities: CachedServerCapabilities) {
        let mut cache = self.cache.lock();
        cache.version = CACHE_VERSION;
        cache.servers.insert(name.to_string(), capabilities);
    }

    fn persist_payload(&self) -> Option<PersistPayload> {
        let path = self.path.clone()?;
        let cache = self.cache.lock().clone();
        Some(PersistPayload { path, cache })
    }

    fn persist_blocking(&self) -> Result<()> {
        let Some(payload) = self.persist_payload() else {
            return Ok(());
        };
        payload.write_to_disk()
    }

    async fn persist_async(&self) -> Result<()> {
        let Some(payload) = self.persist_payload() else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || payload.write_to_disk())
            .await
            .map_err(|err| GatewayError::Backend(format!("persist join failed: {err}")))?
    }
}

struct PersistPayload {
    path: PathBuf,
    cache: CapabilityCache,
}

impl PersistPayload {
    fn write_to_disk(self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = format!("{}\n", serde_json::to_string_pretty(&self.cache)?);
        let tmp =
            self.path
                .with_extension(format!("tmp-{}-{}", std::process::id(), Uuid::new_v4()));
        fs::write(&tmp, content)?;
        fs::rename(tmp, &self.path)?;
        Ok(())
    }
}

async fn list_items(
    backend: &Arc<dyn BackendTransport>,
    method: &str,
    result_key: &str,
    required: bool,
) -> Result<Vec<Value>> {
    let request = JsonRpcRequest::new(
        JsonRpcId::String(format!("capability-refresh-{}-{}", method, Uuid::new_v4())),
        method,
        None,
    );
    let response = backend.send_request(request).await?;
    if let Some(error) = response.error {
        if required {
            return Err(GatewayError::Backend(format!(
                "capability refresh failed for {method}: {}",
                error.message
            )));
        }
        return Ok(Vec::new());
    }
    let Some(result) = response.result else {
        if required {
            return Err(GatewayError::Backend(format!(
                "capability refresh failed for {method}: missing result"
            )));
        }
        return Ok(Vec::new());
    };
    let Some(items) = result.get(result_key).and_then(Value::as_array) else {
        if required {
            return Err(GatewayError::Backend(format!(
                "capability refresh failed for {method}: missing result.{result_key}"
            )));
        }
        return Ok(Vec::new());
    };
    Ok(items.clone())
}

fn config_fingerprint(server: &ServerConfig) -> String {
    let public_config = json!({
        "transport": server.transport,
        "command": server.command,
        "args": server.args,
        "lazy": server.lazy,
        "singleton": server.singleton,
        "dangerous": server.dangerous,
        "path_allowlist": server.path_allowlist,
        "idle_timeout_seconds": server.idle_timeout_seconds,
        "startup_timeout_seconds": server.startup_timeout_seconds,
        "request_timeout_seconds": server.request_timeout_seconds,
        "restart_policy": server.restart_policy,
    });
    let mut hasher = DefaultHasher::new();
    public_config.to_string().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn now_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    format!("{seconds}")
}
