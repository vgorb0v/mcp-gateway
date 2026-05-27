use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use mcp_gateway::config::Config;
use mcp_gateway::http;
use mcp_gateway::metrics::{spawn_sampler_task, ProcessSampler};
use mcp_gateway::native::{load_env_file, write_state_file, NativeInstallPaths};
use mcp_gateway::registry::BackendRegistry;
use mcp_gateway::sessions::SessionManager;
use tokio::net::TcpListener;
use tracing::warn;

#[derive(Debug, Parser)]
#[command(name = "mcp-gateway", version, about = "Local MCP Gateway daemon")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Serve {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        env_file: Option<PathBuf>,
        #[arg(long)]
        state_file: Option<PathBuf>,
        #[arg(long)]
        capability_cache_file: Option<PathBuf>,
    },
}

// The gateway daemon is a user-session background service. Two worker threads
// are enough to handle concurrent backend stdio + HTTP traffic without
// over-subscribing the host. Override with TOKIO_WORKER_THREADS if needed for
// special deployments.
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    match cli.command {
        Commands::Serve {
            config,
            env_file,
            state_file,
            capability_cache_file,
        } => {
            let native_paths = default_native_paths();
            let config = config.unwrap_or_else(|| native_paths.config_file.clone());
            let env_file = env_file.unwrap_or_else(|| native_paths.env_file.clone());
            let state_file = state_file.unwrap_or_else(|| native_paths.state_file.clone());
            let capability_cache_file =
                capability_cache_file.unwrap_or_else(|| native_paths.capability_cache_file.clone());
            load_env_file(&env_file)?;
            let cfg = Arc::new(Config::load_file(&config)?);
            for (name, server) in &cfg.servers {
                if server.dangerous() {
                    warn!(
                        server = %name,
                        "dangerous MCP backend enabled; connected agents may inspect or modify sensitive local state"
                    );
                }
            }

            let addr: SocketAddr = cfg.listen.parse()?;
            let registry =
                BackendRegistry::new_with_capability_cache(cfg.clone(), capability_cache_file)?;
            let shutdown_registry = registry.clone();
            let sessions = SessionManager::new(cfg.clone());
            sessions.start_notification_forwarder(registry.notification_stream());
            let sampler = ProcessSampler::new();
            spawn_sampler_task(
                registry.clone(),
                sampler,
                Duration::from_secs(cfg.metrics.sample_interval_secs),
            );
            let app = http::app(cfg.clone(), registry, sessions);
            let listener = TcpListener::bind(addr).await?;
            let bound_addr = listener.local_addr()?;
            write_state_file(&state_file, bound_addr)?;
            tracing::info!(addr = %bound_addr, "mcp-gateway listening");
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await?;
            shutdown_registry.shutdown_all().await;
        }
    }
    Ok(())
}

fn default_native_paths() -> NativeInstallPaths {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    NativeInstallPaths::for_home(home)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
