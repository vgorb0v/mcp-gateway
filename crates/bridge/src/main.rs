use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context};
use clap::Parser;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use url::Url;

const MAX_STDIN_LINE_BYTES: usize = 8_388_608;
const GATEWAY_READY_RETRIES: usize = 50;
const GATEWAY_READY_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

#[derive(Debug, Parser)]
#[command(
    name = "mcp-gateway-bridge",
    version,
    about = "stdio bridge for MCP Gateway"
)]
struct Cli {
    #[arg(long)]
    base_url: Option<String>,
    #[arg(long, default_value = "coding")]
    group: String,
    #[arg(long)]
    server: Option<String>,
    #[arg(long)]
    url: Option<String>,
    #[arg(long)]
    state_file: Option<PathBuf>,
    /// Identifier of the client launching this bridge process (e.g. `codex`,
    /// `claude-code`, `claude-desktop`, `antigravity`, `vscode`). Passed through
    /// to the gateway so sessions can be attributed per client.
    #[arg(long)]
    client: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BridgeTarget {
    Group(String),
    Server(String),
}

#[derive(Debug, Deserialize)]
struct GatewayStateFile {
    base_url: String,
}

// The bridge process is a thin stdin/stdout → HTTP shuttle. Each MCP client
// spawns its own bridge per backend, so dozens may run concurrently — using a
// single-threaded runtime keeps idle worker threads from accumulating.
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let (base_url, target, client_name) = resolve_target(&cli)?;
    run_bridge(base_url, target, client_name).await
}

fn resolve_target(cli: &Cli) -> anyhow::Result<(String, BridgeTarget, Option<String>)> {
    if let Some(url) = &cli.url {
        let parsed = Url::parse(url).context("invalid --url")?;
        let segments: Vec<_> = parsed
            .path_segments()
            .ok_or_else(|| anyhow!("--url has no path"))?
            .collect();
        if segments.len() >= 3 && segments[0] == "groups" && segments[2] == "mcp" {
            let mut base = parsed.clone();
            base.set_path("");
            base.set_query(None);
            return Ok((
                base.as_str().trim_end_matches('/').to_string(),
                BridgeTarget::Group(segments[1].to_string()),
                cli.client.clone(),
            ));
        }
    }
    let base_url = match &cli.base_url {
        Some(base_url) => base_url.trim_end_matches('/').to_string(),
        None => read_base_url_from_state(
            cli.state_file
                .clone()
                .unwrap_or_else(default_state_file)
                .as_path(),
        )?,
    };
    let target = match &cli.server {
        Some(server) => BridgeTarget::Server(server.clone()),
        None => BridgeTarget::Group(cli.group.clone()),
    };
    Ok((base_url, target, cli.client.clone()))
}

async fn run_bridge(
    base_url: String,
    target: BridgeTarget,
    client_name: Option<String>,
) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    wait_for_gateway_health(&client, &base_url).await?;
    let mut session_id =
        create_bridge_session(&client, &base_url, &target, client_name.as_deref()).await?;

    let stdout = Arc::new(Mutex::new(tokio::io::stdout()));
    let events_client = client.clone();
    let events_base = base_url.clone();
    let events_session = session_id.clone();
    let events_stdout = stdout.clone();
    let events_task = tokio::spawn(async move {
        loop {
            let response = events_client
                .get(format!("{events_base}/sessions/{events_session}/events"))
                .send()
                .await;
            let Ok(response) = response else {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                continue;
            };
            if !response.status().is_success() {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                continue;
            }
            let Ok(events) = response.json::<Vec<Value>>().await else {
                continue;
            };
            for event in events {
                if write_json_line(&events_stdout, &event).await.is_err() {
                    return;
                }
            }
        }
    });

    let run_result = async {
        let mut stdin = BufReader::new(tokio::io::stdin());
        let mut line = Vec::with_capacity(1024);
        while let Some(truncated) =
            read_capped_line(&mut stdin, &mut line, MAX_STDIN_LINE_BYTES).await?
        {
            if truncated {
                tracing::warn!("ignoring oversized JSON-RPC input line");
                continue;
            }
            if line.iter().all(|byte| byte.is_ascii_whitespace()) {
                continue;
            }
            let msg: Value = match serde_json::from_slice(&line) {
                Ok(msg) => msg,
                Err(err) => {
                    tracing::warn!(%err, "ignoring invalid JSON-RPC input");
                    continue;
                }
            };
            let response = post_session_message(&client, &base_url, &session_id, &msg).await;
            let response = match response {
                Ok(response) => response,
                Err(_) => {
                    session_id =
                        create_bridge_session(&client, &base_url, &target, client_name.as_deref())
                            .await?;
                    post_session_message(&client, &base_url, &session_id, &msg).await?
                }
            };
            if !response.is_null() {
                write_json_line(&stdout, &response).await?;
            }
        }
        Ok(())
    }
    .await;

    events_task.abort();
    let _ = client
        .delete(format!("{base_url}/sessions/{session_id}"))
        .send()
        .await;
    run_result
}

async fn create_bridge_session(
    client: &reqwest::Client,
    base_url: &str,
    target: &BridgeTarget,
    client_name: Option<&str>,
) -> anyhow::Result<String> {
    let mut session_payload = match target {
        BridgeTarget::Group(group) => serde_json::json!({ "group": group }),
        BridgeTarget::Server(server) => serde_json::json!({ "server": server }),
    };
    if let Some(name) = client_name {
        session_payload["client"] = Value::String(name.to_string());
    }
    let session: Value = client
        .post(format!("{base_url}/sessions"))
        .json(&session_payload)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    session
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("gateway did not return a session id"))
}

async fn post_session_message(
    client: &reqwest::Client,
    base_url: &str,
    session_id: &str,
    msg: &Value,
) -> anyhow::Result<Value> {
    let response = client
        .post(format!("{base_url}/sessions/{session_id}/mcp"))
        .json(&msg)
        .send()
        .await?;
    Ok(response.error_for_status()?.json::<Value>().await?)
}

fn read_base_url_from_state(path: &Path) -> anyhow::Result<String> {
    let mut last_error = None;
    for _ in 0..GATEWAY_READY_RETRIES {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let state: GatewayStateFile = serde_json::from_str(&text)
                    .with_context(|| format!("invalid gateway state file at {}", path.display()))?;
                return Ok(state.base_url.trim_end_matches('/').to_string());
            }
            Err(err) => {
                last_error = Some(err);
                std::thread::sleep(GATEWAY_READY_DELAY);
            }
        }
    }
    Err(last_error
        .map(anyhow::Error::from)
        .unwrap_or_else(|| anyhow!("gateway state file did not become available"))
        .context(format!(
            "failed to read gateway state file at {}",
            path.display()
        )))
}

fn default_state_file() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".mcp-gateway/run/state.json")
}

async fn write_json_line(
    stdout: &Arc<Mutex<tokio::io::Stdout>>,
    value: &Value,
) -> anyhow::Result<()> {
    let mut stdout = stdout.lock().await;
    stdout
        .write_all(serde_json::to_string(value)?.as_bytes())
        .await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}

async fn wait_for_gateway_health(client: &reqwest::Client, base_url: &str) -> anyhow::Result<()> {
    let health_url = format!("{}/health", base_url.trim_end_matches('/'));
    let mut last_error = None;
    for _ in 0..GATEWAY_READY_RETRIES {
        match client.get(&health_url).send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            Ok(response) => {
                last_error = Some(anyhow!("gateway health returned {}", response.status()));
            }
            Err(err) => {
                last_error = Some(anyhow!(err));
            }
        }
        tokio::time::sleep(GATEWAY_READY_DELAY).await;
    }
    Err(last_error.unwrap_or_else(|| anyhow!("gateway health did not become ready")))
}

async fn read_capped_line<R>(
    reader: &mut R,
    line: &mut Vec<u8>,
    max_bytes: usize,
) -> io::Result<Option<bool>>
where
    R: AsyncBufRead + Unpin,
{
    line.clear();
    let mut truncated = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            trim_line_end(line);
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(truncated))
            };
        }

        if let Some(pos) = available.iter().position(|byte| *byte == b'\n') {
            truncated |= append_capped(line, &available[..pos], max_bytes);
            reader.consume(pos + 1);
            trim_line_end(line);
            return Ok(Some(truncated));
        }

        let consumed = available.len();
        truncated |= append_capped(line, available, max_bytes);
        reader.consume(consumed);
    }
}

fn append_capped(line: &mut Vec<u8>, chunk: &[u8], max_bytes: usize) -> bool {
    let remaining = max_bytes.saturating_sub(line.len());
    let copied = remaining.min(chunk.len());
    if copied > 0 {
        line.extend_from_slice(&chunk[..copied]);
    }
    copied < chunk.len()
}

fn trim_line_end(line: &mut Vec<u8>) {
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
}
