use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use mcp_gateway::native::{read_state_file, NativeInstallPaths};
use serde::{Deserialize, Serialize};

use crate::cli::output::{print_backend_table, OutputFormat};

#[derive(Debug, Deserialize, Serialize)]
struct ServersResponse {
    servers: Vec<BackendRow>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct BackendRow {
    pub(crate) name: String,
    pub(crate) state: String,
    pub(crate) pid: Option<u32>,
    pub(crate) uptime_seconds: Option<u64>,
    #[serde(default)]
    pub(crate) rss_kb: Option<u64>,
    pub(crate) initialized: bool,
    #[serde(default)]
    pub(crate) last_used_seconds: Option<u64>,
    #[serde(default)]
    pub(crate) active_requests: Option<u64>,
    #[serde(default)]
    pub(crate) pending_requests: Option<u64>,
}

pub(crate) fn run_ps(
    format: OutputFormat,
    gateway: Option<String>,
    state_file: Option<PathBuf>,
) -> Result<()> {
    let gateway = gateway_url(gateway, state_file)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let response: ServersResponse = rt.block_on(async {
        reqwest::Client::new()
            .get(format!("{}/servers", gateway.trim_end_matches('/')))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
    })?;
    match format {
        OutputFormat::Table => {
            print_backend_table(&response.servers);
            for row in &response.servers {
                if let Some(pid) = row.pid {
                    if let Some(tree) = process_tree_text(pid) {
                        let warning = chrome_separation_warning(&row.name, &tree);
                        if !warning.is_empty() {
                            eprintln!("{warning}");
                        }
                    }
                }
            }
        }
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&response)?),
    }
    Ok(())
}

pub(crate) fn run_stop(
    server: String,
    gateway: Option<String>,
    state_file: Option<PathBuf>,
) -> Result<()> {
    let gateway = gateway_url(gateway, state_file)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let client = reqwest::Client::new();
    let stopped = rt.block_on(async {
        let targets = if server == "all" {
            let response: ServersResponse = client
                .get(format!("{}/servers", gateway.trim_end_matches('/')))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            response
                .servers
                .into_iter()
                .map(|row| row.name)
                .collect::<Vec<_>>()
        } else {
            vec![server]
        };

        let mut rows = Vec::new();
        for target in targets {
            let row: BackendRow = client
                .post(format!(
                    "{}/servers/{}/stop",
                    gateway.trim_end_matches('/'),
                    target
                ))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            rows.push(row);
        }
        anyhow::Ok(rows)
    })?;

    for row in stopped {
        println!("{} {}", row.name, row.state);
    }
    Ok(())
}

fn gateway_url(gateway: Option<String>, state_file: Option<PathBuf>) -> Result<String> {
    match gateway {
        Some(gateway) => Ok(gateway),
        None => {
            let home = dirs::home_dir().context("could not determine home directory")?;
            let state_file =
                state_file.unwrap_or_else(|| NativeInstallPaths::for_home(home).state_file);
            Ok(read_state_file(state_file)?.base_url)
        }
    }
}

pub(crate) fn chrome_separation_warning(server: &str, process_tree: &str) -> String {
    let uses_regular_chrome = process_tree.contains("/Applications/Google Chrome.app");
    let uses_chrome_for_testing = process_tree.contains("Google Chrome for Testing.app");
    if uses_regular_chrome && !uses_chrome_for_testing {
        format!("WARNING {server} is using regular Chrome; expected Chrome for Testing")
    } else {
        String::new()
    }
}

fn process_tree_text(root_pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(select_process_tree_lines(&text, root_pid))
}

fn select_process_tree_lines(ps_output: &str, root_pid: u32) -> String {
    let mut rows = BTreeMap::<u32, (u32, String)>::new();
    let mut children = BTreeMap::<u32, Vec<u32>>::new();
    for line in ps_output.lines() {
        let mut parts = line.split_whitespace();
        let Some(pid) = parts.next().and_then(|part| part.parse::<u32>().ok()) else {
            continue;
        };
        let Some(ppid) = parts.next().and_then(|part| part.parse::<u32>().ok()) else {
            continue;
        };
        rows.insert(pid, (ppid, line.to_string()));
        children.entry(ppid).or_default().push(pid);
    }

    let mut selected = Vec::new();
    let mut queue = VecDeque::from([root_pid]);
    while let Some(pid) = queue.pop_front() {
        if let Some((_, line)) = rows.get(&pid) {
            selected.push(line.clone());
        }
        if let Some(child_pids) = children.get(&pid) {
            queue.extend(child_pids);
        }
    }
    selected.join("\n")
}
