use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::error::{GatewayError, Result};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TransportKind {
    Stdio,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy {
    Never,
    OnFailure,
    Always,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct DefaultsConfig {
    pub idle_timeout_seconds: u64,
    pub startup_timeout_seconds: u64,
    pub request_timeout_seconds: u64,
    pub restart_policy: RestartPolicy,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            idle_timeout_seconds: 1200,
            startup_timeout_seconds: 30,
            request_timeout_seconds: 60,
            restart_policy: RestartPolicy::OnFailure,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ServerConfig {
    pub transport: TransportKind,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub lazy: Option<bool>,
    pub singleton: Option<bool>,
    pub dangerous: Option<bool>,
    pub path_allowlist: Vec<PathBuf>,
    pub idle_timeout_seconds: Option<u64>,
    pub startup_timeout_seconds: Option<u64>,
    pub request_timeout_seconds: Option<u64>,
    pub restart_policy: Option<RestartPolicy>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            transport: TransportKind::Stdio,
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            lazy: Some(true),
            singleton: None,
            dangerous: None,
            path_allowlist: Vec::new(),
            idle_timeout_seconds: None,
            startup_timeout_seconds: None,
            request_timeout_seconds: None,
            restart_policy: None,
        }
    }
}

impl ServerConfig {
    pub fn lazy(&self) -> bool {
        self.lazy.unwrap_or(true)
    }

    pub fn singleton(&self) -> bool {
        self.singleton.unwrap_or(false)
    }

    pub fn dangerous(&self) -> bool {
        self.dangerous.unwrap_or(false)
    }

    pub fn idle_timeout_seconds(&self, defaults: &DefaultsConfig) -> u64 {
        self.idle_timeout_seconds
            .unwrap_or(defaults.idle_timeout_seconds)
    }

    pub fn startup_timeout_seconds(&self, defaults: &DefaultsConfig) -> u64 {
        self.startup_timeout_seconds
            .unwrap_or(defaults.startup_timeout_seconds)
    }

    pub fn request_timeout_seconds(&self, defaults: &DefaultsConfig) -> u64 {
        self.request_timeout_seconds
            .unwrap_or(defaults.request_timeout_seconds)
    }

    pub fn restart_policy(&self, defaults: &DefaultsConfig) -> RestartPolicy {
        self.restart_policy.unwrap_or(defaults.restart_policy)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct GroupConfig {
    pub servers: Vec<String>,
    pub expose_tools: bool,
    pub expose_prompts: bool,
    pub expose_resources: bool,
}

impl Default for GroupConfig {
    fn default() -> Self {
        Self {
            servers: Vec::new(),
            expose_tools: true,
            expose_prompts: true,
            expose_resources: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ClientsConfig {
    pub default_group: String,
    pub server_name: String,
    pub managed_clients: Vec<String>,
}

impl Default for ClientsConfig {
    fn default() -> Self {
        Self {
            default_group: "coding".to_string(),
            server_name: "mcp-gateway".to_string(),
            managed_clients: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct LimitsConfig {
    pub max_clients: usize,
    pub max_message_bytes: usize,
    pub max_batch_items: usize,
    pub max_pending_requests_per_backend: usize,
    pub backend_stdin_queue: usize,
    pub max_session_events: usize,
    pub session_ttl_seconds: u64,
    pub max_log_entries_per_server: usize,
    pub max_log_line_bytes: usize,
    pub backend_notification_broadcast: usize,
    pub gateway_notification_broadcast: usize,
    pub max_restart_attempts: usize,
    pub restart_window_seconds: u64,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_clients: 16,
            max_message_bytes: 8_388_608,
            max_batch_items: 32,
            max_pending_requests_per_backend: 128,
            backend_stdin_queue: 64,
            max_session_events: 128,
            session_ttl_seconds: 900,
            max_log_entries_per_server: 256,
            max_log_line_bytes: 8192,
            backend_notification_broadcast: 128,
            gateway_notification_broadcast: 256,
            max_restart_attempts: 3,
            restart_window_seconds: 60,
        }
    }
}

impl LimitsConfig {
    pub fn validate(&self) -> Result<()> {
        let checks = [
            ("max_clients", self.max_clients),
            ("max_message_bytes", self.max_message_bytes),
            ("max_batch_items", self.max_batch_items),
            (
                "max_pending_requests_per_backend",
                self.max_pending_requests_per_backend,
            ),
            ("backend_stdin_queue", self.backend_stdin_queue),
            ("max_session_events", self.max_session_events),
            (
                "max_log_entries_per_server",
                self.max_log_entries_per_server,
            ),
            ("max_log_line_bytes", self.max_log_line_bytes),
            (
                "backend_notification_broadcast",
                self.backend_notification_broadcast,
            ),
            (
                "gateway_notification_broadcast",
                self.gateway_notification_broadcast,
            ),
            ("max_restart_attempts", self.max_restart_attempts),
        ];
        for (name, value) in checks {
            if value == 0 {
                return Err(GatewayError::Config(format!(
                    "limits.{name} must be greater than zero"
                )));
            }
        }
        if self.session_ttl_seconds == 0 {
            return Err(GatewayError::Config(
                "limits.session_ttl_seconds must be greater than zero".to_string(),
            ));
        }
        if self.restart_window_seconds == 0 {
            return Err(GatewayError::Config(
                "limits.restart_window_seconds must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }
}

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
    #[serde(default)]
    pub idle_timeout_secs: Option<u64>,
    #[serde(default)]
    pub startup_timeout_secs: Option<u64>,
    #[serde(default)]
    pub request_timeout_secs: Option<u64>,
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
            idle_timeout_secs: None,
            startup_timeout_secs: None,
            request_timeout_secs: None,
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
            idle_timeout_seconds: self.runtime.idle_timeout_secs,
            startup_timeout_seconds: self.runtime.startup_timeout_secs,
            request_timeout_seconds: self.runtime.request_timeout_secs,
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

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct MetricsConfig {
    pub sample_interval_secs: u64,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            sample_interval_secs: 5,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    pub listen: String,
    pub defaults: DefaultsConfig,
    pub servers: BTreeMap<String, ServerConfig>,
    pub groups: BTreeMap<String, GroupConfig>,
    pub clients: ClientsConfig,
    pub limits: LimitsConfig,
    pub metrics: MetricsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:0".to_string(),
            defaults: DefaultsConfig::default(),
            servers: BTreeMap::new(),
            groups: BTreeMap::new(),
            clients: ClientsConfig::default(),
            limits: LimitsConfig::default(),
            metrics: MetricsConfig::default(),
        }
    }
}

impl Config {
    pub fn load_file(path: impl AsRef<Path>) -> Result<Self> {
        let text = fs::read_to_string(path.as_ref())?;
        let mut cfg = Self::load_str(&text)?;
        // Merge user-installed servers from `<config_dir>/servers.d/*.yaml`.
        // Resolved relative to the gateway.yaml we just loaded so a test-only
        // config layout (gateway.yaml + sibling servers.d/) Just Works.
        if let Some(config_dir) = path.as_ref().parent() {
            cfg.apply_servers_d_overlay(&config_dir.join("servers.d"))?;
            cfg.validate()?;
        }
        Ok(cfg)
    }

    pub fn load_str(text: &str) -> Result<Self> {
        reject_duplicate_keys(text, "servers")?;
        reject_duplicate_keys(text, "groups")?;
        let expanded = expand_env_placeholders(text)?;
        let cfg: Config = serde_yaml::from_str(&expanded)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Merge every `*.yaml` file under `dir` into `self.servers`. Each file
    /// describes one user-imported managed server. Disabled entries are
    /// skipped. Built-in servers defined in the upstream `gateway.yaml` always
    /// win: overlay files can extend but never override.
    pub fn apply_servers_d_overlay(&mut self, dir: &Path) -> Result<Vec<ManagedServerOverlay>> {
        let mut applied = Vec::new();
        let entries = match fs::read_dir(dir) {
            Ok(read) => read,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(applied),
            Err(err) => return Err(err.into()),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if !path
                .extension()
                .is_some_and(|ext| ext == "yaml" || ext == "yml")
            {
                continue;
            }
            let text = fs::read_to_string(&path)?;
            let expanded = expand_env_placeholders(&text)?;
            let overlay: ManagedServerOverlay = serde_yaml::from_str(&expanded).map_err(|err| {
                GatewayError::Config(format!("invalid servers.d file {}: {err}", path.display()))
            })?;
            if !overlay.enabled {
                applied.push(overlay);
                continue;
            }
            if overlay.id.trim().is_empty() {
                return Err(GatewayError::Config(format!(
                    "servers.d file {} is missing `id`",
                    path.display()
                )));
            }
            if self.servers.contains_key(&overlay.id) {
                // Built-in or duplicated overlay wins — refuse to silently shadow
                // an already-defined server so operators see the conflict.
                return Err(GatewayError::Config(format!(
                    "servers.d file {} defines `{}` which is already configured upstream",
                    path.display(),
                    overlay.id
                )));
            }
            let server = overlay.to_server_config()?;
            self.servers.insert(overlay.id.clone(), server);
            for group in &overlay.group_memberships {
                if let Some(group) = self.groups.get_mut(group) {
                    if !group.servers.iter().any(|s| s == &overlay.id) {
                        group.servers.push(overlay.id.clone());
                    }
                }
            }
            applied.push(overlay);
        }
        Ok(applied)
    }

    pub fn validate(&self) -> Result<()> {
        self.limits.validate()?;
        validate_loopback_listen_addr(&self.listen)?;

        for (name, server) in &self.servers {
            validate_identifier("server", name)?;
            if server.command.trim().is_empty() {
                return Err(GatewayError::Config(format!(
                    "server '{name}' must define a command"
                )));
            }
        }

        for (group_name, group) in &self.groups {
            validate_identifier("group", group_name)?;
            let mut seen = BTreeSet::new();
            for server in &group.servers {
                if !self.servers.contains_key(server) {
                    return Err(GatewayError::Config(format!(
                        "group '{group_name}' references unknown server '{server}'"
                    )));
                }
                if !seen.insert(server) {
                    return Err(GatewayError::Config(format!(
                        "group '{group_name}' contains duplicate server '{server}'"
                    )));
                }
            }
        }

        if !self.groups.is_empty() && !self.groups.contains_key(&self.clients.default_group) {
            return Err(GatewayError::Config(format!(
                "clients.default_group '{}' does not exist",
                self.clients.default_group
            )));
        }
        Ok(())
    }
}

pub fn validate_identifier(kind: &str, value: &str) -> Result<()> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(GatewayError::Config(format!("{kind} id must not be empty")));
    };
    let valid = first.is_ascii_alphanumeric()
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    if !valid {
        return Err(GatewayError::Config(format!(
            "{kind} id '{value}' is not path-safe; use ASCII letters, digits, '-' or '_'"
        )));
    }
    Ok(())
}

fn validate_loopback_listen_addr(listen: &str) -> Result<()> {
    let addr: SocketAddr = listen.parse().map_err(|err| {
        GatewayError::Config(format!(
            "listen address '{listen}' must be an IP socket address: {err}"
        ))
    })?;
    if !addr.ip().is_loopback() {
        return Err(GatewayError::Config(format!(
            "listen address '{listen}' must be loopback-only; use 127.0.0.1 or ::1"
        )));
    }
    Ok(())
}

fn expand_env_placeholders(text: &str) -> Result<String> {
    let re = Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}").expect("valid env regex");
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in re.captures_iter(text) {
        let m = caps.get(0).expect("whole match");
        out.push_str(&text[last..m.start()]);
        let key = &caps[1];
        let value = std::env::var(key).map_err(|_| {
            GatewayError::Config(format!(
                "environment variable '{key}' is required by config"
            ))
        })?;
        out.push_str(&value);
        last = m.end();
    }
    out.push_str(&text[last..]);
    Ok(out)
}

fn reject_duplicate_keys(text: &str, section: &str) -> Result<()> {
    let mut in_section = false;
    let mut section_indent = 0usize;
    let mut seen = BTreeSet::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if !in_section {
            if trimmed == format!("{section}:") {
                in_section = true;
                section_indent = indent;
            }
            continue;
        }
        if indent <= section_indent {
            break;
        }
        if indent == section_indent + 2 && trimmed.ends_with(':') {
            let key = trimmed
                .trim_end_matches(':')
                .trim_matches('"')
                .trim_matches('\'');
            if !seen.insert(key.to_string()) {
                return Err(GatewayError::Config(format!(
                    "duplicate {section} entry '{key}'"
                )));
            }
        }
    }

    Ok(())
}
