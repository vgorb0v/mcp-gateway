pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod manifest;
pub mod reconciler;

pub use reconciler::{
    ApplyOutcome, ClientConfigDiff, ClientConfigReconciler, ClientDiff, ClientDriftStatus,
    ClientStatus, EntryChange, ManagedEntry, ReconcilerOptions,
};

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::ValueEnum;
use serde_json::{json, Value};

use crate::config::validate_identifier;
use crate::error::{GatewayError, Result};
use crate::native::NativeInstallPaths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, ValueEnum)]
pub enum ClientKind {
    Codex,
    ClaudeCode,
    ClaudeDesktop,
    Antigravity,
    Vscode,
}

impl ClientKind {
    pub fn from_config_name(name: &str) -> Option<Self> {
        match name {
            "codex" => Some(Self::Codex),
            "claude-code" => Some(Self::ClaudeCode),
            "claude-desktop" => Some(Self::ClaudeDesktop),
            "antigravity" => Some(Self::Antigravity),
            "vscode" => Some(Self::Vscode),
            _ => None,
        }
    }

    /// Stable identifier emitted as the `--client` argument in generated
    /// client configs and accepted by `mcp-gateway-bridge --client`.
    pub fn config_name(self) -> &'static str {
        match self {
            ClientKind::Codex => "codex",
            ClientKind::ClaudeCode => "claude-code",
            ClientKind::ClaudeDesktop => "claude-desktop",
            ClientKind::Antigravity => "antigravity",
            ClientKind::Vscode => "vscode",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApplyConfigOptions {
    pub clients: Vec<ClientKind>,
    pub servers: Vec<String>,
    pub bridge_bin: PathBuf,
    pub mcp_dir: PathBuf,
    pub state_file: PathBuf,
    pub dedupe: Vec<String>,
    pub dry_run: bool,
    pub home: PathBuf,
}

pub fn apply_configs(options: ApplyConfigOptions) -> Result<Vec<PathBuf>> {
    validate_server_names(&options.servers)?;
    if options.dry_run {
        return apply_configs_dry_run(options);
    }
    // Production path: route through the reconciler so every legacy caller
    // gets named backups + manifest tracking for free. The reconciler picks
    // up paths from `NativeInstallPaths::for_home(&options.home)` so behavior
    // matches `install-native` even when callers pass custom mcp_dir/state.
    let paths = NativeInstallPaths::for_home(&options.home);
    let reconciler_options = ReconcilerOptions {
        clients: options.clients.clone(),
        servers: options.servers.clone(),
        bridge_bin: options.bridge_bin.clone(),
        mcp_dir: options.mcp_dir.clone(),
        state_file: options.state_file.clone(),
        dedupe: options.dedupe.clone(),
        home: options.home.clone(),
        manifest_path: paths.root.join("state/client-manifest.json"),
        backups_dir: paths.backups_dir.clone(),
    };
    let outcome = ClientConfigReconciler::new(reconciler_options).apply("apply-configs")?;
    Ok(outcome.changed_paths)
}

/// Dry-run path keeps the legacy semantics: render each client config in
/// memory and report which files *would* change, without touching disk.
fn apply_configs_dry_run(options: ApplyConfigOptions) -> Result<Vec<PathBuf>> {
    validate_server_names(&options.servers)?;
    let mut changed = Vec::new();
    for server in &options.servers {
        let path = options.mcp_dir.join(server);
        let content = render_server_shim(&options.bridge_bin, &options.state_file, server);
        if write_if_changed(&path, &content, true)? {
            changed.push(path);
        }
    }
    for client in &options.clients {
        let path = client_path(*client, &options.home);
        let client_id = Some(client.config_name());
        let rendered = match client {
            ClientKind::Codex => codex::render_codex_config(
                read_optional(&path)?.as_deref(),
                &options.mcp_dir,
                &options.servers,
                &options.dedupe,
                client_id,
            )?,
            ClientKind::ClaudeCode | ClientKind::ClaudeDesktop | ClientKind::Antigravity => {
                render_json_config(
                    read_optional(&path)?.as_deref(),
                    &options.mcp_dir,
                    &options.servers,
                    &options.dedupe,
                    client_id,
                )?
            }
            ClientKind::Vscode => render_vscode_mcp_config(
                read_optional(&path)?.as_deref(),
                &options.mcp_dir,
                &options.servers,
                &options.dedupe,
                client_id,
            )?,
        };
        if write_if_changed(&path, &rendered, true)? {
            validate_client_file(*client, &path, &rendered, true)?;
            changed.push(path);
        }
        if matches!(client, ClientKind::Vscode) {
            let settings_path = vscode_settings_path(&options.home);
            let settings = render_vscode_settings(read_optional(&settings_path)?.as_deref())?;
            if write_if_changed(&settings_path, &settings, true)? {
                validate_json_file(&settings_path, &settings, true)?;
                changed.push(settings_path);
            }
        }
    }
    Ok(changed)
}

pub(super) fn validate_server_names(server_names: &[String]) -> Result<()> {
    for name in server_names {
        validate_identifier("server", name)?;
    }
    Ok(())
}

pub(super) fn render_server_shim(bridge_bin: &Path, state_file: &Path, server: &str) -> String {
    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
exec {bridge_bin} --state-file {state_file} --server {server} "$@"
"#,
        bridge_bin = shell_quote(&bridge_bin.to_string_lossy()),
        state_file = shell_quote(&state_file.to_string_lossy()),
        server = shell_quote(server),
    )
}

#[cfg(unix)]
pub(super) fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

pub fn client_path(client: ClientKind, home: &Path) -> PathBuf {
    match client {
        ClientKind::Codex => home.join(".codex/config.toml"),
        ClientKind::ClaudeCode => home.join(".claude.json"),
        ClientKind::ClaudeDesktop => {
            #[cfg(target_os = "windows")]
            {
                std::env::var_os("APPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join("AppData/Roaming"))
                    .join("Claude/claude_desktop_config.json")
            }
            #[cfg(not(target_os = "windows"))]
            {
                home.join("Library/Application Support/Claude/claude_desktop_config.json")
            }
        }
        ClientKind::Antigravity => home.join(".gemini/antigravity/mcp_config.json"),
        ClientKind::Vscode => home.join("Library/Application Support/Code/User/mcp.json"),
    }
}

pub(super) fn vscode_settings_path(home: &Path) -> PathBuf {
    home.join("Library/Application Support/Code/User/settings.json")
}

pub(crate) fn render_json_config(
    existing: Option<&str>,
    mcp_dir: &Path,
    server_names: &[String],
    dedupe: &[String],
    client_id: Option<&str>,
) -> Result<String> {
    let mut root = match existing {
        Some(text) if !text.trim().is_empty() => serde_json::from_str::<Value>(text)?,
        _ => json!({}),
    };
    if !root.is_object() {
        return Err(GatewayError::Config(
            "JSON client config root must be an object".to_string(),
        ));
    }
    let object = root.as_object_mut().expect("object checked");
    let servers = object.entry("mcpServers").or_insert_with(|| json!({}));
    if !servers.is_object() {
        return Err(GatewayError::Config(
            "mcpServers must be a JSON object".to_string(),
        ));
    }
    let servers = servers.as_object_mut().expect("object checked");
    servers.remove("mcp-gateway");
    for name in dedupe {
        servers.remove(name);
    }
    let args = client_args(client_id);
    for name in server_names {
        servers.insert(
            name.clone(),
            json!({
                "command": mcp_dir.join(name).to_string_lossy(),
                "args": args,
            }),
        );
    }
    Ok(format!("{}\n", serde_json::to_string_pretty(&root)?))
}

pub(crate) fn render_vscode_mcp_config(
    existing: Option<&str>,
    mcp_dir: &Path,
    server_names: &[String],
    dedupe: &[String],
    client_id: Option<&str>,
) -> Result<String> {
    let mut root = match existing {
        Some(text) if !text.trim().is_empty() => serde_json::from_str::<Value>(text)?,
        _ => json!({}),
    };
    if !root.is_object() {
        return Err(GatewayError::Config(
            "VS Code MCP config root must be a JSON object".to_string(),
        ));
    }
    let object = root.as_object_mut().expect("object checked");
    let servers = object.entry("servers").or_insert_with(|| json!({}));
    if !servers.is_object() {
        return Err(GatewayError::Config(
            "VS Code MCP servers must be a JSON object".to_string(),
        ));
    }
    let servers = servers.as_object_mut().expect("object checked");
    servers.remove("mcp-gateway");
    for name in dedupe {
        servers.remove(name);
    }
    let args = client_args(client_id);
    for name in server_names {
        servers.insert(
            name.clone(),
            json!({
                "type": "stdio",
                "command": mcp_dir.join(name).to_string_lossy(),
                "args": args,
            }),
        );
    }
    Ok(format!("{}\n", serde_json::to_string_pretty(&root)?))
}

pub(super) fn client_args(client_id: Option<&str>) -> Vec<String> {
    match client_id {
        Some(id) => vec!["--client".to_string(), id.to_string()],
        None => Vec::new(),
    }
}

pub(super) fn render_vscode_settings(existing: Option<&str>) -> Result<String> {
    let mut root = match existing {
        Some(text) if !text.trim().is_empty() => serde_json::from_str::<Value>(text)?,
        _ => json!({}),
    };
    if !root.is_object() {
        return Err(GatewayError::Config(
            "VS Code settings root must be a JSON object".to_string(),
        ));
    }
    root.as_object_mut()
        .expect("object checked")
        .insert("chat.mcp.autostart".to_string(), Value::Bool(true));
    Ok(format!("{}\n", serde_json::to_string_pretty(&root)?))
}

pub(super) fn remove_antigravity_gateway_cache(home: &Path, dry_run: bool) -> Result<Vec<PathBuf>> {
    let path = home.join(".gemini/antigravity/mcp/mcp-gateway");
    if !path.exists() {
        return Ok(Vec::new());
    }
    if !dry_run {
        fs::remove_dir_all(&path)?;
    }
    Ok(vec![path])
}

pub(super) fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn write_if_changed(path: &Path, content: &str, dry_run: bool) -> Result<bool> {
    let existing = read_optional(path)?;
    if existing.as_deref() == Some(content) {
        return Ok(false);
    }
    if dry_run {
        return Ok(true);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if existing.is_some() {
        let backup = backup_path(path);
        fs::copy(path, backup)?;
    }
    fs::write(path, content)?;
    Ok(true)
}

pub(super) fn validate_client_file(
    client: ClientKind,
    path: &Path,
    rendered: &str,
    dry_run: bool,
) -> Result<()> {
    let text = if dry_run {
        rendered.to_string()
    } else {
        fs::read_to_string(path)?
    };
    match client {
        ClientKind::Codex => {
            let _ = text.parse::<toml_edit::DocumentMut>()?;
        }
        ClientKind::ClaudeCode | ClientKind::ClaudeDesktop | ClientKind::Antigravity => {
            let _ = serde_json::from_str::<Value>(&text)?;
        }
        ClientKind::Vscode => {
            let _ = serde_json::from_str::<Value>(&text)?;
        }
    }
    Ok(())
}

pub(super) fn validate_json_file(path: &Path, rendered: &str, dry_run: bool) -> Result<()> {
    let text = if dry_run {
        rendered.to_string()
    } else {
        fs::read_to_string(path)?
    };
    let _ = serde_json::from_str::<Value>(&text)?;
    Ok(())
}

fn backup_path(path: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "config".to_string());
    path.with_file_name(format!("{file_name}.bak.{stamp}"))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
