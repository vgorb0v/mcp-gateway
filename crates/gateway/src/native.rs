use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;

pub const LAUNCH_AGENT_LABEL: &str = "io.github.mcpgateway.daemon";
pub const LEGACY_LAUNCH_AGENT_LABEL: &str = concat!("com.", "vgor", "bov", ".mcp-gateway");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeInstallPaths {
    pub root: PathBuf,
    pub bin_dir: PathBuf,
    pub backends_dir: PathBuf,
    pub browsers_dir: PathBuf,
    pub chrome_for_testing_dir: PathBuf,
    pub mcp_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub capability_cache_file: PathBuf,
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub config_servers_d_dir: PathBuf,
    pub env_file: PathBuf,
    pub run_dir: PathBuf,
    pub state_file: PathBuf,
    pub logs_dir: PathBuf,
    pub backups_dir: PathBuf,
    pub audit_log_file: PathBuf,
    pub launch_agent_file: PathBuf,
    pub legacy_launch_agent_file: PathBuf,
}

impl NativeInstallPaths {
    pub fn for_home(home: impl AsRef<Path>) -> Self {
        let home = home.as_ref();
        let root = home.join(".mcp-gateway");
        let bin_dir = root.join("bin");
        let backends_dir = root.join("mcp-backends");
        let browsers_dir = root.join("browsers");
        let chrome_for_testing_dir = browsers_dir.join("chrome-for-testing");
        let mcp_dir = root.join("mcps");
        let cache_dir = root.join("cache");
        let capability_cache_file = cache_dir.join("mcp_manifest_cache.json");
        let config_dir = root.join("config");
        let config_file = config_dir.join("gateway.yaml");
        let config_servers_d_dir = config_dir.join("servers.d");
        let env_file = root.join("env");
        let run_dir = root.join("run");
        let state_file = run_dir.join("state.json");
        let logs_dir = root.join("logs");
        let backups_dir = root.join("backups");
        let audit_log_file = root.join("audit.log");
        let launch_agent_file = home
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LAUNCH_AGENT_LABEL}.plist"));
        let legacy_launch_agent_file = home
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LEGACY_LAUNCH_AGENT_LABEL}.plist"));
        Self {
            root,
            bin_dir,
            backends_dir,
            browsers_dir,
            chrome_for_testing_dir,
            mcp_dir,
            cache_dir,
            capability_cache_file,
            config_dir,
            config_file,
            config_servers_d_dir,
            env_file,
            run_dir,
            state_file,
            logs_dir,
            backups_dir,
            audit_log_file,
            launch_agent_file,
            legacy_launch_agent_file,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct GatewayStateFile {
    pub base_url: String,
    pub pid: u32,
}

pub fn write_state_file(path: impl AsRef<Path>, addr: SocketAddr) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let state = GatewayStateFile {
        base_url: format!("http://{addr}"),
        pid: std::process::id(),
    };
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(&state)?))?;
    set_owner_only_permissions(path)?;
    Ok(())
}

pub fn write_owner_only_file(path: impl AsRef<Path>, content: &str) -> Result<bool> {
    let path = path.as_ref();
    let changed = fs::read_to_string(path).ok().as_deref() != Some(content);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if changed {
        fs::write(path, content)?;
    }
    set_owner_only_permissions(path)?;
    Ok(changed)
}

#[cfg(unix)]
pub fn set_owner_only_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
pub fn set_owner_only_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

pub fn read_state_file(path: impl AsRef<Path>) -> Result<GatewayStateFile> {
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

pub fn load_env_file(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = unquote_env_value(value.trim());
        std::env::set_var(key, value);
    }
    Ok(())
}

pub fn render_launch_agent_plist(paths: &NativeInstallPaths) -> String {
    let gateway_bin = paths.bin_dir.join("mcp-gateway");
    let backend_bin = paths.backends_dir.join("node_modules/.bin");
    let path_env = format!(
        "{}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        backend_bin.display()
    );
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{gateway_bin}</string>
    <string>serve</string>
    <string>--config</string>
    <string>{config_file}</string>
    <string>--env-file</string>
    <string>{env_file}</string>
    <string>--state-file</string>
    <string>{state_file}</string>
    <string>--capability-cache-file</string>
    <string>{capability_cache_file}</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path_env}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ProcessType</key>
  <string>Background</string>
  <key>LowPriorityIO</key>
  <true/>
  <key>Nice</key>
  <integer>5</integer>
  <key>WorkingDirectory</key>
  <string>{root}</string>
  <key>StandardOutPath</key>
  <string>{stdout_log}</string>
  <key>StandardErrorPath</key>
  <string>{stderr_log}</string>
</dict>
</plist>
"#,
        label = escape_xml(LAUNCH_AGENT_LABEL),
        gateway_bin = escape_xml(&gateway_bin.to_string_lossy()),
        config_file = escape_xml(&paths.config_file.to_string_lossy()),
        env_file = escape_xml(&paths.env_file.to_string_lossy()),
        state_file = escape_xml(&paths.state_file.to_string_lossy()),
        capability_cache_file = escape_xml(&paths.capability_cache_file.to_string_lossy()),
        path_env = escape_xml(&path_env),
        root = escape_xml(&paths.root.to_string_lossy()),
        stdout_log = escape_xml(&paths.logs_dir.join("gateway.out.log").to_string_lossy()),
        stderr_log = escape_xml(&paths.logs_dir.join("gateway.err.log").to_string_lossy()),
    )
}

fn unquote_env_value(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
