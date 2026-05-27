use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};
use mcp_gateway::native::{read_state_file, NativeInstallPaths};

use crate::cli::{
    install_native, run_refresh_capabilities, write_if_changed, InstallNativeOptions, Provision,
};

pub(crate) struct DemoOptions {
    pub(crate) home: Option<PathBuf>,
    pub(crate) keep: bool,
}

struct DemoCleanup {
    path: PathBuf,
    enabled: bool,
}

impl Drop for DemoCleanup {
    fn drop(&mut self) {
        if self.enabled {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

struct DemoDaemon {
    child: Child,
}

impl Drop for DemoDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(crate) fn run_demo(options: DemoOptions) -> Result<()> {
    let generated_home = options.home.is_none();
    let home = options.home.unwrap_or_else(demo_temp_home);
    fs::create_dir_all(&home)?;
    let _cleanup = DemoCleanup {
        path: home.clone(),
        enabled: generated_home && !options.keep,
    };
    let paths = NativeInstallPaths::for_home(&home);

    println!("Using demo home {}", home.display());
    install_native(InstallNativeOptions {
        clients: Vec::new(),
        group: "coding".to_string(),
        provision: Vec::<Provision>::new(),
        skip_pnpm_install: true,
        skip_launchctl: true,
        add_to_path: false,
        no_path_prompt: true,
        homebrew: false,
        home: home.clone(),
    })?;

    fs::create_dir_all(&paths.config_servers_d_dir)?;
    let overlay_path = paths.config_servers_d_dir.join("demo-tools.yaml");
    write_if_changed(&overlay_path, &render_demo_overlay(&paths)?)?;
    run_refresh_capabilities(
        "all".to_string(),
        "coding".to_string(),
        None,
        None,
        Some(home.clone()),
    )?;

    let _daemon = start_demo_daemon(&paths)?;
    let shim = paths.mcp_dir.join("demo-tools");
    let responses = run_demo_shim_conversation(&shim)?;
    println!("initialize: {}", demo_response_summary(&responses, 1));
    println!("tools/list: {}", demo_response_summary(&responses, 2));
    println!("tools/call: {}", demo_response_summary(&responses, 3));
    if options.keep || !generated_home {
        println!("Demo files kept at {}", home.display());
    }
    Ok(())
}

fn demo_temp_home() -> PathBuf {
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("mcp-gateway-demo-{}-{suffix}", std::process::id()))
}

fn render_demo_overlay(paths: &NativeInstallPaths) -> Result<String> {
    let command = yaml_scalar(&paths.bin_dir.join("mcpgateway").to_string_lossy())?;
    Ok(format!(
        r#"id: demo-tools
display_name: "Demo tools"
enabled: true
group_memberships:
  - coding
runtime:
  transport: stdio
  command: {command}
  args:
    - "__demo-server"
  lazy: true
"#
    ))
}

fn yaml_scalar(value: &str) -> Result<String> {
    Ok(serde_yaml::to_string(value)?.trim().to_string())
}

fn start_demo_daemon(paths: &NativeInstallPaths) -> Result<DemoDaemon> {
    let child = Command::new(paths.bin_dir.join("mcp-gateway"))
        .args([
            "serve",
            "--config",
            paths.config_file.to_str().unwrap_or_default(),
            "--env-file",
            paths.env_file.to_str().unwrap_or_default(),
            "--state-file",
            paths.state_file.to_str().unwrap_or_default(),
            "--capability-cache-file",
            paths.capability_cache_file.to_str().unwrap_or_default(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to start demo gateway daemon")?;
    wait_for_state_file(&paths.state_file)?;
    Ok(DemoDaemon { child })
}

fn wait_for_state_file(path: &Path) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if path.exists() && read_state_file(path).is_ok() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    anyhow::bail!("demo gateway did not write {}", path.display())
}

fn run_demo_shim_conversation(shim: &Path) -> Result<Vec<serde_json::Value>> {
    let mut child = Command::new(shim)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run demo shim {}", shim.display()))?;
    {
        let stdin = child.stdin.as_mut().context("demo shim stdin missing")?;
        for message in [
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"mcpgateway-demo","version":env!("CARGO_PKG_VERSION")}}}),
            serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"echo","arguments":{"message":"hello from mcp-gateway"}}}),
        ] {
            writeln!(stdin, "{message}")?;
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        anyhow::bail!(
            "demo shim failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut responses = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if !line.trim().is_empty() {
            responses.push(serde_json::from_str(line)?);
        }
    }
    Ok(responses)
}

fn demo_response_summary(responses: &[serde_json::Value], id: i64) -> String {
    let Some(response) = responses
        .iter()
        .find(|response| response.get("id").and_then(serde_json::Value::as_i64) == Some(id))
    else {
        return "missing response".to_string();
    };
    if let Some(error) = response.get("error") {
        return format!("error {error}");
    }
    match id {
        1 => response["result"]["serverInfo"]["name"]
            .as_str()
            .unwrap_or("ok")
            .to_string(),
        2 => response["result"]["tools"]
            .as_array()
            .map(|tools| format!("{} tool(s)", tools.len()))
            .unwrap_or_else(|| "ok".to_string()),
        3 => response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("ok")
            .to_string(),
        _ => "ok".to_string(),
    }
}

pub(crate) fn run_demo_server() -> Result<()> {
    let name = "demo-tools";
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: serde_json::Value = serde_json::from_str(&line)?;
        if msg.get("id").is_none() {
            continue;
        }
        let id = msg.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let method = msg
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let response = match method {
            "initialize" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": { "listChanged": true },
                        "prompts": { "listChanged": true },
                        "resources": { "listChanged": true }
                    },
                    "serverInfo": { "name": name, "version": env!("CARGO_PKG_VERSION") }
                }
            }),
            "tools/list" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [
                        { "name": "echo", "description": "Echo demo arguments", "inputSchema": { "type": "object" } }
                    ]
                }
            }),
            "tools/call" => {
                let params = msg
                    .get("params")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": format!("{name}:{params}") }]
                    }
                })
            }
            "prompts/list" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "prompts": [] }
            }),
            "resources/list" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "resources": [] }
            }),
            _ => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("unknown method {method}") }
            }),
        };
        println!("{response}");
        io::stdout().flush()?;
    }
    Ok(())
}
