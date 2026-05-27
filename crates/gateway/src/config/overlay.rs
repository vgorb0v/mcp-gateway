use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{RestartPolicy, ServerConfig, TransportKind};
use crate::error::{GatewayError, Result};

/// Single-server YAML schema accepted in `~/.mcp-gateway/config/servers.d/`.
/// Lives separately from `ServerConfig` so the overlay schema can grow
/// without bleeding into the core validated config struct.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ManagedServerOverlay {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub group_memberships: Vec<String>,
    pub runtime: ManagedServerRuntime,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub secrets: Vec<ManagedServerSecret>,
}

impl Default for ManagedServerOverlay {
    fn default() -> Self {
        Self {
            id: String::new(),
            enabled: true,
            display_name: None,
            group_memberships: Vec::new(),
            runtime: ManagedServerRuntime::default(),
            env: BTreeMap::new(),
            secrets: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ManagedServerRuntime {
    #[serde(default = "default_stdio_transport")]
    pub transport: TransportKind,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub lazy: Option<bool>,
    #[serde(alias = "idle_timeout_secs", default)]
    pub idle_timeout_seconds: Option<u64>,
    #[serde(alias = "startup_timeout_secs", default)]
    pub startup_timeout_seconds: Option<u64>,
    #[serde(alias = "request_timeout_secs", default)]
    pub request_timeout_seconds: Option<u64>,
    #[serde(default)]
    pub restart_policy: Option<RestartPolicy>,
    #[serde(default)]
    pub path_allowlist: Vec<PathBuf>,
    #[serde(default)]
    pub dangerous: Option<bool>,
    #[serde(default)]
    pub singleton: Option<bool>,
}

impl Default for ManagedServerRuntime {
    fn default() -> Self {
        Self {
            transport: TransportKind::Stdio,
            command: String::new(),
            args: Vec::new(),
            lazy: None,
            idle_timeout_seconds: None,
            startup_timeout_seconds: None,
            request_timeout_seconds: None,
            restart_policy: None,
            path_allowlist: Vec::new(),
            dangerous: None,
            singleton: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ManagedServerSecret {
    pub name: String,
    #[serde(default)]
    pub required: bool,
}

impl ManagedServerOverlay {
    pub fn to_server_config(&self) -> Result<ServerConfig> {
        if self.runtime.command.trim().is_empty() {
            return Err(GatewayError::Config(format!(
                "managed server `{}` is missing runtime.command",
                self.id
            )));
        }
        Ok(ServerConfig {
            transport: self.runtime.transport.clone(),
            command: self.runtime.command.clone(),
            args: self.runtime.args.clone(),
            env: self.env.clone(),
            lazy: self.runtime.lazy,
            singleton: self.runtime.singleton,
            dangerous: self.runtime.dangerous,
            path_allowlist: self.runtime.path_allowlist.clone(),
            idle_timeout_seconds: self.runtime.idle_timeout_seconds,
            startup_timeout_seconds: self.runtime.startup_timeout_seconds,
            request_timeout_seconds: self.runtime.request_timeout_seconds,
            restart_policy: self.runtime.restart_policy,
        })
    }
}

fn default_true() -> bool {
    true
}

fn default_stdio_transport() -> TransportKind {
    TransportKind::Stdio
}
