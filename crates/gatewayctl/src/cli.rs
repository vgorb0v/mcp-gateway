use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use mcp_gateway::client_config::{
    client_path, ApplyConfigOptions, ClientConfigReconciler, ClientDriftStatus, ClientKind,
    ReconcilerOptions,
};
use mcp_gateway::config::{validate_identifier, Config};
use mcp_gateway::native::{
    load_env_file, read_state_file, render_launch_agent_plist, write_owner_only_file,
    NativeInstallPaths, LAUNCH_AGENT_LABEL, LEGACY_LAUNCH_AGENT_LABEL,
};
use mcp_gateway::registry::BackendRegistry;

#[path = "cmd/mod.rs"]
mod cmd;
#[path = "output.rs"]
mod output;

use cmd::demo::{run_demo, run_demo_server, DemoOptions};
use cmd::status::{run_ps, run_stop};
use output::OutputFormat;

#[derive(Debug, Parser)]
#[command(
    name = "mcpgateway",
    version,
    about = "Host setup helper for MCP Gateway",
    long_about = "Install, verify, and operate the host-native MCP Gateway user service.",
    after_help = "Examples:\n  mcpgateway install\n  mcpgateway add @modelcontextprotocol/server-filesystem --name filesystem --arg=/tmp\n  mcpgateway refresh all\n  mcpgateway apply-configs --clients codex,claude-code\n\nDocs: https://github.com/vgorb0v/mcp-gateway"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Apply generated gateway shims to supported AI client configs.
    ApplyConfigs {
        #[arg(long, value_delimiter = ',')]
        clients: Vec<ClientKind>,
        #[arg(long, default_value = "coding")]
        group: String,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        bridge_bin: Option<PathBuf>,
        #[arg(long)]
        mcp_dir: Option<PathBuf>,
        #[arg(long)]
        state_file: Option<PathBuf>,
        #[arg(long, value_delimiter = ',')]
        dedupe: Vec<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        home: Option<PathBuf>,
    },
    /// Install or refresh native binaries, config, shims, and the LaunchAgent.
    #[command(name = "install", alias = "install-native")]
    InstallNative {
        #[arg(long, value_delimiter = ',')]
        clients: Vec<ClientKind>,
        #[arg(long, default_value = "coding")]
        group: String,
        #[arg(long, value_delimiter = ',')]
        provision: Vec<Provision>,
        #[arg(long)]
        skip_pnpm_install: bool,
        #[arg(long)]
        skip_launchctl: bool,
        #[arg(long)]
        add_to_path: bool,
        #[arg(long)]
        no_path_prompt: bool,
        #[arg(long)]
        homebrew: bool,
        #[arg(long)]
        home: Option<PathBuf>,
    },
    /// Install a pnpm package as a gateway-managed MCP server.
    #[command(name = "add", alias = "install-package")]
    Add {
        package: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        bin: Option<String>,
        #[arg(long, default_value = "coding")]
        group: String,
        #[arg(long = "arg")]
        args: Vec<String>,
        #[arg(long)]
        home: Option<PathBuf>,
    },
    /// Run a 90-second local proof path with the built-in demo MCP server.
    Demo {
        #[arg(long)]
        home: Option<PathBuf>,
        #[arg(long)]
        keep: bool,
    },
    #[command(name = "__demo-server", hide = true)]
    DemoServer,
    /// Print a table of backend health + RSS.
    #[command(alias = "status")]
    Ps {
        #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
        format: OutputFormat,
        #[arg(long, env = "MCP_GATEWAY_URL")]
        gateway: Option<String>,
        #[arg(long)]
        state_file: Option<PathBuf>,
    },
    /// Stop one backend, or every backend with `all`.
    Stop {
        server: String,
        #[arg(long, env = "MCP_GATEWAY_URL")]
        gateway: Option<String>,
        #[arg(long)]
        state_file: Option<PathBuf>,
    },
    /// Refresh cached MCP tool/prompt/resource metadata without leaving backends running.
    #[command(alias = "refresh")]
    RefreshCapabilities {
        #[arg(default_value = "all")]
        target: String,
        #[arg(long, default_value = "coding")]
        group: String,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        cache_file: Option<PathBuf>,
        #[arg(long)]
        home: Option<PathBuf>,
    },
    /// Inspect the native install, launchd state, config, cache, shims, and daemon health.
    Doctor {
        #[arg(long)]
        home: Option<PathBuf>,
        #[arg(long, env = "MCP_GATEWAY_URL")]
        gateway: Option<String>,
        #[arg(long)]
        state_file: Option<PathBuf>,
        #[arg(long)]
        skip_launchctl: bool,
        #[arg(long)]
        homebrew: bool,
    },
    /// Scan each detected client config for MCP entries that the gateway
    /// didn't write. Useful before flipping someone over to gateway-managed
    /// configs so manually-added MCPs aren't accidentally dropped.
    ImportClients {
        #[arg(
            long,
            value_delimiter = ',',
            default_value = "codex,claude-code,claude-desktop,antigravity,vscode"
        )]
        clients: Vec<ClientKind>,
        /// Write the discovered entries as `~/.mcp-gateway/config/servers.d/<name>.yaml`
        /// stubs so a future install can promote them. Without this
        /// flag the command just prints findings.
        #[arg(long)]
        write: bool,
        #[arg(long)]
        home: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Provision {
    ChromeForTesting,
}

pub fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    match cli.command {
        Commands::ApplyConfigs {
            clients,
            group,
            config,
            bridge_bin,
            mcp_dir,
            state_file,
            dedupe,
            dry_run,
            home,
        } => {
            let home = home
                .or_else(dirs::home_dir)
                .context("could not determine home directory")?;
            let paths = NativeInstallPaths::for_home(&home);
            let config_path = config.unwrap_or_else(|| paths.config_file.clone());
            let cfg = Config::load_file(&config_path)?;
            let homebrew_prefix = detected_homebrew_prefix(false)?;
            let group_config = cfg.groups.get(&group).with_context(|| {
                format!(
                    "config group '{group}' does not exist in {}",
                    config_path.display()
                )
            })?;
            let changed = mcp_gateway::client_config::apply_configs(ApplyConfigOptions {
                clients,
                servers: group_config.servers.clone(),
                bridge_bin: bridge_bin
                    .unwrap_or_else(|| default_bridge_bin(&paths, homebrew_prefix.as_deref())),
                mcp_dir: mcp_dir.unwrap_or(paths.mcp_dir),
                state_file: state_file.unwrap_or(paths.state_file),
                dedupe,
                dry_run,
                home,
            })?;
            if changed.is_empty() {
                println!("No files changed");
            } else {
                println!("Changed files:");
                for path in changed {
                    println!("{}", path.display());
                }
            }
        }
        Commands::InstallNative {
            clients,
            group,
            provision,
            skip_pnpm_install,
            skip_launchctl,
            add_to_path,
            no_path_prompt,
            homebrew,
            home,
        } => {
            let home = home
                .or_else(dirs::home_dir)
                .context("could not determine home directory")?;
            let changed = install_native(InstallNativeOptions {
                clients,
                group,
                provision,
                skip_pnpm_install,
                skip_launchctl,
                add_to_path,
                no_path_prompt,
                homebrew,
                home,
            })?;
            if changed.is_empty() {
                println!("Native install already up to date");
            } else {
                println!("Native install changed:");
                for path in changed {
                    println!("{}", path.display());
                }
            }
        }
        Commands::Add {
            package,
            name,
            bin,
            group,
            args,
            home,
        } => {
            let home = home
                .or_else(dirs::home_dir)
                .context("could not determine home directory")?;
            let installed = install_package(InstallPackageOptions {
                package,
                server_id: name,
                bin,
                group,
                args,
                home,
            })?;
            println!(
                "Installed {} as MCP server `{}`",
                installed.package_spec, installed.server_id
            );
            println!("Wrote {}", installed.overlay_path.display());
            println!(
                "Run `mcpgateway refresh-capabilities {}` when the server is configured and ready.",
                installed.server_id
            );
        }
        Commands::Demo { home, keep } => run_demo(DemoOptions { home, keep })?,
        Commands::DemoServer => run_demo_server()?,
        Commands::Ps {
            format,
            gateway,
            state_file,
        } => run_ps(format, gateway, state_file)?,
        Commands::Stop {
            server,
            gateway,
            state_file,
        } => run_stop(server, gateway, state_file)?,
        Commands::RefreshCapabilities {
            target,
            group,
            config,
            cache_file,
            home,
        } => run_refresh_capabilities(target, group, config, cache_file, home)?,
        Commands::Doctor {
            home,
            gateway,
            state_file,
            skip_launchctl,
            homebrew,
        } => run_doctor(DoctorOptions {
            home,
            gateway,
            state_file,
            skip_launchctl,
            homebrew,
        })?,
        Commands::ImportClients {
            clients,
            write,
            home,
        } => run_import_clients(clients, write, home)?,
    }
    Ok(())
}

struct DoctorOptions {
    home: Option<PathBuf>,
    gateway: Option<String>,
    state_file: Option<PathBuf>,
    skip_launchctl: bool,
    homebrew: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoctorStatus {
    Ok,
    Warn,
    Fail,
    Info,
}

struct DoctorCheck {
    status: DoctorStatus,
    critical: bool,
    message: String,
}

impl DoctorCheck {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            status: DoctorStatus::Ok,
            critical: false,
            message: message.into(),
        }
    }

    fn info(message: impl Into<String>) -> Self {
        Self {
            status: DoctorStatus::Info,
            critical: false,
            message: message.into(),
        }
    }

    fn warn(message: impl Into<String>) -> Self {
        Self {
            status: DoctorStatus::Warn,
            critical: false,
            message: message.into(),
        }
    }

    fn fail(message: impl Into<String>) -> Self {
        Self {
            status: DoctorStatus::Fail,
            critical: true,
            message: message.into(),
        }
    }
}

fn run_doctor(options: DoctorOptions) -> Result<()> {
    let home = options
        .home
        .or_else(dirs::home_dir)
        .context("could not determine home directory")?;
    let paths = NativeInstallPaths::for_home(&home);
    let homebrew_prefix = detected_homebrew_prefix(options.homebrew)?;
    let mut checks = Vec::new();

    checks.push(if cfg!(target_os = "macos") {
        DoctorCheck::ok("OS is macOS")
    } else {
        DoctorCheck::warn("OS is not macOS; host-native install is macOS-focused")
    });

    for binary in ["mcp-gateway", "mcp-gateway-bridge", "mcpgateway"] {
        let path = doctor_binary_path(binary, &paths, homebrew_prefix.as_deref());
        if !path.exists() {
            checks.push(DoctorCheck::fail(format!(
                "{} binary missing: run `mcpgateway install` or `brew install mcp-gateway`",
                binary_label(binary)
            )));
            continue;
        }
        checks.push(DoctorCheck::ok(format!(
            "{} binary exists: {}",
            binary_label(binary),
            path.display()
        )));
        match Command::new(&path).arg("--version").output() {
            Ok(output) if output.status.success() => {
                let version = String::from_utf8_lossy(&output.stdout);
                let version = version.lines().next().unwrap_or(binary);
                checks.push(DoctorCheck::info(format!("{binary} version: {version}")));
            }
            Ok(output) => checks.push(DoctorCheck::warn(format!(
                "{binary} --version exited with {}",
                output.status
            ))),
            Err(err) => checks.push(DoctorCheck::warn(format!(
                "{binary} version unavailable: {err}"
            ))),
        }
    }

    if homebrew_prefix.is_some() {
        checks.push(DoctorCheck::info(
            "service managed by Homebrew: use `brew services start mcp-gateway`",
        ));
        if paths.launch_agent_file.exists() {
            checks.push(DoctorCheck::warn(format!(
                "manual launchd plist still exists: {}",
                paths.launch_agent_file.display()
            )));
        }
    } else if !paths.launch_agent_file.exists() {
        checks.push(DoctorCheck::fail(format!(
            "launchd plist missing: {}",
            paths.launch_agent_file.display()
        )));
    } else {
        match fs::read_to_string(&paths.launch_agent_file) {
            Ok(text) if text.contains(LAUNCH_AGENT_LABEL) => checks.push(DoctorCheck::ok(format!(
                "launchd plist label matches: {LAUNCH_AGENT_LABEL}"
            ))),
            Ok(_) => checks.push(DoctorCheck::fail(format!(
                "launchd plist label does not match {LAUNCH_AGENT_LABEL}"
            ))),
            Err(err) => checks.push(DoctorCheck::fail(format!(
                "launchd plist unreadable: {err}"
            ))),
        }
    }
    if paths.legacy_launch_agent_file.exists() {
        checks.push(DoctorCheck::warn(format!(
            "legacy launchd plist still exists: {}",
            paths.legacy_launch_agent_file.display()
        )));
    }
    if homebrew_prefix.is_none() && !options.skip_launchctl && paths.launch_agent_file.exists() {
        checks.push(match launchctl_print_label(LAUNCH_AGENT_LABEL) {
            Ok(()) => DoctorCheck::ok(format!("launchd loaded: {LAUNCH_AGENT_LABEL}")),
            Err(err) => DoctorCheck::warn(format!("launchd not loaded or not printable: {err}")),
        });
    }

    let state_file = options.state_file.unwrap_or(paths.state_file.clone());
    let gateway = match options.gateway {
        Some(gateway) => {
            checks.push(DoctorCheck::info(format!(
                "using gateway URL from CLI: {gateway}"
            )));
            Some(gateway)
        }
        None => match read_state_file(&state_file) {
            Ok(state) => {
                checks.push(DoctorCheck::ok(format!(
                    "state file readable: {}",
                    state_file.display()
                )));
                Some(state.base_url)
            }
            Err(err) if err.to_string().contains("No such file") => {
                checks.push(DoctorCheck::warn(format!(
                    "state file missing: {}",
                    state_file.display()
                )));
                None
            }
            Err(err) => {
                checks.push(DoctorCheck::fail(format!("state file unreadable: {err}")));
                None
            }
        },
    };
    if state_file.exists() {
        checks.push(check_owner_only(&state_file, "state file"));
    }
    if paths.env_file.exists() {
        checks.push(check_owner_only(&paths.env_file, "env file"));
    }

    if let Some(gateway) = gateway {
        checks.push(check_daemon_health(&gateway));
    } else {
        checks.push(DoctorCheck::warn(
            "daemon health skipped: no gateway URL or state file",
        ));
    }

    let cfg = match Config::load_file(&paths.config_file) {
        Ok(cfg) => {
            checks.push(DoctorCheck::ok(format!(
                "config parses: {}",
                paths.config_file.display()
            )));
            Some(cfg)
        }
        Err(err) => {
            checks.push(DoctorCheck::fail(format!(
                "config does not parse: {} ({err})",
                paths.config_file.display()
            )));
            None
        }
    };

    if paths.capability_cache_file.exists() {
        checks.push(DoctorCheck::ok(format!(
            "capability cache exists: {}",
            paths.capability_cache_file.display()
        )));
    } else {
        checks.push(DoctorCheck::warn(
            "capability cache missing: run `mcpgateway refresh-capabilities all`",
        ));
    }
    if paths.mcp_dir.exists() {
        checks.push(DoctorCheck::ok(format!(
            "MCP shim directory exists: {}",
            paths.mcp_dir.display()
        )));
    } else {
        checks.push(DoctorCheck::warn(format!(
            "MCP shim directory missing: {}",
            paths.mcp_dir.display()
        )));
    }

    if let Some(cfg) = &cfg {
        checks.extend(check_managed_clients(
            cfg,
            &paths,
            &home,
            homebrew_prefix.as_deref(),
        ));
    }
    checks.push(check_command_available("pnpm"));
    checks.push(check_command_available("npx"));

    println!("MCP Gateway doctor\n");
    for check in &checks {
        println!("{} {}", doctor_prefix(check.status), check.message);
    }

    if checks.iter().any(|check| check.critical) {
        anyhow::bail!("doctor found critical failures");
    }
    Ok(())
}

fn binary_label(binary: &str) -> &'static str {
    match binary {
        "mcp-gateway-bridge" => "bridge",
        "mcpgateway" => "CLI",
        _ => "daemon",
    }
}

fn doctor_binary_path(
    binary: &str,
    paths: &NativeInstallPaths,
    homebrew_prefix: Option<&Path>,
) -> PathBuf {
    let local = paths.bin_dir.join(binary);
    if local.exists() {
        return local;
    }
    homebrew_prefix
        .map(|prefix| prefix.join("bin").join(binary))
        .unwrap_or(local)
}

fn doctor_prefix(status: DoctorStatus) -> &'static str {
    match status {
        DoctorStatus::Ok => "[ok]",
        DoctorStatus::Warn => "[warn]",
        DoctorStatus::Fail => "[fail]",
        DoctorStatus::Info => "[info]",
    }
}

fn check_daemon_health(gateway: &str) -> DoctorCheck {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => return DoctorCheck::fail(format!("daemon health runtime failed: {err}")),
    };
    let url = format!("{}/health", gateway.trim_end_matches('/'));
    let result = rt.block_on(async {
        reqwest::Client::new()
            .get(&url)
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await?
            .error_for_status()
    });
    match result {
        Ok(_) => DoctorCheck::ok(format!("daemon healthy: {gateway}")),
        Err(err) => DoctorCheck::fail(format!("daemon health failed at {url}: {err}")),
    }
}

fn check_managed_clients(
    cfg: &Config,
    paths: &NativeInstallPaths,
    home: &Path,
    homebrew_prefix: Option<&Path>,
) -> Vec<DoctorCheck> {
    let clients = cfg
        .clients
        .managed_clients
        .iter()
        .filter_map(|client| ClientKind::from_config_name(client))
        .collect::<Vec<_>>();
    if clients.is_empty() {
        return vec![DoctorCheck::info("managed clients: none configured")];
    }
    let Some(group) = cfg.groups.get(&cfg.clients.default_group) else {
        return vec![DoctorCheck::fail(format!(
            "clients.default_group '{}' does not exist",
            cfg.clients.default_group
        ))];
    };
    let reconciler = ClientConfigReconciler::new(ReconcilerOptions {
        clients,
        servers: group.servers.clone(),
        bridge_bin: default_bridge_bin(paths, homebrew_prefix),
        mcp_dir: paths.mcp_dir.clone(),
        state_file: paths.state_file.clone(),
        dedupe: vec!["mcp-gateway".to_string()],
        home: home.to_path_buf(),
        manifest_path: paths.root.join("state/client-manifest.json"),
        backups_dir: paths.backups_dir.clone(),
    });
    match reconciler.status() {
        Ok(statuses) if statuses.is_empty() => {
            vec![DoctorCheck::info("managed clients: none configured")]
        }
        Ok(statuses) => statuses
            .into_iter()
            .map(|status| match status.drift {
                ClientDriftStatus::Unmanaged => DoctorCheck::warn(format!(
                    "client {} not yet managed: {}",
                    status.client,
                    status.path.display()
                )),
                ClientDriftStatus::InSync => {
                    DoctorCheck::ok(format!("client {} in sync", status.client))
                }
                ClientDriftStatus::Drifted => DoctorCheck::warn(format!(
                    "client {} drifted: review before applying configs",
                    status.client
                )),
            })
            .collect(),
        Err(err) => vec![DoctorCheck::warn(format!(
            "managed client status unavailable: {err}"
        ))],
    }
}

fn check_command_available(command: &str) -> DoctorCheck {
    match Command::new(command).arg("--version").output() {
        Ok(output) if output.status.success() => DoctorCheck::info(format!("{command} available")),
        Ok(output) => DoctorCheck::info(format!(
            "{command} present but --version exited with {}",
            output.status
        )),
        Err(_) => DoctorCheck::info(format!(
            "{command} not found; only needed for Node-backed MCP installs"
        )),
    }
}

fn launchctl_print_label(label: &str) -> Result<()> {
    let service = format!("gui/{}/{}", current_uid(), label);
    let status = Command::new("launchctl")
        .args(["print", &service])
        .status()
        .context("failed to run launchctl print")?;
    if !status.success() {
        anyhow::bail!("launchctl print exited with {status}");
    }
    Ok(())
}

#[cfg(unix)]
fn check_owner_only(path: &Path, label: &str) -> DoctorCheck {
    use std::os::unix::fs::PermissionsExt;
    match fs::metadata(path) {
        Ok(metadata) => {
            let mode = metadata.permissions().mode() & 0o777;
            if mode == 0o600 {
                DoctorCheck::ok(format!("{label} owner-only: {}", path.display()))
            } else {
                DoctorCheck::fail(format!(
                    "{label} permissions are {mode:o}, expected 600: {}",
                    path.display()
                ))
            }
        }
        Err(err) => DoctorCheck::fail(format!("{label} metadata unavailable: {err}")),
    }
}

#[cfg(not(unix))]
fn check_owner_only(path: &Path, label: &str) -> DoctorCheck {
    DoctorCheck::info(format!(
        "{label} permission check skipped on this OS: {}",
        path.display()
    ))
}

fn run_import_clients(clients: Vec<ClientKind>, write: bool, home: Option<PathBuf>) -> Result<()> {
    let home = home
        .or_else(dirs::home_dir)
        .context("could not determine home directory")?;
    let paths = NativeInstallPaths::for_home(&home);
    let mcp_dir_str = paths.mcp_dir.to_string_lossy().to_string();
    let mut total = 0usize;
    let mut stubs_written: Vec<PathBuf> = Vec::new();

    for client in &clients {
        let path = client_path(*client, &home);
        let Some(text) = read_optional_text(&path)? else {
            continue;
        };
        let entries = match client {
            ClientKind::Codex => extract_codex_entries(&text)?,
            _ => extract_json_entries(&text, json_servers_key(*client))?,
        };
        let foreign: Vec<_> = entries
            .into_iter()
            .filter(|(_, value)| !points_at_gateway(value, &mcp_dir_str))
            .collect();
        if foreign.is_empty() {
            continue;
        }
        total += foreign.len();
        println!(
            "{} ({}): {} unmanaged entr{} found",
            client.config_name(),
            path.display(),
            foreign.len(),
            if foreign.len() == 1 { "y" } else { "ies" }
        );
        for (name, value) in &foreign {
            let command = value
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("(missing command)");
            println!("  {name:<20} {command}");
        }
        if write {
            stubs_written.extend(write_servers_d_stubs(
                &paths.config_servers_d_dir,
                client.config_name(),
                &foreign,
            )?);
        }
    }

    if total == 0 {
        println!("No unmanaged MCP entries found across the inspected client configs.");
    }
    if write && !stubs_written.is_empty() {
        println!("\nWrote {} servers.d stub(s):", stubs_written.len());
        for path in &stubs_written {
            println!("  {}", path.display());
        }
    } else if write {
        println!("\nNo new servers.d stubs needed.");
    }
    Ok(())
}

fn json_servers_key(client: ClientKind) -> &'static str {
    match client {
        ClientKind::Vscode => "servers",
        _ => "mcpServers",
    }
}

fn read_optional_text(path: &std::path::Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn extract_json_entries(text: &str, key: &str) -> Result<Vec<(String, serde_json::Value)>> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let root: serde_json::Value = serde_json::from_str(text)?;
    let Some(servers) = root.get(key).and_then(|v| v.as_object()) else {
        return Ok(Vec::new());
    };
    Ok(servers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect())
}

fn extract_codex_entries(text: &str) -> Result<Vec<(String, serde_json::Value)>> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let doc: toml_edit::DocumentMut = text.parse()?;
    let Some(table) = doc.get("mcp_servers").and_then(|item| item.as_table()) else {
        return Ok(Vec::new());
    };
    Ok(table
        .iter()
        .map(|(name, item)| (name.to_string(), toml_to_json(item)))
        .collect())
}

fn toml_to_json(item: &toml_edit::Item) -> serde_json::Value {
    match item {
        toml_edit::Item::Value(v) => match v {
            toml_edit::Value::String(s) => serde_json::Value::String(s.value().clone()),
            toml_edit::Value::Integer(i) => serde_json::Value::Number((*i.value()).into()),
            toml_edit::Value::Boolean(b) => serde_json::Value::Bool(*b.value()),
            toml_edit::Value::Array(arr) => serde_json::Value::Array(
                arr.iter()
                    .map(|el| toml_to_json(&toml_edit::Item::Value(el.clone())))
                    .collect(),
            ),
            other => serde_json::Value::String(other.to_string()),
        },
        toml_edit::Item::Table(table) => {
            let mut map = serde_json::Map::new();
            for (k, v) in table.iter() {
                map.insert(k.to_string(), toml_to_json(v));
            }
            serde_json::Value::Object(map)
        }
        _ => serde_json::Value::Null,
    }
}

fn points_at_gateway(value: &serde_json::Value, mcp_dir: &str) -> bool {
    let Some(command) = value.get("command").and_then(|v| v.as_str()) else {
        return false;
    };
    command.starts_with(mcp_dir) || command.contains("mcp-gateway-bridge")
}

fn write_servers_d_stubs(
    dir: &std::path::Path,
    client_id: &str,
    foreign: &[(String, serde_json::Value)],
) -> Result<Vec<PathBuf>> {
    for (name, _) in foreign {
        validate_identifier("server", name)?;
    }
    fs::create_dir_all(dir)?;
    let mut written = Vec::new();
    for (name, value) in foreign {
        let stub_path = dir.join(format!("{name}.yaml"));
        if stub_path.exists() {
            // Don't clobber an existing managed server stub. The user can
            // delete it manually if they want a fresh import.
            continue;
        }
        let command = value
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("# TODO: command not detected");
        let args: Vec<String> = value
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let mut content = String::new();
        content.push_str(&format!(
            "# Imported from {client_id} on {}\n",
            chrono_like_now()
        ));
        content.push_str(&format!("id: {name}\n"));
        content.push_str(&format!("display_name: \"{name}\"\n"));
        content.push_str("enabled: false\n");
        content.push_str("runtime:\n");
        content.push_str("  transport: stdio\n");
        content.push_str(&format!("  command: {command:?}\n"));
        content.push_str("  args:\n");
        for arg in &args {
            content.push_str(&format!("    - {arg:?}\n"));
        }
        content.push_str("  lazy: true\n");
        fs::write(&stub_path, content)?;
        written.push(stub_path);
    }
    Ok(written)
}

fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    format!("epoch={secs}")
}

pub(crate) struct InstallNativeOptions {
    pub(crate) clients: Vec<ClientKind>,
    pub(crate) group: String,
    pub(crate) provision: Vec<Provision>,
    pub(crate) skip_pnpm_install: bool,
    pub(crate) skip_launchctl: bool,
    pub(crate) add_to_path: bool,
    pub(crate) no_path_prompt: bool,
    pub(crate) homebrew: bool,
    pub(crate) home: PathBuf,
}

pub(crate) fn install_native(options: InstallNativeOptions) -> Result<Vec<PathBuf>> {
    let paths = NativeInstallPaths::for_home(&options.home);
    let homebrew_prefix = detected_homebrew_prefix(options.homebrew)?;
    let is_homebrew = homebrew_prefix.is_some();
    let first_install = !is_homebrew && !paths.bin_dir.join("mcpgateway").exists();
    create_native_dirs(&paths)?;

    let mut changed = Vec::new();
    if migrate_legacy_launch_agent(&paths, options.skip_launchctl)? {
        changed.push(paths.legacy_launch_agent_file.clone());
    }
    if is_homebrew {
        if remove_manual_launch_agent(&paths, options.skip_launchctl)? {
            changed.push(paths.launch_agent_file.clone());
        }
    } else {
        changed.extend(copy_binaries(&paths)?);
    }
    if remove_legacy_ctl_binary(&paths)? {
        changed.push(paths.bin_dir.join("mcp-gatewayctl"));
    }
    changed.extend(ensure_backend_project(&paths.backends_dir)?);

    if !options.skip_pnpm_install
        && backend_manifest_requires_pnpm_install(&paths.backends_dir.join("package.json"))?
    {
        let frozen = paths.backends_dir.join("pnpm-lock.yaml").exists();
        run_pnpm_install(&paths, frozen)?;
    }
    let chrome_for_testing_executable = if options.provision.contains(&Provision::ChromeForTesting)
    {
        run_chrome_for_testing_install(&paths)?;
        Some(
            find_chrome_for_testing_executable(&paths.chrome_for_testing_dir).with_context(
                || {
                    format!(
                        "Chrome for Testing executable was not found under {} after install",
                        paths.chrome_for_testing_dir.display()
                    )
                },
            )?,
        )
    } else {
        None
    };

    let existing_env = fs::read_to_string(&paths.env_file).ok();
    let rendered_env = match chrome_for_testing_executable.as_deref() {
        Some(path) => render_native_env_with_provisioned_chrome(existing_env.as_deref(), path),
        None => render_native_env(existing_env.as_deref()),
    };
    if write_owner_only_file(&paths.env_file, &rendered_env)? {
        changed.push(paths.env_file.clone());
    }
    if !paths.config_file.exists()
        && write_if_changed(&paths.config_file, &render_native_config()?)?
    {
        changed.push(paths.config_file.clone());
    }
    if !is_homebrew
        && write_if_changed(&paths.launch_agent_file, &render_launch_agent_plist(&paths))?
    {
        changed.push(paths.launch_agent_file.clone());
    }
    if !is_homebrew
        && (options.add_to_path
            || (first_install && !options.no_path_prompt && prompt_add_to_path()?))
    {
        let profile = shell_profile_path(&options.home);
        if ensure_path_profile_entry(&profile)? {
            changed.push(profile);
        }
    }

    load_env_file(&paths.env_file)?;
    prepend_backend_path(&paths);
    let cfg = Config::load_file(&paths.config_file)?;
    let group = cfg.groups.get(&options.group).with_context(|| {
        format!(
            "config group '{}' does not exist in {}",
            options.group,
            paths.config_file.display()
        )
    })?;
    let clients = install_clients_from_cli_or_config(options.clients, &cfg);
    if !is_homebrew {
        refresh_capabilities_in_process(&cfg, &paths, &options.group, "all")?;
    }
    if !is_homebrew && paths.capability_cache_file.exists() {
        changed.push(paths.capability_cache_file.clone());
    }
    changed.extend(mcp_gateway::client_config::apply_configs(
        ApplyConfigOptions {
            clients,
            servers: group.servers.clone(),
            bridge_bin: default_bridge_bin(&paths, homebrew_prefix.as_deref()),
            mcp_dir: paths.mcp_dir.clone(),
            state_file: paths.state_file.clone(),
            dedupe: vec!["mcp-gateway".to_string()],
            dry_run: false,
            home: options.home.clone(),
        },
    )?);

    if !is_homebrew && !options.skip_launchctl {
        load_launch_agent(&paths)?;
    }

    Ok(changed)
}

fn install_clients_from_cli_or_config(
    cli_clients: Vec<ClientKind>,
    cfg: &Config,
) -> Vec<ClientKind> {
    if !cli_clients.is_empty() {
        return cli_clients;
    }
    cfg.clients
        .managed_clients
        .iter()
        .filter_map(|client| ClientKind::from_config_name(client))
        .collect()
}

struct InstallPackageOptions {
    package: String,
    server_id: Option<String>,
    bin: Option<String>,
    group: String,
    args: Vec<String>,
    home: PathBuf,
}

struct InstalledPackage {
    package_spec: String,
    server_id: String,
    overlay_path: PathBuf,
}

fn install_package(options: InstallPackageOptions) -> Result<InstalledPackage> {
    let paths = NativeInstallPaths::for_home(&options.home);
    create_native_dirs(&paths)?;
    ensure_backend_project(&paths.backends_dir)?;

    let parsed = parse_package_spec(&options.package)?;
    let server_id = options
        .server_id
        .unwrap_or_else(|| parsed.default_server_id.clone());
    validate_identifier("server", &server_id)?;
    validate_identifier("group", &options.group)?;
    run_pnpm_add(&paths, &options.package)?;
    let package_json_path = installed_package_json_path(&paths.backends_dir, &parsed.package_name);
    let package_json = fs::read_to_string(&package_json_path)
        .with_context(|| format!("failed to read {}", package_json_path.display()))?;
    let package_manifest: serde_json::Value = serde_json::from_str(&package_json)
        .with_context(|| format!("invalid package manifest {}", package_json_path.display()))?;
    let bin = infer_package_bin(&package_manifest, options.bin.as_deref())?;
    let overlay = render_installed_server_overlay(InstalledServerOverlay {
        server_id: &server_id,
        package_spec: &options.package,
        bin: &bin,
        args: &options.args,
        group: &options.group,
    });
    fs::create_dir_all(&paths.config_servers_d_dir)?;
    let overlay_path = paths.config_servers_d_dir.join(format!("{server_id}.yaml"));
    fs::write(&overlay_path, overlay)?;
    Ok(InstalledPackage {
        package_spec: options.package,
        server_id,
        overlay_path,
    })
}

fn create_native_dirs(paths: &NativeInstallPaths) -> Result<()> {
    for dir in [
        &paths.bin_dir,
        &paths.backends_dir,
        &paths.mcp_dir,
        &paths.config_dir,
        &paths.config_servers_d_dir,
        &paths.cache_dir,
        &paths.run_dir,
        &paths.logs_dir,
        &paths.backups_dir,
        &paths.browsers_dir,
        &paths.chrome_for_testing_dir,
    ] {
        fs::create_dir_all(dir)?;
    }
    if let Some(parent) = paths.launch_agent_file.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

pub(crate) fn run_refresh_capabilities(
    target: String,
    group: String,
    config: Option<PathBuf>,
    cache_file: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Result<()> {
    let home = home
        .or_else(dirs::home_dir)
        .context("could not determine home directory")?;
    let paths = NativeInstallPaths::for_home(&home);
    let config_path = config.unwrap_or_else(|| paths.config_file.clone());
    let cache_file = cache_file.unwrap_or_else(|| paths.capability_cache_file.clone());
    load_env_file(&paths.env_file)?;
    prepend_backend_path(&paths);
    let cfg = Config::load_file(&config_path)?;
    let mut refresh_paths = paths.clone();
    refresh_paths.capability_cache_file = cache_file;
    refresh_capabilities_in_process(&cfg, &refresh_paths, &group, &target)?;
    sync_managed_client_configs(&cfg, &paths, &home, &group)?;
    println!("Refreshed capabilities for {target}");
    Ok(())
}

fn refresh_capabilities_in_process(
    cfg: &Config,
    paths: &NativeInstallPaths,
    group: &str,
    target: &str,
) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let cfg = cfg.clone();
    let cache_file = paths.capability_cache_file.clone();
    let group = group.to_string();
    let target = target.to_string();
    rt.block_on(async {
        let registry = BackendRegistry::new_with_capability_cache(cfg, cache_file)?;
        if target == "all" {
            registry.refresh_group_capabilities(&group).await?;
        } else {
            registry.refresh_server_capabilities(&target).await?;
        }
        registry.shutdown_all().await;
        anyhow::Ok(())
    })
}

fn sync_managed_client_configs(
    cfg: &Config,
    paths: &NativeInstallPaths,
    home: &Path,
    group: &str,
) -> Result<Vec<PathBuf>> {
    let group_config = cfg
        .groups
        .get(group)
        .with_context(|| format!("config group '{group}' does not exist"))?;
    let clients = cfg
        .clients
        .managed_clients
        .iter()
        .filter_map(|client| ClientKind::from_config_name(client))
        .collect::<Vec<_>>();
    Ok(mcp_gateway::client_config::apply_configs(
        ApplyConfigOptions {
            clients,
            servers: group_config.servers.clone(),
            bridge_bin: default_bridge_bin(paths, detected_homebrew_prefix(false)?.as_deref()),
            mcp_dir: paths.mcp_dir.clone(),
            state_file: paths.state_file.clone(),
            dedupe: vec!["mcp-gateway".to_string()],
            dry_run: false,
            home: home.to_path_buf(),
        },
    )?)
}

fn prepend_backend_path(paths: &NativeInstallPaths) {
    let backend_bin = paths.backends_dir.join("node_modules/.bin");
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![backend_bin];
    paths.extend(std::env::split_paths(&existing));
    if let Ok(joined) = std::env::join_paths(paths) {
        std::env::set_var("PATH", joined);
    }
}

fn detected_homebrew_prefix(force_homebrew: bool) -> Result<Option<PathBuf>> {
    let current_exe = std::env::current_exe()?;
    if let Some(prefix) = homebrew_prefix_from_executable(&current_exe) {
        return Ok(Some(prefix));
    }
    if !force_homebrew {
        return Ok(None);
    }
    Ok(std::env::var_os("HOMEBREW_PREFIX")
        .map(PathBuf::from)
        .or_else(|| Some(default_homebrew_prefix())))
}

fn default_homebrew_prefix() -> PathBuf {
    for prefix in [PathBuf::from("/opt/homebrew"), PathBuf::from("/usr/local")] {
        if prefix.exists() {
            return prefix;
        }
    }
    PathBuf::from("/opt/homebrew")
}

fn homebrew_prefix_from_executable(executable: &Path) -> Option<PathBuf> {
    for prefix in homebrew_prefix_candidates() {
        if executable == prefix.join("bin/mcpgateway")
            || executable.starts_with(prefix.join("Cellar/mcp-gateway"))
            || executable.starts_with(prefix.join("opt/mcp-gateway"))
        {
            return Some(prefix);
        }
    }

    let components = executable.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Some(next) = components.get(index + 1) else {
            continue;
        };
        if !matches!(component.as_os_str().to_str(), Some("Cellar" | "opt"))
            || next.as_os_str() != "mcp-gateway"
        {
            continue;
        }
        let mut prefix = PathBuf::new();
        for prefix_component in &components[..index] {
            prefix.push(prefix_component.as_os_str());
        }
        return Some(prefix);
    }
    None
}

fn homebrew_prefix_candidates() -> Vec<PathBuf> {
    let mut prefixes = Vec::new();
    if let Some(prefix) = std::env::var_os("HOMEBREW_PREFIX").map(PathBuf::from) {
        prefixes.push(prefix);
    }
    prefixes.push(PathBuf::from("/opt/homebrew"));
    prefixes.push(PathBuf::from("/usr/local"));
    prefixes.sort();
    prefixes.dedup();
    prefixes
}

fn default_bridge_bin(paths: &NativeInstallPaths, homebrew_prefix: Option<&Path>) -> PathBuf {
    homebrew_prefix
        .map(|prefix| prefix.join("bin/mcp-gateway-bridge"))
        .unwrap_or_else(|| paths.bin_dir.join("mcp-gateway-bridge"))
}

fn copy_binaries(paths: &NativeInstallPaths) -> Result<Vec<PathBuf>> {
    let current_exe = std::env::current_exe()?;
    let source_dir = current_exe
        .parent()
        .context("current executable has no parent directory")?;
    let mut changed = Vec::new();
    for binary in ["mcp-gateway", "mcp-gateway-bridge", "mcpgateway"] {
        let source = if binary == "mcpgateway" {
            current_exe.clone()
        } else {
            source_dir.join(binary)
        };
        if !source.exists() {
            anyhow::bail!("missing binary '{}' at {}", binary, source.display());
        }
        let destination = paths.bin_dir.join(binary);
        if copy_executable_atomically(&source, &destination)? {
            make_executable(&destination)?;
            changed.push(destination);
        }
    }
    Ok(changed)
}

fn remove_legacy_ctl_binary(paths: &NativeInstallPaths) -> Result<bool> {
    let legacy = paths.bin_dir.join("mcp-gatewayctl");
    if !legacy.exists() {
        return Ok(false);
    }
    fs::remove_file(&legacy)?;
    Ok(true)
}

fn remove_manual_launch_agent(paths: &NativeInstallPaths, skip_launchctl: bool) -> Result<bool> {
    if !paths.launch_agent_file.exists() {
        return Ok(false);
    }
    if !skip_launchctl {
        unload_launch_agent_label(LAUNCH_AGENT_LABEL, Some(&paths.launch_agent_file));
    }
    fs::remove_file(&paths.launch_agent_file)?;
    Ok(true)
}

// Replacing Mach-O binaries atomically avoids invalidating running shim paths on macOS.
fn copy_executable_atomically(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> Result<bool> {
    let source_bytes = fs::read(source)?;
    let changed = fs::read(destination).ok().as_deref() != Some(source_bytes.as_slice());
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = destination.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));
    fs::write(&tmp, source_bytes)?;
    make_executable(&tmp)?;
    fs::rename(&tmp, destination)?;
    Ok(changed)
}

fn ensure_backend_project(backends_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut changed = Vec::new();
    fs::create_dir_all(backends_dir)?;
    let package_json = backends_dir.join("package.json");
    if !package_json.exists() {
        let content = format!(
            "{}\n",
            serde_json::to_string_pretty(&serde_json::json!({
                "name": "mcpgateway-backends",
                "private": true
            }))?
        );
        fs::write(&package_json, content)?;
        changed.push(package_json);
    }
    Ok(changed)
}

fn run_pnpm_install(paths: &NativeInstallPaths, frozen: bool) -> Result<()> {
    let mut command = Command::new("pnpm");
    command.args(["install", "--prod"]);
    if frozen {
        command.arg("--frozen-lockfile");
    }
    let status = command
        .current_dir(&paths.backends_dir)
        .status()
        .context("failed to run pnpm install")?;
    if !status.success() {
        anyhow::bail!("pnpm install failed with {status}");
    }
    Ok(())
}

fn run_pnpm_add(paths: &NativeInstallPaths, package: &str) -> Result<()> {
    let status = Command::new("pnpm")
        .args(["add", "--save-prod", package])
        .current_dir(&paths.backends_dir)
        .status()
        .with_context(|| format!("failed to run pnpm add {package}"))?;
    if !status.success() {
        anyhow::bail!("pnpm add {package} failed with {status}");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedPackageSpec {
    package_name: String,
    default_server_id: String,
}

fn parse_package_spec(spec: &str) -> Result<ParsedPackageSpec> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        anyhow::bail!("package spec must not be empty");
    }
    let package_name = package_name_from_spec(trimmed)?;
    let default_server_id = default_server_id_from_package_name(&package_name)?;
    Ok(ParsedPackageSpec {
        package_name,
        default_server_id,
    })
}

fn package_name_from_spec(spec: &str) -> Result<String> {
    if spec.starts_with('@') {
        let mut parts = spec.splitn(3, '/');
        let Some(scope) = parts.next() else {
            anyhow::bail!("invalid package spec '{spec}'");
        };
        let Some(rest) = parts.next() else {
            anyhow::bail!("scoped package spec '{spec}' must include a package name");
        };
        let package = rest.rsplit_once('@').map(|(name, _)| name).unwrap_or(rest);
        if package.is_empty() {
            anyhow::bail!("scoped package spec '{spec}' must include a package name");
        }
        Ok(format!("{scope}/{package}"))
    } else {
        let package = spec.rsplit_once('@').map(|(name, _)| name).unwrap_or(spec);
        if package.is_empty() {
            anyhow::bail!("package spec '{spec}' must include a package name");
        }
        Ok(package.to_string())
    }
}

fn default_server_id_from_package_name(package_name: &str) -> Result<String> {
    let base = package_name
        .rsplit('/')
        .next()
        .filter(|part| !part.is_empty())
        .context("package name must not be empty")?;
    let id = sanitize_server_id(base);
    if id.is_empty() {
        anyhow::bail!("could not derive a server id from package '{package_name}'");
    }
    Ok(id)
}

fn sanitize_server_id(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn installed_package_json_path(backends_dir: &Path, package_name: &str) -> PathBuf {
    backends_dir
        .join("node_modules")
        .join(package_name)
        .join("package.json")
}

fn infer_package_bin(manifest: &serde_json::Value, explicit_bin: Option<&str>) -> Result<String> {
    if let Some(bin) = explicit_bin {
        if bin.trim().is_empty() {
            anyhow::bail!("--bin must not be empty");
        }
        return Ok(bin.to_string());
    }
    match manifest.get("bin") {
        Some(serde_json::Value::String(_)) => {
            let name = manifest
                .get("name")
                .and_then(serde_json::Value::as_str)
                .context("package has a string `bin` but no package `name`")?;
            default_server_id_from_package_name(name)
        }
        Some(serde_json::Value::Object(bins)) if bins.len() == 1 => {
            Ok(bins.keys().next().expect("one bin").to_string())
        }
        Some(serde_json::Value::Object(bins)) => {
            let name = manifest
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let preferred = name
                .rsplit('/')
                .next()
                .map(sanitize_server_id)
                .unwrap_or_default();
            if bins.contains_key(&preferred) {
                return Ok(preferred);
            }
            anyhow::bail!(
                "package exposes multiple binaries; rerun with --bin <name>. Available: {}",
                bins.keys().cloned().collect::<Vec<_>>().join(", ")
            );
        }
        _ => anyhow::bail!("package does not expose a bin; rerun with --bin <name> if needed"),
    }
}

struct InstalledServerOverlay<'a> {
    server_id: &'a str,
    package_spec: &'a str,
    bin: &'a str,
    args: &'a [String],
    group: &'a str,
}

fn render_installed_server_overlay(server: InstalledServerOverlay<'_>) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Installed from {}\n", server.package_spec));
    out.push_str(&format!("id: {}\n", server.server_id));
    out.push_str(&format!("display_name: \"{}\"\n", server.server_id));
    out.push_str("enabled: true\n");
    out.push_str("group_memberships:\n");
    out.push_str(&format!("  - {}\n", server.group));
    out.push_str("runtime:\n");
    out.push_str("  transport: stdio\n");
    out.push_str(&format!("  command: {:?}\n", server.bin));
    out.push_str("  args:\n");
    for arg in server.args {
        out.push_str(&format!("    - {arg:?}\n"));
    }
    out.push_str("  lazy: true\n");
    out
}

fn backend_manifest_requires_pnpm_install(package_json: &Path) -> Result<bool> {
    let text = fs::read_to_string(package_json)
        .with_context(|| format!("failed to read {}", package_json.display()))?;
    let root: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("invalid package manifest {}", package_json.display()))?;
    Ok(json_object_has_entries(&root, "dependencies")
        || json_object_has_entries(&root, "optionalDependencies"))
}

fn json_object_has_entries(root: &serde_json::Value, key: &str) -> bool {
    root.get(key)
        .and_then(serde_json::Value::as_object)
        .is_some_and(|object| !object.is_empty())
}

fn run_chrome_for_testing_install(paths: &NativeInstallPaths) -> Result<()> {
    let install_path = paths.chrome_for_testing_dir.to_string_lossy().into_owned();
    let status = Command::new("npx")
        .args([
            "--yes",
            "@puppeteer/browsers",
            "install",
            "chrome@stable",
            "--path",
            install_path.as_str(),
        ])
        .status()
        .context("failed to run Chrome for Testing install via npx @puppeteer/browsers")?;
    if !status.success() {
        anyhow::bail!("Chrome for Testing install failed with {status}");
    }
    Ok(())
}

fn migrate_legacy_launch_agent(paths: &NativeInstallPaths, skip_launchctl: bool) -> Result<bool> {
    if !paths.legacy_launch_agent_file.exists() {
        return Ok(false);
    }
    if !skip_launchctl {
        unload_launch_agent_label(
            LEGACY_LAUNCH_AGENT_LABEL,
            Some(&paths.legacy_launch_agent_file),
        );
    }
    fs::remove_file(&paths.legacy_launch_agent_file)?;
    Ok(true)
}

fn load_launch_agent(paths: &NativeInstallPaths) -> Result<()> {
    let domain = format!("gui/{}", current_uid());
    let service = format!("{domain}/{LAUNCH_AGENT_LABEL}");
    unload_launch_agent_label(LAUNCH_AGENT_LABEL, Some(&paths.launch_agent_file));
    let bootstrap = Command::new("launchctl")
        .args([
            "bootstrap",
            &domain,
            paths.launch_agent_file.to_string_lossy().as_ref(),
        ])
        .status()
        .context("failed to run launchctl bootstrap")?;
    if !bootstrap.success() {
        anyhow::bail!("launchctl bootstrap failed with {bootstrap}");
    }
    let _ = Command::new("launchctl")
        .args(["kickstart", "-k", &service])
        .status();
    Ok(())
}

fn unload_launch_agent_label(label: &str, plist: Option<&Path>) {
    let domain = format!("gui/{}", current_uid());
    let service = format!("{domain}/{label}");
    let _ = Command::new("launchctl")
        .args(["bootout", &service])
        .status();
    if let Some(plist) = plist {
        let _ = Command::new("launchctl")
            .args(["bootout", &domain, plist.to_string_lossy().as_ref()])
            .status();
    }
}

fn prompt_add_to_path() -> Result<bool> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Ok(false);
    }
    print!("Add ~/.mcp-gateway/bin to PATH for the `mcpgateway` command? [y/N] ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(
        input.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn shell_profile_path(home: &Path) -> PathBuf {
    let shell = std::env::var("SHELL").unwrap_or_default();
    if shell.ends_with("zsh") {
        home.join(".zshrc")
    } else if shell.ends_with("bash") {
        home.join(".bashrc")
    } else {
        home.join(".profile")
    }
}

fn ensure_path_profile_entry(profile: &Path) -> Result<bool> {
    let existing = fs::read_to_string(profile).unwrap_or_default();
    if existing.contains(".mcp-gateway/bin") {
        return Ok(false);
    }
    if let Some(parent) = profile.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut next = existing;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str("\n# Added by mcpgateway\n");
    next.push_str("export PATH=\"$HOME/.mcp-gateway/bin:$PATH\"\n");
    fs::write(profile, next)?;
    Ok(true)
}

fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

fn render_native_env(existing: Option<&str>) -> String {
    render_native_env_inner(existing, None)
}

fn render_native_env_with_provisioned_chrome(
    existing: Option<&str>,
    executable_path: &Path,
) -> String {
    render_native_env_inner(
        existing,
        Some((
            "MCP_GATEWAY_CHROME_FOR_TESTING_EXECUTABLE",
            executable_path.to_string_lossy().to_string(),
        )),
    )
}

fn render_native_env_inner(existing: Option<&str>, provisioned: Option<(&str, String)>) -> String {
    let mut lines = Vec::new();
    let provisioned_key = provisioned.as_ref().map(|(key, _)| *key);
    let mut saw_provisioned = false;

    if let Some(existing) = existing {
        for line in existing.lines() {
            if line.starts_with("CHROME_REMOTE_DEBUGGING_URL=") {
                continue;
            }
            if provisioned_key.is_some_and(|key| line.starts_with(&format!("{key}="))) {
                if !saw_provisioned {
                    if let Some((key, value)) = &provisioned {
                        lines.push(format!("{key}={value}"));
                    }
                }
                saw_provisioned = true;
                continue;
            }
            if !line.trim().is_empty() {
                lines.push(line.to_string());
            }
        }
    }

    if let Some((key, value)) = provisioned {
        if !saw_provisioned {
            lines.push(format!("{key}={value}"));
        }
    }

    format!("{}\n", lines.join("\n"))
}

fn render_native_config() -> Result<String> {
    Ok(mcp_gateway::config::render_native_empty_core_yaml()?)
}

pub(crate) fn write_if_changed(path: &std::path::Path, content: &str) -> Result<bool> {
    if fs::read_to_string(path).ok().as_deref() == Some(content) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(true)
}

fn find_chrome_for_testing_executable(root: &Path) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    let mut candidates = Vec::new();
    while let Some(path) = stack.pop() {
        let Ok(entries) = fs::read_dir(path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.file_name().and_then(|name| name.to_str()) != Some("Google Chrome for Testing")
            {
                continue;
            }
            let path_text = path.to_string_lossy();
            if path_text.contains("Google Chrome for Testing.app/Contents/MacOS/") {
                candidates.push(path);
            }
        }
    }
    candidates.sort();
    candidates.pop()
}

#[cfg(unix)]
fn make_executable(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn native_env_is_empty_core_by_default_and_omits_legacy_chrome_debug_url() {
        let existing = "\
CHROME_REMOTE_DEBUGGING_URL=http://old-debug-url
";

        let rendered = super::render_native_env(Some(existing));

        assert!(!rendered.contains("CHROME_REMOTE_DEBUGGING_URL"));
        assert!(!rendered.contains("old-debug-url"));
        assert!(!rendered.contains("GITHUB_TOKEN"));
        assert!(!rendered.contains("CONTEXT7_API_KEY"));
        assert_eq!(rendered, "\n");
    }

    #[test]
    fn native_env_writes_chrome_for_testing_path_only_when_provisioned() {
        let rendered = super::render_native_env_with_provisioned_chrome(
            None,
            Path::new("/tmp/Google Chrome for Testing"),
        );

        assert!(rendered.contains(
            "MCP_GATEWAY_CHROME_FOR_TESTING_EXECUTABLE=/tmp/Google Chrome for Testing\n"
        ));
        assert!(!rendered.contains("chrome-devtools"));
        assert!(!rendered.contains("GITHUB_TOKEN"));
        assert!(!rendered.contains("CONTEXT7_API_KEY"));
    }

    #[test]
    fn native_config_uses_empty_core_defaults() {
        let rendered = super::render_native_config().unwrap();

        for expected in ["servers: {}", "servers: []", "managed_clients: []"] {
            assert!(
                rendered.contains(expected),
                "missing {expected} in:\n{rendered}"
            );
        }

        for forbidden in [
            "/Users/example-dev",
            "CHROME_REMOTE_DEBUGGING_URL",
            "GITHUB_TOKEN",
            "CONTEXT7_API_KEY",
            "chrome-devtools",
            "github",
            "filesystem",
            "shopify-dev-mcp",
            "context7",
            "chrome-devtools-mcp",
            "mcp-server-github",
            "mcp-server-filesystem",
            "9222",
            "--browser-url",
            "--remote-debugging-port",
            "/Applications/Google Chrome.app",
            "open -na",
            "docker",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "unexpected {forbidden} in native config:\n{rendered}"
            );
        }
    }

    #[test]
    fn empty_backend_manifest_does_not_require_pnpm_install() {
        let home = tempfile::tempdir().expect("home");
        let package_json = home.path().join("package.json");
        std::fs::write(
            &package_json,
            r#"{"name":"mcp-gateway-backends","private":true}"#,
        )
        .unwrap();

        assert!(
            !super::backend_manifest_requires_pnpm_install(&package_json).expect("manifest parses")
        );
    }

    #[test]
    fn backend_manifest_with_runtime_dependencies_requires_pnpm_install() {
        let home = tempfile::tempdir().expect("home");
        let package_json = home.path().join("package.json");
        std::fs::write(
            &package_json,
            r#"{"name":"mcp-gateway-backends","private":true,"dependencies":{"example-mcp":"latest"}}"#,
        )
        .unwrap();

        assert!(
            super::backend_manifest_requires_pnpm_install(&package_json).expect("manifest parses")
        );
    }

    #[test]
    fn backend_project_is_created_on_demand_without_repo_manifest() {
        let home = tempfile::tempdir().expect("home");
        let backends_dir = home.path().join(".mcp-gateway/mcp-backends");

        let changed = super::ensure_backend_project(&backends_dir).expect("create project");

        assert!(changed.contains(&backends_dir.join("package.json")));
        let package_json = std::fs::read_to_string(backends_dir.join("package.json")).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&package_json).unwrap();
        assert_eq!(manifest["name"], "mcpgateway-backends");
        assert_eq!(manifest["private"], true);
        assert!(manifest.get("dependencies").is_none());
    }

    #[test]
    fn homebrew_install_provisions_state_without_copying_binaries_or_writing_plist() {
        let home = tempfile::tempdir().expect("home");
        let paths = mcp_gateway::native::NativeInstallPaths::for_home(home.path());

        let changed = super::install_native(super::InstallNativeOptions {
            clients: vec![],
            group: "coding".to_string(),
            provision: vec![],
            skip_pnpm_install: true,
            skip_launchctl: false,
            add_to_path: false,
            no_path_prompt: true,
            homebrew: true,
            home: home.path().to_path_buf(),
        })
        .expect("homebrew install provisions local state");

        assert!(paths.config_file.exists());
        assert!(paths.config_servers_d_dir.exists());
        assert!(paths.mcp_dir.exists());
        assert!(paths.logs_dir.exists());
        assert!(paths.run_dir.exists());
        assert!(!paths.bin_dir.join("mcpgateway").exists());
        assert!(!paths.bin_dir.join("mcp-gateway").exists());
        assert!(!paths.bin_dir.join("mcp-gateway-bridge").exists());
        assert!(!paths.launch_agent_file.exists());
        assert!(!changed.contains(&paths.launch_agent_file));
    }

    #[test]
    fn detects_homebrew_executables_from_cellar_opt_or_prefix_bin_paths() {
        let arm_prefix = Path::new("/opt/homebrew");
        let intel_prefix = Path::new("/usr/local");

        assert_eq!(
            super::homebrew_prefix_from_executable(
                &arm_prefix.join("Cellar/mcp-gateway/0.1.0/bin/mcpgateway")
            ),
            Some(arm_prefix.to_path_buf())
        );
        assert_eq!(
            super::homebrew_prefix_from_executable(
                &arm_prefix.join("opt/mcp-gateway/bin/mcpgateway")
            ),
            Some(arm_prefix.to_path_buf())
        );
        assert_eq!(
            super::homebrew_prefix_from_executable(&intel_prefix.join("bin/mcpgateway")),
            Some(intel_prefix.to_path_buf())
        );
        assert_eq!(
            super::homebrew_prefix_from_executable(Path::new(
                "/Users/dev/project/target/release/mcpgateway"
            )),
            None
        );
    }

    #[test]
    fn legacy_launch_agent_plist_is_removed_during_skip_launchctl_migration() {
        let home = tempfile::tempdir().expect("home");
        let paths = mcp_gateway::native::NativeInstallPaths::for_home(home.path());
        std::fs::create_dir_all(paths.legacy_launch_agent_file.parent().unwrap()).unwrap();
        std::fs::write(&paths.legacy_launch_agent_file, "legacy plist").unwrap();

        let changed = super::migrate_legacy_launch_agent(&paths, true).expect("migrate");

        assert!(changed);
        assert!(!paths.legacy_launch_agent_file.exists());
        assert!(!paths.launch_agent_file.exists());
    }

    #[test]
    fn package_spec_derives_package_name_and_default_server_id() {
        let scoped = super::parse_package_spec("@scope/server-example@1.2.3").unwrap();
        assert_eq!(scoped.package_name, "@scope/server-example");
        assert_eq!(scoped.default_server_id, "server-example");

        let unscoped = super::parse_package_spec("plain-mcp@latest").unwrap();
        assert_eq!(unscoped.package_name, "plain-mcp");
        assert_eq!(unscoped.default_server_id, "plain-mcp");
    }

    #[test]
    fn package_bin_inference_prefers_explicit_bin_then_single_bin() {
        let single = serde_json::json!({
            "name": "@scope/server-example",
            "bin": { "server-example": "./dist/index.js" }
        });
        assert_eq!(
            super::infer_package_bin(&single, None).unwrap(),
            "server-example"
        );
        assert_eq!(
            super::infer_package_bin(&single, Some("custom-bin")).unwrap(),
            "custom-bin"
        );
    }

    #[test]
    fn installed_server_overlay_is_enabled_and_joins_group() {
        let overlay = super::render_installed_server_overlay(super::InstalledServerOverlay {
            server_id: "server-example",
            package_spec: "@scope/server-example@latest",
            bin: "server-example",
            args: &["--mode".to_string(), "stdio".to_string()],
            group: "coding",
        });

        assert!(overlay.contains("id: server-example\n"));
        assert!(overlay.contains("enabled: true\n"));
        assert!(overlay.contains("command: \"server-example\"\n"));
        assert!(overlay.contains("    - \"--mode\"\n"));
        assert!(overlay.contains("    - \"stdio\"\n"));
        assert!(overlay.contains("  - coding\n"));
        assert!(overlay.contains("Installed from @scope/server-example@latest"));
    }

    #[test]
    fn path_profile_update_adds_mcpgateway_bin_once() {
        let home = tempfile::tempdir().expect("home");
        let profile = home.path().join(".zshrc");

        assert!(super::ensure_path_profile_entry(&profile).expect("first add"));
        assert!(!super::ensure_path_profile_entry(&profile).expect("second add"));

        let profile_text = std::fs::read_to_string(profile).unwrap();
        assert_eq!(profile_text.matches(".mcp-gateway/bin").count(), 1);
        assert!(profile_text.contains("# Added by mcpgateway"));
    }

    #[test]
    fn legacy_ctl_binary_is_removed_after_command_rename() {
        let home = tempfile::tempdir().expect("home");
        let paths = mcp_gateway::native::NativeInstallPaths::for_home(home.path());
        std::fs::create_dir_all(&paths.bin_dir).unwrap();
        std::fs::write(paths.bin_dir.join("mcp-gatewayctl"), "old").unwrap();

        assert!(super::remove_legacy_ctl_binary(&paths).expect("remove old binary"));
        assert!(!paths.bin_dir.join("mcp-gatewayctl").exists());
        assert!(!super::remove_legacy_ctl_binary(&paths).expect("already removed"));
    }

    #[test]
    fn install_clients_are_empty_without_cli_or_config_selection() {
        let cfg = mcp_gateway::config::Config::default();

        let selected = super::install_clients_from_cli_or_config(Vec::new(), &cfg);

        assert!(selected.is_empty());
    }

    #[test]
    fn install_clients_use_config_when_cli_selection_is_empty() {
        let mut cfg = mcp_gateway::config::Config::default();
        cfg.clients.managed_clients = vec!["codex".to_string(), "vscode".to_string()];

        let selected = super::install_clients_from_cli_or_config(Vec::new(), &cfg);

        assert_eq!(
            selected,
            vec![
                mcp_gateway::client_config::ClientKind::Codex,
                mcp_gateway::client_config::ClientKind::Vscode
            ]
        );
    }

    #[test]
    fn install_clients_cli_selection_overrides_config_selection() {
        let mut cfg = mcp_gateway::config::Config::default();
        cfg.clients.managed_clients = vec!["codex".to_string()];

        let selected = super::install_clients_from_cli_or_config(
            vec![mcp_gateway::client_config::ClientKind::ClaudeCode],
            &cfg,
        );

        assert_eq!(
            selected,
            vec![mcp_gateway::client_config::ClientKind::ClaudeCode]
        );
    }

    #[test]
    fn finds_chrome_for_testing_executable_in_macos_install_tree() {
        let home = tempfile::tempdir().expect("home");
        let executable = home.path()
            .join(".mcp-gateway/browsers/chrome-for-testing/chrome/mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, "").unwrap();

        let found = super::find_chrome_for_testing_executable(
            &home.path().join(".mcp-gateway/browsers/chrome-for-testing"),
        )
        .expect("find executable");

        assert_eq!(found, executable);
    }

    #[test]
    fn chrome_separation_warning_flags_regular_chrome_process_tree() {
        let tree = "\
123 browser-mcp-server
124 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome --headless=new
";

        let warning = super::cmd::status::chrome_separation_warning("browser-tools", tree);

        assert!(warning.contains("browser-tools"));
        assert!(warning.contains("regular Chrome"));
        assert!(warning.contains("Chrome for Testing"));
    }
}
